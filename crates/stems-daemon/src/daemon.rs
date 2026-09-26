//! The daemon: built-in methods, the supervisor slot and the lifecycle
//! (`Daemon::run`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use stems_api::{
    API_VERSION, DAEMON_ACTOR, DaemonInfo, DaemonStatus, EventKind, EventsParams, EventsResult,
    LoadWorkspaceParams, Method, ShutdownResult, VERSION, WorkspaceLoaded,
};
use stems_config::{LoadOptions, Resolved};
use stems_core::{Error, ErrorCode, ValidateOptions};
use tokio::sync::watch;

use crate::debug::{DebugRpc, ENV_DEBUG_RPC};
use crate::events::{EventBus, EventDraft};
use crate::handler::{Handler, RequestCtx, SupervisorHooks};
use crate::lock;
use crate::paths::{DaemonPaths, workspace_root};
use crate::server::{self, Server};

/// Directory name under the home for a daemon started without a workspace.
pub const NO_WORKSPACE_DIR: &str = "no-workspace";

/// Deadline for the supervisor's shutdown hook.
const SUPERVISOR_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(60);
/// How long connections get to flush after `daemon.stopped`.
const FLUSH_DEADLINE: Duration = Duration::from_secs(2);

/// Options for [`Daemon::run`].
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// Stems home (`$STEMS_HOME`).
    pub home: PathBuf,
    /// Workspace to own and load at startup.
    pub workspace: Option<PathBuf>,
    /// Also log to stderr.
    pub foreground: bool,
    /// `tracing` filter (default `info`; `STEMS_LOG` overrides).
    pub log_level: Option<String>,
    /// Enable `_debug.*` RPCs (default: `STEMS_DEBUG_RPC=1`).
    pub debug_rpc: bool,
    /// Handle SIGTERM/SIGINT/SIGHUP (default true; in-process tests turn it off).
    pub handle_signals: bool,
}

impl RunOptions {
    /// Defaults for `home`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            workspace: None,
            foreground: false,
            log_level: None,
            debug_rpc: std::env::var(ENV_DEBUG_RPC).is_ok_and(|v| v == "1"),
            handle_signals: true,
        }
    }

    /// The daemon paths these options resolve to.
    pub fn paths(&self) -> DaemonPaths {
        match &self.workspace {
            Some(ws) => DaemonPaths::for_workspace(&self.home, ws),
            None => DaemonPaths::in_dir(self.home.join(NO_WORKSPACE_DIR)),
        }
    }
}

struct Loaded {
    root: PathBuf,
    resolved: Option<Arc<Resolved>>,
}

/// The daemon. Shared as `Arc<Daemon>`; implements [`Handler`].
pub struct Daemon {
    paths: DaemonPaths,
    events: Arc<EventBus>,
    started_at: DateTime<Utc>,
    started: Instant,
    pid: u32,
    start_time: u64,
    workspace: RwLock<Loaded>,
    supervisor: RwLock<Option<Arc<dyn SupervisorHooks>>>,
    debug: Option<DebugRpc>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_by: Mutex<Option<(String, String)>>,
    started_by_up: AtomicBool,
}

fn read<T>(l: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(l: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

fn params<T: DeserializeOwned>(method: &str, v: Value) -> Result<T, Error> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|e| {
        Error::usage(
            format!("invalid params for `{method}`: {e}"),
            "see docs/protocol.md for each method's params",
        )
    })
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::internal(e.to_string()))
}

impl Daemon {
    /// A daemon for `paths` (no socket, no lock: see [`Daemon::run`]).
    pub fn new(paths: DaemonPaths, workspace: Option<PathBuf>, debug_rpc: bool) -> Arc<Self> {
        let pid = std::process::id();
        let events = Arc::new(EventBus::default());
        Arc::new(Self {
            paths,
            started_at: Utc::now(),
            started: Instant::now(),
            pid,
            start_time: stems_runtime::os::process_start_time(pid as i32).map_or(0, |s| s.0),
            workspace: RwLock::new(Loaded {
                root: workspace.unwrap_or_default(),
                resolved: None,
            }),
            supervisor: RwLock::new(None),
            debug: debug_rpc.then(|| DebugRpc::new(events.clone())),
            events,
            shutdown_tx: watch::Sender::new(false),
            shutdown_by: Mutex::new(None),
            started_by_up: AtomicBool::new(false),
        })
    }

    /// The event bus (the supervisor emits through it).
    pub fn events(&self) -> &Arc<EventBus> {
        &self.events
    }

    /// This daemon's paths.
    pub fn paths(&self) -> &DaemonPaths {
        &self.paths
    }

    /// The loaded workspace, if `load_workspace` (or startup) succeeded.
    pub fn resolved(&self) -> Option<Arc<Resolved>> {
        read(&self.workspace).resolved.clone()
    }

    /// Install the supervisor hooks (deliverable 10).
    pub fn set_supervisor(&self, hooks: Arc<dyn SupervisorHooks>) {
        *write(&self.supervisor) = Some(hooks);
    }

    /// The daemon was auto-started by `stems up` (then `down` of the last
    /// running stem also shuts it down).
    pub fn started_by_up(&self) -> bool {
        self.started_by_up.load(Ordering::SeqCst)
    }

    /// Record that `stems up` auto-started this daemon.
    pub fn set_started_by_up(&self) {
        self.started_by_up.store(true, Ordering::SeqCst);
    }

    fn supervisor(&self) -> Option<Arc<dyn SupervisorHooks>> {
        read(&self.supervisor).clone()
    }

    /// Trigger the orderly shutdown path (idempotent).
    pub fn request_shutdown(&self, actor: &str, reason: &str) {
        let mut by = self.shutdown_by.lock().unwrap_or_else(|e| e.into_inner());
        if by.is_none() {
            *by = Some((actor.to_string(), reason.to_string()));
        }
        self.shutdown_tx.send_replace(true);
    }

    /// Resolves once shutdown has been requested.
    pub async fn shutdown_requested(&self) {
        let mut rx = self.shutdown_tx.subscribe();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// `info`.
    pub fn info(&self) -> DaemonInfo {
        let root = read(&self.workspace).root.clone();
        DaemonInfo {
            version: VERSION.to_string(),
            api_version: API_VERSION,
            workspace: (!root.as_os_str().is_empty()).then_some(root),
            pid: self.pid,
            start_time: self.start_time,
            uptime_s: self.started.elapsed().as_secs(),
            started_at: self.started_at,
        }
    }

    fn status(&self) -> DaemonStatus {
        let resolved = self.resolved();
        DaemonStatus {
            info: self.info(),
            workspace_name: resolved.as_ref().map(|r| r.workspace.name.clone()),
            stem_count: resolved.as_ref().map_or(0, |r| r.workspace.stems.len()),
            last_seq: self.events.last_seq(),
            subscribers: self.events.subscriber_count(),
            debug_rpc: self.debug.is_some(),
        }
    }

    /// Load + validate a workspace, store it and emit `workspace.loaded`.
    pub fn load_workspace(&self, path: &Path, actor: &str) -> Result<WorkspaceLoaded, Error> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let load = LoadOptions {
            workspace: Some(path.to_path_buf()),
            cwd,
            env: std::env::vars().collect(),
            skip_local: false,
        };
        let resolved =
            stems_core::load_and_validate(load, &ValidateOptions::default()).map_err(|errs| {
                let all = to_value(&errs).unwrap_or(Value::Null);
                let mut first = errs.0.into_iter().next().unwrap_or_else(|| {
                    Error::internal("workspace failed to load without an error")
                });
                if all.as_array().is_some_and(|a| a.len() > 1) {
                    first.details = json!({ "errors": all });
                }
                first
            })?;
        let ws = &resolved.workspace;
        let out = WorkspaceLoaded {
            root: ws.root.clone(),
            name: ws.name.clone(),
            stems: ws.stems.keys().cloned().collect(),
            sources: resolved.sources.clone(),
        };
        {
            let mut w = write(&self.workspace);
            w.root = ws.root.clone();
            w.resolved = Some(Arc::new(resolved));
        }
        tracing::info!(root = %out.root.display(), name = %out.name, stems = out.stems.len(), "workspace loaded");
        self.events.emit(
            EventDraft::new(EventKind::WORKSPACE_LOADED, actor).data(json!({
                "root": out.root,
                "name": out.name,
                "stems": out.stems,
            })),
        );
        Ok(out)
    }

    /// Run a daemon with the [`Supervisor`](crate::supervisor::Supervisor)
    /// installed until shutdown (see the crate docs for the lifecycle).
    pub async fn run(opts: RunOptions) -> Result<(), Error> {
        Self::run_with(opts, |d| {
            d.set_supervisor(crate::supervisor::Supervisor::for_daemon(d));
        })
        .await
    }

    /// [`Daemon::run`] with a hook called once the daemon exists, before the
    /// socket accepts connections (install the supervisor here).
    pub async fn run_with(opts: RunOptions, setup: impl FnOnce(&Arc<Daemon>)) -> Result<(), Error> {
        let workspace = opts.workspace.as_deref().map(workspace_root);
        let paths = opts.paths();
        paths
            .ensure_dir()
            .map_err(|e| Error::internal(format!("cannot create {}: {e}", paths.dir.display())))?;
        crate::logging::init(
            &paths.log_path(),
            opts.foreground,
            opts.log_level.as_deref(),
        );

        let guard = match lock::acquire(&paths) {
            Ok(g) => g,
            Err(e) => {
                tracing::error!(code = %e.code, "{}", e.message);
                return Err(e);
            }
        };
        let listener = server::bind(&paths.socket).inspect_err(|e| {
            tracing::error!("{}", e.message);
        })?;

        let daemon = Daemon::new(paths.clone(), workspace.clone(), opts.debug_rpc);
        setup(&daemon);
        tracing::info!(
            pid = daemon.pid,
            version = VERSION,
            socket = %paths.socket.display(),
            workspace = ?workspace,
            debug_rpc = opts.debug_rpc,
            "daemon starting"
        );

        let (closing_tx, closing_rx) = watch::channel(false);
        let (stop_tx, stop_rx) = watch::channel(false);
        let srv = Arc::new(Server {
            handler: daemon.clone(),
            events: daemon.events.clone(),
            closing: closing_rx,
        });
        let accept = tokio::spawn(server::accept_loop(listener, srv, stop_rx));

        daemon.events.emit(
            EventDraft::new(EventKind::DAEMON_STARTED, DAEMON_ACTOR).data(json!({
                "pid": daemon.pid,
                "version": VERSION,
                "api_version": API_VERSION,
                "socket": paths.socket,
                "workspace": workspace,
                "reclaimed_stale_lock_of": guard.reclaimed_from,
            })),
        );
        if let Some(ws) = &workspace
            && let Err(e) = daemon.load_workspace(ws, DAEMON_ACTOR)
        {
            tracing::warn!(code = %e.code, "workspace did not load: {}", e.message);
        }
        tracing::info!("daemon listening");

        tokio::select! {
            _ = daemon.shutdown_requested() => {}
            sig = wait_for_signal(opts.handle_signals) => {
                tracing::info!(signal = sig, "signal received");
                daemon.request_shutdown("signal", &format!("received {sig}"));
            }
        }

        // ---- orderly shutdown -------------------------------------------------
        let (actor, reason) = daemon
            .shutdown_by
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| (DAEMON_ACTOR.into(), "shutdown".into()));
        tracing::info!(%actor, %reason, "daemon stopping");
        daemon.events.emit(
            EventDraft::new(EventKind::DAEMON_STOPPING, actor.clone()).reason(reason.clone()),
        );
        if let Some(sup) = daemon.supervisor()
            && tokio::time::timeout(SUPERVISOR_SHUTDOWN_DEADLINE, sup.shutdown())
                .await
                .is_err()
        {
            tracing::error!("supervisor shutdown hook exceeded its deadline");
        }
        if let Some(d) = &daemon.debug {
            d.stop_all().await;
        }
        stop_tx.send_replace(true);
        let mut conns = accept.await.unwrap_or_default();
        match std::fs::remove_file(&paths.socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "cannot remove socket"),
        }
        guard.release();
        drop(guard);
        daemon
            .events
            .emit(EventDraft::new(EventKind::DAEMON_STOPPED, actor).reason(reason));
        closing_tx.send_replace(true);
        let _ = tokio::time::timeout(FLUSH_DEADLINE, async {
            while conns.join_next().await.is_some() {}
        })
        .await;
        conns.abort_all();
        tracing::info!("daemon stopped");
        Ok(())
    }
}

async fn wait_for_signal(enabled: bool) -> &'static str {
    use tokio::signal::unix::{SignalKind, signal};
    if !enabled {
        return std::future::pending().await;
    }
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    ) else {
        tracing::error!("cannot install signal handlers");
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
        _ = hup.recv() => "SIGHUP",
    }
}

#[async_trait::async_trait]
impl Handler for Daemon {
    async fn handle(&self, ctx: RequestCtx, method: &str, p: Value) -> Result<Value, Error> {
        match method {
            m if m == Method::PING => Ok(json!({ "pong": true })),
            m if m == Method::INFO => to_value(self.info()),
            m if m == Method::DAEMON_STATUS => to_value(self.status()),
            m if m == Method::EVENTS => {
                let p: EventsParams = params(m, p)?;
                let mut events = self.events.replay(p.since_seq.unwrap_or(0));
                if let Some(n) = p.limit {
                    events.truncate(n);
                }
                to_value(EventsResult {
                    events,
                    last_seq: self.events.last_seq(),
                })
            }
            m if m == Method::LOAD_WORKSPACE => {
                let p: LoadWorkspaceParams = params(m, p)?;
                to_value(self.load_workspace(&p.path, &ctx.actor)?)
            }
            m if m == Method::SHUTDOWN => {
                self.request_shutdown(&ctx.actor, "shutdown requested");
                to_value(ShutdownResult { stopping: true })
            }
            m if m == Method::SUBSCRIBE_EVENTS => Err(Error::usage(
                "`subscribe_events` is a streaming method handled by the server",
                "send it as a request on its own connection",
            )),
            _ => {
                if method.starts_with("_debug.")
                    && let Some(d) = &self.debug
                    && let Some(r) = d.handle(&ctx, method, p.clone()).await
                {
                    return r;
                }
                if let Some(sup) = self.supervisor()
                    && let Some(r) = sup.handle(&ctx, method, &p).await
                {
                    return r;
                }
                Err(not_implemented(method, self.debug.is_some()))
            }
        }
    }
}

fn not_implemented(method: &str, debug: bool) -> Error {
    let hint = if method.starts_with("_debug.") && !debug {
        format!("`{method}` needs the daemon started with STEMS_DEBUG_RPC=1")
    } else {
        format!(
            "`{method}` is not a method of this daemon (misspelled, or it lands in a later version); see docs/protocol.md for the method list"
        )
    };
    Error::new(
        ErrorCode::NotImplemented,
        format!("method `{method}` is not implemented"),
    )
    .with_hint(hint)
    .with_details(json!({ "method": method }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestCtx {
        RequestCtx {
            actor: "cli:test".into(),
            ..RequestCtx::daemon()
        }
    }

    #[tokio::test]
    async fn builtins() {
        let d = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(DaemonPaths::in_dir(d.path()), None, false);
        assert_eq!(
            daemon.handle(ctx(), "ping", json!({})).await.unwrap(),
            json!({"pong": true})
        );
        let info: DaemonInfo =
            serde_json::from_value(daemon.handle(ctx(), "info", Value::Null).await.unwrap())
                .unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.workspace, None);
        let e = daemon.handle(ctx(), "up", json!({})).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::NotImplemented);
        assert!(e.hint.unwrap().contains("`up`"));
        let e = daemon
            .handle(ctx(), "_debug.start_raw", json!({}))
            .await
            .unwrap_err();
        assert!(e.hint.unwrap().contains("STEMS_DEBUG_RPC"));
        let e = daemon
            .handle(ctx(), "events", json!({"since_seq": "x"}))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
    }

    struct Sup;
    #[async_trait::async_trait]
    impl SupervisorHooks for Sup {
        async fn handle(
            &self,
            _ctx: &RequestCtx,
            method: &str,
            _params: &Value,
        ) -> Option<Result<Value, Error>> {
            (method == "up").then(|| Ok(json!({"up": true})))
        }
        async fn shutdown(&self) {}
    }

    #[tokio::test]
    async fn supervisor_slot() {
        let d = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(DaemonPaths::in_dir(d.path()), None, false);
        daemon.set_supervisor(Arc::new(Sup));
        assert_eq!(
            daemon.handle(ctx(), "up", json!({})).await.unwrap(),
            json!({"up": true})
        );
        assert_eq!(
            daemon
                .handle(ctx(), "down", json!({}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotImplemented
        );
    }

    #[tokio::test]
    async fn load_workspace_errors_are_config_errors() {
        let d = tempfile::tempdir().unwrap();
        let daemon = Daemon::new(DaemonPaths::in_dir(d.path()), None, false);
        let e = daemon
            .handle(
                ctx(),
                "load_workspace",
                json!({"path": d.path().join("nope")}),
            )
            .await
            .unwrap_err();
        assert_eq!(e.exit_code(), 2, "{e}");
    }
}
