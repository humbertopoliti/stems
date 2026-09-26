//! Where a workspace's daemon keeps its socket, lock, log and state.
//!
//! `<home>/<12 hex chars of sha256(canonical workspace root)>/{stemsd.sock,
//! stemsd.lock, stemsd.log, state.json}`. `<home>` is `$STEMS_HOME` or
//! [`default_home`]. The CLI imports these functions so both sides agree.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Env var overriding the stems home directory.
pub const ENV_HOME: &str = "STEMS_HOME";
/// Env var overriding the daemon log path.
pub const ENV_DAEMON_LOG: &str = "STEMS_DAEMON_LOG";

/// File names inside a workspace's daemon directory.
pub const SOCKET_FILE: &str = "stemsd.sock";
/// Lock file name.
pub const LOCK_FILE: &str = "stemsd.lock";
/// Log file name.
pub const LOG_FILE: &str = "stemsd.log";
/// State file name (deliverable 11).
pub const STATE_FILE: &str = "state.json";

/// Per-workspace daemon paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonPaths {
    /// `<home>/<ws-hash>`.
    pub dir: PathBuf,
    /// `<dir>/stemsd.sock`.
    pub socket: PathBuf,
    /// `<dir>/stemsd.lock`.
    pub lock: PathBuf,
    /// `<dir>/stemsd.log` (the daemon may log elsewhere with `STEMS_DAEMON_LOG`; see [`DaemonPaths::log_path`]).
    pub log: PathBuf,
    /// `<dir>/state.json`.
    pub state: PathBuf,
}

impl DaemonPaths {
    /// Paths for `workspace` (its root directory, or its `stems.yaml`) under `home`.
    pub fn for_workspace(home: &Path, workspace: &Path) -> Self {
        Self::in_dir(home.join(workspace_hash(workspace)))
    }

    /// Paths inside an explicit directory.
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            socket: dir.join(SOCKET_FILE),
            lock: dir.join(LOCK_FILE),
            log: dir.join(LOG_FILE),
            state: dir.join(STATE_FILE),
            dir,
        }
    }

    /// The log file actually used: `$STEMS_DAEMON_LOG` or [`DaemonPaths::log`].
    pub fn log_path(&self) -> PathBuf {
        match std::env::var_os(ENV_DAEMON_LOG) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => self.log.clone(),
        }
    }

    /// `<dir>/logs`: one sub-directory per stem holding `current.log` and
    /// its rotations (deliverable 12, `docs/logs.md`).
    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join(crate::logs::LOGS_DIR)
    }

    /// Create `dir` (mode 0700) if missing.
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
    }
}

/// Canonical workspace root: canonicalized; a path to a file (`stems.yaml`)
/// is replaced by its directory.
pub fn workspace_root(workspace: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    if canon.is_file() {
        canon.parent().map(Path::to_path_buf).unwrap_or(canon)
    } else {
        canon
    }
}

/// First 12 hex chars of the sha256 of the canonical workspace root path.
pub fn workspace_hash(workspace: &Path) -> String {
    let root = workspace_root(workspace);
    let digest = Sha256::digest(root.as_os_str().as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Platform default stems home: `~/Library/Application Support/stems` on
/// macOS; `$XDG_STATE_HOME/stems` or `~/.local/state/stems` elsewhere.
pub fn default_home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("stems")
    } else {
        match std::env::var_os("XDG_STATE_HOME") {
            Some(p) if !p.is_empty() && Path::new(&p).is_absolute() => {
                PathBuf::from(p).join("stems")
            }
            _ => home.join(".local").join("state").join("stems"),
        }
    }
}

/// `$STEMS_HOME` or [`default_home`].
pub fn resolve_home() -> PathBuf {
    match std::env::var_os(ENV_HOME) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => default_home(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_12_hex() {
        let d = tempfile::tempdir().unwrap();
        let a = workspace_hash(d.path());
        let b = workspace_hash(d.path());
        assert_eq!(a, b);
        assert_eq!(a.len(), 12);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn hash_of_known_nonexistent_path_is_fixed() {
        // Non-existent paths are hashed as given: sha256("/nonexistent/stems-ws").
        assert_eq!(workspace_hash(Path::new("/nonexistent/stems-ws")), {
            let d = Sha256::digest(b"/nonexistent/stems-ws");
            d.iter()
                .take(6)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
        insta::assert_snapshot!(workspace_hash(Path::new("/nonexistent/stems-ws")), @"ee54b6528d78");
    }

    #[test]
    fn file_and_dir_and_symlink_agree() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        std::fs::write(ws.join("stems.yaml"), "x").unwrap();
        let link = d.path().join("link");
        std::os::unix::fs::symlink(&ws, &link).unwrap();
        let h = workspace_hash(&ws);
        assert_eq!(workspace_hash(&ws.join("stems.yaml")), h);
        assert_eq!(workspace_hash(&link), h);
        assert_eq!(workspace_hash(&ws.join(".")), h);
    }

    #[test]
    fn paths_layout() {
        let p = DaemonPaths::for_workspace(Path::new("/h"), Path::new("/nonexistent/stems-ws"));
        let dir = Path::new("/h").join(workspace_hash(Path::new("/nonexistent/stems-ws")));
        assert_eq!(p.dir, dir);
        assert_eq!(p.socket, dir.join("stemsd.sock"));
        assert_eq!(p.lock, dir.join("stemsd.lock"));
        assert_eq!(p.log, dir.join("stemsd.log"));
        assert_eq!(p.state, dir.join("state.json"));
    }

    #[test]
    fn default_home_is_absolute() {
        assert!(default_home().is_absolute());
    }
}
