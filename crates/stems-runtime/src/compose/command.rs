//! Pure `docker compose` command-line construction and env-file rendering
//! (golden-tested, no Docker needed).

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::docker::grace_secs;

/// What a compose invocation addresses: `-f <file> [-f <override>] -p
/// <project> [--env-file <file>]` plus the service the operation acts on.
///
/// `file: None` is a project-only invocation (`docker compose -p <project>
/// ...`), which compose v2 resolves from container labels: used for the
/// project-in-use check, the orphan scan and adopted containers whose
/// compose file is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeTarget {
    pub file: Option<PathBuf>,
    pub project: String,
    /// Empty for whole-project operations (`ps`).
    pub service: String,
    pub env_file: Option<PathBuf>,
    /// `<env_dir>/<stem>.override.yaml` (`inject_env: true`): a second
    /// `-f` that adds the env file to the service's `env_file:`. Only
    /// passed together with `file`.
    pub override_file: Option<PathBuf>,
}

impl ComposeTarget {
    /// The target of a stem: its file, project, service and
    /// `<env_dir>/<stem>.env`.
    pub fn for_spec(spec: &super::ComposeSpec, env_dir: &Path) -> Self {
        Self {
            file: Some(spec.file.clone()),
            project: spec.project_name.clone(),
            service: spec.service.clone(),
            env_file: Some(env_file_path(env_dir, &spec.stem)),
            override_file: spec
                .needs_override()
                .then(|| override_file_path(env_dir, &spec.stem)),
        }
    }

    /// A whole project, addressed by name only.
    pub fn project(project: impl Into<String>) -> Self {
        Self {
            file: None,
            project: project.into(),
            service: String::new(),
            env_file: None,
            override_file: None,
        }
    }
}

/// The argument vectors passed to the `docker` binary (they start with
/// `compose`; the binary itself is [`super::ComposeOptions::binary`]).
pub struct ComposeCommand;

impl ComposeCommand {
    /// `compose --ansi never [-f <file> [-f <override>]] -p <project>
    /// [--env-file <f>]`.
    fn base(t: &ComposeTarget) -> Vec<OsString> {
        let mut v: Vec<OsString> = vec!["compose".into(), "--ansi".into(), "never".into()];
        if let Some(f) = &t.file {
            v.push("-f".into());
            v.push(f.into());
            if let Some(o) = &t.override_file {
                v.push("-f".into());
                v.push(o.into());
            }
        }
        v.push("-p".into());
        v.push(t.project.clone().into());
        if let Some(e) = &t.env_file {
            v.push("--env-file".into());
            v.push(e.into());
        }
        v
    }

    fn with(t: &ComposeTarget, tail: &[&str]) -> Vec<OsString> {
        let mut v = Self::base(t);
        v.extend(tail.iter().map(OsString::from));
        if !t.service.is_empty() {
            v.push(t.service.clone().into());
        }
        v
    }

    /// `up -d --no-deps [--build] <service>`: stems' graph is
    /// authoritative, so compose `depends_on` is never followed; `build`
    /// rebuilds the service's image first (`ComposeSpec.build`, default
    /// true; a service without `build:` in the file is unaffected).
    pub fn up(t: &ComposeTarget, build: bool) -> Vec<OsString> {
        if build {
            Self::with(t, &["up", "-d", "--no-deps", "--build"])
        } else {
            Self::with(t, &["up", "-d", "--no-deps"])
        }
    }

    /// `up -d --no-deps --no-build <service>`: the images were built by
    /// [`Self::bake`] (44), compose must not build them again (it cannot
    /// pass the secrets).
    pub fn up_no_build(t: &ComposeTarget) -> Vec<OsString> {
        Self::with(t, &["up", "-d", "--no-deps", "--no-build"])
    }

    /// `buildx bake --load --progress plain -f <file> [-f <override>]
    /// [--allow fs.read=<path>]… [--set <svc>.tags=<image>]… <svc>…` (44):
    /// the compose file(s) as the Bake definition, one target per service,
    /// each tagged with the image compose runs (`<project>-<svc>` when the
    /// service names none), loaded into the engine. `allow` is passed in
    /// the order given (callers sort it).
    pub fn bake(
        t: &ComposeTarget,
        images: &[(String, String)],
        allow: &[PathBuf],
    ) -> Vec<OsString> {
        let mut v: Vec<OsString> = ["buildx", "bake", "--load", "--progress", "plain"]
            .map(OsString::from)
            .into();
        if let Some(f) = &t.file {
            v.push("-f".into());
            v.push(f.into());
            if let Some(o) = &t.override_file {
                v.push("-f".into());
                v.push(o.into());
            }
        }
        for p in allow {
            let mut a = OsString::from("fs.read=");
            a.push(p);
            v.push("--allow".into());
            v.push(a);
        }
        for (svc, image) in images {
            v.push("--set".into());
            v.push(format!("{svc}.tags={image}").into());
        }
        v.extend(images.iter().map(|(svc, _)| OsString::from(svc)));
        v
    }

    /// `stop -t <grace secs, rounded up> <service>`.
    pub fn stop(t: &ComposeTarget, grace: Duration) -> Vec<OsString> {
        let secs = grace_secs(grace).to_string();
        Self::with(t, &["stop", "-t", &secs])
    }

    /// `rm -f -s -v <service>` (stops first if it is still running; `-v`
    /// takes the container's anonymous volumes, e.g. redis's `/data`, never
    /// named ones).
    pub fn rm(t: &ComposeTarget) -> Vec<OsString> {
        Self::with(t, &["rm", "-f", "-s", "-v"])
    }

    /// `-p <project> down` (no `-f`, no `-v`, no `--rmi`): removes what is
    /// left of an empty project, i.e. its `<project>_default` network.
    pub fn down_project(project: &str) -> Vec<OsString> {
        let mut v = Self::base(&ComposeTarget::project(project));
        v.push("down".into());
        v
    }

    /// `ps --format json -a [<service>]`.
    pub fn ps(t: &ComposeTarget) -> Vec<OsString> {
        Self::with(t, &["ps", "--format", "json", "-a"])
    }

    /// `compose version --format json`.
    pub fn version() -> Vec<OsString> {
        ["compose", "version", "--format", "json"]
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    /// `config --services` (lists the services of the file).
    pub fn config(t: &ComposeTarget) -> Vec<OsString> {
        let mut v = Self::base(t);
        v.extend(["config", "--services"].map(OsString::from));
        v
    }

    /// `config --format json`: the resolved file (absolute `build.context`,
    /// interpolated values), parsed by [`super::parse::service_config`].
    pub fn config_json(t: &ComposeTarget) -> Vec<OsString> {
        let mut v = Self::base(t);
        v.extend(["config", "--format", "json"].map(OsString::from));
        v
    }
}

/// A printable command line (`docker compose ...`), single-quoting
/// arguments that need it. Used in goldens and `COMPOSE_FAILED`.
pub fn display_command(binary: &OsStr, args: &[OsString]) -> String {
    std::iter::once(binary)
        .chain(args.iter().map(OsString::as_os_str))
        .map(|a| shell_quote(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:@,+%".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `<env_dir>/<stem>.env`.
pub fn env_file_path(env_dir: &Path, stem: &str) -> PathBuf {
    env_dir.join(format!("{stem}.env"))
}

/// `<env_dir>/<stem>.override.yaml` (`inject_env: true`).
pub fn override_file_path(env_dir: &Path, stem: &str) -> PathBuf {
    env_dir.join(format!("{stem}.override.yaml"))
}

/// What the generated override file (`<env_dir>/<stem>.override.yaml`)
/// declares: the `inject_env` part (38) and the build secrets (44).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverrideInputs<'a> {
    /// The stem's service.
    pub service: &'a str,
    /// `inject_env: true`: the stem's env file.
    pub env_file: Option<&'a Path>,
    /// With `env_file`: the container port the override gives as `PORT`.
    pub container_port: Option<u16>,
    /// Build secrets: declared top-level and added to each of
    /// `build_services`' `build.secrets`.
    pub secrets: &'a [crate::docker::BuildSecret],
    pub build_services: &'a [String],
}

/// The override compose file (passed as a second `-f`): with an env file,
/// [`render_override_file`]'s `env_file:`/`PORT` for the service; with
/// secrets, a top-level `secrets:` (`file:` or `environment:`) and each
/// build service's `build.secrets` naming them (compose and Bake merge
/// both with the user's file, so it needs no stems-specific edits).
/// Strings are JSON-quoted (valid YAML).
pub fn render_override(o: &OverrideInputs<'_>) -> String {
    use crate::docker::SecretSource;
    let q = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"));
    let mut out = String::from(match (o.env_file.is_some(), o.secrets.is_empty()) {
        (true, true) => {
            "# Written by stems for `inject_env: true` (passed as a second `-f`); do not edit.\n"
        }
        (false, _) => {
            "# Written by stems for `build.secrets` (passed as a second `-f`); do not edit.\n"
        }
        (true, false) => {
            "# Written by stems for `inject_env: true` and `build.secrets` (passed as a second `-f`); do not edit.\n"
        }
    });
    out.push_str("services:\n");
    let mut services: Vec<&str> = Vec::new();
    if o.env_file.is_some() || o.secrets.is_empty() {
        services.push(o.service);
    }
    if !o.secrets.is_empty() {
        for s in o.build_services {
            if !services.contains(&s.as_str()) {
                services.push(s);
            }
        }
    }
    for svc in services {
        out.push_str(&format!("  {}:\n", q(svc)));
        if svc == o.service
            && let Some(env_file) = o.env_file
        {
            out.push_str("    env_file:\n");
            out.push_str(&format!("      - {}\n", q(&env_file.to_string_lossy())));
            if let Some(p) = o.container_port {
                out.push_str("    environment:\n");
                out.push_str(&format!("      PORT: \"{p}\"\n"));
            }
        }
        if !o.secrets.is_empty() && o.build_services.iter().any(|b| b == svc) {
            out.push_str("    build:\n      secrets:\n");
            for s in o.secrets {
                out.push_str(&format!("        - {}\n", q(&s.id)));
            }
        }
    }
    if !o.secrets.is_empty() {
        out.push_str("secrets:\n");
        for s in o.secrets {
            out.push_str(&format!("  {}:\n", q(&s.id)));
            match &s.source {
                SecretSource::File(f) => {
                    out.push_str(&format!("    file: {}\n", q(&f.to_string_lossy())));
                }
                SecretSource::Env(v) => out.push_str(&format!("    environment: {}\n", q(v))),
            }
        }
    }
    out
}

/// The override compose file of an `inject_env: true` stem: the stem's env
/// file appended to `service`'s `env_file:` (compose appends `env_file`
/// lists across `-f` files, later files winning; the service's own
/// `environment:` still wins over every env file), plus `environment: {PORT:
/// <container_port>}` when the stem's primary port maps to a container
/// port, so the container sees the port it listens on, not the host port.
/// Strings are JSON-quoted (valid YAML).
pub fn render_override_file(service: &str, env_file: &Path, container_port: Option<u16>) -> String {
    render_override(&OverrideInputs {
        service,
        env_file: Some(env_file),
        container_port,
        ..Default::default()
    })
}

/// `<env_dir>/<project>.owned`: written after a successful `up`, it marks
/// the compose project as created (or explicitly adopted) by stems.
pub fn marker_path(env_dir: &Path, project: &str) -> PathBuf {
    env_dir.join(format!("{project}.owned"))
}

fn valid_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Quote a value for compose's dotenv parser. Single quotes are literal
/// (no `$` interpolation, no escapes), so they are used whenever the value
/// has no `'` and no newline; otherwise double quotes with `\\`, `\"`,
/// `\n`, `\r` and `\$` escaped.
pub fn quote_env_value(v: &str) -> String {
    if !v.contains('\'') && !v.contains('\n') && !v.contains('\r') {
        return format!("'{v}'");
    }
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '$' => out.push_str(r"\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The env file for a stem: one `KEY='value'` per line, sorted by key.
/// Keys that are not valid variable names are an error (they could not
/// be exported to the compose process either).
pub fn render_env_file(env: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out =
        String::from("# Written by stems for `docker compose --env-file`; do not edit.\n");
    for (k, v) in env {
        if !valid_key(k) {
            return Err(format!("invalid environment variable name `{k}`"));
        }
        out.push_str(k);
        out.push('=');
        out.push_str(&quote_env_value(v));
        out.push('\n');
    }
    Ok(out)
}

/// Write `path` atomically (temp file + rename), mode 0600: stem env may
/// carry secrets.
pub fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Render and write a stem's env file.
pub fn write_env_file(path: &Path, env: &BTreeMap<String, String>) -> std::io::Result<()> {
    let content = render_env_file(env).map_err(std::io::Error::other)?;
    write_private(path, &content)
}

/// The environment exported to a compose process on top of the daemon's:
/// the stem env, plus `DOCKER_HOST` pointing at the daemon the
/// [`crate::DockerRuntime`] talks to (unless the stem env sets it), so the
/// CLI and the API client always see the same engine.
pub fn compose_env(
    stem_env: &BTreeMap<String, String>,
    docker_host: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env = stem_env.clone();
    if let Some(h) = docker_host.filter(|h| !h.is_empty()) {
        env.entry("DOCKER_HOST".into()).or_insert_with(|| h.into());
    }
    env
}
