//! Talking to the workspace daemon from the CLI: resolve the workspace to its
//! [`DaemonPaths`], connect with the `cli:<user>` actor, and turn
//! `DAEMON_NOT_RUNNING` into an actionable message (a stale lock left by a
//! crashed daemon gets `stale lock: ...` in the hint, FR-CR-6; stems of a
//! previous run that are still alive get `run stems down`, deliverable 11).
//!
//! Commands are synchronous; [`block_on`] runs the async client on a small
//! current-thread tokio runtime.

use std::path::PathBuf;

use serde_json::json;
use stems_api::client::{Client, ClientOptions};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::lock::{self, LockState};

use crate::commands::Ctx;
use crate::paths::{DaemonPaths, workspace_root};

/// A workspace and where its daemon lives.
#[derive(Clone, Debug)]
pub struct Target {
    /// Canonical workspace root (the directory holding `stems.yaml`).
    pub workspace: PathBuf,
    /// Stems home (absolute).
    pub home: PathBuf,
    /// Socket, lock, log and state paths.
    pub paths: DaemonPaths,
}

/// Resolve `--workspace` / `STEMS_WORKSPACE` / the cwd to a workspace root
/// and its daemon paths. Only discovery runs (the config is not loaded).
pub fn target(ctx: &Ctx) -> Result<Target, Errors> {
    let file = stems_config::discover(ctx.global.workspace.as_deref(), &ctx.cwd, &ctx.env)
        .map_err(Errors::from)?;
    let workspace = workspace_root(&file);
    let home = ctx.home();
    let paths = DaemonPaths::for_workspace(&home, &workspace);
    Ok(Target {
        workspace,
        home,
        paths,
    })
}

/// The actor sent with every request: `cli:<$USER>`.
pub fn actor(ctx: &Ctx) -> String {
    let user = ctx
        .env
        .get("USER")
        .or_else(|| ctx.env.get("LOGNAME"))
        .filter(|u| !u.is_empty())
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    format!("cli:{user}")
}

/// Client options for `ctx` (actor, version from `STEMS_FAKE_VERSION`).
pub fn options(ctx: &Ctx) -> ClientOptions {
    let mut opts = ClientOptions::new(actor(ctx));
    if let Some(v) = ctx
        .env
        .get(stems_api::client::ENV_FAKE_VERSION)
        .filter(|v| !v.is_empty())
    {
        opts.client_version = v.clone();
    }
    opts
}

/// Connect to the workspace daemon (strict version check).
pub async fn connect(ctx: &Ctx) -> Result<Client, Errors> {
    let t = target(ctx)?;
    connect_to(&t, options(ctx)).await.map_err(Errors::from)
}

/// Connect to `t`'s daemon; `DAEMON_NOT_RUNNING` gets the lock-aware hint.
pub async fn connect_to(t: &Target, opts: ClientOptions) -> Result<Client, Error> {
    Client::connect(&t.paths.socket, opts)
        .await
        .map_err(|e| not_running_hint(e, &t.paths))
}

/// Add the stale-lock hint (and `details.lock`) to a `DAEMON_NOT_RUNNING`.
pub fn not_running_hint(e: Error, paths: &DaemonPaths) -> Error {
    if e.code != ErrorCode::DaemonNotRunning {
        return e;
    }
    let state = lock::probe(paths);
    let (hint, lock) = match &state {
        LockState::Stale { pid } => (
            format!(
                "stale lock: the daemon (pid {pid}) exited without cleaning up (crashed or killed); run `stems daemon start` to reclaim it"
            ),
            json!({ "state": "stale", "pid": pid, "path": paths.lock }),
        ),
        LockState::Held { pid, version } => (
            format!(
                "a daemon (pid {pid}, stems {version}) holds the lock but does not answer; see its log {} or stop it with `stems daemon stop`",
                paths.log_path().display()
            ),
            json!({ "state": "held", "pid": pid, "path": paths.lock }),
        ),
        LockState::Free => (
            "start it with `stems daemon start` (or `stems up`)".to_string(),
            json!({ "state": "free", "path": paths.lock }),
        ),
    };
    // Stems of a previous (crashed) run still alive: `stems down` recovers them.
    let alive: Vec<String> = stems_daemon::state::StateFile::peek(&paths.state)
        .map(|f| {
            f.stems
                .iter()
                .filter(|(_, r)| r.is_alive())
                .map(|(n, _)| n.clone())
                .collect()
        })
        .unwrap_or_default();
    let hint = if alive.is_empty() {
        hint
    } else {
        let stale = match &state {
            LockState::Stale { pid } => format!(" (the daemon, pid {pid}, crashed or was killed)"),
            _ => String::new(),
        };
        format!(
            "stems from a previous run are still alive ({}){stale}; run `stems down` to stop them",
            alive.join(", ")
        )
    };
    let mut details = match e.details.clone() {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    details.insert("socket".into(), json!(paths.socket));
    details.insert("lock".into(), lock);
    if !alive.is_empty() {
        details.insert(
            "state".into(),
            json!({ "path": paths.state, "alive": alive }),
        );
    }
    e.with_hint(hint)
        .with_details(serde_json::Value::Object(details))
}

/// Run a future to completion on a fresh current-thread runtime.
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(f)
}
