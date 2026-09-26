//! Where stems keeps per-user state. A thin re-export of
//! [`stems_daemon::paths`] so the CLI and the daemon share one
//! implementation of the layout:
//!
//! - home: `--home`, else `$STEMS_HOME`, else [`default_home`]
//!   (`~/Library/Application Support/stems` on macOS; `$XDG_STATE_HOME/stems`
//!   or `~/.local/state/stems` elsewhere);
//! - per-workspace directory: `<home>/<`[`workspace_hash`]`>` holding
//!   `stemsd.sock`, `stemsd.lock`, `stemsd.log` and `state.json`
//!   ([`DaemonPaths`]). The hash is 12 hex characters so the socket path fits
//!   the 104-byte Unix socket limit on macOS.

pub use stems_daemon::paths::{
    ENV_DAEMON_LOG, ENV_HOME, LOCK_FILE, LOG_FILE, SOCKET_FILE, STATE_FILE, workspace_root,
};
pub use stems_daemon::{DaemonPaths, default_home, resolve_home, workspace_hash};
