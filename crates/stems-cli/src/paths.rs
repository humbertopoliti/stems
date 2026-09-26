//! Where stems keeps per-user state: `STEMS_HOME` and the per-workspace
//! subdirectory (socket, lock, state, logs) keyed by a hash of the workspace
//! path.
//!
//! - [`home_dir`]: `--home`, else `$STEMS_HOME`, else the platform default
//!   (`~/Library/Application Support/stems` on macOS; `$XDG_STATE_HOME/stems`
//!   or `~/.local/state/stems` elsewhere).
//! - [`workspace_dir`]: `<home>/<first 12 hex chars of sha256(canonical
//!   workspace root)>`. Kept short so `<dir>/stemsd.sock` fits the 104-byte
//!   Unix socket path limit on macOS.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Environment variable overriding the stems home.
pub const ENV_HOME: &str = "STEMS_HOME";

/// Length of the workspace hash (hex characters).
pub const WORKSPACE_HASH_LEN: usize = 12;

/// The stems home: `explicit` (`--home`, which clap already fills from
/// `STEMS_HOME`), else `$STEMS_HOME`, else the platform default.
pub fn home_dir(explicit: Option<&Path>) -> PathBuf {
    home_dir_with(
        explicit,
        &|k| std::env::var(k).ok(),
        cfg!(target_os = "macos"),
    )
}

/// [`home_dir`] with an injectable environment and platform (for tests).
pub fn home_dir_with(
    explicit: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
    macos: bool,
) -> PathBuf {
    let nonempty = |k: &str| env(k).filter(|v| !v.is_empty());
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(p) = nonempty(ENV_HOME) {
        return PathBuf::from(p);
    }
    let user_home = PathBuf::from(nonempty("HOME").unwrap_or_else(|| "/".to_string()));
    if macos {
        return user_home.join("Library/Application Support/stems");
    }
    match nonempty("XDG_STATE_HOME").map(PathBuf::from) {
        Some(p) if p.is_absolute() => p.join("stems"),
        _ => user_home.join(".local/state/stems"),
    }
}

/// Hex hash of the canonical workspace path (first [`WORKSPACE_HASH_LEN`]
/// characters of its sha256). Falls back to the path as given when it cannot
/// be canonicalised.
pub fn workspace_hash(workspace: &Path) -> String {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    hex.truncate(WORKSPACE_HASH_LEN);
    hex
}

/// Per-workspace state directory: `<home>/<workspace_hash(workspace)>`.
/// `workspace` is the integration repo root (the directory holding
/// `stems.yaml`).
pub fn workspace_dir(home: &Path, workspace: &Path) -> PathBuf {
    home.join(workspace_hash(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn explicit_wins_then_env() {
        let e = env(&[("STEMS_HOME", "/from/env"), ("HOME", "/Users/u")]);
        assert_eq!(
            home_dir_with(Some(Path::new("/flag")), &e, true),
            PathBuf::from("/flag")
        );
        assert_eq!(home_dir_with(None, &e, true), PathBuf::from("/from/env"));
    }

    #[test]
    fn platform_defaults() {
        let e = env(&[("HOME", "/Users/u")]);
        assert_eq!(
            home_dir_with(None, &e, true),
            PathBuf::from("/Users/u/Library/Application Support/stems")
        );
        assert_eq!(
            home_dir_with(None, &e, false),
            PathBuf::from("/Users/u/.local/state/stems")
        );
        let e = env(&[("HOME", "/home/u"), ("XDG_STATE_HOME", "/xdg/state")]);
        assert_eq!(
            home_dir_with(None, &e, false),
            PathBuf::from("/xdg/state/stems")
        );
        let e = env(&[("HOME", "/home/u"), ("XDG_STATE_HOME", "relative")]);
        assert_eq!(
            home_dir_with(None, &e, false),
            PathBuf::from("/home/u/.local/state/stems")
        );
    }

    #[test]
    fn workspace_dir_is_a_stable_short_hash() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        let a = workspace_dir(Path::new("/h"), &ws);
        let b = workspace_dir(Path::new("/h"), &ws.join("sub/.."));
        assert_eq!(a, b, "the canonical path is hashed");
        let name = a.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), WORKSPACE_HASH_LEN);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, workspace_dir(Path::new("/h"), &ws.join("sub")));
    }

    #[test]
    fn known_hash() {
        // `printf %s /nonexistent/stems-ws | shasum -a 256` (a missing path
        // is hashed as given).
        assert_eq!(
            workspace_hash(Path::new("/nonexistent/stems-ws")),
            "ee54b6528d78"
        );
    }
}
