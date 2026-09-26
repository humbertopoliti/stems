//! The stems daemon (`stemsd`, run as the hidden `stems daemon` subcommand):
//! workspace lock, JSON-RPC server on a Unix socket, event bus and lifecycle.
//!
//! * [`paths`] — where a workspace's socket/lock/log/state live (shared with the CLI).
//! * [`lock`] — the single-daemon lock with stale reclaim.
//! * [`events`] — the event ring + broadcast.
//! * [`server`] — the socket server (newline-delimited JSON-RPC 2.0).
//! * [`Daemon`] — built-in methods, the [`SupervisorHooks`] slot, [`Daemon::run`].
//! * [`supervisor`] — `up`/`down`/`start`/`stop`/`restart`/`status` (deliverable 10).
//! * [`spawn_detached`] / [`wait_for_socket`] — used by the CLI to auto-start.
//!
//! Lifecycle of [`Daemon::run`]: take the lock (reclaiming a stale one), log
//! to `stemsd.log`, optionally load the workspace, bind the socket (0600),
//! emit `daemon.started`, serve until a `shutdown` RPC or SIGTERM/SIGINT/SIGHUP,
//! then: `daemon.stopping` → supervisor shutdown hook → stop debug processes →
//! close the listener → remove socket and lock → `daemon.stopped` → exit.

mod daemon;
pub mod debug;
pub mod events;
mod handler;
pub mod lock;
pub mod logging;
pub mod paths;
pub mod server;
mod spawn;
pub mod supervisor;

pub use daemon::{Daemon, NO_WORKSPACE_DIR, RunOptions};
pub use events::{EventBus, EventDraft};
pub use handler::{Handler, RequestCtx, SupervisorHooks};
pub use lock::{Lock, LockGuard, LockState};
pub use paths::{DaemonPaths, default_home, resolve_home, workspace_hash, workspace_root};
pub use spawn::{daemon_args, spawn_detached, wait_for_socket};
pub use supervisor::Supervisor;

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-daemon");
    }
}
