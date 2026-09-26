//! `DockerRuntime`: docker stems through the Docker Engine API (`bollard`).
//! See `docs/docker.md` for labels, naming, stop semantics, adoption and
//! the error mapping.
//!
//! Everything that decides something (spec mapping, adoption, orphan
//! selection, stop classification, log parsing, host resolution) is a pure
//! function tested without a Docker daemon; the methods on
//! [`DockerRuntime`] only move data between those functions and the API.
//! The compose runtime (15) reuses the container-level operations
//! ([`DockerRuntime::attach`], `describe`, `output_stream`, `stop`,
//! [`DockerRuntime::remove_container_id`], [`verify_labels`]) once it has a
//! container id from `docker compose ps`.

mod context;
mod spec;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::container::LogOutput;
use bollard::errors::Error as DockerError;
use bollard::models::{ContainerInspectResponse, ContainerSummary, NetworkCreateRequest};
use bollard::query_parameters::{
    BuildImageOptions, CreateContainerOptions, CreateImageOptions, InspectContainerOptions,
    InspectNetworkOptions, KillContainerOptions, ListContainersOptions, LogsOptions,
    RemoveContainerOptions, RemoveVolumeOptions, RestartContainerOptions, StopContainerOptions,
    WaitContainerOptions,
};
use bollard::{API_DEFAULT_VERSION, Docker};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::os::StartTime;
use crate::output::{
    LineSplitter, OUTPUT_CHANNEL_CAPACITY, OutputLine, OutputStream, OutputStreamKind,
};
use crate::runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, Orphan, OrphanKind, OrphanScope, Runtime,
    RuntimeError, RuntimeFacts, StartSpec, StopOutcome,
};

pub use context::{DockerIgnore, EXTERNAL_DOCKERFILE, context_tar};
pub use spec::{
    BuildSpec, ContainerSpec, HealthcheckSpec, ImageProgress, LABEL_RUN_ID, LABEL_SPEC_HASH,
    LABEL_STEM, LABEL_WORKSPACE, PortMapping, PortProto, ProgressSink, PullCoalescer, PullProgress,
    VolumeMount, container_name, create_body, default_network, grace_secs, split_image_ref,
    to_bollard, volume_name,
};

/// Lines of build output kept for [`RuntimeError::BuildFailed`].
pub const BUILD_TAIL_LINES: usize = 20;
/// Hint attached to [`RuntimeError::DockerUnavailable`].
pub const UNAVAILABLE_HINT: &str =
    "start Docker Desktop (or Colima), or point DOCKER_HOST at a running daemon";
/// Networks that exist in every daemon and are never created.
const BUILTIN_NETWORKS: [&str; 3] = ["bridge", "host", "none"];
/// How long to wait for a container to die after `kill`.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How to reach the Docker daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerOptions {
    /// `DOCKER_HOST`-style address (`unix:///path`, `tcp://host:port`,
    /// `http://...` or a bare socket path). `None`: `$DOCKER_HOST`, then the
    /// usual socket locations.
    pub host: Option<String>,
    /// Per-request timeout (at least 1 s; long streams are not bounded) and
    /// the bound on the initial ping.
    pub timeout: Duration,
    /// Workspace this runtime serves: used by the trait-level `adopt`
    /// (label check) and `scan_orphans` falls back to it.
    pub workspace: Option<String>,
}

impl Default for DockerOptions {
    fn default() -> Self {
        Self {
            host: None,
            timeout: Duration::from_secs(30),
            workspace: None,
        }
    }
}

/// A container labelled for the workspace but absent from state (FR-CR-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerOrphan {
    pub id: String,
    /// Name without the leading `/`.
    pub name: String,
    /// Value of the `stems.stem` label.
    pub stem_label: Option<String>,
    /// Docker state (`running`, `exited`, `created`, ...).
    pub state: String,
    pub image: Option<String>,
}

impl From<ContainerOrphan> for Orphan {
    fn from(o: ContainerOrphan) -> Self {
        Orphan {
            kind: OrphanKind::Container,
            port: None,
            pid: None,
            pgid: None,
            command: match &o.image {
                Some(img) => format!("{img} ({}, {})", o.name, o.state),
                None => format!("{} ({})", o.name, o.state),
            },
            container_id: Some(o.id),
            stem: o.stem_label,
            // The workspace label proves stems created it.
            matches_start_command: true,
        }
    }
}

/// Why a recorded container was not adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptRejection {
    NoContainerId,
    IdMismatch { found: String },
    WrongWorkspace { found: Option<String> },
    WrongStem { found: Option<String> },
    NotRunning,
}

/// Live, non-serialisable state of a container handle.
struct Live {
    container_id: String,
    /// Weak so the channel closes when the log follower ends (container stopped).
    output: Option<broadcast::WeakSender<OutputLine>>,
    first_rx: Option<broadcast::Receiver<OutputLine>>,
    dropped: Arc<AtomicU64>,
    follower: Option<tokio::task::AbortHandle>,
}

/// Runs [`ContainerSpec`]s through the Docker API. Cheap to share behind an `Arc`.
pub struct DockerRuntime {
    docker: Docker,
    host: String,
    workspace: Option<String>,
    next_id: AtomicU64,
    live: Mutex<HashMap<HandleId, Live>>,
}

impl std::fmt::Debug for DockerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerRuntime")
            .field("host", &self.host)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested without Docker)
// ---------------------------------------------------------------------------

/// Where to connect: explicit host, else `$DOCKER_HOST`, else the first
/// existing well-known socket, else `/var/run/docker.sock`.
pub fn resolve_host(
    explicit: Option<&str>,
    env: Option<&str>,
    home: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> String {
    if let Some(h) = explicit.or(env).filter(|h| !h.trim().is_empty()) {
        return h.trim().to_string();
    }
    let mut candidates = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(home) = home {
        for rel in [
            ".docker/run/docker.sock",
            ".colima/default/docker.sock",
            ".orbstack/run/docker.sock",
            ".rd/docker.sock",
        ] {
            candidates.push(home.join(rel));
        }
    }
    let chosen = candidates
        .iter()
        .find(|p| exists(p))
        .unwrap_or(&candidates[0]);
    format!("unix://{}", chosen.display())
}

/// Errors that mean "the daemon is not there" rather than "it said no".
pub fn is_unavailable(e: &DockerError) -> bool {
    matches!(
        e,
        DockerError::SocketNotFoundError(_)
            | DockerError::IOError { .. }
            | DockerError::HyperResponseError { .. }
            | DockerError::HyperLegacyError { .. }
            | DockerError::RequestTimeoutError
            | DockerError::UnsupportedURISchemeError { .. }
    )
}

fn status_code(e: &DockerError) -> Option<u16> {
    match e {
        DockerError::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

fn is_not_found(e: &DockerError) -> bool {
    status_code(e) == Some(404)
}

fn server_message(e: &DockerError) -> String {
    match e {
        DockerError::DockerResponseServerError { message, .. } => message.clone(),
        DockerError::DockerStreamError { error } => error.clone(),
        other => other.to_string(),
    }
}

fn unavailable(host: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::DockerUnavailable {
        hint: format!("cannot reach the Docker daemon at {host} ({e}); {UNAVAILABLE_HINT}"),
    }
}

/// Parse one `timestamps=true` log line (`<RFC3339Nano> <text>`).
pub fn parse_log_line(raw: &str, stream: OutputStreamKind) -> OutputLine {
    if let Some((ts, text)) = raw.split_once(' ')
        && let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts)
    {
        let ts =
            UNIX_EPOCH + Duration::new(dt.timestamp().max(0) as u64, dt.timestamp_subsec_nanos());
        return OutputLine {
            ts,
            stream,
            text: text.to_string(),
        };
    }
    OutputLine {
        ts: SystemTime::now(),
        stream,
        text: raw.to_string(),
    }
}

fn labels_of(inspect: &ContainerInspectResponse) -> Option<&HashMap<String, String>> {
    inspect.config.as_ref()?.labels.as_ref()
}

fn is_running(inspect: &ContainerInspectResponse) -> bool {
    inspect
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false)
}

fn ids_match(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// Label check shared with the compose runtime: the container carries
/// `stems.workspace=<ws>` and `stems.stem=<stem>`. The run id is *not*
/// compared: an adopted container keeps the run id of the run that made it.
pub fn verify_labels(
    labels: Option<&HashMap<String, String>>,
    workspace: &str,
    stem: &str,
) -> Result<(), AdoptRejection> {
    let get = |k: &str| labels.and_then(|l| l.get(k)).cloned();
    let ws = get(LABEL_WORKSPACE);
    if ws.as_deref() != Some(workspace) {
        return Err(AdoptRejection::WrongWorkspace { found: ws });
    }
    let st = get(LABEL_STEM);
    if st.as_deref() != Some(stem) {
        return Err(AdoptRejection::WrongStem { found: st });
    }
    Ok(())
}

/// Adoption decision for a recorded container: same id, labels
/// `stems.workspace`/`stems.stem` match, and running.
pub fn verify_adoption(
    inspect: &ContainerInspectResponse,
    record: &AdoptRecord,
    workspace: &str,
    stem: &str,
) -> Result<(), AdoptRejection> {
    let want = record
        .container_id
        .as_deref()
        .ok_or(AdoptRejection::NoContainerId)?;
    let found = inspect.id.clone().unwrap_or_default();
    if !ids_match(&found, want) {
        return Err(AdoptRejection::IdMismatch { found });
    }
    verify_labels(labels_of(inspect), workspace, stem)?;
    if !is_running(inspect) {
        return Err(AdoptRejection::NotRunning);
    }
    Ok(())
}

/// The stem a container belongs to according to its name `/<ws>-<stem>`.
fn stem_from_name<'a>(name: &'a str, workspace: &str) -> Option<&'a str> {
    name.trim_start_matches('/')
        .strip_prefix(workspace)?
        .strip_prefix('-')
        .filter(|s| !s.is_empty())
}

/// Host ports bound by the container, sorted and de-duplicated.
pub fn bound_ports(inspect: &ContainerInspectResponse) -> Vec<u16> {
    let mut ports = BTreeSet::new();
    if let Some(map) = inspect
        .network_settings
        .as_ref()
        .and_then(|n| n.ports.as_ref())
    {
        for binding in map.values().flatten().flatten() {
            if let Some(p) = binding.host_port.as_deref().and_then(|p| p.parse().ok()) {
                ports.insert(p);
            }
        }
    }
    ports.into_iter().collect()
}

/// `RuntimeFacts` of a container from its inspect data.
pub fn facts_from_inspect(inspect: &ContainerInspectResponse, dropped_lines: u64) -> RuntimeFacts {
    let state = inspect.state.as_ref();
    let pid = state
        .and_then(|s| s.pid)
        .and_then(|p| i32::try_from(p).ok())
        .unwrap_or(0);
    let health = state
        .and_then(|s| s.health.as_ref())
        .and_then(|h| h.status)
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    RuntimeFacts {
        pid,
        pgid: 0,
        start_time: StartTime(0),
        children: Vec::new(),
        container_id: inspect.id.clone(),
        ports: bound_ports(inspect),
        dropped_lines,
        container_health: health,
    }
}

/// How a `docker stop` ended: exit code 137 (128 + SIGKILL) means Docker had
/// to kill it after `t`; still running means we must `kill` ourselves.
pub fn classify_stop(still_running: bool, exit_code: Option<i64>) -> StopOutcome {
    if still_running || exit_code == Some(137) {
        StopOutcome::Killed
    } else {
        StopOutcome::Graceful
    }
}

/// Recreate on restart when the stored spec hash differs (or is missing).
pub fn needs_recreate(labels: Option<&HashMap<String, String>>, spec: &ContainerSpec) -> bool {
    labels
        .and_then(|l| l.get(LABEL_SPEC_HASH))
        .is_none_or(|h| *h != spec.spec_hash())
}

/// Named volumes to delete with `--volumes`: only `<ws>_*` volumes (never
/// a user's pre-existing volume, never bind mounts).
pub fn volumes_to_remove(inspect: &ContainerInspectResponse) -> Vec<String> {
    let Some(ws) = labels_of(inspect).and_then(|l| l.get(LABEL_WORKSPACE)) else {
        return Vec::new();
    };
    let prefix = format!("{ws}_");
    inspect
        .mounts
        .iter()
        .flatten()
        .filter(|m| m.typ.as_deref() == Some("volume"))
        .filter_map(|m| m.name.clone())
        .filter(|n| n.starts_with(&prefix))
        .collect()
}

/// Containers labelled for `scope.workspace` whose ids are not in `scope.known`.
pub fn select_orphans(summaries: &[ContainerSummary], scope: &OrphanScope) -> Vec<ContainerOrphan> {
    summaries
        .iter()
        .filter_map(|c| {
            let id = c.id.clone()?;
            let labels = c.labels.as_ref();
            if labels.and_then(|l| l.get(LABEL_WORKSPACE)) != Some(&scope.workspace) {
                return None;
            }
            let known = scope
                .known
                .iter()
                .filter_map(|r| r.container_id.as_deref())
                .any(|k| ids_match(k, &id));
            if known {
                return None;
            }
            Some(ContainerOrphan {
                name: c
                    .names
                    .as_ref()
                    .and_then(|n| n.first())
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_default(),
                stem_label: labels.and_then(|l| l.get(LABEL_STEM)).cloned(),
                state: c.state.map(|s| s.to_string()).unwrap_or_default(),
                image: c.image.clone(),
                id,
            })
        })
        .collect()
}

fn push_tail(tail: &mut Vec<String>, line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    tail.push(line.to_string());
    if tail.len() > BUILD_TAIL_LINES {
        tail.remove(0);
    }
}

// ---------------------------------------------------------------------------
// Live runtime
// ---------------------------------------------------------------------------

fn client_for(host: &str, timeout: Duration) -> Result<Docker, DockerError> {
    let secs = timeout.as_secs().max(1);
    if host.starts_with("unix://") {
        Docker::connect_with_unix(host, secs, API_DEFAULT_VERSION)
    } else if host.starts_with('/') {
        Docker::connect_with_unix(&format!("unix://{host}"), secs, API_DEFAULT_VERSION)
    } else if host.starts_with("tcp://") || host.starts_with("http://") {
        Docker::connect_with_http(host, secs, API_DEFAULT_VERSION)
    } else {
        Err(DockerError::UnsupportedURISchemeError {
            uri: host.to_string(),
        })
    }
}

/// Follow a container's logs into `tx` until the stream ends (container
/// stopped or removed). `since` is unix seconds (0 = from the beginning).
async fn follow_logs(docker: Docker, id: String, since: i32, tx: broadcast::Sender<OutputLine>) {
    let mut stream = docker.logs(
        &id,
        Some(LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            timestamps: true,
            since,
            ..Default::default()
        }),
    );
    let mut out = LineSplitter::new();
    let mut err = LineSplitter::new();
    let send = |raw: String, kind| {
        let _ = tx.send(parse_log_line(&raw, kind));
    };
    while let Some(item) = stream.next().await {
        match item {
            Ok(LogOutput::StdOut { message }) | Ok(LogOutput::Console { message }) => {
                out.push(&message)
                    .into_iter()
                    .for_each(|l| send(l, OutputStreamKind::Out));
            }
            Ok(LogOutput::StdErr { message }) => {
                err.push(&message)
                    .into_iter()
                    .for_each(|l| send(l, OutputStreamKind::Err));
            }
            Ok(LogOutput::StdIn { .. }) => {}
            Err(e) => {
                tracing::debug!(container = %id, error = %e, "log stream ended with an error");
                break;
            }
        }
    }
    if let Some(l) = out.finish() {
        send(l, OutputStreamKind::Out);
    }
    if let Some(l) = err.finish() {
        send(l, OutputStreamKind::Err);
    }
}

impl DockerRuntime {
    /// Connect and ping. Any failure is [`RuntimeError::DockerUnavailable`].
    pub async fn connect(opts: DockerOptions) -> Result<Self, RuntimeError> {
        let env = std::env::var("DOCKER_HOST").ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let host = resolve_host(opts.host.as_deref(), env.as_deref(), home.as_deref(), |p| {
            p.exists()
        });
        let docker = client_for(&host, opts.timeout).map_err(|e| unavailable(&host, e))?;
        let rt = Self {
            docker,
            host,
            workspace: opts.workspace,
            next_id: AtomicU64::new(1),
            live: Mutex::new(HashMap::new()),
        };
        match tokio::time::timeout(opts.timeout.max(Duration::from_millis(100)), rt.ping()).await {
            Ok(r) => r?,
            Err(_) => return Err(unavailable(&rt.host, "ping timed out")),
        }
        Ok(rt)
    }

    /// `GET /_ping`.
    pub async fn ping(&self) -> Result<(), RuntimeError> {
        self.docker
            .ping()
            .await
            .map(|_| ())
            .map_err(|e| unavailable(&self.host, e))
    }

    /// The address this runtime talks to.
    pub fn host(&self) -> &str {
        &self.host
    }

    fn map_err(&self, e: DockerError) -> RuntimeError {
        if is_unavailable(&e) {
            unavailable(&self.host, e)
        } else {
            RuntimeError::Container(server_message(&e))
        }
    }

    fn alloc_id(&self) -> HandleId {
        HandleId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, HashMap<HandleId, Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn container_of<'a>(&self, h: &'a Handle) -> Result<&'a str, RuntimeError> {
        h.container_id().ok_or_else(|| {
            RuntimeError::Unsupported(format!("handle {} is not a container", h.id()))
        })
    }

    /// Inspect; `Ok(None)` when the container does not exist.
    pub async fn inspect(
        &self,
        id: &str,
    ) -> Result<Option<ContainerInspectResponse>, RuntimeError> {
        match self
            .docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await
        {
            Ok(i) => Ok(Some(i)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(self.map_err(e)),
        }
    }

    /// Create `<ws>_net` (or the override) unless it exists or is built in.
    async fn ensure_network(&self, spec: &ContainerSpec) -> Result<(), RuntimeError> {
        let name = spec.network_name();
        if BUILTIN_NETWORKS.contains(&name.as_str()) {
            return Ok(());
        }
        match self
            .docker
            .inspect_network(&name, None::<InspectNetworkOptions>)
            .await
        {
            Ok(_) => return Ok(()),
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(self.map_err(e)),
        }
        let labels = HashMap::from([(LABEL_WORKSPACE.to_string(), spec.workspace.clone())]);
        match self
            .docker
            .create_network(NetworkCreateRequest {
                name: name.clone(),
                driver: Some("bridge".into()),
                labels: Some(labels),
                ..Default::default()
            })
            .await
        {
            Ok(_) => Ok(()),
            // Raced with another creator.
            Err(e) if status_code(&e) == Some(409) => Ok(()),
            Err(e) => Err(self.map_err(e)),
        }
    }

    /// Pull `image` unless it is present locally.
    async fn ensure_image(&self, image: &str, progress: &ProgressSink) -> Result<(), RuntimeError> {
        match self.docker.inspect_image(image).await {
            Ok(_) => return Ok(()),
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(self.map_err(e)),
        }
        let (repo, tag) = split_image_ref(image);
        let failed = |message: String| RuntimeError::ImagePullFailed {
            image: image.to_string(),
            message,
        };
        let mut stream = self.docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(repo),
                tag: Some(tag),
                ..Default::default()
            }),
            None,
            None,
        );
        let mut coalescer = PullCoalescer::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(info) => {
                    if let Some(msg) = info.error_detail.and_then(|d| d.message) {
                        return Err(failed(msg));
                    }
                    if let Some(p) =
                        coalescer.observe(image, info.id.as_deref(), info.status.as_deref())
                    {
                        progress.send(ImageProgress::Pull(p));
                    }
                }
                Err(e) if is_unavailable(&e) => return Err(unavailable(&self.host, e)),
                Err(e) => return Err(failed(server_message(&e))),
            }
        }
        Ok(())
    }

    /// `docker build` the context, tagged `stems/<ws>/<stem>:<run_id>`.
    async fn build_image(
        &self,
        spec: &ContainerSpec,
        build: &BuildSpec,
    ) -> Result<(), RuntimeError> {
        let tag = spec.build_tag().expect("build spec has a tag");
        let (context, dockerfile) = (build.context.clone(), build.dockerfile.clone());
        let (tar, dockerfile) =
            tokio::task::spawn_blocking(move || context_tar(&context, &dockerfile))
                .await
                .map_err(std::io::Error::other)??;
        let labels = HashMap::from([
            (LABEL_WORKSPACE.to_string(), spec.workspace.clone()),
            (LABEL_STEM.to_string(), spec.stem.clone()),
        ]);
        let mut stream = self.docker.build_image(
            BuildImageOptions {
                dockerfile,
                t: Some(tag),
                rm: true,
                forcerm: true,
                labels: Some(labels),
                ..Default::default()
            },
            None,
            Some(bollard::body_full(bytes::Bytes::from(tar))),
        );
        let mut tail = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(info) => {
                    if let Some(s) = info.stream.as_deref() {
                        for line in s.lines() {
                            push_tail(&mut tail, line);
                            spec.progress.send(ImageProgress::Build {
                                stem: spec.stem.clone(),
                                line: line.to_string(),
                            });
                        }
                    }
                    if let Some(msg) = info.error_detail.and_then(|d| d.message) {
                        push_tail(&mut tail, &msg);
                        return Err(RuntimeError::BuildFailed { tail });
                    }
                }
                Err(e) if is_unavailable(&e) => return Err(unavailable(&self.host, e)),
                Err(e) => {
                    push_tail(&mut tail, &server_message(&e));
                    return Err(RuntimeError::BuildFailed { tail });
                }
            }
        }
        Ok(())
    }

    /// A leftover container with our name: removed if it is ours and not
    /// running; a running one is an error (adopt it or clean orphans).
    async fn clear_name(&self, spec: &ContainerSpec) -> Result<(), RuntimeError> {
        let name = spec.container_name();
        let Some(existing) = self.inspect(&name).await? else {
            return Ok(());
        };
        let id = existing.id.clone().unwrap_or_else(|| name.clone());
        let ours = verify_labels(labels_of(&existing), &spec.workspace, &spec.stem).is_ok();
        if ours && !is_running(&existing) {
            tracing::debug!(container = %id, "removing stopped leftover container");
            return self.remove_container_id(&id, false).await;
        }
        Err(RuntimeError::Container(format!(
            "a container named `{name}` already exists ({}{}); remove it or run `stems doctor --orphans`",
            &id[..id.len().min(12)],
            if is_running(&existing) {
                ", running"
            } else {
                ""
            }
        )))
    }

    async fn start_container(&self, spec: &ContainerSpec) -> Result<Handle, RuntimeError> {
        let image = spec.image_ref().ok_or_else(|| {
            RuntimeError::Container(format!(
                "docker stem `{}` has neither `image` nor `build`",
                spec.stem
            ))
        })?;
        self.ensure_network(spec).await?;
        match &spec.build {
            Some(b) => self.build_image(spec, b).await?,
            None => self.ensure_image(&image, &spec.progress).await?,
        }
        self.clear_name(spec).await?;
        let name = spec.container_name();
        let created = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(name.clone()),
                    ..Default::default()
                }),
                create_body(spec),
            )
            .await
            .map_err(|e| self.map_err(e))?;
        let id = created.id;
        if let Err(e) = self.docker.start_container(&id, None).await {
            let err = self.map_err(e);
            // Do not leave a created-but-never-started container behind.
            let _ = self
                .docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
            return Err(err);
        }
        tracing::debug!(container = %id, %name, "container started");
        Ok(self.register(id, name, 0))
    }

    /// Track a container and start following its logs from `since`
    /// (unix seconds; 0 = from the beginning).
    fn register(&self, container_id: String, name: String, since: i32) -> Handle {
        let id = self.alloc_id();
        let (tx, first_rx) = broadcast::channel(OUTPUT_CHANNEL_CAPACITY);
        let weak = tx.downgrade();
        let task = tokio::spawn(follow_logs(
            self.docker.clone(),
            container_id.clone(),
            since,
            tx,
        ));
        self.registry().insert(
            id,
            Live {
                container_id: container_id.clone(),
                output: Some(weak),
                first_rx: Some(first_rx),
                dropped: Arc::new(AtomicU64::new(0)),
                follower: Some(task.abort_handle()),
            },
        );
        Handle::Container {
            id,
            container_id,
            name,
        }
    }

    fn now_secs() -> i32 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i32::try_from(d.as_secs()).unwrap_or(i32::MAX))
    }

    /// Attach to an existing container by id (compose runtime, adoption):
    /// logs are followed from now on.
    pub async fn attach(&self, container_id: &str) -> Result<Handle, RuntimeError> {
        self.attach_since(container_id, Self::now_secs()).await
    }

    /// [`DockerRuntime::attach`], following logs from `since` (unix
    /// seconds; 0 = from the beginning). The compose runtime passes the
    /// time just before `docker compose up` so start-up output is kept.
    pub async fn attach_since(
        &self,
        container_id: &str,
        since: i32,
    ) -> Result<Handle, RuntimeError> {
        let inspect = self.inspect(container_id).await?.ok_or_else(|| {
            RuntimeError::Container(format!("no such container `{container_id}`"))
        })?;
        let id = inspect
            .id
            .clone()
            .unwrap_or_else(|| container_id.to_string());
        let name = inspect
            .name
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        Ok(self.register(id, name, since))
    }

    /// Adopt the container recorded for `workspace`/`stem` if it is still
    /// the same container, labelled for them, and running.
    pub async fn adopt_container(
        &self,
        record: &AdoptRecord,
        workspace: &str,
        stem: &str,
    ) -> Option<Handle> {
        let want = record.container_id.as_deref()?;
        let inspect = match self.inspect(want).await {
            Ok(Some(i)) => i,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(container = want, error = %e, "adopt: inspect failed");
                return None;
            }
        };
        if let Err(why) = verify_adoption(&inspect, record, workspace, stem) {
            tracing::debug!(container = want, ?why, "adopt: container rejected");
            return None;
        }
        let id = inspect.id.clone()?;
        Some(self.register(id, container_name(workspace, stem), Self::now_secs()))
    }

    /// Stop (if needed) and remove a container; with `volumes`, also its
    /// anonymous volumes and its `<ws>_*` named volumes.
    pub async fn remove(&self, h: &Handle, volumes: bool) -> Result<(), RuntimeError> {
        let id = self.container_of(h)?.to_string();
        self.remove_container_id(&id, volumes).await?;
        self.release(h);
        Ok(())
    }

    /// [`DockerRuntime::remove`] by container id (orphan clean-up).
    pub async fn remove_container_id(&self, id: &str, volumes: bool) -> Result<(), RuntimeError> {
        let Some(inspect) = self.inspect(id).await? else {
            return Ok(());
        };
        match self
            .docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: volumes,
                    ..Default::default()
                }),
            )
            .await
        {
            Ok(()) => {}
            Err(e) if is_not_found(&e) => {}
            Err(e) => return Err(self.map_err(e)),
        }
        if volumes {
            for v in volumes_to_remove(&inspect) {
                match self
                    .docker
                    .remove_volume(&v, None::<RemoveVolumeOptions>)
                    .await
                {
                    Ok(()) => tracing::debug!(volume = %v, "volume removed"),
                    Err(e) if is_not_found(&e) => {}
                    Err(e) => tracing::warn!(volume = %v, error = %e, "volume not removed"),
                }
            }
        }
        Ok(())
    }

    /// Restart with `spec`: recreate when its hash differs from the running
    /// container's `stems.spec_hash` label, else `docker restart`. Either
    /// way the old handle is released and a new one returned.
    pub async fn restart(&self, h: &Handle, spec: &ContainerSpec) -> Result<Handle, RuntimeError> {
        let id = self.container_of(h)?.to_string();
        let inspect = self.inspect(&id).await?;
        let recreate = inspect
            .as_ref()
            .is_none_or(|i| needs_recreate(labels_of(i), spec));
        if recreate {
            if inspect.is_some() {
                self.stop(h, spec.stop_grace).await?;
                self.remove_container_id(&id, false).await?;
            }
            self.release(h);
            return self.start_container(spec).await;
        }
        let since = Self::now_secs();
        self.docker
            .restart_container(
                &id,
                Some(RestartContainerOptions {
                    t: Some(grace_secs(spec.stop_grace) as i32),
                    signal: None,
                }),
            )
            .await
            .map_err(|e| self.map_err(e))?;
        self.release(h);
        Ok(self.register(id, spec.container_name(), since))
    }

    /// Containers labelled for `scope.workspace` that are not in `scope.known`.
    pub async fn scan_container_orphans(
        &self,
        scope: &OrphanScope,
    ) -> Result<Vec<ContainerOrphan>, RuntimeError> {
        let filters = HashMap::from([(
            "label".to_string(),
            vec![format!("{LABEL_WORKSPACE}={}", scope.workspace)],
        )]);
        let list = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
            .map_err(|e| self.map_err(e))?;
        Ok(select_orphans(&list, scope))
    }

    /// Output lines lost to slow subscribers of `h` so far.
    pub fn dropped_lines(&self, h: &Handle) -> u64 {
        self.registry()
            .get(&h.id())
            .map_or(0, |l| l.dropped.load(Ordering::Relaxed))
    }
}

#[async_trait::async_trait]
impl Runtime for DockerRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        match spec {
            StartSpec::Docker(c) => self.start_container(c).await,
            StartSpec::Process(p) => Err(RuntimeError::Unsupported(format!(
                "the docker runtime does not run processes (`{}`)",
                p.command
            ))),
            StartSpec::External { stem } => Err(RuntimeError::Unsupported(format!(
                "`{stem}` is an external stem; the docker runtime does not start it"
            ))),
            StartSpec::Compose(c) => Err(RuntimeError::Unsupported(format!(
                "`{}` is a compose stem; start it with the compose runtime",
                c.stem
            ))),
        }
    }

    /// `docker stop -t <grace>` (SIGTERM, then Docker's own SIGKILL), then
    /// `kill` if it is somehow still running.
    async fn stop(&self, h: &Handle, grace: Duration) -> Result<StopOutcome, RuntimeError> {
        let id = self.container_of(h)?.to_string();
        match self.inspect(&id).await? {
            Some(i) if is_running(&i) => {}
            _ => return Ok(StopOutcome::AlreadyDead),
        }
        let t = grace_secs(grace);
        let stop = self.docker.stop_container(
            &id,
            Some(StopContainerOptions {
                t: Some(t as i32),
                signal: None,
            }),
        );
        match tokio::time::timeout(grace + Duration::from_secs(10), stop).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if is_not_found(&e) => return Ok(StopOutcome::AlreadyDead),
            Ok(Err(e)) if status_code(&e) == Some(304) => {}
            Ok(Err(e)) if is_unavailable(&e) => return Err(unavailable(&self.host, e)),
            Ok(Err(e)) => tracing::warn!(container = %id, error = %e, "docker stop failed"),
            Err(_) => tracing::warn!(container = %id, "docker stop timed out"),
        }
        let after = self.inspect(&id).await?;
        let running = after.as_ref().is_some_and(is_running);
        let exit_code = after
            .as_ref()
            .and_then(|i| i.state.as_ref())
            .and_then(|s| s.exit_code);
        if running {
            tracing::debug!(container = %id, "still running after stop; killing");
            match self
                .docker
                .kill_container(
                    &id,
                    Some(KillContainerOptions {
                        signal: "SIGKILL".into(),
                    }),
                )
                .await
            {
                Ok(()) => {}
                Err(e) if is_not_found(&e) || status_code(&e) == Some(409) => {}
                Err(e) => return Err(self.map_err(e)),
            }
            let deadline = tokio::time::Instant::now() + KILL_WAIT;
            while tokio::time::Instant::now() < deadline {
                if !self.inspect(&id).await?.as_ref().is_some_and(is_running) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Ok(classify_stop(running, exit_code))
    }

    async fn is_alive(&self, h: &Handle) -> bool {
        let Some(id) = h.container_id() else {
            return false;
        };
        matches!(self.inspect(id).await, Ok(Some(i)) if is_running(&i))
    }

    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        let id = self.container_of(h)?;
        let inspect = self
            .inspect(id)
            .await?
            .ok_or_else(|| RuntimeError::Container(format!("no such container `{id}`")))?;
        Ok(facts_from_inspect(&inspect, self.dropped_lines(h)))
    }

    fn output_stream(&self, h: &Handle) -> Option<OutputStream> {
        let mut reg = self.registry();
        let live = reg.get_mut(&h.id())?;
        let rx = match live.first_rx.take() {
            Some(rx) => rx,
            None => live.output.as_ref()?.upgrade()?.subscribe(),
        };
        Some(OutputStream::new(rx, live.dropped.clone()))
    }

    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        let id = self.container_of(h)?.to_string();
        loop {
            let mut s = self.docker.wait_container(
                &id,
                Some(WaitContainerOptions {
                    condition: "not-running".into(),
                }),
            );
            match s.next().await {
                Some(Ok(r)) => {
                    return Ok(ExitStatus {
                        code: i32::try_from(r.status_code).ok(),
                        signal: None,
                    });
                }
                Some(Err(DockerError::DockerContainerWaitError { code, .. })) => {
                    return Ok(ExitStatus {
                        code: i32::try_from(code).ok(),
                        signal: None,
                    });
                }
                Some(Err(e)) if is_not_found(&e) => return Ok(ExitStatus::UNKNOWN),
                // The per-request timeout can fire before the container exits.
                Some(Err(DockerError::RequestTimeoutError)) => continue,
                Some(Err(e)) => return Err(self.map_err(e)),
                None => {
                    let code = self
                        .inspect(&id)
                        .await?
                        .and_then(|i| i.state)
                        .and_then(|s| s.exit_code)
                        .and_then(|c| i32::try_from(c).ok());
                    return Ok(ExitStatus { code, signal: None });
                }
            }
        }
    }

    /// Trait-level adoption: the stem is taken from the container name
    /// `<ws>-<stem>` and must match the `stems.stem` label; the workspace
    /// is this runtime's ([`DockerOptions::workspace`]). Callers that know
    /// the stem should prefer [`DockerRuntime::adopt_container`].
    async fn adopt(&self, record: &AdoptRecord) -> Option<Handle> {
        let ws = self.workspace.clone()?;
        let want = record.container_id.as_deref()?;
        let inspect = self.inspect(want).await.ok()??;
        let stem = stem_from_name(inspect.name.as_deref()?, &ws)?.to_string();
        self.adopt_container(record, &ws, &stem).await
    }

    fn release(&self, h: &Handle) {
        if let Some(live) = self.registry().remove(&h.id())
            && let Some(task) = live.follower
        {
            tracing::trace!(container = %live.container_id, "log follower released");
            task.abort();
        }
    }

    async fn scan_orphans(&self, scope: &OrphanScope) -> Vec<Orphan> {
        match self.scan_container_orphans(scope).await {
            Ok(v) => v.into_iter().map(Orphan::from).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "container orphan scan failed");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests;
