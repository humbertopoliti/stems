//! Workspace discovery (FR-WS-1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::codebase::normalize;
use crate::diagnostic::{ConfigErrors, Diagnostic, codes};

/// The workspace config file name.
pub const CONFIG_FILE: &str = "stems.yaml";
/// The per-developer overlay file name.
pub const LOCAL_FILE: &str = "stems.local.yaml";
/// Environment variable naming the workspace.
pub const ENV_WORKSPACE: &str = "STEMS_WORKSPACE";

/// Find the workspace's `stems.yaml`.
///
/// Order: `explicit` (a directory or a file; relative to `cwd`), else
/// `STEMS_WORKSPACE` from `env`, else walk up from `cwd`. The returned path is
/// canonical (symlinks resolved).
pub fn discover(
    explicit: Option<&Path>,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<PathBuf, ConfigErrors> {
    let named = explicit
        .map(|p| (p.to_path_buf(), "--workspace"))
        .or_else(|| {
            env.get(ENV_WORKSPACE)
                .filter(|v| !v.is_empty())
                .map(|v| (PathBuf::from(v), ENV_WORKSPACE))
        });
    if let Some((p, source)) = named {
        let p = normalize(&cwd.join(p));
        let file = if p.is_dir() { p.join(CONFIG_FILE) } else { p };
        return file.canonicalize().map_err(|_| {
            ConfigErrors::one(
                Diagnostic::new(
                    codes::WORKSPACE_NOT_FOUND,
                    format!("no {CONFIG_FILE} at {} (from {source})", file.display()),
                )
                .with_hint(format!(
                    "point {source} at a directory containing {CONFIG_FILE}"
                )),
            )
        });
    }
    let start = normalize(cwd);
    let mut dir: Option<&Path> = Some(&start);
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE);
        if candidate.is_file() {
            return candidate.canonicalize().map_err(|e| {
                ConfigErrors::one(Diagnostic::new(
                    codes::WORKSPACE_NOT_FOUND,
                    format!("cannot resolve {}: {e}", candidate.display()),
                ))
            });
        }
        dir = d.parent();
    }
    Err(ConfigErrors::one(Diagnostic::new(
        codes::WORKSPACE_NOT_FOUND,
        format!(
            "no {CONFIG_FILE} found in {} or any parent directory",
            start.display()
        ),
    )
    .with_hint(format!(
        "run inside an integration repo, pass --workspace <dir>, set {ENV_WORKSPACE}, or run `stems init`"
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn ws() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap().join("repo");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join(CONFIG_FILE), "name: x\n").unwrap();
        (t, root)
    }

    #[test]
    fn walks_up_from_a_subdirectory() {
        let (_t, root) = ws();
        let env = HashMap::new();
        let f = discover(None, &root.join("a/b"), &env).unwrap();
        assert_eq!(f, root.join(CONFIG_FILE));
    }

    #[test]
    fn explicit_dir_file_and_relative() {
        let (_t, root) = ws();
        let env = HashMap::new();
        assert_eq!(
            discover(Some(&root), Path::new("/"), &env).unwrap(),
            root.join(CONFIG_FILE)
        );
        assert_eq!(
            discover(Some(&root.join(CONFIG_FILE)), Path::new("/"), &env).unwrap(),
            root.join(CONFIG_FILE)
        );
        assert_eq!(
            discover(Some(Path::new("../..")), &root.join("a/b"), &env).unwrap(),
            root.join(CONFIG_FILE)
        );
    }

    #[test]
    fn env_var_is_used_and_explicit_wins() {
        let (_t, root) = ws();
        let other = tempfile::tempdir().unwrap();
        let mut env = HashMap::new();
        env.insert(ENV_WORKSPACE.to_string(), root.display().to_string());
        assert_eq!(
            discover(None, other.path(), &env).unwrap(),
            root.join(CONFIG_FILE)
        );
        let e = discover(Some(other.path()), other.path(), &env)
            .unwrap_err()
            .errors
            .remove(0);
        assert_eq!(e.code, codes::WORKSPACE_NOT_FOUND);
    }

    #[test]
    fn not_found_outside_any_workspace() {
        let t = tempfile::tempdir().unwrap();
        let e = discover(None, t.path(), &HashMap::new())
            .unwrap_err()
            .errors
            .remove(0);
        assert_eq!(e.code, codes::WORKSPACE_NOT_FOUND);
        assert!(e.hint.is_some());
        let mut env = HashMap::new();
        env.insert(
            ENV_WORKSPACE.to_string(),
            "/definitely/not/here".to_string(),
        );
        let e = discover(None, t.path(), &env).unwrap_err().errors.remove(0);
        assert!(e.message.contains(ENV_WORKSPACE), "{}", e.message);
    }
}
