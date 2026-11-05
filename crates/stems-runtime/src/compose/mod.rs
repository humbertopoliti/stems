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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;

use crate::docker::{
    BuildSecret, DockerRuntime, ImageProgress, ProgressSink, SecretSource, classify_stop,
};
use crate::output::{LineSplitter, OutputStream};
use crate::runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, Orphan, OrphanScope, Runtime, RuntimeError,
    RuntimeFacts, StartSpec, StopOutcome,
};

pub use command::{
    ComposeCommand, ComposeTarget, OverrideInputs, compose_env, display_command, env_file_path,
    marker_path, override_file_path, quote_env_value, render_env_file, render_override,
    render_override_file, write_env_file,
};
pub use parse::{
    COMPOSE_TAIL_LINES, ComposeAdoptRejection, LABEL_COMPOSE_CONFIG_FILES, LABEL_COMPOSE_PROJECT,
    LABEL_COMPOSE_SERVICE, ProjectDecision, PsEntry, ServiceBuild, ServiceConfig,
    default_project_name, failure_error, output_tail, parse_ps_json, parse_version, pick_container,
    project_decision, require_v2, select_compose_orphans, service_config, verify_compose_adoption,
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
    /// `up --build`: rebuild the service's image on every start (default
    /// true; deliverable 35). A service without `build:` is unaffected.
    #[serde(default = "default_build")]
    pub build: bool,
    /// `inject_env: true` (38): also put the env file into the service's
    /// container through `<env_dir>/<stem>.override.yaml`, a second `-f`.
    #[serde(default)]
    pub inject_env: bool,
    /// With `inject_env`: the container port of the stem's primary port;
    /// the override sets `PORT` to it (the env file's `PORT` is the host
    /// port).
    #[serde(default)]
    pub container_port: Option<u16>,
    /// The object form of `build:` (44): images built with `docker buildx
    /// bake` (secrets, `allow_read`, extra services), then `up --no-build`.
    #[serde(default, skip_serializing_if = "BakeOptions::is_plain")]
    pub bake: BakeOptions,
    /// The images were already built in this start sequence
    /// (`build.before_setup`, 44): `up` does not build them again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prebuilt: bool,
    /// Where the output lines of `up --build` go (`docker.build` events).
    /// Ignored by equality and serde.
    #[serde(skip)]
    pub progress: ProgressSink,
    /// `env` build secrets' values from the request (44): given to the
    /// compose/Bake process only. Ignored by equality and serde.
    #[serde(skip)]
    pub secret_env: crate::docker::SecretEnv,
}

fn default_build() -> bool {
    true
}

/// The object form of a compose stem's `build:` (44).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BakeOptions {
    /// Services built; empty: the stem's service.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    /// Built before `setup` (the daemon's concern; recorded for `show`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub before_setup: bool,
    /// BuildKit secrets, declared in the override file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<BuildSecret>,
    /// Extra `--allow fs.read=` paths (exact, absolute).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_read: Vec<PathBuf>,
}

impl BakeOptions {
    /// The bool form: nothing for Bake to do.
    pub fn is_plain(&self) -> bool {
        self == &Self::default()
    }
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
            build: true,
            inject_env: false,
            container_port: None,
            bake: BakeOptions::default(),
            prebuilt: false,
            progress: ProgressSink::default(),
            secret_env: crate::docker::SecretEnv::default(),
        }
    }

    /// Images are built with `docker buildx bake` (44): `build` is on and
    /// the object form asks for more than `build: true`.
    pub fn uses_bake(&self) -> bool {
        self.build && !self.bake.is_plain()
    }

    /// The services whose images are built: `bake.services`, else the
    /// stem's service.
    pub fn build_services(&self) -> Vec<String> {
        if self.bake.services.is_empty() {
            vec![self.service.clone()]
        } else {
            self.bake.services.clone()
        }
    }

    /// A generated override file is passed (`inject_env`, build secrets).
    pub fn needs_override(&self) -> bool {
        self.inject_env || !self.bake.secrets.is_empty()
    }

    /// The generated override file's content (`env_file`: the stem's env
    /// file, `<env_dir>/<stem>.env`), or `None` when none is passed
    /// ([`Self::needs_override`]). The runtime and `stems compose` (43) both
    /// write exactly this.
    pub fn override_body(&self, env_file: &Path) -> Option<String> {
        if !self.needs_override() {
            return None;
        }
        let services = self.build_services();
        Some(render_override(&OverrideInputs {
            service: &self.service,
            env_file: self.inject_env.then_some(env_file),
            container_port: self.container_port,
            secrets: &self.bake.secrets,
            build_services: &services,
        }))
    }
}

/// Construction options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeOptions {
    pub workspace: String,
    /// `$STEMS_HOME/<ws>/compose/`: env files and ownership markers.
    pub env_dir: PathBuf,
    /// The `docker` CLI (default [`crate::docker::docker_program`]: `PATH`,
    /// then the well-known install locations).
    pub binary: OsString,
}

impl ComposeOptions {
    pub fn new(workspace: impl Into<String>, env_dir: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            env_dir: env_dir.into(),
            binary: crate::docker::docker_program(),
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

/// The last [`COMPOSE_TAIL_LINES`] lines of a streamed output.
#[derive(Default)]
struct Tail(VecDeque<String>);

impl Tail {
    fn push(&mut self, line: String) {
        if self.0.len() == COMPOSE_TAIL_LINES {
            self.0.pop_front();
        }
        self.0.push_back(line);
    }

    fn join(&self) -> String {
        self.0.iter().map(|l| format!("{l}\n")).collect()
    }
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

/// Grant `fs.read` on a path stems itself needs Bake to read (a context, a
/// Dockerfile's directory, a secret file): normalised, plus its canonical
/// form when a symlink makes it differ. The user's `allow_read` entries are
/// passed exactly as written.
fn grant(allow: &mut BTreeSet<PathBuf>, p: &Path) {
    let p = crate::docker::normalize(p);
    if let Ok(real) = std::fs::canonicalize(&p)
        && real != p
    {
        allow.insert(real);
    }
    allow.insert(p);
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

    /// Write `spec`'s env file and, with `inject_env` or build secrets,
    /// its override file.
    fn write_inputs(&self, spec: &ComposeSpec) -> Result<(), RuntimeError> {
        let env_file = env_file_path(&self.opts.env_dir, &spec.stem);
        write_env_file(&env_file, &spec.env)?;
        if let Some(body) = spec.override_body(&env_file) {
            command::write_private(&override_file_path(&self.opts.env_dir, &spec.stem), &body)?;
        }
        Ok(())
    }

    fn spec_invocation(&self, spec: &ComposeSpec) -> Invocation {
        Invocation {
            target: ComposeTarget::for_spec(spec, &self.opts.env_dir),
            env: {
                let mut env = compose_env(&spec.env, Some(self.docker.host()));
                spec.secret_env.apply_to(&mut env);
                env
            },
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

    /// Like [`Self::run`], but stdout and stderr are read line by line as
    /// they come and each line goes to `on_line` (the build output of `up
    /// --build`, deliverable 35); only the last [`COMPOSE_TAIL_LINES`] of
    /// each are kept for a failure.
    async fn run_streamed(
        &self,
        inv: &Invocation,
        args: Vec<OsString>,
        timeout: Option<Duration>,
        on_line: &(dyn Fn(String) + Sync),
    ) -> Result<Ran, RuntimeError> {
        let shown = display_command(&self.opts.binary, &args);
        tracing::debug!(command = %shown, "docker compose (streamed)");
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
        let mut child = match cmd.spawn() {
            Ok(c) => c,
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
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let pump = |reader: Option<tokio::process::ChildStdout>,
                    err: Option<tokio::process::ChildStderr>| async move {
            let mut tail = Tail::default();
            let mut splitter = LineSplitter::new();
            let mut buf = vec![0u8; 8192];
            // One of the two is `Some`; a boxed reader keeps one loop.
            let mut reader: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>> =
                match (reader, err) {
                    (Some(r), _) => Box::pin(r),
                    (None, Some(e)) => Box::pin(e),
                    (None, None) => return tail,
                };
            loop {
                match reader.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        for line in splitter.push(&buf[..n]) {
                            on_line(line.clone());
                            tail.push(line);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if let Some(last) = splitter.finish() {
                on_line(last.clone());
                tail.push(last);
            }
            tail
        };
        let outputs = async { tokio::join!(pump(stdout, None), pump(None, stderr)) };
        let wait = async {
            let (out, err) = outputs.await;
            let status = child.wait().await;
            (out, err, status)
        };
        let (out, err, status) = match timeout {
            Some(t) => match tokio::time::timeout(t, wait).await {
                Ok(r) => r,
                Err(_) => {
                    return Err(RuntimeError::ComposeFailed {
                        command: shown,
                        exit: None,
                        tail: vec![format!("timed out after {}s", t.as_secs())],
                    });
                }
            },
            None => wait.await,
        };
        let status = status.map_err(|source| RuntimeError::SpawnFailed {
            command: shown.clone(),
            source,
        })?;
        let stdout = out.join();
        if status.success() {
            return Ok(Ran { stdout });
        }
        Err(failure_error(shown, status.code(), &stdout, &err.join()))
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

        self.write_inputs(spec)?;
        let inv = self.spec_invocation(spec);

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

        if spec.uses_bake() && !spec.prebuilt {
            self.bake_images(spec, &inv).await?;
        }
        // A second of slack so lines logged in the same second as `up` are kept.
        let since = now_secs().saturating_sub(1);
        let up_build = spec.build && !spec.uses_bake();
        let up = if spec.uses_bake() {
            ComposeCommand::up_no_build(&inv.target)
        } else {
            ComposeCommand::up(&inv.target, spec.build)
        };
        if up_build {
            // The build output is visible as `docker.build` events.
            let (progress, stem) = (spec.progress.clone(), spec.stem.clone());
            let on_line = move |line: String| {
                progress.send(ImageProgress::Build {
                    stem: stem.clone(),
                    line,
                });
            };
            self.run_streamed(&inv, up, None, &on_line).await?;
        } else {
            self.run(&inv, up, None).await?;
        }
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

    /// Build `spec`'s images with Bake (44), before `setup`
    /// (`build.before_setup`): writes the env and override files first.
    /// Returns `(service, image)` for each built service (`STEMS_IMAGES`).
    pub async fn build_images(
        &self,
        spec: &ComposeSpec,
    ) -> Result<Vec<(String, String)>, RuntimeError> {
        self.ensure_version().await?;
        let lock = self.project_lock(&spec.project_name);
        let _guard = lock.lock().await;
        self.write_inputs(spec)?;
        let inv = self.spec_invocation(spec);
        self.bake_images(spec, &inv).await
    }

    /// The images `spec`'s build services run, without building anything
    /// (`STEMS_IMAGES` when the build is off for this run).
    pub async fn service_images(
        &self,
        spec: &ComposeSpec,
    ) -> Result<Vec<(String, String)>, RuntimeError> {
        let json = self.config_json_text(spec).await?;
        spec.build_services()
            .into_iter()
            .map(|svc| {
                let cfg = service_config(&json, &spec.project_name, &svc, &spec.file)?;
                Ok((svc, cfg.image))
            })
            .collect()
    }

    /// `docker buildx bake` of `spec`'s build services ([`ComposeCommand::
    /// bake`]); the caller wrote the inputs and holds the project lock.
    /// Every service must have `build:`; its context, its Dockerfile's
    /// directory and the secret files are granted `fs.read` along with
    /// `allow_read`. The stem env (plus `COMPOSE_PROJECT_NAME`) is the
    /// process's env, so the files interpolate like `compose up`.
    async fn bake_images(
        &self,
        spec: &ComposeSpec,
        inv: &Invocation,
    ) -> Result<Vec<(String, String)>, RuntimeError> {
        let json = self.config_json_text(spec).await?;
        let mut images = Vec::new();
        let mut allow: BTreeSet<PathBuf> = spec.bake.allow_read.iter().cloned().collect();
        for svc in spec.build_services() {
            let cfg = service_config(&json, &spec.project_name, &svc, &spec.file)?;
            let Some(build) = cfg.build else {
                return Err(RuntimeError::BuildFailed {
                    tail: vec![format!(
                        "service `{svc}` has no `build:` in {}; list only services that are built in `build.services`",
                        spec.file.display()
                    )],
                });
            };
            grant(&mut allow, &build.context);
            if let Some(dir) = build.dockerfile.parent() {
                grant(&mut allow, dir);
            }
            images.push((svc, cfg.image));
        }
        for s in &spec.bake.secrets {
            if let SecretSource::File(f) = &s.source {
                grant(&mut allow, f);
            }
        }
        let allow: Vec<PathBuf> = allow.into_iter().collect();
        let args = ComposeCommand::bake(&inv.target, &images, &allow);
        let mut env = inv.env.clone();
        env.entry("COMPOSE_PROJECT_NAME".into())
            .or_insert_with(|| spec.project_name.clone());
        // The `fs` entitlement checks stay on whatever the environment says.
        env.insert("BUILDX_BAKE_ENTITLEMENTS_FS".into(), "1".into());
        let (progress, stem) = (spec.progress.clone(), spec.stem.clone());
        let on_line = move |line: String| {
            progress.send(ImageProgress::Build {
                stem: stem.clone(),
                line,
            });
        };
        crate::docker::run_build_command(&self.opts.binary, &args, &env, &inv.cwd, &on_line)
            .await?;
        Ok(images)
    }

    /// `docker compose config --format json` for `spec`'s service: its
    /// image name and build inputs ([`ServiceConfig`], deliverable 35).
    /// The stem env is exported for interpolation; the env file (and the
    /// `inject_env` override, which names it) is passed only when it exists
    /// (the first start has not written it yet).
    pub async fn service_config(&self, spec: &ComposeSpec) -> Result<ServiceConfig, RuntimeError> {
        let json = self.config_json_text(spec).await?;
        service_config(&json, &spec.project_name, &spec.service, &spec.file)
    }

    /// The text of `docker compose config --format json` (see
    /// [`Self::service_config`]).
    async fn config_json_text(&self, spec: &ComposeSpec) -> Result<String, RuntimeError> {
        let mut inv = self.spec_invocation(spec);
        if inv.target.env_file.as_ref().is_some_and(|f| !f.exists()) {
            inv.target.env_file = None;
            inv.target.override_file = None;
        }
        if inv
            .target
            .override_file
            .as_ref()
            .is_some_and(|f| !f.exists())
        {
            inv.target.override_file = None;
        }
        let ran = self
            .run(
                &inv,
                ComposeCommand::config_json(&inv.target),
                Some(QUICK_TIMEOUT),
            )
            .await?;
        Ok(ran.stdout)
    }

    fn invocation_of(&self, h: &Handle) -> Option<Invocation> {
        self.targets().get(&h.id()).cloned()
    }

    /// `docker compose rm -f -s -v <service>` and forget the handle
    /// (`down`); an owned project left empty is `down`ed (its network).
    pub async fn remove(&self, h: &Handle) -> Result<(), RuntimeError> {
        let Some(inv) = self.invocation_of(h) else {
            return Err(RuntimeError::NotFound(h.id()));
        };
        {
            let lock = self.project_lock(&inv.target.project);
            let _guard = lock.lock().await;
            self.run(&inv, ComposeCommand::rm(&inv.target), Some(QUICK_TIMEOUT))
                .await?;
            self.prune_project(&inv.target.project).await;
        }
        self.release(h);
        Ok(())
    }

    /// After a service container was removed: when the project is
    /// stems-owned and has no container left, `down` it so its
    /// `<project>_default` network does not outlive it. Best-effort; the
    /// caller holds the project lock.
    async fn prune_project(&self, project: &str) {
        if !self.is_owned(project) {
            return;
        }
        match self.project_ps(project).await {
            Ok(left) if left.is_empty() => {
                let inv = self.project_invocation(project);
                if let Err(e) = self
                    .run(
                        &inv,
                        ComposeCommand::down_project(project),
                        Some(QUICK_TIMEOUT),
                    )
                    .await
                {
                    tracing::debug!(project, error = %e, "compose down of an empty project");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(project, error = %e, "compose ps before prune"),
        }
    }

    /// `docker compose rm -f -s -v <service>` for `spec` without a handle
    /// (the daemon's `down` of a compose stem that was already stopped).
    pub async fn remove_service(&self, spec: &ComposeSpec) -> Result<(), RuntimeError> {
        let mut inv = self.spec_invocation(spec);
        if inv
            .target
            .override_file
            .as_ref()
            .is_some_and(|f| !f.exists())
        {
            inv.target.override_file = None;
        }
        let lock = self.project_lock(&inv.target.project);
        let _guard = lock.lock().await;
        self.run(&inv, ComposeCommand::rm(&inv.target), Some(QUICK_TIMEOUT))
            .await?;
        self.prune_project(&inv.target.project).await;
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
        if let Err(e) = self.write_inputs(spec) {
            tracing::warn!(error = %e, "compose adopt: env file not written");
        }
        let inv = self.spec_invocation(spec);
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
