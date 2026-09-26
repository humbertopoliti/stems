//! `stems daemon [start|stop|status]` (FR-CR-5, FR-CR-6, FR-DS-2).
//!
//! * `stems daemon` (hidden run mode, what `start` spawns): runs
//!   [`stems_daemon::Daemon::run`] on a 2-thread tokio runtime until a
//!   `shutdown` RPC or SIGTERM/SIGINT/SIGHUP.
//! * `start [--foreground]`: `data: { already_running, pid, version,
//!   api_version, workspace, socket, lock, log }`.
//! * `status`: `data: { running, pid, version, api_version, workspace,
//!   workspace_name, uptime_s, started_at, socket, lock, lock_state, log,
//!   stem_count, last_seq, subscribers, debug_rpc, client_version,
//!   compatible, state }`; not running: `data: { running: false, workspace,
//!   socket, lock, lock_state, log, state }` plus `DAEMON_NOT_RUNNING` (exit
//!   4). `state` is `{ run_id, stems, alive, path }` read from `state.json`
//!   (`null` without one; deliverable 11).
//! * `stop`: `data: { stopped: true, pid }`; a stale lock (crashed daemon)
//!   is cleaned up with `data: { stopped: false, stale_lock_removed: true,
//!   pid }` (exit 0); nothing at all: `DAEMON_NOT_RUNNING` (exit 4).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use stems_api::client::ClientOptions;
use stems_api::{DaemonInfo, Method, version_mismatch};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::lock::{self, LockState};
use stems_daemon::{Daemon, RunOptions, daemon_args, spawn_detached, wait_for_socket};

use crate::cli::{DaemonArgs, DaemonCommand};
use crate::client::{self, Target, block_on, connect_to};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// How long `start` waits for the socket and `stop` for socket + lock removal.
pub const WAIT: Duration = Duration::from_secs(5);

/// Env var enabling the daemon's `_debug.*` RPCs.
pub const ENV_DEBUG_RPC: &str = "STEMS_DEBUG_RPC";

/// Run `stems daemon ...`.
pub fn run(ctx: &Ctx, args: &DaemonArgs) -> CommandOutput {
    let r = match &args.action {
        None => return run_daemon(ctx, None, args.foreground, args.log_level.clone()),
        Some(DaemonCommand::Start(a)) => start(ctx, a.foreground, args.log_level.clone()),
        Some(DaemonCommand::Stop) => stop(ctx),
        Some(DaemonCommand::Status) => return status(ctx),
    };
    r.unwrap_or_else(CommandOutput::failed)
}

fn lenient(ctx: &Ctx) -> ClientOptions {
    let mut o = client::options(ctx);
    o.check_version = false;
    o
}

/// The hidden run mode: own the workspace until shutdown.
fn run_daemon(
    ctx: &Ctx,
    workspace: Option<PathBuf>,
    foreground: bool,
    log_level: Option<String>,
) -> CommandOutput {
    let workspace = workspace.or_else(|| match &ctx.global.workspace {
        Some(p) => Some(ctx.cwd.join(p)),
        None => client::target(ctx).ok().map(|t| t.workspace),
    });
    let opts = RunOptions {
        workspace,
        foreground,
        log_level,
        debug_rpc: ctx.env.get(ENV_DEBUG_RPC).is_some_and(|v| v == "1"),
        ..RunOptions::new(ctx.home())
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return Error::internal(format!("cannot build the tokio runtime: {e}")).into(),
    };
    match rt.block_on(Daemon::run(opts)) {
        // Detached, stdout is the daemon log: print nothing unless --json.
        Ok(()) => CommandOutput::data(json!({ "stopped": true })).with_raw(""),
        Err(e) => e.into(),
    }
}

fn paths_json(t: &Target) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("workspace".into(), json!(t.workspace));
    m.insert("socket".into(), json!(t.paths.socket));
    m.insert("lock".into(), json!(t.paths.lock));
    m.insert("log".into(), json!(t.paths.log_path()));
    m
}

/// `data.state` of `daemon status`: `{run_id, stems, alive, path}` from
/// `state.json` (`null` when there is none) — deliverable 11.
fn state_json(t: &Target) -> Value {
    match stems_daemon::state::StateSummary::of(&t.paths.state) {
        Some(s) => json!({
            "run_id": s.run_id,
            "stems": s.stems,
            "alive": s.alive,
            "path": t.paths.state,
        }),
        None => Value::Null,
    }
}

fn info_json(info: &DaemonInfo, t: &Target) -> serde_json::Map<String, Value> {
    let mut m = paths_json(t);
    m.insert("pid".into(), json!(info.pid));
    m.insert("version".into(), json!(info.version));
    m.insert("api_version".into(), json!(info.api_version));
    m.insert("uptime_s".into(), json!(info.uptime_s));
    m
}

fn start(ctx: &Ctx, foreground: bool, log_level: Option<String>) -> Result<CommandOutput, Errors> {
    let t = client::target(ctx)?;
    let want = client::options(ctx);
    match block_on(connect_to(&t, lenient(ctx))) {
        Ok(c) => {
            let info = c.info();
            let mut data = info_json(info, &t);
            data.insert("already_running".into(), json!(true));
            let out = CommandOutput::data(Value::Object(data));
            if info.version != want.client_version {
                return Ok(out.with_errors(version_mismatch(
                    &want.client_version,
                    stems_api::API_VERSION,
                    &info.version,
                    info.api_version,
                )));
            }
            return Ok(out);
        }
        Err(e) if e.code == ErrorCode::DaemonVersionMismatch => return Err(e.into()),
        Err(_) => {}
    }
    if foreground {
        return Ok(run_daemon(ctx, Some(t.workspace), true, log_level));
    }
    let exe = std::env::current_exe()
        .map_err(|e| Error::internal(format!("cannot locate the stems binary: {e}")))?;
    let mut args = daemon_args(&t.home, Some(&t.workspace), false);
    if let Some(level) = log_level {
        args.push("--log-level".into());
        args.push(level.into());
    }
    spawn_detached(&exe, &args, &t.paths)?;
    let c = block_on(async {
        wait_for_socket(&t.paths, WAIT).await?;
        connect_to(&t, want).await
    })?;
    let mut data = info_json(c.info(), &t);
    data.insert("already_running".into(), json!(false));
    Ok(CommandOutput::data(Value::Object(data)))
}

fn lock_state_json(state: &LockState) -> Value {
    match state {
        LockState::Free => json!("free"),
        LockState::Held { .. } => json!("held"),
        LockState::Stale { .. } => json!("stale"),
    }
}

fn status(ctx: &Ctx) -> CommandOutput {
    let t = match client::target(ctx) {
        Ok(t) => t,
        Err(e) => return e.into(),
    };
    let want = client::options(ctx).client_version;
    let r: Result<Value, Error> = block_on(async {
        let c = connect_to(&t, lenient(ctx)).await?;
        c.call(Method::DAEMON_STATUS, json!({})).await
    });
    match r {
        Ok(st) => {
            let mut data = paths_json(&t);
            data.insert("running".into(), json!(true));
            data.insert("lock_state".into(), json!("held"));
            data.insert("state".into(), state_json(&t));
            if let Value::Object(m) = st {
                for (k, v) in m {
                    data.entry(k).or_insert(v);
                }
            }
            let version = data.get("version").and_then(Value::as_str).unwrap_or("");
            let compatible = version == want;
            let mismatch = (!compatible).then(|| {
                version_mismatch(
                    &want,
                    stems_api::API_VERSION,
                    version,
                    data.get("api_version").and_then(Value::as_u64).unwrap_or(0) as u32,
                )
            });
            data.insert("client_version".into(), json!(want));
            data.insert("compatible".into(), json!(compatible));
            let out = CommandOutput::data(Value::Object(data));
            match mismatch {
                Some(e) => out.with_errors(e),
                None => out,
            }
        }
        Err(e) => {
            let mut data = paths_json(&t);
            data.insert("running".into(), json!(false));
            data.insert("lock_state".into(), lock_state_json(&lock::probe(&t.paths)));
            data.insert("state".into(), state_json(&t));
            CommandOutput::data(Value::Object(data)).with_errors(e)
        }
    }
}

/// Poll until `pred` holds or `WAIT` expires.
fn wait_until(pred: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn gone(t: &Target) -> bool {
    !t.paths.socket.exists() && !t.paths.lock.exists()
}

fn stop(ctx: &Ctx) -> Result<CommandOutput, Errors> {
    let t = client::target(ctx)?;
    let r: Result<u32, Error> = block_on(async {
        let c = connect_to(&t, lenient(ctx)).await?;
        let pid = c.info().pid;
        c.shutdown().await?;
        Ok(pid)
    });
    let (pid, via) = match r {
        Ok(pid) => (pid as i32, "rpc"),
        Err(e) => match lock::probe(&t.paths) {
            // A live holder that we cannot talk to (hung, or an incompatible
            // API version): the same orderly path via SIGTERM.
            LockState::Held { pid, .. } => {
                terminate(pid)?;
                (pid, "signal")
            }
            LockState::Stale { pid } => {
                remove_stale(&t.paths.lock, &t.paths.socket, pid);
                return Ok(CommandOutput::data(json!({
                    "stopped": false,
                    "stale_lock_removed": true,
                    "pid": pid,
                    "lock": t.paths.lock,
                })));
            }
            LockState::Free => return Err(e.into()),
        },
    };
    if !wait_until(|| gone(&t)) {
        return Err(Error::new(
            ErrorCode::Internal,
            format!(
                "the daemon (pid {pid}) did not stop within {}s",
                WAIT.as_secs()
            ),
        )
        .with_hint(format!(
            "see its log {}; `kill -TERM {pid}` retries the orderly shutdown",
            t.paths.log_path().display()
        ))
        .with_details(json!({ "pid": pid, "socket": t.paths.socket, "lock": t.paths.lock }))
        .into());
    }
    Ok(CommandOutput::data(
        json!({ "stopped": true, "pid": pid, "via": via }),
    ))
}

fn terminate(pid: i32) -> Result<(), Error> {
    use nix::sys::signal::{Signal, kill};
    match kill(nix::unistd::Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(Error::internal(format!(
            "cannot signal daemon pid {pid}: {e}"
        ))),
    }
}

/// Remove a stale lock (only if it still names the dead `pid`) and its socket.
fn remove_stale(lock_path: &Path, socket: &Path, pid: i32) {
    let still_ours = match lock::read(lock_path) {
        Some(Ok(l)) => l.pid == pid,
        Some(Err(_)) => pid == 0,
        None => false,
    };
    if still_ours {
        let _ = std::fs::remove_file(lock_path);
        let _ = std::fs::remove_file(socket);
    }
}
