//! `stems doctor` (FR-CL-3, FR-WD-4, FR-CR-4; deliverable 19,
//! `docs/doctor.md`): why this machine is not ready to run the workspace,
//! and what can be fixed safely.
//!
//! * A **check** ([`Check`]) produces one or more [`CheckResult`]s
//!   `{ id, status: ok|warn|fail, message, hint, fixable, details }`. Every
//!   check runs with a [`CHECK_TIMEOUT`] (a wedged Docker never hangs
//!   `doctor`); one that runs out reports `fail` "timed out".
//! * The **registry** ([`registry`]) is built from the workspace: one check
//!   per tool / stem / port, so each has its own timeout. Later deliverables
//!   add checks here (metrics thresholds, config reload).
//! * [`run`] runs the registry concurrently and returns a [`DoctorReport`];
//!   [`fix::apply`] applies the fixable items and the report is re-run.
//!
//! Works daemonless: the CLI runs the checks itself and only asks a running
//! daemon for its health ([`DaemonProbe`]). The fast subset `stems up` runs
//! before starting anything is [`preflight_up`].

mod checks;
pub mod fix;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use stems_api::DaemonStatus;
use stems_config::{LoadOptions, Resolved, StemType, Workspace};
use stems_core::{Error, ErrorCode, Glyph};
use stems_runtime::{DockerOptions, DockerRuntime};

use crate::paths::DaemonPaths;
use crate::state::StateFile;

pub use checks::{hot_reload_command, watch_overlaps_sources};

/// Time budget of one check.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(2);
/// How long connecting to Docker may take (inside the check budget).
pub const DOCKER_CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Free space below this is a warning (`disk.*`).
pub const MIN_FREE_BYTES: u64 = 1 << 30;

/// Outcome of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// Fine.
    Ok,
    /// Works, but something deserves attention (exit 0 unless `--strict`).
    Warn,
    /// Will not work (exit 1).
    Fail,
}

impl CheckStatus {
    /// `ok` / `warn` / `fail`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }

    /// The status glyph (`✓`, `!`, `✗`).
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Ok => Glyph::Healthy.unicode(),
            Self::Warn => Glyph::Degraded.unicode(),
            Self::Fail => Glyph::Failed.unicode(),
        }
    }
}

/// One line of the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Stable id (`requires.node`, `ports.shop-api.http`; see `docs/doctor.md`).
    pub id: String,
    /// Outcome.
    pub status: CheckStatus,
    /// One line.
    pub message: String,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// `stems doctor --fix` can repair it.
    pub fixable: bool,
    /// Machine-readable facts (pid, versions, paths).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    /// The error code a failure maps to (`TOOL_VERSION`, `DOCKER_UNAVAILABLE`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
}

impl CheckResult {
    fn new(id: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status,
            message: message.into(),
            hint: None,
            fixable: false,
            details: Value::Null,
            code: None,
        }
    }

    /// An `ok` result.
    pub fn ok(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Ok, message)
    }

    /// A `warn` result.
    pub fn warn(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Warn, message)
    }

    /// A `fail` result.
    pub fn fail(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Fail, message)
    }

    /// A result from a stems error (message, hint, details and code).
    pub fn from_error(id: impl Into<String>, status: CheckStatus, e: &Error) -> Self {
        Self {
            hint: e.hint.clone(),
            details: e.details.clone(),
            code: Some(e.code),
            ..Self::new(id, status, e.message.clone())
        }
    }

    /// Set the hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Mark as fixable by `--fix`.
    #[must_use]
    pub fn fixable(mut self) -> Self {
        self.fixable = true;
        self
    }

    /// Set the details.
    #[must_use]
    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// Set the error code.
    #[must_use]
    pub fn code(mut self, code: ErrorCode) -> Self {
        self.code = Some(code);
        self
    }

    /// The stems error for this result (`errors[]` of the envelope).
    pub fn to_error(&self) -> Error {
        let mut details = match &self.details {
            Value::Object(m) => m.clone(),
            Value::Null => serde_json::Map::new(),
            other => {
                let mut m = serde_json::Map::new();
                m.insert("value".into(), other.clone());
                m
            }
        };
        details.insert("check".into(), json!(self.id));
        let mut e = Error::new(
            self.code.unwrap_or(ErrorCode::Internal),
            self.message.clone(),
        )
        .with_details(Value::Object(details));
        if let Some(h) = &self.hint {
            e = e.with_hint(h.clone());
        }
        e
    }
}

/// What `--fix` did for one item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixOutcome {
    /// The check id it fixed.
    pub id: String,
    /// What was done (`remove_stale_lock`, `remove_overlay`, `kill_orphan`, ...).
    pub action: String,
    /// Whether it worked.
    pub ok: bool,
    /// One line.
    pub message: String,
}

/// Counts per status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// `ok` results.
    pub ok: usize,
    /// `warn` results.
    pub warn: usize,
    /// `fail` results.
    pub fail: usize,
}

/// `stems doctor --json` → `data`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// No `fail` (and, with `--strict`, no `warn`).
    pub ok: bool,
    /// Every result, in registry order.
    pub checks: Vec<CheckResult>,
    /// What `--fix` did (empty without `--fix`).
    pub fixed: Vec<FixOutcome>,
    /// Counts.
    pub summary: Summary,
}

impl DoctorReport {
    /// A report over `checks` (`ok` = no failure, and no warning if `strict`).
    pub fn new(checks: Vec<CheckResult>, strict: bool) -> Self {
        let mut summary = Summary::default();
        for c in &checks {
            match c.status {
                CheckStatus::Ok => summary.ok += 1,
                CheckStatus::Warn => summary.warn += 1,
                CheckStatus::Fail => summary.fail += 1,
            }
        }
        Self {
            ok: summary.fail == 0 && !(strict && summary.warn > 0),
            checks,
            fixed: Vec::new(),
            summary,
        }
    }

    /// 0 when `ok`, else 1.
    pub fn exit_code(&self) -> i32 {
        if self.ok {
            0
        } else {
            stems_core::exit::RUNTIME
        }
    }

    /// The results `--fix` can repair.
    pub fn fixable(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks
            .iter()
            .filter(|c| c.fixable && c.status != CheckStatus::Ok)
    }

    /// The human table: `CHECK STATUS MESSAGE`, then hints, fixes and the summary.
    pub fn render_human(&self) -> String {
        let w = self
            .checks
            .iter()
            .map(|c| c.id.chars().count())
            .max()
            .unwrap_or(5)
            .max(5);
        let mut out = format!("{:<w$}  {:<6}  MESSAGE\n", "CHECK", "STATUS");
        for c in &self.checks {
            let fix = if c.fixable && c.status != CheckStatus::Ok {
                " (fixable)"
            } else {
                ""
            };
            out.push_str(&format!(
                "{:<w$}  {} {:<4}  {}{fix}\n",
                c.id,
                c.status.glyph(),
                c.status.as_str(),
                c.message
            ));
        }
        let hints: Vec<&CheckResult> = self
            .checks
            .iter()
            .filter(|c| c.status != CheckStatus::Ok && c.hint.is_some())
            .collect();
        if !hints.is_empty() {
            out.push_str("\nhints:\n");
            for c in hints {
                out.push_str(&format!(
                    "  {}: {}\n",
                    c.id,
                    c.hint.as_deref().unwrap_or("")
                ));
            }
        }
        if !self.fixed.is_empty() {
            out.push_str("\nfixed:\n");
            for f in &self.fixed {
                out.push_str(&format!(
                    "  {} {}: {} ({})\n",
                    if f.ok {
                        CheckStatus::Ok.glyph()
                    } else {
                        CheckStatus::Fail.glyph()
                    },
                    f.id,
                    f.message,
                    f.action
                ));
            }
        }
        out.push_str(&format!(
            "\n{} ok, {} warn, {} fail\n",
            self.summary.ok, self.summary.warn, self.summary.fail
        ));
        out
    }
}

/// What the CLI learned about the workspace daemon.
#[derive(Clone, Debug)]
pub enum DaemonProbe {
    /// Nothing answers on the socket.
    NotRunning,
    /// A compatible daemon answered `daemon_status`.
    Running(Box<DaemonStatus>),
    /// A daemon answered, but it is another stems / API version.
    Incompatible(Error),
    /// Something else went wrong talking to it.
    Unreachable(Error),
}

impl DaemonProbe {
    /// The daemon's pid, if one answered.
    pub fn pid(&self) -> Option<i32> {
        match self {
            Self::Running(s) => i32::try_from(s.info.pid).ok(),
            _ => None,
        }
    }
}

/// Inputs of a doctor run.
#[derive(Clone, Debug)]
pub struct DoctorInput {
    /// The workspace's daemon files.
    pub paths: DaemonPaths,
    /// `STEMS_HOME`.
    pub home: PathBuf,
    /// How to load the workspace.
    pub load: LoadOptions,
    /// The daemon, as probed by the caller.
    pub daemon: DaemonProbe,
    /// `DOCKER_HOST` (from the caller's environment).
    pub docker_host: Option<String>,
    /// Pids that are never orphans (the caller itself, the daemon).
    pub ignore_pids: Vec<i32>,
}

/// Everything the checks share: the loaded workspace, the state file and a
/// lazily connected Docker client.
pub struct DoctorCtx {
    /// The inputs.
    pub input: DoctorInput,
    /// The workspace, if it loaded.
    pub resolved: Option<Arc<Resolved>>,
    /// Load or validation errors.
    pub config_errors: Vec<Error>,
    /// `state.json`, if present.
    pub state: Option<StateFile>,
    docker: tokio::sync::OnceCell<Result<Arc<DockerRuntime>, Error>>,
}

impl std::fmt::Debug for DoctorCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoctorCtx")
            .field("input", &self.input)
            .field("config_errors", &self.config_errors)
            .finish_non_exhaustive()
    }
}

impl DoctorCtx {
    /// Load the workspace (validated, without probing tools or overlays:
    /// those are checks of their own) and peek at the state file.
    pub fn load(input: DoctorInput) -> Self {
        let (resolved, config_errors) = match stems_config::load(input.load.clone()) {
            Ok(r) => {
                let opts = stems_core::ValidateOptions {
                    skip_requires: true,
                    skip_overlays: true,
                    ..Default::default()
                };
                let errs = stems_core::validate(&r, &opts);
                (Some(Arc::new(r)), errs)
            }
            Err(e) => (None, stems_core::Errors::from(e).0),
        };
        let state = StateFile::peek(&input.paths.state);
        Self {
            input,
            resolved,
            config_errors,
            state,
            docker: tokio::sync::OnceCell::new(),
        }
    }

    /// The loaded workspace.
    pub fn workspace(&self) -> Option<&Workspace> {
        self.resolved.as_deref().map(|r| &r.workspace)
    }

    /// Any enabled docker or compose stem?
    pub fn needs_docker(&self) -> bool {
        self.workspace().is_some_and(|ws| {
            ws.stems()
                .any(|s| matches!(s.kind(), StemType::Docker | StemType::Compose))
        })
    }

    /// Any enabled compose stem?
    pub fn needs_compose(&self) -> bool {
        self.workspace()
            .is_some_and(|ws| ws.stems().any(|s| s.kind() == StemType::Compose))
    }

    /// Docker, connected once (bounded by [`DOCKER_CONNECT_TIMEOUT`]).
    pub async fn docker(&self) -> Result<Arc<DockerRuntime>, Error> {
        self.docker
            .get_or_init(|| async {
                connect_docker(self.input.docker_host.clone(), self.workspace())
                    .await
                    .map(Arc::new)
            })
            .await
            .clone()
    }
}

/// Connect to Docker (`DOCKER_UNAVAILABLE` on any failure or timeout).
pub async fn connect_docker(
    host: Option<String>,
    ws: Option<&Workspace>,
) -> Result<DockerRuntime, Error> {
    let opts = DockerOptions {
        host: host.filter(|h| !h.trim().is_empty()),
        timeout: DOCKER_CONNECT_TIMEOUT,
        workspace: ws.map(|w| w.name.clone()),
    };
    match tokio::time::timeout(
        DOCKER_CONNECT_TIMEOUT + Duration::from_millis(200),
        DockerRuntime::connect(opts),
    )
    .await
    {
        Ok(Ok(d)) => Ok(d),
        Ok(Err(e)) => Err(runtime_error(e)),
        Err(_) => Err(Error::new(
            ErrorCode::DockerUnavailable,
            "connecting to the Docker daemon timed out",
        )
        .with_hint(stems_runtime::docker::UNAVAILABLE_HINT)),
    }
}

/// A runtime error as a stems error (`DOCKER_UNAVAILABLE` for an
/// unreachable engine or a missing Compose v2).
pub fn runtime_error(e: stems_runtime::RuntimeError) -> Error {
    use stems_runtime::RuntimeError as R;
    match e {
        R::DockerUnavailable { hint } => Error::new(
            ErrorCode::DockerUnavailable,
            "the Docker daemon is not reachable",
        )
        .with_hint(hint),
        R::ComposeUnavailable { hint } => Error::new(
            ErrorCode::DockerUnavailable,
            "docker compose v2 is not available",
        )
        .with_hint(hint),
        R::ComposeFailed { .. } => Error::new(ErrorCode::ComposeFailed, e.to_string()),
        other => Error::internal(other.to_string()),
    }
}

/// One doctor check. `id` names the results of a check that times out.
#[async_trait]
pub trait Check: Send + Sync {
    /// Id of the (main) result.
    fn id(&self) -> String;
    /// Run; blocking work goes through `spawn_blocking`.
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult>;
}

/// Run one check within `timeout`.
pub async fn run_one(check: &dyn Check, cx: Arc<DoctorCtx>, timeout: Duration) -> Vec<CheckResult> {
    match tokio::time::timeout(timeout, check.run(cx)).await {
        Ok(r) => r,
        Err(_) => vec![
            CheckResult::fail(
                check.id(),
                format!("timed out after {:.1}s", timeout.as_secs_f64()),
            )
            .hint("something this check talks to does not answer (Docker, git, a tool); retry, or run the command it needs by hand"),
        ],
    }
}

/// Run `checks` concurrently, each within `timeout`; results in registry order.
pub async fn run_checks(
    checks: &[Box<dyn Check>],
    cx: Arc<DoctorCtx>,
    timeout: Duration,
) -> Vec<CheckResult> {
    let futs = checks
        .iter()
        .map(|c| run_one(c.as_ref(), cx.clone(), timeout));
    futures::future::join_all(futs)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// The checks for this workspace.
pub fn registry(cx: &DoctorCtx) -> Vec<Box<dyn Check>> {
    checks::registry(cx)
}

/// Load, run the registry and build the report.
pub async fn run(input: DoctorInput, strict: bool) -> DoctorReport {
    let cx = Arc::new(DoctorCtx::load(input));
    let checks = registry(&cx);
    DoctorReport::new(run_checks(&checks, cx, CHECK_TIMEOUT).await, strict)
}

/// The fast subset `stems up` runs before starting anything: Docker must be
/// reachable when the selection (with its hard dependencies) contains a
/// docker or compose stem. Ports and orphans are `up`'s orphan scan; a stale
/// lock is reclaimed by the daemon it starts.
pub async fn preflight_up(
    resolved: &Resolved,
    requested: &[String],
    docker_host: Option<String>,
) -> Result<(), Error> {
    let Ok(plan) = crate::supervisor::schedule::plan(resolved, requested, false) else {
        // Unknown stems etc. are reported by the `up` RPC itself.
        return Ok(());
    };
    let ws = &resolved.workspace;
    let docker: Vec<&str> = plan
        .order
        .iter()
        .filter(|n| {
            ws.stem(n)
                .is_some_and(|s| matches!(s.kind(), StemType::Docker | StemType::Compose))
        })
        .map(String::as_str)
        .collect();
    if docker.is_empty() {
        return Ok(());
    }
    connect_docker(docker_host, Some(ws))
        .await
        .map(|_| ())
        .map_err(|e| {
            let mut details = match e.details.clone() {
                Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            details.insert("stems".into(), json!(docker));
            let hint = e
                .hint
                .clone()
                .unwrap_or_else(|| stems_runtime::docker::UNAVAILABLE_HINT.to_string());
            Error::new(
                ErrorCode::DockerUnavailable,
                format!(
                    "{} (needed by {}); nothing was started",
                    e.message,
                    docker.join(", ")
                ),
            )
            .with_hint(format!(
                "{hint}; or start only process stems (`stems up <stem>`)"
            ))
            .with_details(Value::Object(details))
        })
}

#[cfg(test)]
mod tests;
