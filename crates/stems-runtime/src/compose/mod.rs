//! `ComposeRuntime`: one service of a user's compose file per stem, driven
//! by shelling out to `docker compose` (v2). See `docs/compose.md`.
//!
//! stems owns lifecycle (`up -d --no-deps`, `stop`, `rm`), logs, status and
//! adoption; compose owns the service definition. Once `docker compose ps`
//! has named the service's container, everything container-level (logs,
//! describe, wait, liveness) goes through [`DockerRuntime`] by container
//! id, and the handle is an ordinary [`Handle::Container`].
//!
//! The decisions (command lines, env file, `ps`/`version` parsing, the
//! project-in-use table, adoption, orphan selection, error mapping) are
//! pure functions in [`command`] and [`parse`], tested without Docker.

pub mod command;
pub mod parse;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::docker::{DockerRuntime, classify_stop};
use crate::output::OutputStream;
use crate::runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, Orphan, OrphanScope, Runtime, RuntimeError,
    RuntimeFacts, StartSpec, StopOutcome,
};

pub use command::{
    ComposeCommand, ComposeTarget, compose_env, display_command, env_file_path, marker_path,
    quote_env_value, render_env_file, write_env_file,
};
pub use parse::{
    COMPOSE_TAIL_LINES, ComposeAdoptRejection, LABEL_COMPOSE_CONFIG_FILES, LABEL_COMPOSE_PROJECT,
    LABEL_COMPOSE_SERVICE, ProjectDecision, PsEntry, default_project_name, failure_error,
    output_tail, parse_ps_json, parse_version, pick_container, project_decision, require_v2,
    select_compose_orphans, verify_compose_adoption,
};

/// Bound on quick invocations (`version`, `ps`, `rm`).
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);
/// Extra time given to `compose stop` beyond the grace period.
const STOP_SLACK: Duration = Duration::from_secs(30);

/// Everything the compose runtime needs to run one stem's service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComposeSpec {
    pub workspace: String,
    pub stem: String,
    /// Daemon run id (informational: compose containers do not carry
    /// stems labels).
    pub run_id: String,
    /// Compose file, absolute.
    pub file: PathBuf,
    pub service: String,
    /// `-p`; default [`default_project_name`] (`stems-<ws>`).
    pub project_name: String,
    /// Stem env: written to `<env_dir>/<stem>.env` and exported to the
    /// compose process, so it overrides `${VAR:-default}` in the file.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `compose stop -t` (rounded up to whole seconds).
    pub stop_grace: Duration,
    /// Co-manage a project that already runs outside stems.
    #[serde(default)]
    pub adopt: bool,
}

impl ComposeSpec {
    /// Default project `stems-<ws>`, empty env, 10 s grace, no adoption.
    pub fn new(
        workspace: impl Into<String>,
        stem: impl Into<String>,
        run_id: impl Into<String>,
        file: impl Into<PathBuf>,
        service: impl Into<String>,
    ) -> Self {
        let workspace = workspace.into();
        Self {
            project_name: default_project_name(&workspace),
            workspace,
            stem: stem.into(),
            run_id: run_id.into(),
            file: file.into(),
            service: service.into(),
            env: BTreeMap::new(),
            stop_grace: Duration::from_secs(10),
            adopt: false,
        }
    }
}

/// Construction options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeOptions {
    pub workspace: String,
    /// `$STEMS_HOME/<ws>/compose/`: env files and ownership markers.
    pub env_dir: PathBuf,
    /// The `docker` CLI (default `docker`, looked up on `PATH`).
    pub binary: OsString,
}

impl ComposeOptions {
    pub fn new(workspace: impl Into<String>, env_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            env_dir: env_dir.into(),
            binary: "docker".into(),
        }
    }
}

/// How to re-run compose for a handle (stop / rm).
#[derive(Debug, Clone)]
struct Invocation {
    target: ComposeTarget,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
}

/// Output of a successful invocation.
struct Ran {
    stdout: String,
}

/// Runs [`ComposeSpec`]s. Cheap to share behind an `Arc`.
pub struct ComposeRuntime {
    docker: Arc<DockerRuntime>,
    opts: ComposeOptions,
    version: tokio::sync::OnceCell<Version>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    targets: Mutex<HashMap<HandleId, Invocation>>,
}

impl std::fmt::Debug for ComposeRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeRuntime")
            .field("opts", &self.opts)
            .field("version", &self.version.get())
            .finish_non_exhaustive()
    }
}

fn now_secs() -> i32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i32::try_from(d.as_secs()).unwrap_or(i32::MAX))
}

/// Projects that have an ownership marker in `env_dir`.
fn marked_projects(env_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(env_dir) else {
        return Vec::new();
    };
    rd.filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".owned")
                .map(str::to_string)
        })
        .collect()
}

impl ComposeRuntime {
    pub fn new(docker: Arc<DockerRuntime>, opts: ComposeOptions) -> Self {
        Self {
            docker,
            opts,
            version: tokio::sync::OnceCell::new(),
            locks: Mutex::new(HashMap::new()),
            targets: Mutex::new(HashMap::new()),
        }
    }

    /// The Docker runtime container-level operations are delegated to.
    pub fn docker(&self) -> &Arc<DockerRuntime> {
        &self.docker
    }

    pub fn options(&self) -> &ComposeOptions {
        &self.opts
    }

    /// `stems-<ws>`.
    pub fn default_project(&self) -> String {
        default_project_name(&self.opts.workspace)
    }

    fn targets(&self) -> std::sync::MutexGuard<'_, HashMap<HandleId, Invocation>> {
        self.targets.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn project_lock(&self, project: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(project.to_string())
            .or_default()
            .clone()
    }

    fn is_owned(&self, project: &str) -> bool {
        project == self.default_project() || marker_path(&self.opts.env_dir, project).exists()
    }

    fn project_invocation(&self, project: &str) -> Invocation {
        Invocation {
            target: ComposeTarget::project(project),
            env: compose_env(&BTreeMap::new(), Some(self.docker.host())),
            cwd: self.opts.env_dir.clone(),
        }
    }

    fn spec_invocation(&self, spec: &ComposeSpec) -> Invocation {
        Invocation {
            target: ComposeTarget::for_spec(spec, &self.opts.env_dir),
            env: compose_env(&spec.env, Some(self.docker.host())),
            cwd: spec
                .file
                .parent()
                .map_or_else(|| self.opts.env_dir.clone(), Path::to_path_buf),
        }
    }

    /// Run `docker <args>` with captured output. Non-zero exits and
    /// timeouts go through [`failure_error`].
    async fn run(
        &self,
        inv: &Invocation,
        args: Vec<OsString>,
        timeout: Option<Duration>,
    ) -> Result<Ran, RuntimeError> {
        let shown = display_command(&self.opts.binary, &args);
        tracing::debug!(command = %shown, "docker compose");
        if !inv.cwd.exists() {
            std::fs::create_dir_all(&inv.cwd)?;
        }
        let mut cmd = tokio::process::Command::new(&self.opts.binary);
        cmd.args(&args)
            .envs(&inv.env)
            .current_dir(&inv.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let fut = cmd.output();
        let out = match timeout {
            Some(t) => match tokio::time::timeout(t, fut).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(RuntimeError::ComposeFailed {
                        command: shown,
                        exit: None,
                        tail: vec![format!("timed out after {}s", t.as_secs())],
                    });
                }
            },
            None => fut.await,
        };
        let out = match out {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(RuntimeError::ComposeUnavailable {
                    hint: format!(
                        "`{}` not found on PATH; {}",
                        self.opts.binary.to_string_lossy(),
                        parse::COMPOSE_MISSING_HINT
                    ),
                });
            }
            Err(source) => {
                return Err(RuntimeError::SpawnFailed {
                    command: shown,
                    source,
                });
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            return Ok(Ran { stdout });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(failure_error(shown, out.status.code(), &stdout, &stderr))
    }

    /// `docker compose version`, checked once per runtime (successes are
    /// cached; failures are retried on the next start).
    pub async fn ensure_version(&self) -> Result<Version, RuntimeError> {
        self.version
            .get_or_try_init(|| async {
                let inv = Invocation {
                    target: ComposeTarget::project(""),
                    env: compose_env(&BTreeMap::new(), Some(self.docker.host())),
                    cwd: self.opts.env_dir.clone(),
                };
                let ran = match self
                    .run(&inv, ComposeCommand::version(), Some(QUICK_TIMEOUT))
                    .await
                {
                    Ok(r) => r,
                    Err(RuntimeError::ComposeFailed { tail, .. }) => {
                        return Err(RuntimeError::ComposeUnavailable {
                            hint: format!(
                                "`docker compose version` failed ({}); {}",
                                tail.join(" / "),
                                parse::COMPOSE_MISSING_HINT
                            ),
                        });
                    }
                    Err(e) => return Err(e),
                };
                let v = require_v2(&ran.stdout)?;
                tracing::debug!(version = %v, "docker compose");
                Ok(v)
            })
            .await
            .cloned()
    }

    /// `docker compose -p <project> ps -a --format json`: every container
    /// of the project, whoever created it.
    pub async fn project_ps(&self, project: &str) -> Result<Vec<PsEntry>, RuntimeError> {
        let inv = self.project_invocation(project);
        let ran = self
            .run(&inv, ComposeCommand::ps(&inv.target), Some(QUICK_TIMEOUT))
            .await?;
        Ok(parse_ps_json(&ran.stdout))
    }

    async fn start_service(&self, spec: &ComposeSpec) -> Result<Handle, RuntimeError> {
        self.ensure_version().await?;
        let lock = self.project_lock(&spec.project_name);
        let _guard = lock.lock().await;

        let inv = self.spec_invocation(spec);
        if let Some(env_file) = &inv.target.env_file {
            write_env_file(env_file, &spec.env)?;
        }

        let existing = self.project_ps(&spec.project_name).await?;
        let marker = marker_path(&self.opts.env_dir, &spec.project_name);
        let decision = project_decision(
            &existing,
            &spec.project_name,
            spec.project_name == self.default_project(),
            marker.exists(),
            spec.adopt,
        );
        tracing::debug!(project = %spec.project_name, ?decision, "compose project check");
        if !decision.allows_start() {
            return Err(RuntimeError::ComposeProjectInUse {
                project: spec.project_name.clone(),
            });
        }

        // A second of slack so lines logged in the same second as `up` are kept.
        let since = now_secs().saturating_sub(1);
        self.run(&inv, ComposeCommand::up(&inv.target), None)
            .await?;
        command::write_private(
            &marker,
            &format!(
                "workspace={}\nproject={}\n",
                self.opts.workspace, spec.project_name
            ),
        )?;

        let ran = self
            .run(&inv, ComposeCommand::ps(&inv.target), Some(QUICK_TIMEOUT))
            .await?;
        let entries = parse_ps_json(&ran.stdout);
        let entry =
            pick_container(&entries, &spec.project_name, &spec.service).ok_or_else(|| {
                RuntimeError::ComposeFailed {
                    command: display_command(&self.opts.binary, &ComposeCommand::ps(&inv.target)),
                    exit: Some(0),
                    tail: vec![format!(
                        "no container for service `{}` in project `{}` after `up`",
                        spec.service, spec.project_name
                    )],
                }
            })?;
        let handle = self.docker.attach_since(&entry.id, since).await?;
        self.targets().insert(handle.id(), inv);
        Ok(handle)
    }

    fn invocation_of(&self, h: &Handle) -> Option<Invocation> {
        self.targets().get(&h.id()).cloned()
    }

    /// `docker compose rm -f -s <service>` and forget the handle (`down`).
    pub async fn remove(&self, h: &Handle) -> Result<(), RuntimeError> {
        let Some(inv) = self.invocation_of(h) else {
            return Err(RuntimeError::NotFound(h.id()));
        };
        {
            let lock = self.project_lock(&inv.target.project);
            let _guard = lock.lock().await;
            self.run(&inv, ComposeCommand::rm(&inv.target), Some(QUICK_TIMEOUT))
                .await?;
        }
        self.release(h);
        Ok(())
    }

    /// Adopt the recorded container of `spec` after a daemon restart: same
    /// id, compose labels name `spec.project_name`/`spec.service`, running.
    pub async fn adopt_service(&self, record: &AdoptRecord, spec: &ComposeSpec) -> Option<Handle> {
        let want = record.container_id.as_deref()?;
        let inspect = match self.docker.inspect(want).await {
            Ok(Some(i)) => i,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(container = want, error = %e, "compose adopt: inspect failed");
                return None;
            }
        };
        if let Err(why) =
            verify_compose_adoption(&inspect, record, &spec.project_name, Some(&spec.service))
        {
            tracing::debug!(container = want, ?why, "compose adopt: rejected");
            return None;
        }
        let inv = self.spec_invocation(spec);
        if let Some(env_file) = &inv.target.env_file
            && let Err(e) = write_env_file(env_file, &spec.env)
        {
            tracing::warn!(error = %e, "compose adopt: env file not written");
        }
        let h = self.docker.attach(inspect.id.as_deref()?).await.ok()?;
        self.targets().insert(h.id(), inv);
        Some(h)
    }
}

#[async_trait::async_trait]
impl Runtime for ComposeRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        match spec {
            StartSpec::Compose(c) => self.start_service(c).await,
            StartSpec::Process(p) => Err(RuntimeError::Unsupported(format!(
                "the compose runtime does not run processes (`{}`)",
                p.command
            ))),
            StartSpec::External { stem } => Err(RuntimeError::Unsupported(format!(
                "`{stem}` is an external stem; the compose runtime does not start it"
            ))),
            StartSpec::Docker(c) => Err(RuntimeError::Unsupported(format!(
                "`{}` is a docker stem; start it with the docker runtime",
                c.stem
            ))),
        }
    }

    /// `docker compose stop -t <grace> <service>`; if the container is
    /// somehow still running afterwards, the Docker runtime kills it.
    async fn stop(&self, h: &Handle, grace: Duration) -> Result<StopOutcome, RuntimeError> {
        let id = h
            .container_id()
            .ok_or_else(|| {
                RuntimeError::Unsupported(format!("handle {} is not a container", h.id()))
            })?
            .to_string();
        match self.docker.inspect(&id).await? {
            Some(i) if parse::inspect_running(&i) => {}
            _ => return Ok(StopOutcome::AlreadyDead),
        }
        let Some(inv) = self.invocation_of(h) else {
            // Not started/adopted by this runtime instance: stop the container directly.
            return self.docker.stop(h, grace).await;
        };
        {
            let lock = self.project_lock(&inv.target.project);
            let _guard = lock.lock().await;
            match self
                .run(
                    &inv,
                    ComposeCommand::stop(&inv.target, grace),
                    Some(grace + STOP_SLACK),
                )
                .await
            {
                Ok(_) => {}
                // Stopping must not depend on the compose file still loading:
                // fall back to a container-level stop.
                Err(e @ RuntimeError::ComposeFailed { .. }) => {
                    tracing::warn!(container = %id, error = %e, "compose stop failed; stopping the container directly");
                    return self.docker.stop(h, grace).await;
                }
                Err(e) => return Err(e),
            }
        }
        let after = self.docker.inspect(&id).await?;
        if after.as_ref().is_some_and(parse::inspect_running) {
            tracing::debug!(container = %id, "still running after compose stop");
            self.docker.stop(h, grace).await?;
            return Ok(StopOutcome::Killed);
        }
        let exit_code = after
            .as_ref()
            .and_then(|i| i.state.as_ref())
            .and_then(|s| s.exit_code);
        Ok(classify_stop(false, exit_code))
    }

    async fn is_alive(&self, h: &Handle) -> bool {
        self.docker.is_alive(h).await
    }

    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        self.docker.describe(h).await
    }

    fn output_stream(&self, h: &Handle) -> Option<OutputStream> {
        self.docker.output_stream(h)
    }

    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        self.docker.wait(h).await
    }

    /// Trait-level adoption: the container must carry compose labels of a
    /// stems-owned project (the default `stems-<ws>` or one with an
    /// ownership marker) and be running. The project and service come from
    /// its labels; stop/rm then address the project by name only. Callers
    /// that know the spec should prefer [`ComposeRuntime::adopt_service`].
    async fn adopt(&self, record: &AdoptRecord) -> Option<Handle> {
        let want = record.container_id.as_deref()?;
        let inspect = self.docker.inspect(want).await.ok()??;
        let labels = parse::inspect_labels(&inspect)?;
        let project = labels.get(LABEL_COMPOSE_PROJECT)?.clone();
        let service = labels.get(LABEL_COMPOSE_SERVICE)?.clone();
        if !self.is_owned(&project) {
            tracing::debug!(%project, "compose adopt: project not owned by stems");
            return None;
        }
        verify_compose_adoption(&inspect, record, &project, Some(&service)).ok()?;
        let mut inv = self.project_invocation(&project);
        inv.target.service = service;
        let h = self.docker.attach(inspect.id.as_deref()?).await.ok()?;
        self.targets().insert(h.id(), inv);
        Some(h)
    }

    fn release(&self, h: &Handle) {
        self.targets().remove(&h.id());
        self.docker.release(h);
    }

    /// Containers of stems-owned compose projects (`stems-<ws>` plus every
    /// project with an ownership marker) not in `scope.known`.
    async fn scan_orphans(&self, scope: &OrphanScope) -> Vec<Orphan> {
        let mut projects: BTreeSet<String> =
            marked_projects(&self.opts.env_dir).into_iter().collect();
        let ws = if scope.workspace.is_empty() {
            &self.opts.workspace
        } else {
            &scope.workspace
        };
        projects.insert(default_project_name(ws));
        let mut out = Vec::new();
        for p in projects {
            match self.project_ps(&p).await {
                Ok(entries) => out.extend(select_compose_orphans(&entries, scope)),
                Err(e) => tracing::warn!(project = %p, error = %e, "compose orphan scan failed"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
