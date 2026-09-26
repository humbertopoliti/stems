//! The daemon behind the MCP server: workspace discovery, connections with
//! the caller's actor, `--auto-start`, and the shutdown of a daemon this
//! server started.
//!
//! A fresh connection is opened for every tool call (a local `info`
//! round-trip), so a daemon restarted behind the server's back is simply
//! picked up again, and a long `up` never blocks a concurrent `get_status`.
//!
//! **Auto-start** (`stems mcp --auto-start`): when a tool needs the daemon
//! and none answers, the server starts it detached (`stems daemon --home …
//! --workspace …`, which loads the workspace) and remembers that it did.
//! When the MCP client disconnects, a daemon this server started is shut
//! down if no stem is running; stems an agent left running keep the daemon
//! alive (stop them with `down`, or `stems down`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::json;
use stems_api::client::{Client, ClientOptions};
use stems_api::{Method, StatusParams, StatusResult};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::DaemonPaths;
use tokio::sync::Mutex;

use crate::McpOptions;

/// How long an auto-started daemon may take to listen.
pub const START_WAIT: Duration = Duration::from_secs(10);
/// How long to wait for a daemon to go away after `shutdown`.
pub const STOP_WAIT: Duration = Duration::from_secs(10);

/// A workspace and where its daemon lives.
#[derive(Clone, Debug)]
pub struct Target {
    /// Canonical workspace root.
    pub workspace: PathBuf,
    /// Socket, lock, log and state paths.
    pub paths: DaemonPaths,
}

/// Connections to the workspace daemon.
#[derive(Debug)]
pub struct Backend {
    opts: McpOptions,
    /// Serialises auto-starts (two concurrent calls must not spawn two daemons).
    starting: Mutex<()>,
    /// This server started the daemon (`--auto-start`).
    started: AtomicBool,
    /// The actor of the last connection (used for the final shutdown).
    last_actor: std::sync::Mutex<Option<String>>,
}

/// One error out of a config error list (the first, with every error in
/// `details.errors` when there are several).
pub fn first_error(errors: Errors) -> Error {
    let mut v = errors.into_vec();
    stems_core::sort_errors(&mut v);
    match v.len() {
        0 => Error::internal("unknown configuration error"),
        1 => v.remove(0),
        _ => {
            let all = serde_json::to_value(&v).unwrap_or_default();
            let first = v.remove(0);
            let details = match first.details.clone() {
                serde_json::Value::Object(mut m) => {
                    m.insert("errors".into(), all);
                    serde_json::Value::Object(m)
                }
                _ => json!({ "errors": all }),
            };
            first.with_details(details)
        }
    }
}

impl Backend {
    /// A backend for `opts`.
    pub fn new(opts: McpOptions) -> Self {
        Self {
            opts,
            starting: Mutex::new(()),
            started: AtomicBool::new(false),
            last_actor: std::sync::Mutex::new(None),
        }
    }

    /// The invocation options.
    pub fn options(&self) -> &McpOptions {
        &self.opts
    }

    /// Whether this server started the daemon.
    pub fn auto_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    /// Resolve the workspace and its daemon paths (discovery only).
    pub fn target(&self) -> Result<Target, Error> {
        let file = stems_config::discover(
            self.opts.workspace.as_deref(),
            &self.opts.cwd,
            &self.opts.env,
        )
        .map_err(|e| first_error(Errors::from(e)))?;
        let workspace = stems_daemon::paths::workspace_root(&file);
        let paths = DaemonPaths::for_workspace(&self.opts.home, &workspace);
        Ok(Target { workspace, paths })
    }

    /// Load the workspace config from disk.
    pub fn load_config(&self) -> Result<stems_config::Resolved, Error> {
        stems_config::load(self.opts.load_options()).map_err(|e| first_error(Errors::from(e)))
    }

    fn client_options(actor: &str) -> ClientOptions {
        ClientOptions::new(actor)
    }

    fn not_running(t: &Target, e: Error) -> Error {
        if e.code != ErrorCode::DaemonNotRunning {
            return e;
        }
        let mut details = match e.details.clone() {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        details.insert("socket".into(), json!(t.paths.socket));
        details.insert("workspace".into(), json!(t.workspace));
        e.with_hint(
            "start it with `stems up` or `stems daemon start` in the workspace, or run the MCP server with `stems mcp --auto-start`",
        )
        .with_details(serde_json::Value::Object(details))
    }

    /// Connect without ever starting a daemon.
    pub async fn connect_existing(&self, actor: &str) -> Result<Client, Error> {
        let t = self.target()?;
        Client::connect(&t.paths.socket, Self::client_options(actor))
            .await
            .map_err(|e| Self::not_running(&t, e))
    }

    /// Connect to the daemon; with `--auto-start`, start it first if it is
    /// not running.
    pub async fn connect(&self, actor: &str) -> Result<Client, Error> {
        let t = self.target()?;
        *self
            .last_actor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(actor.to_string());
        match Client::connect(&t.paths.socket, Self::client_options(actor)).await {
            Ok(c) => return Ok(c),
            Err(e) if e.code != ErrorCode::DaemonNotRunning || !self.opts.auto_start => {
                return Err(Self::not_running(&t, e));
            }
            Err(_) => {}
        }
        let _guard = self.starting.lock().await;
        // Another call may have started it meanwhile.
        if let Ok(c) = Client::connect(&t.paths.socket, Self::client_options(actor)).await {
            return Ok(c);
        }
        let args = stems_daemon::daemon_args(&self.opts.home, Some(&t.workspace), false);
        stems_daemon::spawn_detached(&self.opts.binary, &args, &t.paths)?;
        stems_daemon::wait_for_socket(&t.paths, START_WAIT).await?;
        self.started.store(true, Ordering::SeqCst);
        Client::connect(&t.paths.socket, Self::client_options(actor))
            .await
            .map_err(|e| Self::not_running(&t, e))
    }

    /// Shut down a daemon this server started if no stem is running, and
    /// wait (bounded) for its socket and lock to disappear.
    pub async fn stop_if_auto_started(&self) {
        if !self.auto_started() {
            return;
        }
        let Ok(t) = self.target() else { return };
        let actor = self
            .last_actor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| format!("mcp:{}", crate::server::UNKNOWN_CLIENT));
        let Ok(c) = Client::connect(&t.paths.socket, Self::client_options(&actor)).await else {
            return;
        };
        let st: Result<StatusResult, Error> = c.call(Method::STATUS, StatusParams::default()).await;
        let idle = st.is_ok_and(|s| s.stems.iter().all(|x| !x.state.is_running()));
        if !idle {
            return;
        }
        if c.shutdown().await.is_err() {
            return;
        }
        let deadline = Instant::now() + STOP_WAIT;
        while (t.paths.socket.exists() || t.paths.lock.exists()) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
