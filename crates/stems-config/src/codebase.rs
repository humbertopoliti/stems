//! Path helpers and codebase resolution (FR-WS-3).

use std::path::{Component, Path, PathBuf};

use crate::model::Codebase;
use crate::raw::RawCodebase;

/// Expand a leading `~` / `~/` using `home`. Other forms are returned as-is.
pub fn expand_tilde(s: &str, home: Option<&str>) -> PathBuf {
    match (s, home) {
        ("~", Some(h)) => PathBuf::from(h),
        (s, Some(h)) if s.starts_with("~/") => Path::new(h).join(&s[2..]),
        _ => PathBuf::from(s),
    }
}

/// Lexically normalise a path: drop `.`, fold `..` (never touches the disk,
/// so it works for paths that do not exist yet).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let popped =
                    matches!(out.components().next_back(), Some(Component::Normal(_))) && out.pop();
                if !popped && !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `s` (after `~` expansion) against `base` unless absolute, then normalise.
pub fn resolve_path(base: &Path, s: &str, home: Option<&str>) -> PathBuf {
    let p = expand_tilde(s, home);
    normalize(&if p.is_absolute() { p } else { base.join(p) })
}

/// True if a codebase string is a git URL rather than a local path:
/// `ssh://`, `git://`, `http(s)://`, `file://` URLs and scp-style `user@host:path`.
pub fn is_git_url(s: &str) -> bool {
    const SCHEMES: [&str; 6] = [
        "ssh://",
        "git://",
        "http://",
        "https://",
        "file://",
        "git+ssh://",
    ];
    if SCHEMES.iter().any(|p| s.starts_with(p)) {
        return true;
    }
    // scp-like: user@host:path (no slash before the colon)
    match (s.find('@'), s.find(':')) {
        (Some(at), Some(colon)) => at < colon && !s[..colon].contains('/'),
        _ => false,
    }
}

/// Resolve a stem's codebase. Local paths are relative to the integration repo
/// root; git codebases get `<repos_dir>/<stem>` unless `path` overrides it.
pub fn resolve_codebase(
    raw: &RawCodebase,
    stem: &str,
    root: &Path,
    repos_dir: &Path,
    home: Option<&str>,
) -> Codebase {
    match raw {
        RawCodebase::Path(s) if is_git_url(s) => Codebase::Git {
            url: s.clone(),
            git_ref: None,
            path: repos_dir.join(stem),
        },
        RawCodebase::Path(s) => Codebase::Local {
            path: resolve_path(root, s, home),
        },
        RawCodebase::Git(g) => Codebase::Git {
            url: g.git.clone(),
            git_ref: g.git_ref.clone(),
            path: g
                .path
                .as_deref()
                .map_or_else(|| repos_dir.join(stem), |p| resolve_path(root, p, home)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::RawGitCodebase;

    #[test]
    fn tilde_expansion() {
        assert_eq!(
            expand_tilde("~/work/api", Some("/home/u")),
            PathBuf::from("/home/u/work/api")
        );
        assert_eq!(expand_tilde("~", Some("/home/u")), PathBuf::from("/home/u"));
        assert_eq!(
            expand_tilde("~other/x", Some("/home/u")),
            PathBuf::from("~other/x")
        );
        assert_eq!(expand_tilde("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn normalisation() {
        assert_eq!(
            normalize(Path::new("/a/b/../../c/./d")),
            PathBuf::from("/c/d")
        );
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn git_url_detection() {
        for g in [
            "git@github.com:acme/api.git",
            "https://github.com/acme/api.git",
            "ssh://git@host/x.git",
            "file:///tmp/repo",
        ] {
            assert!(is_git_url(g), "{g}");
        }
        for p in ["../web", "/abs/path", "~/work/api", "dir/with@at:colon"] {
            assert!(!is_git_url(p), "{p}");
        }
    }

    #[test]
    fn resolves_local_and_git() {
        let root = Path::new("/ws/integration");
        let repos = Path::new("/ws/integration/.stems/repos");
        let home = Some("/home/u");
        let c = resolve_codebase(
            &RawCodebase::Path("../web".into()),
            "web",
            root,
            repos,
            home,
        );
        assert_eq!(
            c,
            Codebase::Local {
                path: "/ws/web".into()
            }
        );
        let c = resolve_codebase(
            &RawCodebase::Path("~/work/api".into()),
            "api",
            root,
            repos,
            home,
        );
        assert_eq!(
            c,
            Codebase::Local {
                path: "/home/u/work/api".into()
            }
        );
        let c = resolve_codebase(
            &RawCodebase::Path("git@github.com:acme/api.git".into()),
            "api",
            root,
            repos,
            home,
        );
        assert_eq!(
            c,
            Codebase::Git {
                url: "git@github.com:acme/api.git".into(),
                git_ref: None,
                path: repos.join("api"),
            }
        );
        let c = resolve_codebase(
            &RawCodebase::Git(RawGitCodebase {
                git: "https://example.com/a.git".into(),
                git_ref: Some("main".into()),
                path: Some("checkouts/a".into()),
            }),
            "a",
            root,
            repos,
            home,
        );
        assert_eq!(
            c,
            Codebase::Git {
                url: "https://example.com/a.git".into(),
                git_ref: Some("main".into()),
                path: "/ws/integration/checkouts/a".into(),
            }
        );
    }
}
