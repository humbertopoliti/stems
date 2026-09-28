//! Registry credentials for image pulls, resolved the way the `docker` CLI
//! does, so whatever `docker login` or a registry's own setup command
//! (`gcloud auth configure-docker`, `aws ecr get-login-password | docker
//! login`, ...) configured works unchanged. See `docs/docker.md`
//! ("Registry credentials").
//!
//! For the image's registry host, in order:
//!
//! 1. `credHelpers[host]` in the docker config: run
//!    `docker-credential-<helper> get`. A failure is an error (the helper
//!    was configured for exactly this registry);
//! 2. `credsStore`: the same protocol with the default store. A failure is
//!    only logged (a stale `credsStore: desktop` must not break pulls of
//!    public images);
//! 3. `auths[host]`: inline `auth` (base64 `user:password`), `username` /
//!    `password`, or `identitytoken`;
//! 4. otherwise anonymous.
//!
//! Nothing is cached: helpers run on every pull, so short-lived tokens (GAR,
//! ECR) are always fresh.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::Engine as _;
use bollard::auth::DockerCredentials;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::cli::{WELL_KNOWN_DOCKER_PATHS, docker_cli_path, is_executable};

/// The key Docker Hub credentials are stored under.
pub const DOCKER_HUB_KEY: &str = "https://index.docker.io/v1/";

/// How long one credential helper may take (gcloud can be slow to refresh).
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// The registry host of an image reference: the first path component when
/// it looks like a host (contains `.` or `:`, or is `localhost`), else
/// Docker Hub. Docker Hub's aliases all normalise to `docker.io`.
pub fn registry_host(image: &str) -> String {
    let host = match image.split_once('/') {
        Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => first,
        _ => "docker.io",
    };
    normalize_host(host)
}

/// A config key (`https://ghcr.io`, `https://index.docker.io/v1/`,
/// `ghcr.io`) reduced to its host, lowercase, Docker Hub as `docker.io`.
fn normalize_host(key: &str) -> String {
    let key = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let host = key.split('/').next().unwrap_or(key).to_ascii_lowercase();
    match host.as_str() {
        "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => {
            "docker.io".into()
        }
        _ => host,
    }
}

/// What is passed to a credential helper and sent as `serveraddress`.
pub fn server_address(host: &str) -> String {
    if host == "docker.io" {
        DOCKER_HUB_KEY.into()
    } else {
        host.into()
    }
}

/// `$DOCKER_CONFIG/config.json`, else `~/.docker/config.json`.
pub fn config_path(docker_config: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match docker_config.filter(|d| !d.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir).join("config.json")),
        None => home.map(|h| h.join(".docker/config.json")),
    }
}

/// The parts of the docker CLI config that hold credentials.
#[derive(Debug, Default, Deserialize)]
pub struct DockerConfigFile {
    #[serde(default)]
    pub auths: HashMap<String, AuthEntry>,
    #[serde(rename = "credsStore")]
    pub creds_store: Option<String>,
    #[serde(rename = "credHelpers", default)]
    pub cred_helpers: HashMap<String, String>,
}

/// One `auths` entry.
#[derive(Debug, Default, Deserialize)]
pub struct AuthEntry {
    pub auth: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub identitytoken: Option<String>,
}

impl DockerConfigFile {
    /// Parse the config; an unreadable file is an error, a missing one is
    /// empty (anonymous pulls).
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("cannot parse {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    /// The helper configured for `host` in `credHelpers`.
    fn helper_for(&self, host: &str) -> Option<&str> {
        self.cred_helpers
            .iter()
            .find(|(k, _)| normalize_host(k) == host)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    /// Inline credentials for `host` from `auths`.
    fn inline_for(&self, host: &str) -> Result<Option<DockerCredentials>, String> {
        let Some(e) = self
            .auths
            .iter()
            .find(|(k, _)| normalize_host(k) == host)
            .map(|(_, e)| e)
        else {
            return Ok(None);
        };
        let server = Some(server_address(host));
        if let Some(token) = e.identitytoken.as_ref().filter(|t| !t.is_empty()) {
            return Ok(Some(DockerCredentials {
                identitytoken: Some(token.clone()),
                serveraddress: server,
                ..Default::default()
            }));
        }
        let (username, password) = match e.auth.as_ref().filter(|a| !a.is_empty()) {
            Some(auth) => {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(auth.trim())
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .ok_or_else(|| format!("`auths.{host}.auth` is not base64 `user:password`"))?;
                let (u, p) = decoded
                    .split_once(':')
                    .ok_or_else(|| format!("`auths.{host}.auth` is not `user:password`"))?;
                (u.to_string(), p.to_string())
            }
            None => match (&e.username, &e.password) {
                (Some(u), Some(p)) => (u.clone(), p.clone()),
                // A bare entry: `credsStore` holds the secret (already tried).
                _ => return Ok(None),
            },
        };
        Ok(Some(DockerCredentials {
            username: Some(username),
            password: Some(password),
            serveraddress: server,
            ..Default::default()
        }))
    }
}

/// Where a pull from one registry gets its credentials (the first source
/// [`resolve_with`] tries), for `stems doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource {
    /// `credHelpers[host]`: this helper, and nothing else.
    Helper(String),
    /// `credsStore`: this helper, then `auths`, then anonymous.
    Store(String),
    /// Inline credentials in `auths`.
    Inline,
    /// An `auths` entry that cannot be used (the pull fails with this).
    InvalidInline(String),
    /// None: anonymous (public images only).
    Anonymous,
}

impl DockerConfigFile {
    /// The source a pull from `host` uses first (see the module docs).
    pub fn source_for(&self, host: &str) -> CredentialSource {
        if let Some(h) = self.helper_for(host) {
            return CredentialSource::Helper(h.to_string());
        }
        if let Some(s) = self.creds_store.as_deref().filter(|s| !s.is_empty()) {
            return CredentialSource::Store(s.to_string());
        }
        match self.inline_for(host) {
            Ok(Some(_)) => CredentialSource::Inline,
            Err(e) => CredentialSource::InvalidInline(e),
            Ok(None) => CredentialSource::Anonymous,
        }
    }
}

/// A credential helper's answer to `get`.
#[derive(Debug, Deserialize)]
struct HelperReply {
    #[serde(rename = "Username", default)]
    username: String,
    #[serde(rename = "Secret", default)]
    secret: String,
}

/// Turn a helper's `get` reply into credentials (`<token>` as the username
/// means the secret is an identity token).
fn from_helper_reply(stdout: &[u8], server: &str) -> Result<DockerCredentials, String> {
    let r: HelperReply = serde_json::from_slice(stdout)
        .map_err(|e| format!("unexpected credential helper output: {e}"))?;
    Ok(if r.username == "<token>" {
        DockerCredentials {
            identitytoken: Some(r.secret),
            serveraddress: Some(server.into()),
            ..Default::default()
        }
    } else {
        DockerCredentials {
            username: Some(r.username),
            password: Some(r.secret),
            serveraddress: Some(server.into()),
            ..Default::default()
        }
    })
}

/// Resolve credentials for `host` from `config` (see the module docs).
/// `run_helper(helper, server)` runs `docker-credential-<helper> get`:
/// `Ok(None)` when it has no credentials for `server`.
pub async fn resolve_with(
    config: &DockerConfigFile,
    host: &str,
    run_helper: impl AsyncFn(&str, &str) -> Result<Option<DockerCredentials>, String>,
) -> Result<Option<DockerCredentials>, String> {
    let server = server_address(host);
    if let Some(helper) = config.helper_for(host) {
        return run_helper(helper, &server).await.map_err(|e| {
            format!("credential helper `docker-credential-{helper}` (credHelpers) failed: {e}")
        });
    }
    if let Some(store) = config.creds_store.as_deref().filter(|s| !s.is_empty()) {
        match run_helper(store, &server).await {
            Ok(Some(c)) => return Ok(Some(c)),
            Ok(None) => {}
            Err(e) => tracing::warn!(
                helper = %store,
                registry = %host,
                error = %e,
                "credsStore helper failed; trying `auths`, then an anonymous pull"
            ),
        }
    }
    config.inline_for(host)
}

/// Directories searched for `docker-credential-*` after `PATH`: the docker
/// CLI's own directory and those of the well-known install locations.
pub fn helper_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let known = WELL_KNOWN_DOCKER_PATHS
        .iter()
        .filter_map(|p| match p.strip_prefix("~/") {
            Some(rest) => home.map(|h| h.join(rest)),
            None => Some(PathBuf::from(p)),
        });
    for cli in docker_cli_path().into_iter().chain(known) {
        if let Some(d) = cli.parent().map(Path::to_path_buf)
            && !dirs.contains(&d)
        {
            dirs.push(d);
        }
    }
    dirs
}

/// `docker-credential-<name>` on `path`, else in `extra`.
pub fn find_helper(
    name: &str,
    path: Option<&OsStr>,
    extra: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let bin = format!("docker-credential-{name}");
    path.into_iter()
        .flat_map(std::env::split_paths)
        .filter(|d| !d.as_os_str().is_empty())
        .chain(extra.iter().cloned())
        .map(|d| d.join(&bin))
        .find(|c| exists(c))
}

/// Run `docker-credential-<name> get` for `server`.
pub async fn run_helper(name: &str, server: &str) -> Result<Option<DockerCredentials>, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = std::env::var_os("PATH");
    let program = find_helper(
        name,
        path.as_deref(),
        &helper_dirs(home.as_deref()),
        &is_executable,
    )
    .ok_or_else(|| {
        format!(
            "`docker-credential-{name}` was not found on the daemon's PATH; install it, or restart the daemon (`stems daemon stop`, then `stems daemon start`) from a shell where it is on PATH"
        )
    })?;
    run_helper_at(&program, server, HELPER_TIMEOUT).await
}

/// Run the helper binary `program` with `get`, `server` on stdin.
pub async fn run_helper_at(
    program: &Path,
    server: &str,
    timeout: Duration,
) -> Result<Option<DockerCredentials>, String> {
    let mut child = tokio::process::Command::new(program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A helper that exits without reading stdin is fine.
        let _ = stdin.write_all(server.as_bytes()).await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("{} timed out after {timeout:?}", program.display()))?
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if out.status.success() {
        return from_helper_reply(&out.stdout, server).map(Some);
    }
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    if text.to_ascii_lowercase().contains("credentials not found") {
        return Ok(None);
    }
    Err(text.trim().to_string())
}

/// Credentials for pulling `image` from this process's docker config
/// (`$DOCKER_CONFIG`, `~/.docker`); `Ok(None)` pulls anonymously.
pub async fn credentials_for(image: &str) -> Result<Option<DockerCredentials>, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let Some(path) = config_path(
        std::env::var_os("DOCKER_CONFIG").as_deref(),
        home.as_deref(),
    ) else {
        return Ok(None);
    };
    let config = DockerConfigFile::load(&path)?;
    resolve_with(&config, &registry_host(image), run_helper).await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn config(json: &str) -> DockerConfigFile {
        serde_json::from_str(json).unwrap()
    }

    fn creds(user: &str, secret: &str) -> DockerCredentials {
        DockerCredentials {
            username: Some(user.into()),
            password: Some(secret.into()),
            ..Default::default()
        }
    }

    #[test]
    fn registry_host_of_image_refs() {
        for (image, host) in [
            ("postgres:16", "docker.io"),
            ("library/postgres", "docker.io"),
            ("acme/api:1.2", "docker.io"),
            ("docker.io/acme/api", "docker.io"),
            ("index.docker.io/acme/api", "docker.io"),
            ("ghcr.io/acme/api:latest", "ghcr.io"),
            (
                "europe-west2-docker.pkg.dev/proj/repo/api:main",
                "europe-west2-docker.pkg.dev",
            ),
            (
                "123456789012.dkr.ecr.eu-west-2.amazonaws.com/api@sha256:abc",
                "123456789012.dkr.ecr.eu-west-2.amazonaws.com",
            ),
            ("localhost:5000/api", "localhost:5000"),
            ("localhost/api", "localhost"),
            ("Registry.Example.com/api", "registry.example.com"),
        ] {
            assert_eq!(registry_host(image), host, "{image}");
        }
        assert_eq!(server_address("docker.io"), DOCKER_HUB_KEY);
        assert_eq!(server_address("ghcr.io"), "ghcr.io");
    }

    #[test]
    fn config_path_prefers_docker_config() {
        let home = Path::new("/Users/u");
        assert_eq!(
            config_path(Some(OsStr::new("/cfg")), Some(home)),
            Some(PathBuf::from("/cfg/config.json"))
        );
        assert_eq!(
            config_path(Some(OsStr::new("")), Some(home)),
            Some(PathBuf::from("/Users/u/.docker/config.json"))
        );
        assert_eq!(config_path(None, None), None);
    }

    /// Resolve `image` with a fake helper runner that answers from
    /// `answers` (helper name → reply) and records every call.
    fn resolve(
        cfg: &DockerConfigFile,
        image: &str,
        answers: &[(&str, Result<Option<DockerCredentials>, String>)],
    ) -> (Result<Option<DockerCredentials>, String>, Vec<String>) {
        let calls = RefCell::new(Vec::new());
        let run = async |helper: &str, server: &str| {
            calls.borrow_mut().push(format!("{helper} {server}"));
            answers
                .iter()
                .find(|(h, _)| *h == helper)
                .map_or(Ok(None), |(_, r)| r.clone())
        };
        let host = registry_host(image);
        let r = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(resolve_with(cfg, &host, run));
        (r, calls.into_inner())
    }

    #[test]
    fn cred_helper_for_the_host_wins() {
        let cfg = config(
            r#"{"credsStore":"desktop",
                "credHelpers":{"europe-west2-docker.pkg.dev":"gcloud"},
                "auths":{"europe-west2-docker.pkg.dev":{"auth":"dTpw"}}}"#,
        );
        let (r, calls) = resolve(
            &cfg,
            "europe-west2-docker.pkg.dev/p/r/api:latest",
            &[("gcloud", Ok(Some(creds("oauth2accesstoken", "ya29"))))],
        );
        assert_eq!(r.unwrap(), Some(creds("oauth2accesstoken", "ya29")));
        assert_eq!(calls, ["gcloud europe-west2-docker.pkg.dev"]);
    }

    #[test]
    fn a_failing_cred_helper_is_an_error() {
        let cfg = config(r#"{"credHelpers":{"ghcr.io":"gh"}}"#);
        let (r, _) = resolve(&cfg, "ghcr.io/a/b", &[("gh", Err("boom".into()))]);
        let e = r.unwrap_err();
        assert!(
            e.contains("docker-credential-gh") && e.contains("boom"),
            "{e}"
        );
    }

    #[test]
    fn creds_store_then_inline_auths_then_anonymous() {
        let cfg = config(
            r#"{"credsStore":"osxkeychain",
                "auths":{"https://index.docker.io/v1/":{},
                         "https://registry.example.com/v2/":{"auth":"dXNlcjpwYTpzcw=="}}}"#,
        );
        // Docker Hub: the store answers, with the hub key as server.
        let (r, calls) = resolve(
            &cfg,
            "acme/api",
            &[("osxkeychain", Ok(Some(creds("me", "pw"))))],
        );
        assert_eq!(r.unwrap(), Some(creds("me", "pw")));
        assert_eq!(calls, [format!("osxkeychain {DOCKER_HUB_KEY}")]);

        // The store has nothing: inline `auth` (a password may contain `:`).
        let (r, _) = resolve(&cfg, "registry.example.com/api", &[]);
        let c = r.unwrap().unwrap();
        assert_eq!(c.username.as_deref(), Some("user"));
        assert_eq!(c.password.as_deref(), Some("pa:ss"));
        assert_eq!(c.serveraddress.as_deref(), Some("registry.example.com"));

        // The store fails (e.g. a stale `desktop`): not an error.
        let (r, _) = resolve(
            &cfg,
            "registry.example.com/api",
            &[("osxkeychain", Err("not found".into()))],
        );
        assert!(r.unwrap().is_some());

        // A bare `auths` entry and nothing in the store: anonymous.
        let (r, _) = resolve(&cfg, "acme/api", &[]);
        assert_eq!(r.unwrap(), None);
        // No config at all: anonymous, no helper runs.
        let (r, calls) = resolve(&DockerConfigFile::default(), "ghcr.io/a/b", &[]);
        assert_eq!(r.unwrap(), None);
        assert!(calls.is_empty());
    }

    #[test]
    fn source_for_follows_the_resolution_order() {
        let cfg = config(
            r#"{"credsStore":"desktop","credHelpers":{"ghcr.io":"gh"},
                "auths":{"ghcr.io":{"auth":"dTpw"},"a.io":{"auth":"dTpw"}}}"#,
        );
        assert_eq!(
            cfg.source_for("ghcr.io"),
            CredentialSource::Helper("gh".into())
        );
        assert_eq!(
            cfg.source_for("a.io"),
            CredentialSource::Store("desktop".into())
        );
        let cfg = config(r#"{"auths":{"a.io":{"auth":"dTpw"},"b.io":{},"c.io":{"auth":"%"}}}"#);
        assert_eq!(cfg.source_for("a.io"), CredentialSource::Inline);
        assert!(
            matches!(cfg.source_for("c.io"), CredentialSource::InvalidInline(e) if e.contains("base64"))
        );
        assert_eq!(cfg.source_for("b.io"), CredentialSource::Anonymous);
        assert_eq!(cfg.source_for("docker.io"), CredentialSource::Anonymous);
    }

    #[test]
    fn inline_identity_token_and_username_password() {
        let cfg = config(
            r#"{"auths":{"a.io":{"identitytoken":"tok"},
                         "b.io":{"username":"u","password":"p"},
                         "c.io":{"auth":"%%%"}}}"#,
        );
        let c = resolve(&cfg, "a.io/x", &[]).0.unwrap().unwrap();
        assert_eq!(c.identitytoken.as_deref(), Some("tok"));
        assert_eq!(c.username, None);
        let c = resolve(&cfg, "b.io/x", &[]).0.unwrap().unwrap();
        assert_eq!(
            (c.username.as_deref(), c.password.as_deref()),
            (Some("u"), Some("p"))
        );
        let e = resolve(&cfg, "c.io/x", &[]).0.unwrap_err();
        assert!(e.contains("base64"), "{e}");
    }

    #[test]
    fn helper_reply_token_username_means_identity_token() {
        let c = from_helper_reply(br#"{"Username":"<token>","Secret":"s"}"#, "x.io").unwrap();
        assert_eq!(c.identitytoken.as_deref(), Some("s"));
        assert_eq!(c.username, None);
        let c = from_helper_reply(
            br#"{"ServerURL":"x.io","Username":"u","Secret":"s"}"#,
            "x.io",
        )
        .unwrap();
        assert_eq!(
            (c.username.as_deref(), c.password.as_deref()),
            (Some("u"), Some("s"))
        );
        assert!(from_helper_reply(b"nope", "x.io").is_err());
    }

    #[test]
    fn find_helper_searches_path_then_extra_dirs() {
        let exists = |p: &Path| p == Path::new("/extra/docker-credential-gcloud");
        let path = OsStr::new("/usr/bin::/bin");
        assert_eq!(
            find_helper("gcloud", Some(path), &[PathBuf::from("/extra")], &exists),
            Some(PathBuf::from("/extra/docker-credential-gcloud"))
        );
        assert_eq!(find_helper("ecr-login", Some(path), &[], &exists), None);
    }

    /// A fake `docker-credential-*` script: echoes its stdin back as the
    /// username, fails with "credentials not found" for `none.io`, and with
    /// another message for `bad.io`.
    fn fake_helper(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("docker-credential-fake");
        std::fs::write(
            &p,
            r#"#!/bin/sh
[ "$1" = get ] || exit 3
read -r server
case "$server" in
  none.io) echo "credentials not found in native keychain"; exit 1 ;;
  bad.io) echo "token expired, run gcloud auth login" >&2; exit 1 ;;
esac
printf '{"ServerURL":"%s","Username":"%s","Secret":"s3cret"}' "$server" "$server"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[tokio::test]
    async fn run_helper_at_speaks_the_helper_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let helper = fake_helper(dir.path());
        let t = Duration::from_secs(5);
        let c = run_helper_at(&helper, "ghcr.io", t).await.unwrap().unwrap();
        assert_eq!(c.username.as_deref(), Some("ghcr.io"));
        assert_eq!(c.password.as_deref(), Some("s3cret"));
        assert_eq!(run_helper_at(&helper, "none.io", t).await.unwrap(), None);
        let e = run_helper_at(&helper, "bad.io", t).await.unwrap_err();
        assert!(e.contains("token expired"), "{e}");
        let e = run_helper_at(&dir.path().join("missing"), "x", t)
            .await
            .unwrap_err();
        assert!(e.contains("cannot run"), "{e}");
    }
}
