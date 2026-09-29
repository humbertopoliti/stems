//! Docker (deliverable 14) and compose (15) stems in the supervisor.
//!
//! * [`Containers`] — the lazily connected [`DockerRuntime`] and
//!   [`ComposeRuntime`]. Nothing connects to Docker until a docker or
//!   compose stem is actually started (or adopted, or `down --volumes`
//!   needs it), so process-only selections never touch Docker. A failed
//!   connection is not cached: the next start retries.
//! * [`LazyRuntime`] — the [`Runtime`] registered for `docker`/`compose`
//!   in the [`RuntimeRegistry`](super::RuntimeRegistry): it connects on
//!   `start` and delegates everything else to the connected runtime.
//! * Pure mappings, unit-tested without Docker: [`container_spec`] and
//!   [`compose_spec`] (config → runtime spec), [`runtime_error`]
//!   (`RuntimeError` → §7.5 catalogue), [`progress_event`] (image progress
//!   → `docker.pull` / `docker.build` events).
//! * Lifecycle glue called by the stem actor and the supervisor: [`start`]
//!   (with `restart` → `docker restart` or recreate), [`after_stop`]
//!   (containers are removed on `down`/shutdown, kept on `stop`),
//!   [`after_down`] (stopped leftovers, `down --volumes`), [`adopt`]
//!   (crash recovery), [`watch_health`] / [`decorate_status`]
//!   (`status.health.container`).
//!
//! See `docs/docker.md` and `docs/compose.md` ("Daemon wiring").

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::json;
use stems_api::{EventKind, StemFailure, StemStatus};
use stems_config::{PortRef, PullPolicy, Stem, StemRuntime, StemType, Workspace};
use stems_core::{Error, ErrorCode};
use stems_runtime::docker::{
    BuildSpec, HealthcheckSpec, ImageProgress, PortMapping, PortProto, ProgressSink, PullOutcome,
    PullPolicy as ImagePull, VolumeMount, container_name, labels_of, verify_labels, volume_name,
};
use stems_runtime::{
    AdoptRecord, ComposeOptions, ComposeRuntime, ContainerSpec, DockerOptions, DockerRuntime,
    ExitStatus, Handle, Orphan, OrphanScope, OutputStream, Runtime, RuntimeError, RuntimeFacts,
    StartSpec, StopOutcome,
};

use super::Core;
use super::actor::StemCell;
use crate::events::EventDraft;

/// Bound on connecting to (and pinging) Docker.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a running container's `State.Health` is refreshed for `status`.
pub const HEALTH_POLL: Duration = Duration::from_secs(1);

/// How [`Containers`] reaches Docker. Production: [`default_connector`];
/// tests inject a fake that counts calls.
pub type Connector =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Arc<DockerRuntime>, RuntimeError>> + Send + Sync>;

/// `DockerRuntime::connect` with `$DOCKER_HOST` (or the usual sockets),
/// bounded by [`CONNECT_TIMEOUT`].
pub fn default_connector() -> Connector {
    Arc::new(|| {
        Box::pin(async {
            let opts = DockerOptions {
                host: std::env::var("DOCKER_HOST")
                    .ok()
                    .filter(|h| !h.trim().is_empty()),
                timeout: CONNECT_TIMEOUT,
                workspace: None,
            };
            match tokio::time::timeout(
                CONNECT_TIMEOUT + Duration::from_millis(200),
                DockerRuntime::connect(opts),
            )
            .await
            {
                Ok(r) => r.map(Arc::new),
                Err(_) => Err(RuntimeError::DockerUnavailable {
                    hint: format!(
                        "connecting to Docker timed out; {}",
                        stems_runtime::docker::UNAVAILABLE_HINT
                    ),
                }),
            }
        })
    })
}

/// Pulls images for `stems pull` ([`Containers`] in production, a fake in
/// tests).
#[async_trait::async_trait]
pub trait ImagePuller: Send + Sync {
    /// Pull `image` now, reporting progress to `progress`.
    async fn pull(&self, image: &str, progress: ProgressSink) -> Result<PullOutcome, RuntimeError>;
}

#[async_trait::async_trait]
impl ImagePuller for Containers {
    async fn pull(&self, image: &str, progress: ProgressSink) -> Result<PullOutcome, RuntimeError> {
        self.docker().await?.pull(image, &progress).await
    }
}

/// The catalogue error of a failed `stems pull` of `stem`'s `image`.
pub fn pull_error(stem: &str, image: &str, e: RuntimeError) -> Error {
    match e {
        RuntimeError::DockerUnavailable { hint } => Error::new(
            ErrorCode::DockerUnavailable,
            format!("cannot pull `{image}` for `{stem}`: the Docker daemon is not reachable"),
        )
        .with_hint(hint)
        .with_details(json!({ "stem": stem, "image": image, "stems": [stem] })),
        RuntimeError::ImagePullFailed { image, message } => Error::new(
            ErrorCode::ImagePullFailed,
            format!("pulling `{image}` for `{stem}` failed: {message}"),
        )
        .with_hint("check the image name and tag and your network; for a private registry, check that `docker pull <image>` works in your shell and run `stems doctor` (`docker.credentials`)")
        .with_details(json!({ "stem": stem, "image": image, "message": message })),
        other => runtime_error(stem, other),
    }
}

/// The lazily connected container runtimes of one daemon.
pub struct Containers {
    connector: Connector,
    docker: tokio::sync::OnceCell<Arc<DockerRuntime>>,
    compose: tokio::sync::OnceCell<Arc<ComposeRuntime>>,
    connects: AtomicUsize,
    /// `<data dir>/compose`: compose env files and ownership markers (the
    /// same directory `doctor` uses).
    compose_dir: PathBuf,
    /// Kind of each stem's current container handle.
    kinds: Mutex<HashMap<String, StemType>>,
    /// Containers stopped by `restart`, to `docker restart` (or recreate).
    restart_ids: Mutex<HashMap<String, String>>,
    /// Last `State.Health.Status` per running stem.
    health: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for Containers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Containers")
            .field("connected", &self.docker.get().is_some())
            .field("connects", &self.connects())
            .field("compose_dir", &self.compose_dir)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Containers {
    /// Runtimes reached through `connector`; compose state under `compose_dir`.
    pub fn new(connector: Connector, compose_dir: impl Into<PathBuf>) -> Self {
        Self {
            connector,
            docker: tokio::sync::OnceCell::new(),
            compose: tokio::sync::OnceCell::new(),
            connects: AtomicUsize::new(0),
            compose_dir: compose_dir.into(),
            kinds: Mutex::new(HashMap::new()),
            restart_ids: Mutex::new(HashMap::new()),
            health: Mutex::new(HashMap::new()),
        }
    }

    /// The production runtimes ([`default_connector`]).
    pub fn production(compose_dir: impl Into<PathBuf>) -> Self {
        Self::new(default_connector(), compose_dir)
    }

    /// Connection attempts so far (lazy-connect tests).
    pub fn connects(&self) -> usize {
        self.connects.load(Ordering::SeqCst)
    }

    /// Docker, connecting on first use. A failure is not cached.
    pub async fn docker(&self) -> Result<Arc<DockerRuntime>, RuntimeError> {
        self.docker
            .get_or_try_init(|| {
                self.connects.fetch_add(1, Ordering::SeqCst);
                (self.connector)()
            })
            .await
            .cloned()
    }

    /// Docker if a previous call connected (never connects).
    pub fn docker_if_connected(&self) -> Option<Arc<DockerRuntime>> {
        self.docker.get().cloned()
    }

    /// The compose runtime for `workspace` (connects Docker on first use).
    pub async fn compose(&self, workspace: &str) -> Result<Arc<ComposeRuntime>, RuntimeError> {
        self.compose
            .get_or_try_init(|| async {
                let docker = self.docker().await?;
                Ok(Arc::new(ComposeRuntime::new(
                    docker,
                    ComposeOptions::new(workspace, self.compose_dir.clone()),
                )))
            })
            .await
            .cloned()
    }

    /// The compose runtime if it was created (never connects).
    pub fn compose_if_connected(&self) -> Option<Arc<ComposeRuntime>> {
        self.compose.get().cloned()
    }

    fn now(&self, kind: StemType) -> Option<Arc<dyn Runtime>> {
        match kind {
            StemType::Compose => self.compose_if_connected().map(|c| c as Arc<dyn Runtime>),
            _ => self.docker_if_connected().map(|d| d as Arc<dyn Runtime>),
        }
    }

    /// Last known `State.Health.Status` of `stem`'s container.
    pub fn container_health(&self, stem: &str) -> Option<String> {
        lock(&self.health).get(stem).cloned()
    }

    fn set_health(&self, stem: &str, h: Option<String>) {
        let mut m = lock(&self.health);
        match h {
            Some(h) => {
                m.insert(stem.to_string(), h);
            }
            None => {
                m.remove(stem);
            }
        }
    }
}

/// The [`Runtime`] registered for `docker` / `compose` stems: connects on
/// `start`, delegates everything else to the connected runtime. Handles
/// only exist after a successful start, so the other methods never connect.
pub struct LazyRuntime {
    containers: Arc<Containers>,
    kind: StemType,
}

impl LazyRuntime {
    /// The lazy runtime of `kind` (`Docker` or `Compose`).
    pub fn new(containers: Arc<Containers>, kind: StemType) -> Self {
        Self { containers, kind }
    }

    fn connected(&self) -> Result<Arc<dyn Runtime>, RuntimeError> {
        self.containers.now(self.kind).ok_or_else(|| {
            RuntimeError::Unsupported(format!("the {} runtime is not connected", self.kind))
        })
    }
}

#[async_trait::async_trait]
impl Runtime for LazyRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        match spec {
            StartSpec::Compose(c) => {
                self.containers
                    .compose(&c.workspace)
                    .await?
                    .start(spec)
                    .await
            }
            _ => self.containers.docker().await?.start(spec).await,
        }
    }
    async fn stop(&self, h: &Handle, grace: Duration) -> Result<StopOutcome, RuntimeError> {
        self.connected()?.stop(h, grace).await
    }
    async fn is_alive(&self, h: &Handle) -> bool {
        match self.connected() {
            Ok(rt) => rt.is_alive(h).await,
            Err(_) => false,
        }
    }
    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        self.connected()?.describe(h).await
    }
    fn output_stream(&self, h: &Handle) -> Option<OutputStream> {
        self.connected().ok()?.output_stream(h)
    }
    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        self.connected()?.wait(h).await
    }
    /// Containers are adopted through [`adopt`] (which knows the workspace
    /// and the stem); the trait-level path is not used.
    async fn adopt(&self, _record: &AdoptRecord) -> Option<Handle> {
        None
    }
    fn release(&self, h: &Handle) {
        if let Ok(rt) = self.connected() {
            rt.release(h);
        }
    }
    async fn scan_orphans(&self, scope: &OrphanScope) -> Vec<Orphan> {
        match self.connected() {
            Ok(rt) => rt.scan_orphans(scope).await,
            Err(_) => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure mappings
// ---------------------------------------------------------------------------

fn spec_error(stem: &str, msg: String) -> Error {
    Error::new(
        ErrorCode::StartFailed,
        format!("cannot start `{stem}`: {msg}"),
    )
    .with_hint(format!("fix `stems.{stem}` in stems.yaml"))
    .with_details(json!({ "stem": stem }))
}

/// The container spec of docker stem `stem`: name `<ws>-<stem>`, ports
/// `host:container` (host = the allocated/remapped port in `host_ports`,
/// container = `container_port`), volumes parsed relative to the
/// integration repo (named ones become `<ws>_<name>`, a leading `<ws>-`/
/// `<ws>_` in the config name is not doubled), `env` = what stems sets for
/// the stem (never the daemon's own environment) with `PORT` pointing at
/// the primary *container* port, labels, healthcheck override, stop grace
/// and the daemon's run id. The progress sink is left empty.
pub fn container_spec(
    ws: &Workspace,
    stem: &Stem,
    run_id: &str,
    env: &BTreeMap<String, String>,
    host_ports: &[(String, u16)],
) -> Result<ContainerSpec, Error> {
    let StemRuntime::Docker(d) = &stem.runtime else {
        return Err(Error::internal(format!(
            "`{}` is not a docker stem",
            stem.name
        )));
    };
    let mut spec = ContainerSpec::new(&ws.name, &stem.name, run_id);
    spec.image = d.image.clone();
    spec.pull = match d.pull {
        PullPolicy::Missing => ImagePull::Missing,
        PullPolicy::Always => ImagePull::Always,
        PullPolicy::Never => ImagePull::Never,
    };
    spec.build = d.build.as_ref().map(|b| BuildSpec {
        context: b.context.clone(),
        dockerfile: b.dockerfile.clone(),
    });
    for p in &stem.ports {
        let host = host_ports
            .iter()
            .find(|(n, _)| *n == p.name)
            .map(|(_, v)| *v)
            .or(match p.port {
                PortRef::Fixed(n) => Some(n),
                PortRef::Auto => None,
            })
            .ok_or_else(|| spec_error(&stem.name, format!("port `{}` has no host port", p.name)))?;
        let container = p
            .container_port
            .or(match p.port {
                PortRef::Fixed(n) => Some(n),
                PortRef::Auto => None,
            })
            .ok_or_else(|| {
                spec_error(
                    &stem.name,
                    format!("port `{}` needs a `container_port`", p.name),
                )
            })?;
        spec.ports.push(PortMapping {
            host,
            container,
            proto: PortProto::Tcp,
        });
    }
    for v in &d.volumes {
        spec.volumes
            .push(VolumeMount::parse(v, &ws.root).map_err(|e| spec_error(&stem.name, e))?);
    }
    spec.env = env.clone();
    // `PORT` set by stems (layer 2 of env.rs) is the primary host port;
    // inside the container the service listens on the container port.
    if let (Some(first), Some(port)) = (spec.ports.first(), spec.env.get("PORT"))
        && *port == first.host.to_string()
    {
        spec.env.insert("PORT".into(), first.container.to_string());
    }
    spec.command = d.command.clone();
    spec.entrypoint = d.entrypoint.clone();
    spec.network = d.network.clone();
    spec.labels = d
        .labels
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    spec.healthcheck = d.healthcheck.as_ref().map(|h| {
        if h.disable {
            HealthcheckSpec {
                test: Some(vec!["NONE".into()]),
                ..HealthcheckSpec::default()
            }
        } else {
            HealthcheckSpec {
                test: h.test.clone(),
                interval: h.interval.map(|d| d.as_duration()),
                timeout: h.timeout.map(|d| d.as_duration()),
                retries: h.retries,
                start_period: h.start_period.map(|d| d.as_duration()),
            }
        }
    });
    spec.stop_grace = stem.stop_grace.as_duration();
    Ok(spec)
}

/// The Docker names of `stem`'s named volumes (`<ws>_<name>`), for
/// `down --volumes`. Bind mounts are never included.
pub fn named_volumes(ws: &Workspace, stem: &Stem) -> Vec<String> {
    let StemRuntime::Docker(d) = &stem.runtime else {
        return Vec::new();
    };
    d.volumes
        .iter()
        .filter_map(|v| match VolumeMount::parse(v, &ws.root) {
            Ok(VolumeMount::Named { name, .. }) => Some(volume_name(&ws.name, &name)),
            _ => None,
        })
        .collect()
}

/// The compose spec of compose stem `stem`: the file (already absolute,
/// resolved against the integration repo), service, project (the config
/// default `stems-<ws>` is normalised the way compose wants it), `env`
/// (interpolation input, exported and written to the env file), stop
/// grace and `adopt`.
pub fn compose_spec(
    ws: &Workspace,
    stem: &Stem,
    run_id: &str,
    env: &BTreeMap<String, String>,
) -> Result<stems_runtime::ComposeSpec, Error> {
    let StemRuntime::Compose(c) = &stem.runtime else {
        return Err(Error::internal(format!(
            "`{}` is not a compose stem",
            stem.name
        )));
    };
    let file = c
        .file
        .clone()
        .ok_or_else(|| spec_error(&stem.name, "compose stems need `file:`".into()))?;
    let mut spec = stems_runtime::ComposeSpec::new(&ws.name, &stem.name, run_id, file, &c.service);
    if c.project_name != format!("stems-{}", ws.name) {
        spec.project_name = c.project_name.clone();
    }
    spec.env = env.clone();
    spec.stop_grace = stem.stop_grace.as_duration();
    spec.adopt = c.adopt;
    Ok(spec)
}

/// A container runtime error as a §7.5 catalogue error for `stem`:
///
/// | `RuntimeError` | code |
/// |---|---|
/// | `DockerUnavailable`, `ComposeUnavailable` | `DOCKER_UNAVAILABLE` |
/// | `ImagePullFailed` | `IMAGE_PULL_FAILED` (`details.message`: the registry's text) |
/// | `BuildFailed` | `SETUP_FAILED` (`details.tail`; there is no dedicated build code) |
/// | `ComposeFailed` | `COMPOSE_FAILED` (`details.tail`, `command`, `exit`) |
/// | `ComposeProjectInUse` | `COMPOSE_PROJECT_IN_USE` |
/// | `Container("… port is already allocated …")` | `PORT_IN_USE` |
/// | anything else | `START_FAILED` |
pub fn runtime_error(stem: &str, e: RuntimeError) -> Error {
    match e {
        RuntimeError::DockerUnavailable { hint } => Error::new(
            ErrorCode::DockerUnavailable,
            format!("cannot start `{stem}`: the Docker daemon is not reachable"),
        )
        .with_hint(hint)
        .with_details(json!({ "stem": stem, "stems": [stem] })),
        RuntimeError::ComposeUnavailable { hint } => Error::new(
            ErrorCode::DockerUnavailable,
            format!("cannot start `{stem}`: docker compose v2 is not available"),
        )
        .with_hint(hint)
        .with_details(json!({ "stem": stem, "stems": [stem] })),
        RuntimeError::ImagePullFailed { image, message } => Error::new(
            ErrorCode::ImagePullFailed,
            format!("cannot start `{stem}`: pulling `{image}` failed: {message}"),
        )
        .with_hint("check the image name and tag and your network; for a private registry, check that `docker pull <image>` works in your shell (`docker login`, or the registry's credential helper, e.g. `gcloud auth configure-docker <host>`)")
        .with_details(json!({ "stem": stem, "image": image, "message": message })),
        RuntimeError::BuildFailed { tail } => Error::new(
            ErrorCode::SetupFailed,
            format!("cannot start `{stem}`: building its image failed"),
        )
        .with_hint("fix the Dockerfile or the build context; the last lines of the build output are in `details.tail`")
        .with_details(json!({ "stem": stem, "build": true, "tail": tail })),
        RuntimeError::ComposeFailed { command, exit, tail } => Error::new(
            ErrorCode::ComposeFailed,
            format!(
                "cannot start `{stem}`: `{command}` failed (exit {})",
                exit.map_or_else(|| "none".to_string(), |c| c.to_string())
            ),
        )
        .with_hint("the last lines of compose's output are in `details.tail`; run the command by hand to see all of it")
        .with_details(json!({ "stem": stem, "command": command, "exit": exit, "tail": tail })),
        RuntimeError::ComposeProjectInUse { project } => Error::new(
            ErrorCode::ComposeProjectInUse,
            format!(
                "cannot start `{stem}`: compose project `{project}` is already running outside stems"
            ),
        )
        .with_hint(format!(
            "set `adopt: true` on `stems.{stem}` to co-manage it, or stop it (`docker compose -p {project} down`)"
        ))
        .with_details(json!({ "stem": stem, "project": project })),
        RuntimeError::Container(msg) if msg.contains("port is already allocated") => Error::new(
            ErrorCode::PortInUse,
            format!("cannot start `{stem}`: a host port is already in use ({msg})"),
        )
        .with_hint("another container or process holds the port: `docker ps` / `stems doctor`, or change the port in stems.local.yaml")
        .with_details(json!({ "stem": stem, "message": msg })),
        other => Error::new(
            ErrorCode::StartFailed,
            format!("cannot start `{stem}`: {other}"),
        )
        .with_hint("check `docker ps -a` and the stem's logs (`stems logs <stem>`)")
        .with_details(json!({ "stem": stem })),
    }
}

/// The event for one image progress step: `docker.pull {stem, image,
/// layer, status}` or `docker.build {stem, line}`.
pub fn progress_event(stem: &str, p: ImageProgress) -> EventDraft {
    match p {
        ImageProgress::Pull(pp) => EventDraft::new(EventKind::DOCKER_PULL, stems_api::DAEMON_ACTOR)
            .stem(stem)
            .data(
                json!({ "stem": stem, "image": pp.image, "layer": pp.layer, "status": pp.status }),
            ),
        ImageProgress::Build { stem: s, line } => {
            EventDraft::new(EventKind::DOCKER_BUILD, stems_api::DAEMON_ACTOR)
                .stem(&s)
                .data(json!({ "stem": s, "line": line }))
        }
    }
}

/// A progress sink whose items become events (ends with the last sender).
pub(crate) fn progress_sink(core: &Core, stem: &str) -> ProgressSink {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ImageProgress>(256);
    let events = core.events.clone();
    let stem = stem.to_string();
    tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            events.emit(progress_event(&stem, p));
        }
    });
    ProgressSink(Some(tx))
}

/// `stems validate`'s `requires` auto-check (15): with compose stems,
/// `docker compose` must be v2. A warning (`TOOL_VERSION`, not an error),
/// so process-only CI can still validate a workspace with compose stems.
pub fn compose_warning(ws: &Workspace) -> Option<Error> {
    let stems: Vec<&str> = ws
        .stems()
        .filter(|s| s.kind() == StemType::Compose)
        .map(|s| s.name.as_str())
        .collect();
    if stems.is_empty() {
        return None;
    }
    let out = std::process::Command::new(stems_runtime::docker::docker_program())
        .args(["compose", "version", "--format", "json"])
        .stdin(std::process::Stdio::null())
        .output();
    let problem = match out {
        Err(e) => format!("cannot run `docker` ({e})"),
        Ok(o) if !o.status.success() => format!(
            "`docker compose version` failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Ok(o) => match stems_runtime::compose::require_v2(&String::from_utf8_lossy(&o.stdout)) {
            Ok(_) => return None,
            Err(e) => e.to_string(),
        },
    };
    Some(
        Error::new(
            ErrorCode::ToolVersion,
            format!(
                "compose stems ({}) need docker compose v2: {problem}",
                stems.join(", ")
            ),
        )
        .with_hint("install Docker Desktop (or the docker compose v2 plugin); process stems work without it")
        .with_details(json!({ "tool": "docker compose", "required": ">=2", "stems": stems })),
    )
}

// ---------------------------------------------------------------------------
// Lifecycle glue
// ---------------------------------------------------------------------------

fn containers(core: &Core) -> Result<&Arc<Containers>, Error> {
    core.runtimes.containers().ok_or_else(|| {
        Error::not_implemented("running docker and compose stems", "14")
            .with_details(json!({ "deliverable": "14" }))
    })
}

/// The start spec of a docker or compose stem (the progress sink of a
/// docker spec emits `docker.pull`/`docker.build`).
pub(crate) fn start_spec(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    env: &BTreeMap<String, String>,
    host_ports: &[(String, u16)],
) -> Result<StartSpec, Error> {
    match stem.kind() {
        StemType::Compose => Ok(StartSpec::Compose(Box::new(compose_spec(
            ws,
            stem,
            &core.run_id,
            env,
        )?))),
        _ => {
            let mut spec = container_spec(ws, stem, &core.run_id, env, host_ports)?;
            spec.progress = progress_sink(core, &stem.name);
            Ok(StartSpec::Docker(Box::new(spec)))
        }
    }
}

/// Start a docker/compose stem. A docker container stopped by `restart`
/// is restarted in place (`docker restart`) when its spec is unchanged,
/// else recreated ([`DockerRuntime::restart`]).
pub(crate) async fn start(
    core: &Core,
    stem: &str,
    kind: StemType,
    runtime: &Arc<dyn Runtime>,
    spec: &StartSpec,
) -> Result<Handle, Error> {
    let c = containers(core)?;
    let kept = lock(&c.restart_ids).remove(stem);
    let h = match (kept, spec) {
        (Some(id), StartSpec::Docker(cs)) => {
            let docker = c.docker().await.map_err(|e| runtime_error(stem, e))?;
            match docker.attach(&id).await {
                Ok(old) => docker
                    .restart(&old, cs)
                    .await
                    .map_err(|e| runtime_error(stem, e))?,
                // Gone meanwhile: start a new one.
                Err(_) => runtime
                    .start(spec)
                    .await
                    .map_err(|e| runtime_error(stem, e))?,
            }
        }
        _ => runtime
            .start(spec)
            .await
            .map_err(|e| runtime_error(stem, e))?,
    };
    lock(&c.kinds).insert(stem.to_string(), kind);
    Ok(h)
}

/// After a container stem was stopped (before its handle is released):
/// `stop` keeps the container, `restart` keeps it for [`start`], anything
/// else (`down`, daemon shutdown, `reset`) removes it (volumes kept).
pub(crate) async fn after_stop(core: &Core, stem: &str, h: &Handle, reason: &str) {
    let Some(id) = h.container_id() else { return };
    let Some(c) = core.runtimes.containers() else {
        return;
    };
    c.set_health(stem, None);
    let kind = lock(&c.kinds).get(stem).copied();
    match (reason, kind) {
        ("restart", Some(StemType::Docker)) => {
            lock(&c.restart_ids).insert(stem.to_string(), id.to_string());
        }
        ("stop" | "restart", _) => {}
        (_, Some(StemType::Compose)) => {
            if let Some(rt) = c.compose_if_connected()
                && let Err(e) = rt.remove(h).await
            {
                tracing::warn!(stem, error = %e, "compose rm after stop failed");
            }
        }
        _ => {
            if let Some(rt) = c.docker_if_connected()
                && let Err(e) = rt.remove(h, false).await
            {
                tracing::warn!(stem, error = %e, "container removal after stop failed");
            }
        }
    }
}

/// Stems whose state is `failed`: on daemon shutdown their stopped
/// containers are removed through [`after_down`] (non-container stems are
/// ignored there).
pub(crate) fn failed_selection(
    states: impl IntoIterator<Item = (String, stems_core::StemState)>,
) -> Vec<String> {
    states
        .into_iter()
        .filter(|(_, s)| *s == stems_core::StemState::Failed)
        .map(|(n, _)| n)
        .collect()
}

/// `down`'s container work after the stems were stopped: remove the
/// stopped containers stems left behind for the selected docker/compose
/// stems (`stems stop`, a crash), and with `volumes` their `<ws>_*` named
/// volumes (FR-LC-2). Returns the removed volumes and per-stem failures.
/// Never connects to Docker unless `volumes` is set.
pub(crate) async fn after_down(
    core: &Core,
    ws: &Workspace,
    selection: &[String],
    running: &(dyn Fn(&str) -> bool + Sync),
    volumes: bool,
) -> (Vec<String>, Vec<StemFailure>) {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    let Some(c) = core.runtimes.containers() else {
        return (removed, failed);
    };
    let stems: Vec<&Stem> = ws
        .stems()
        .filter(|s| matches!(s.kind(), StemType::Docker | StemType::Compose))
        .filter(|s| selection.is_empty() || selection.contains(&s.name))
        .filter(|s| !running(&s.name))
        .collect();
    if stems.is_empty() {
        return (removed, failed);
    }
    let docker = if volumes {
        match c.docker().await {
            Ok(d) => Some(d),
            Err(e) => {
                let err = runtime_error("docker", e);
                for s in stems.iter().filter(|s| s.kind() == StemType::Docker) {
                    failed.push(StemFailure {
                        stem: s.name.clone(),
                        error: err.clone(),
                    });
                }
                return (removed, failed);
            }
        }
    } else {
        c.docker_if_connected()
    };
    let Some(docker) = docker else {
        return (removed, failed);
    };
    for s in &stems {
        match s.kind() {
            StemType::Compose => {
                if let Some(rt) = c.compose_if_connected()
                    && let Ok(spec) = compose_spec(ws, s, &core.run_id, &BTreeMap::new())
                    && let Err(e) = rt.remove_service(&spec).await
                {
                    tracing::debug!(stem = %s.name, error = %e, "compose rm on down");
                }
            }
            _ => {
                let name = container_name(&ws.name, &s.name);
                if let Ok(Some(i)) = docker.inspect(&name).await
                    && verify_labels(labels_of(&i), &ws.name, &s.name).is_ok()
                    && let Err(e) = docker.remove_container_id(&name, false).await
                {
                    tracing::warn!(stem = %s.name, error = %e, "leftover container not removed");
                }
                if !volumes {
                    continue;
                }
                for v in named_volumes(ws, s) {
                    match docker.remove_volume(&v).await {
                        Ok(true) => removed.push(v),
                        Ok(false) => {}
                        Err(e) => failed.push(StemFailure {
                            stem: s.name.clone(),
                            error: Error::new(
                                ErrorCode::StartFailed,
                                format!("cannot remove volume `{v}` of `{}`: {e}", s.name),
                            )
                            .with_hint(format!(
                                "is another container using it? `docker ps -a --filter volume={v}`"
                            ))
                            .with_details(json!({ "stem": s.name, "volume": v })),
                        }),
                    }
                }
            }
        }
    }
    (removed, failed)
}

/// Crash recovery of a recorded container (FR-CR-2): the same container,
/// labelled for `ws`/`stem` (docker) or carrying the compose labels of the
/// stem's project/service (compose), and running. Docker unreachable →
/// `None` (the record is dropped; the container shows up as an orphan).
pub(crate) async fn adopt(
    core: &Core,
    kind: StemType,
    record: &AdoptRecord,
    ws: Option<&Workspace>,
    stem: &str,
) -> Option<Handle> {
    let c = core.runtimes.containers()?;
    record.container_id.as_ref()?;
    let ws = ws?;
    let h = match kind {
        StemType::Compose => {
            let rt = c.compose(&ws.name).await.ok()?;
            match ws
                .stem(stem)
                .and_then(|s| compose_spec(ws, s, &core.run_id, &config_env(s)).ok())
            {
                Some(spec) => rt.adopt_service(record, &spec).await,
                None => rt.adopt(record).await,
            }
        }
        _ => {
            c.docker()
                .await
                .ok()?
                .adopt_container(record, &ws.name, stem)
                .await
        }
    }?;
    lock(&c.kinds).insert(stem.to_string(), kind);
    Some(h)
}

fn config_env(stem: &Stem) -> BTreeMap<String, String> {
    stem.env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(stem.local_env.clone())
        .collect()
}

/// Refresh `stem`'s container health (for `status`) while `generation`
/// of `cell` is current.
pub(crate) fn watch_health(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    runtime: Arc<dyn Runtime>,
    handle: Handle,
    generation: u64,
) {
    if handle.container_id().is_none() {
        return;
    }
    let Some(c) = core.runtimes.containers().cloned() else {
        return;
    };
    let cell = cell.clone();
    tokio::spawn(async move {
        loop {
            if cell.info().generation != generation {
                return;
            }
            if let Ok(f) = runtime.describe(&handle).await
                && cell.info().generation == generation
            {
                c.set_health(&cell.name, f.container_health);
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    });
}

/// `status` for a container stem: `health.container` (Docker's
/// `State.Health.Status`, for 21's `docker` probe) filled in when the stem
/// has a health block, and no pid/pgid (the container's processes are not
/// local ones).
pub(crate) fn decorate_status(c: Option<&Containers>, kind: StemType, st: &mut StemStatus) {
    if !matches!(kind, StemType::Docker | StemType::Compose) {
        return;
    }
    st.pid = None;
    st.pgid = None;
    if !st.state.is_running() {
        return;
    }
    if let (Some(h), Some(health)) = (c.and_then(|c| c.container_health(&st.name)), &mut st.health)
    {
        health.container = Some(h);
    }
}

/// `<data dir>/compose` for a daemon whose state file is `state`.
pub fn compose_dir_for(state: Option<&Path>) -> PathBuf {
    state.and_then(Path::parent).map_or_else(
        || std::env::temp_dir().join("stems-compose"),
        |d| d.join("compose"),
    )
}

#[cfg(test)]
mod tests;
