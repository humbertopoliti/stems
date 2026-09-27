//! Locating the `docker` CLI (used by the compose runtime, `doctor`,
//! `validate`, the environment of scripts/health commands/process stems,
//! and the e2e harness).
//!
//! Docker Desktop does not always put `docker` on `PATH` (an install without
//! the `/usr/local/bin` symlinks, or a daemon started from a launchd/IDE
//! environment with a minimal `PATH`), so the lookup is:
//!
//! 1. `$STEMS_DOCKER_CLI` when set and non-empty, taken as-is (no search;
//!    tests use it to simulate a machine without Docker);
//! 2. `docker` on `PATH`;
//! 3. the well-known install locations in [`WELL_KNOWN_DOCKER_PATHS`].
//!
//! See `docs/docker.md` ("Finding the docker CLI").

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Environment variable naming the `docker` CLI explicitly.
pub const DOCKER_CLI_ENV: &str = "STEMS_DOCKER_CLI";

/// Where Docker Desktop, Homebrew, Rancher Desktop and OrbStack install the
/// CLI, in search order (`~/` is the home directory).
pub const WELL_KNOWN_DOCKER_PATHS: [&str; 5] = [
    "/Applications/Docker.app/Contents/Resources/bin/docker",
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "~/.rd/bin/docker",
    "/Applications/OrbStack.app/Contents/MacOS/xbin/docker",
];

/// The inputs of [`find_docker_cli`] (a seam for tests).
#[derive(Debug, Clone, Default)]
pub struct CliSearch<'a> {
    /// `$STEMS_DOCKER_CLI`.
    pub explicit: Option<&'a OsStr>,
    /// `$PATH`.
    pub path: Option<&'a OsStr>,
    /// `$HOME` (for `~/` entries).
    pub home: Option<&'a Path>,
}

/// Is `p` an executable regular file (or a symlink to one)?
pub fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Pure lookup: see the module docs. `exists` decides whether a candidate
/// is usable.
pub fn find_docker_cli(s: &CliSearch<'_>, exists: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    if let Some(e) = s.explicit.filter(|e| !e.is_empty()) {
        return Some(PathBuf::from(e));
    }
    if let Some(path) = s.path {
        for dir in std::env::split_paths(path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let c = dir.join("docker");
            if exists(&c) {
                return Some(c);
            }
        }
    }
    WELL_KNOWN_DOCKER_PATHS.iter().find_map(|p| {
        let c = match p.strip_prefix("~/") {
            Some(rest) => s.home?.join(rest),
            None => PathBuf::from(p),
        };
        exists(&c).then_some(c)
    })
}

/// The `docker` CLI for this process's environment, if any.
pub fn docker_cli_path() -> Option<PathBuf> {
    let explicit = std::env::var_os(DOCKER_CLI_ENV);
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_docker_cli(
        &CliSearch {
            explicit: explicit.as_deref(),
            path: path.as_deref(),
            home: home.as_deref(),
        },
        &is_executable,
    )
}

/// The program to run for `docker ...`: [`docker_cli_path`], else plain
/// `docker` (so the spawn fails with "not found" as before).
pub fn docker_program() -> OsString {
    docker_cli_path()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| "docker".into())
}

/// `path` with `cli`'s directory prepended, unless `docker` already
/// resolves through `path` (then `None`: leave it alone).
pub fn path_with_docker_dir(
    path: Option<&str>,
    cli: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    let cli = cli?;
    let dir = cli.parent().filter(|d| !d.as_os_str().is_empty())?;
    let on_path = path.is_some_and(|p| {
        std::env::split_paths(p).any(|d| !d.as_os_str().is_empty() && exists(&d.join("docker")))
    });
    if on_path {
        return None;
    }
    let dir = dir.to_str()?;
    Some(match path {
        Some(p) if !p.is_empty() => format!("{dir}:{p}"),
        _ => dir.to_string(),
    })
}

/// Make `docker` resolvable through `env["PATH"]` (for scripts, health
/// commands and process stems): prepends [`docker_cli_path`]'s directory
/// when `docker` is not already on that `PATH`. No-op without a CLI.
pub fn ensure_docker_on_path(env: &mut std::collections::BTreeMap<String, String>) {
    let cli = docker_cli_path();
    if let Some(p) = path_with_docker_dir(
        env.get("PATH").map(String::as_str),
        cli.as_deref(),
        &is_executable,
    ) {
        env.insert("PATH".into(), p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search<'a>(explicit: Option<&'a str>, path: Option<&'a str>) -> CliSearch<'a> {
        CliSearch {
            explicit: explicit.map(OsStr::new),
            path: path.map(OsStr::new),
            home: Some(Path::new("/Users/u")),
        }
    }

    #[test]
    fn explicit_wins_without_search() {
        let none = |_: &Path| false;
        assert_eq!(
            find_docker_cli(&search(Some("/nope/docker"), Some("/usr/bin")), &none),
            Some(PathBuf::from("/nope/docker"))
        );
        // Empty means unset.
        assert_eq!(find_docker_cli(&search(Some(""), None), &none), None);
    }

    #[test]
    fn path_before_well_known() {
        let yes = |p: &Path| {
            p == Path::new("/opt/bin/docker")
                || p == Path::new("/Applications/Docker.app/Contents/Resources/bin/docker")
        };
        assert_eq!(
            find_docker_cli(&search(None, Some("/usr/bin::/opt/bin")), &yes),
            Some(PathBuf::from("/opt/bin/docker"))
        );
        assert_eq!(
            find_docker_cli(&search(None, Some("/usr/bin")), &yes),
            Some(PathBuf::from(
                "/Applications/Docker.app/Contents/Resources/bin/docker"
            ))
        );
    }

    #[test]
    fn well_known_order_and_home() {
        let rd = |p: &Path| p == Path::new("/Users/u/.rd/bin/docker");
        assert_eq!(
            find_docker_cli(&search(None, None), &rd),
            Some(PathBuf::from("/Users/u/.rd/bin/docker"))
        );
        let mut s = search(None, None);
        s.home = None;
        assert_eq!(find_docker_cli(&s, &rd), None);
        let orb =
            |p: &Path| p.starts_with("/Applications/OrbStack.app") || p.starts_with("/usr/local");
        assert_eq!(
            find_docker_cli(&search(None, None), &orb),
            Some(PathBuf::from("/usr/local/bin/docker"))
        );
        assert_eq!(find_docker_cli(&search(None, Some("/x")), &|_| false), None);
    }

    #[test]
    fn path_prepend() {
        let cli = Path::new("/Applications/Docker.app/Contents/Resources/bin/docker");
        let only_desktop = |p: &Path| p == cli;
        assert_eq!(
            path_with_docker_dir(Some("/usr/bin:/bin"), Some(cli), &only_desktop).as_deref(),
            Some("/Applications/Docker.app/Contents/Resources/bin:/usr/bin:/bin")
        );
        assert_eq!(
            path_with_docker_dir(None, Some(cli), &only_desktop).as_deref(),
            Some("/Applications/Docker.app/Contents/Resources/bin")
        );
        // Already resolvable: untouched.
        let on_path = |p: &Path| p == Path::new("/usr/local/bin/docker");
        assert_eq!(
            path_with_docker_dir(Some("/usr/local/bin:/usr/bin"), Some(cli), &on_path),
            None
        );
        // No CLI, or a bare name: nothing to prepend.
        assert_eq!(
            path_with_docker_dir(Some("/usr/bin"), None, &only_desktop),
            None
        );
        assert_eq!(
            path_with_docker_dir(Some("/usr/bin"), Some(Path::new("docker")), &only_desktop),
            None
        );
    }
}
