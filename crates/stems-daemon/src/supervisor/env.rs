//! A stem's process environment (FR-ST-4) and start-time substitution of
//! deferred `${stem.<n>.port}` references (FR-WS-10, FR-ST-5).
//!
//! Layers, later wins:
//!
//! 1. the environment of the `stems` command that started it (the
//!    request's `caller_env`, else the daemon's own) minus `STEMS_*`
//!    variables ([`CallEnv`], 37);
//! 2. `PORT` = the stem's primary host port, when it declares ports;
//! 3. workspace `env` → stem `env` (already merged by the loader);
//! 4. `env_files` (`KEY=value` dotenv files, in order; an absent
//!    `required: false` entry is skipped, see `stems_core::envfiles`); an
//!    entry with `override: false` skips the keys layer 3 sets;
//! 5. `stems.local.yaml` env (`Stem::local_env`);
//! 6. the shell environment passed with `stems up --pass-env`;
//! 7. `STEMS_STEM`, `STEMS_WORKSPACE`, `STEMS_CODEBASE`, `STEMS_RUN_ID`,
//!    `STEMS_<DEP>_PORT` for each dependency with a port, `PORT_<NAME>` and
//!    `STEMS_<DEP>_PORT_<NAME>` for explicitly named ports,
//!    `STEMS_<DEP>_CODEBASE` for each dependency with a codebase (49), and
//!    `STEMS_<DEP>_OUTPUT_<X>` for each evaluated output of a dependency (26).
//!
//! Every value is then re-rendered: `${stem.<n>.port}` and
//! `${stem.<n>.ports.<p>}` left in place by the loader (auto ports) become
//! the allocated ports, `${stem.<n>.outputs.<x>}` the evaluated output (26).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use stems_config::{EnvFile, Stem, Workspace};
use stems_core::Error;

use super::ports::PortBook;

/// Inputs of [`build_env`] beyond the config.
pub struct EnvInputs<'a> {
    /// Layer 1: the caller's environment, else the daemon's own
    /// ([`CallEnv::base`]).
    pub base: &'a BTreeMap<String, String>,
    /// `--pass-env` values.
    pub pass_env: &'a BTreeMap<String, String>,
    /// This daemon run's id.
    pub run_id: &'a str,
    /// Port book (auto ports).
    pub ports: &'a PortBook,
    /// Evaluated outputs (26): `STEMS_<DEP>_OUTPUT_<X>` and
    /// `${stem.<n>.outputs.X}`; `None` leaves output references alone.
    pub outputs: Option<&'a super::outputs::OutputStore>,
    /// An output reference without a value is `UNRESOLVED_VARIABLE` (the
    /// start of the stem) instead of being kept literally (build, reset,
    /// custom scripts).
    pub strict_outputs: bool,
}

/// `STEMS_<NAME>_PORT` for a stem name (`shop-api` → `STEMS_SHOP_API_PORT`).
pub fn port_var(stem: &str) -> String {
    let up: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("STEMS_{up}_PORT")
}

/// `STEMS_<NAME>_CODEBASE` for a stem name (`shop-api` →
/// `STEMS_SHOP_API_CODEBASE`), 49.
pub fn codebase_var(stem: &str) -> String {
    let port = port_var(stem);
    format!("{}_CODEBASE", &port[..port.len() - "_PORT".len()])
}

/// A stem's environment: what stems sets (layers 2–7, shown by `status
/// -v`) and the full environment given to the process (layer 1 below it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StemEnv {
    /// Layers 2–7 only (never the inherited daemon environment).
    pub own: BTreeMap<String, String>,
    /// Everything the process gets.
    pub full: BTreeMap<String, String>,
}

/// What a request that spawns something brings along (37): the caller's
/// environment (layer 1) and `--pass-env` (layer 6). A stem keeps the one
/// of its last user start for restarts stems performs on its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallEnv {
    /// The environment of the `stems` command (already without `STEMS_*`);
    /// `None`: the daemon's own.
    pub caller: Option<Arc<BTreeMap<String, String>>>,
    /// `--pass-env` values.
    pub pass: BTreeMap<String, String>,
}

impl CallEnv {
    /// From a request's `caller_env` and `pass_env`.
    pub fn new(caller: Option<BTreeMap<String, String>>, pass: BTreeMap<String, String>) -> Self {
        Self {
            caller: caller.map(|c| Arc::new(caller_base(c))),
            pass,
        }
    }

    /// Layer 1: the caller's environment, else `daemon`.
    pub fn base<'a>(
        &'a self,
        daemon: &'a BTreeMap<String, String>,
    ) -> &'a BTreeMap<String, String> {
        self.caller.as_deref().unwrap_or(daemon)
    }
}

/// A caller's environment as layer 1: without `STEMS_*`, with the `docker`
/// CLI's directory on `PATH` (like the daemon's own).
pub fn caller_base(env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = env
        .into_iter()
        .filter(|(k, _)| !stems_core::callerenv::is_stems_injected(k))
        .collect();
    stems_runtime::docker::ensure_docker_on_path(&mut env);
    env
}

/// Variables of the base layer a script with `inherit_env: false` (or
/// `stems exec --fresh`) still gets; `LC_*` too.
pub const MINIMAL_ENV: [&str; 8] = [
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TERM", "LANG",
];

/// `key` is in the minimal allowlist ([`MINIMAL_ENV`], `LC_*`).
pub fn is_minimal(key: &str) -> bool {
    MINIMAL_ENV.contains(&key) || key.starts_with("LC_")
}

/// The minimal allowlist of `base`.
pub fn minimal_base(base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    base.iter()
        .filter(|(k, _)| is_minimal(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `full` (base + `own`) reduced for `inherit_env: false`: the minimal
/// allowlist of it, then `own` (what stems sets) on top.
pub fn without_inherited(
    full: &BTreeMap<String, String>,
    own: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = minimal_base(full);
    env.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// Build `stem`'s environment. `allocated` receives auto ports allocated
/// while resolving references (stem, port name, port).
pub fn build_env(
    ws: &Workspace,
    stem: &Stem,
    inp: &EnvInputs<'_>,
    allocated: &mut Vec<(String, String, u16)>,
) -> Result<StemEnv, Error> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    if let Some(p) = inp.ports.resolve_ref(ws, &stem.name, None, allocated) {
        env.insert("PORT".into(), p.to_string());
    }
    // Read on every call (never cached): a key removed from a file is gone
    // from the next start, `stems env`, `exec` and `compose`.
    env.extend(config_layers(ws, stem, inp.pass_env, &mut parse_env_file)?);
    env.insert("STEMS_STEM".into(), stem.name.clone());
    env.insert("STEMS_WORKSPACE".into(), ws.root.display().to_string());
    if let Some(cb) = &stem.codebase {
        env.insert("STEMS_CODEBASE".into(), cb.path().display().to_string());
    }
    env.insert("STEMS_RUN_ID".into(), inp.run_id.to_string());
    // A disabled dependency (an `if_disabled: start` edge, 46) has no port.
    let enabled = |n: &str| ws.stem(n).is_some_and(|d| d.enabled);
    for dep in stem.depends_on.iter().filter(|d| enabled(&d.stem)) {
        if let Some(p) = inp.ports.resolve_ref(ws, &dep.stem, None, allocated) {
            env.insert(port_var(&dep.stem), p.to_string());
        }
    }
    // Named ports: `PORT_<NAME>` for the stem's own, `STEMS_<DEP>_PORT_<NAME>`
    // for each dependency's (explicit names only; 42).
    for (i, port) in stem.ports.iter().enumerate() {
        if super::ports::is_named(i, port)
            && let Some(p) = inp
                .ports
                .resolve_ref(ws, &stem.name, Some(&port.name), allocated)
        {
            env.insert(super::ports::own_port_var(&port.name), p.to_string());
        }
    }
    for dep in &stem.depends_on {
        let Some(d) = ws.stem(&dep.stem).filter(|d| d.enabled) else {
            continue;
        };
        for (i, port) in d.ports.iter().enumerate() {
            if super::ports::is_named(i, port)
                && let Some(p) = inp
                    .ports
                    .resolve_ref(ws, &dep.stem, Some(&port.name), allocated)
            {
                env.insert(
                    super::ports::named_port_var(&dep.stem, &port.name),
                    p.to_string(),
                );
            }
        }
    }
    // `STEMS_<DEP>_CODEBASE`: each enabled dependency's codebase, absolute
    // (49); a disabled one is absent, like its `_PORT` (46).
    for dep in &stem.depends_on {
        if let Some(cb) = ws
            .stem(&dep.stem)
            .filter(|d| d.enabled)
            .and_then(|d| d.codebase.as_ref())
        {
            env.insert(codebase_var(&dep.stem), cb.path().display().to_string());
        }
    }
    if let Some(store) = inp.outputs {
        for dep in &stem.depends_on {
            for (name, o) in store.get(&dep.stem) {
                env.insert(super::outputs::output_var(&dep.stem, &name), o.value);
            }
        }
    }
    for (k, v) in env.iter_mut() {
        if v.contains("${stem.") {
            *v = render_refs(v, &stem.name, &mut |n, p| {
                inp.ports.resolve_ref(ws, n, p, allocated)
            });
        }
        if let Some(store) = inp.outputs
            && v.contains("${stem.")
        {
            let mut missing = Vec::new();
            *v = super::outputs::render_output_refs(
                v,
                &stem.name,
                &|n, x| store.value(n, x),
                &mut missing,
            );
            if inp.strict_outputs
                && let Some(r) = missing.first()
            {
                return Err(super::outputs::unresolved_error(&stem.name, r, Some(k)));
            }
        }
    }
    let mut full: BTreeMap<String, String> = inp
        .base
        .iter()
        .filter(|(k, _)| !stems_core::callerenv::is_stems_injected(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    full.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(StemEnv { own: env, full })
}

/// Reads entry `i` of a stem's `env_files` ([`parse_env_file`], or
/// [`EnvFileCache::read`]).
pub type EnvFileReader<'a> =
    dyn FnMut(&Path, &str, usize, &EnvFile) -> Result<Vec<(String, String)>, Error> + 'a;

/// Layers 3–6 of `stem`'s environment, unrendered (`${stem...}` references
/// kept literally): workspace/stem `env:`, `env_files` (read with `read`),
/// `stems.local.yaml` env, then `pass` (`--pass-env`, `STEMS_*` names
/// included, exactly as a start applies them). [`build_env`] is `PORT`,
/// this, then layer 7.
pub fn config_layers(
    ws: &Workspace,
    stem: &Stem,
    pass: &BTreeMap<String, String>,
    read: &mut EnvFileReader<'_>,
) -> Result<BTreeMap<String, String>, Error> {
    let mut env: BTreeMap<String, String> = stem
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (i, file) in stem.env_files.iter().enumerate() {
        for (k, v) in read(&ws.root, &stem.name, i, file)? {
            // `override: false`: a key `env:` sets keeps its `env:` value.
            if !file.override_env && stem.env.contains_key(&k) {
                continue;
            }
            env.insert(k, v);
        }
    }
    for (k, v) in &stem.local_env {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in pass {
        env.insert(k.clone(), v.clone());
    }
    Ok(env)
}

/// Parsed env files keyed by path, valid while the file's modification
/// time and length are unchanged: `status` computes `env_stale` for every
/// running stem on every call (the TUI polls it) with one `stat` per file
/// instead of a read and a parse. Only drift detection uses it; a start,
/// `stems env`, `exec` and `compose` always read the files.
#[derive(Default)]
pub struct EnvFileCache {
    #[allow(clippy::type_complexity)]
    files: std::sync::Mutex<
        std::collections::HashMap<
            std::path::PathBuf,
            (std::time::SystemTime, u64, Vec<(String, String)>),
        >,
    >,
}

impl EnvFileCache {
    /// [`parse_env_file`], served from the cache when the file's mtime and
    /// length match the cached read. A file that cannot be stat'ed (absent,
    /// unreadable) is read as usual (its error or `required: false` skip).
    pub fn read(
        &self,
        root: &Path,
        stem: &str,
        i: usize,
        file: &EnvFile,
    ) -> Result<Vec<(String, String)>, Error> {
        let stamp = std::fs::metadata(&file.path)
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let Some((mtime, len)) = stamp else {
            return parse_env_file(root, stem, i, file);
        };
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((t, l, pairs)) = files.get(&file.path)
            && (*t, *l) == (mtime, len)
        {
            return Ok(pairs.clone());
        }
        let pairs = parse_env_file(root, stem, i, file)?;
        files.insert(file.path.clone(), (mtime, len, pairs.clone()));
        Ok(pairs)
    }
}

/// Replace `${stem.<n>.port}` / `${stem.<n>.ports.<p>}` (and `stem.self`)
/// with `lookup(n, p)`; unknown references are kept literally.
pub fn render_refs(
    s: &str,
    self_name: &str,
    lookup: &mut dyn FnMut(&str, Option<&str>) -> Option<u16>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${stem.") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let expr = &after[..end];
        let parts: Vec<&str> = expr.split('.').collect();
        let value = match parts.as_slice() {
            ["stem", n, "port"] => {
                let n = if *n == "self" { self_name } else { n };
                lookup(n, None)
            }
            ["stem", n, "ports", p] => {
                let n = if *n == "self" { self_name } else { n };
                lookup(n, Some(p))
            }
            _ => None,
        };
        match value {
            Some(v) => out.push_str(&v.to_string()),
            None => out.push_str(&rest[i..i + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Entry `i` of `stem`'s `env_files` (`stems_core::envfiles`): its pairs,
/// none for an absent `required: false` file; a missing required file is
/// `ENV_FILE_NOT_FOUND`, an unreadable one `CONFIG_READ_FAILED`.
pub fn parse_env_file(
    root: &Path,
    stem: &str,
    i: usize,
    file: &EnvFile,
) -> Result<Vec<(String, String)>, Error> {
    Ok(stems_core::envfiles::read(root, stem, i, file)?.unwrap_or_default())
}

pub use stems_core::envfiles::{parse_dotenv, shadow_warnings};

/// `ENV_SHADOWED` of `stem` at a user start (`up`/`start`/`restart`): each
/// warning is logged once and added to the request's warnings
/// (`UpResult.warnings`), never twice within one request.
pub(crate) fn warn_shadowed(core: &super::Core, root: &Path, stem: &Stem) {
    // Only the surprising case (a file silently beating `env:`): with
    // `override: false` the user chose the outcome, and `validate`/`doctor`
    // still report it once.
    let found: Vec<_> = shadow_warnings(root, stem)
        .into_iter()
        .filter(|w| w.details["winner"] != "env")
        .collect();
    if found.is_empty() {
        return;
    }
    let mut warnings = core.warnings.lock().unwrap_or_else(|e| e.into_inner());
    for w in found {
        if warnings.contains(&w) {
            continue;
        }
        tracing::warn!(stem = %stem.name, code = %w.code, "{}", w.message);
        warnings.push(w);
    }
}

/// Variables only `stems exec`/`env` add (never part of a start's env).
const EXEC_ONLY: [&str; 1] = ["STEMS_STATE_DIR"];

/// Layer-7 variable of `stem` (and the exec-only `STEMS_STATE_DIR`): what
/// stems derives from the workspace, the daemon run, ports, codebases and
/// dependency outputs, never from the config layers (a config value for
/// one of these names is overridden anyway).
pub fn is_marker(ws: &Workspace, stem: &Stem, key: &str) -> bool {
    const FIXED: [&str; 4] = [
        "STEMS_STEM",
        "STEMS_WORKSPACE",
        "STEMS_CODEBASE",
        "STEMS_RUN_ID",
    ];
    if FIXED.contains(&key) || EXEC_ONLY.contains(&key) {
        return true;
    }
    let named = |s: &Stem, var: &dyn Fn(&str) -> String| {
        s.ports
            .iter()
            .enumerate()
            .any(|(i, p)| super::ports::is_named(i, p) && var(&p.name) == key)
    };
    if named(stem, &super::ports::own_port_var) {
        return true;
    }
    stem.depends_on.iter().any(|dep| {
        let n = dep.stem.as_str();
        port_var(n) == key
            || codebase_var(n) == key
            || key.starts_with(&super::outputs::output_var(n, ""))
            || ws
                .stem(n)
                .is_some_and(|d| named(d, &|p| super::ports::named_port_var(n, p)))
    })
}

/// `value` is what `template` renders to with some value for each
/// `${stem...}` reference in it (each one matches any text; the literal
/// text around them must be equal). Without a reference: plain equality.
fn matches_template(template: &str, value: &str) -> bool {
    let mut literals: Vec<&str> = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find("${stem.") {
        let Some(end) = rest[i..].find('}') else {
            break;
        };
        literals.push(&rest[..i]);
        rest = &rest[i + end + 1..];
    }
    if literals.is_empty() {
        return template == value;
    }
    literals.push(rest);
    let (first, last) = (literals[0], literals[literals.len() - 1]);
    if value.len() < first.len() + last.len() || !value.starts_with(first) || !value.ends_with(last)
    {
        return false;
    }
    let mut middle = &value[first.len()..value.len() - last.len()];
    for lit in &literals[1..literals.len() - 1] {
        match middle.find(lit) {
            Some(i) => middle = &middle[i + lit.len()..],
            None => return false,
        }
    }
    true
}

/// `ENV_STALE` keys of running `stem`: where `live` (what it got at its
/// start, layers 2–7 rendered) differs from `config` (layers 3–6 as the
/// config, the env files and its `--pass-env` yield now, unrendered:
/// [`config_layers`]). Only the config layers count:
///
/// - layer-7 variables ([`is_marker`]: `STEMS_*` markers, dependency ports,
///   codebases and outputs) and a `PORT` no config layer sets are never
///   drift: a dependency that restarted (new output) or is down (no value)
///   changed nothing in the stem's config;
/// - a config value with `${stem...}` references is compared as a template
///   (each reference matches whatever it rendered to at the start), so a
///   reference that is unresolved now, or resolves to a new dependency
///   output, is not drift; its literal text changing is;
/// - otherwise a key changed, added or removed is drift.
///
/// Sorted.
pub fn drift(
    ws: &Workspace,
    stem: &Stem,
    live: &BTreeMap<String, String>,
    config: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut keys: Vec<String> = live
        .keys()
        .chain(config.keys())
        .filter(|k| !is_marker(ws, stem, k))
        .filter(|k| match (config.get(*k), live.get(*k)) {
            (Some(t), Some(l)) => !matches_template(t, l),
            (Some(_), None) => true,
            (None, Some(_)) => k.as_str() != "PORT",
            (None, None) => false,
        })
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// `env` (what `stems env`/`exec`/`compose` compute now for running `stem`)
/// with `live` (its start-time env) for every key that has no usable value
/// now: a value still holding a `${stem...}` reference (a dependency output
/// cleared while it restarts, an auto port not allocated) and a layer-7
/// variable (a dependency's port or output variable) absent now. Per key:
/// everything else stays as computed now.
pub fn keep_live(
    ws: &Workspace,
    stem: &Stem,
    env: &mut BTreeMap<String, String>,
    live: &BTreeMap<String, String>,
) {
    for (k, v) in env.iter_mut() {
        if v.contains("${stem.")
            && let Some(l) = live.get(k)
        {
            v.clone_from(l);
        }
    }
    for (k, l) in live {
        if !env.contains_key(k) && is_marker(ws, stem, k) {
            env.insert(k.clone(), l.clone());
        }
    }
}

/// `ENV_STALE` (a warning): running `stem` got an environment that differs
/// from its config now in `keys`.
pub fn stale_warning(stem: &str, keys: &[String]) -> Error {
    Error::new(
        stems_core::ErrorCode::EnvStale,
        format!(
            "`{stem}` runs with an environment that differs from its config now: {} (changed, added or removed since it started)",
            keys.join(", ")
        ),
    )
    .with_hint(format!("restart it to apply: `stems restart {stem}`"))
    .with_details(serde_json::json!({ "stem": stem, "keys": keys }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs() {
        let mut look = |n: &str, p: Option<&str>| match (n, p) {
            ("api", None) => Some(8080),
            ("web", Some("http")) => Some(3000),
            _ => None,
        };
        assert_eq!(
            render_refs(
                "http://localhost:${stem.api.port}/x ${stem.self.ports.http} ${stem.nope.port} ${HOME}",
                "web",
                &mut look
            ),
            "http://localhost:8080/x 3000 ${stem.nope.port} ${HOME}"
        );
        assert_eq!(
            render_refs("${stem.api.port", "w", &mut look),
            "${stem.api.port"
        );
    }

    #[test]
    fn base_selection() {
        let daemon: BTreeMap<String, String> = [("FOO", "stale"), ("PATH", "/bin")]
            .map(|(a, b)| (a.into(), b.into()))
            .into();
        let caller: BTreeMap<String, String> = [
            ("FOO", "fresh"),
            ("PATH", "/usr/bin:/bin"),
            ("STEMS_HOME", "/x"),
            ("STEMS_STEM", "leaked"),
            ("LC_ALL", "C"),
            ("SECRET", "s"),
        ]
        .map(|(a, b)| (a.into(), b.into()))
        .into();
        // No caller env: the daemon's.
        let none = CallEnv::default();
        assert_eq!(none.base(&daemon)["FOO"], "stale");
        // The caller's, minus the injected STEMS_* (the CLI's own pass through).
        let call = CallEnv::new(Some(caller), BTreeMap::new());
        let base = call.base(&daemon);
        assert_eq!(base["FOO"], "fresh");
        assert_eq!(base["STEMS_HOME"], "/x");
        assert!(!base.contains_key("STEMS_STEM"));
        // Minimal: the allowlist (LC_* included), then what stems sets.
        let own: BTreeMap<String, String> = [("PORT", "8080"), ("PATH", "/own")]
            .map(|(a, b)| (a.into(), b.into()))
            .into();
        let min = without_inherited(base, &own);
        assert_eq!(min.get("FOO"), None);
        assert_eq!(min.get("SECRET"), None);
        assert_eq!(min["LC_ALL"], "C");
        assert_eq!(min["PORT"], "8080");
        assert_eq!(min["PATH"], "/own");
    }

    #[test]
    fn port_vars() {
        assert_eq!(port_var("shop-api"), "STEMS_SHOP_API_PORT");
        assert_eq!(port_var("pg"), "STEMS_PG_PORT");
        assert_eq!(codebase_var("shop-api"), "STEMS_SHOP_API_CODEBASE");
    }

    #[test]
    fn dependency_codebases() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("api")).unwrap();
        std::fs::write(
            root.join("stems.yaml"),
            "schema_version: 1\nname: t\nstems:\n  api: { type: process, codebase: api, scripts: { start: 'true' } }\n  off: { type: process, codebase: api, enabled: false, scripts: { start: 'true' } }\n  db: { type: process, scripts: { start: 'true' } }\n  web: { type: process, depends_on: [api, db, { stem: off, if_disabled: start }], scripts: { start: 'true' } }\n",
        )
        .unwrap();
        let ws = stems_config::load(stems_config::LoadOptions {
            workspace: Some(root.clone()),
            cwd: root.clone(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap()
        .workspace;
        let none = BTreeMap::new();
        let ports = PortBook::default();
        let env = build_env(
            &ws,
            ws.stem("web").unwrap(),
            &EnvInputs {
                base: &none,
                pass_env: &none,
                run_id: "r",
                ports: &ports,
                outputs: None,
                strict_outputs: false,
            },
            &mut Vec::new(),
        )
        .unwrap()
        .own;
        assert_eq!(
            env["STEMS_API_CODEBASE"],
            root.join("api").display().to_string()
        );
        assert!(!env.contains_key("STEMS_DB_CODEBASE"));
        assert!(!env.contains_key("STEMS_CODEBASE"));
        // A disabled dependency is absent, like its `_PORT` (46).
        assert!(!env.contains_key("STEMS_OFF_CODEBASE"));
    }

    fn load_ws(root: &Path, yaml: &str) -> Workspace {
        std::fs::write(root.join("stems.yaml"), yaml).unwrap();
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(root.to_path_buf()),
            cwd: root.to_path_buf(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap()
        .workspace
    }

    fn own_env(ws: &Workspace, stem: &str) -> BTreeMap<String, String> {
        let none = BTreeMap::new();
        let ports = PortBook::default();
        build_env(
            ws,
            ws.stem(stem).unwrap(),
            &EnvInputs {
                base: &none,
                pass_env: &none,
                run_id: "r",
                ports: &ports,
                outputs: None,
                strict_outputs: false,
            },
            &mut Vec::new(),
        )
        .unwrap()
        .own
    }

    #[test]
    fn override_false_keeps_env_keys() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "PORT=3000\nEXTRA=x\nK=file\n").unwrap();
        std::fs::write(root.join("b.env"), "K=b\n").unwrap();
        let ws = load_ws(
            &root,
            "schema_version: 1\nname: t\nenv: { K: ws }\nstems:\n  keep: { type: process, command: x, env: { PORT: '8080' }, env_files: [{ path: .env, override: false }] }\n  win: { type: process, command: x, env: { PORT: '8080' }, env_files: [.env] }\n  later: { type: process, command: x, env_files: [{ path: .env, override: false }, b.env] }\n",
        );
        let keep = own_env(&ws, "keep");
        assert_eq!(keep["PORT"], "8080");
        assert_eq!(keep["EXTRA"], "x");
        // Workspace `env` counts as `env:`.
        assert_eq!(keep["K"], "ws");
        let win = own_env(&ws, "win");
        assert_eq!(win["PORT"], "3000");
        assert_eq!(win["K"], "file");
        // A later file (override: true) still beats `env:`.
        assert_eq!(own_env(&ws, "later")["K"], "b");
    }

    #[test]
    fn workspace_default_override() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "PORT=3000\n").unwrap();
        let ws = load_ws(
            &root,
            "schema_version: 1\nname: t\nenv_files_override: false\nstems:\n  a: { type: process, command: x, env: { PORT: '8080' }, env_files: [.env] }\n  b: { type: process, command: x, env: { PORT: '8080' }, env_files: [{ path: .env, override: true }] }\n",
        );
        assert_eq!(own_env(&ws, "a")["PORT"], "8080");
        assert_eq!(own_env(&ws, "b")["PORT"], "3000");
    }

    #[test]
    fn env_files_are_reread_and_a_removed_key_is_gone() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join("api.env"), "KEEP=1\nGONE=1\n").unwrap();
        let ws = load_ws(
            &root,
            "schema_version: 1\nname: t\nstems:\n  api: { type: process, command: x, env_files: [api.env] }\n",
        );
        let started = own_env(&ws, "api");
        assert_eq!(started["GONE"], "1");
        // Same loaded workspace: the file is read again, nothing is cached.
        std::fs::write(root.join("api.env"), "KEEP=2\n").unwrap();
        let now = own_env(&ws, "api");
        assert!(!now.contains_key("GONE"));
        assert_eq!(now["KEEP"], "2");
        let stem = ws.stem("api").unwrap();
        let config = layers(&ws, "api", &BTreeMap::new());
        assert_eq!(drift(&ws, stem, &started, &config), vec!["GONE", "KEEP"]);
        // `STEMS_STATE_DIR` (exec only) is never drift.
        let mut exec = started.clone();
        exec.insert("STEMS_STATE_DIR".into(), "/s".into());
        std::fs::write(root.join("api.env"), "KEEP=1\nGONE=1\n").unwrap();
        let config = layers(&ws, "api", &BTreeMap::new());
        assert!(drift(&ws, stem, &exec, &config).is_empty());
        let w = stale_warning("api", &["GONE".to_string(), "KEEP".to_string()]);
        assert_eq!(w.code, stems_core::ErrorCode::EnvStale);
        assert_eq!(
            w.message,
            "`api` runs with an environment that differs from its config now: GONE, KEEP (changed, added or removed since it started)"
        );
        assert_eq!(
            w.hint.as_deref(),
            Some("restart it to apply: `stems restart api`")
        );
    }

    fn layers(
        ws: &Workspace,
        stem: &str,
        pass: &BTreeMap<String, String>,
    ) -> BTreeMap<String, String> {
        config_layers(ws, ws.stem(stem).unwrap(), pass, &mut parse_env_file).unwrap()
    }

    fn env_with(
        ws: &Workspace,
        stem: &str,
        pass: &BTreeMap<String, String>,
        outputs: &super::super::outputs::OutputStore,
    ) -> BTreeMap<String, String> {
        let none = BTreeMap::new();
        let ports = PortBook::default();
        build_env(
            ws,
            ws.stem(stem).unwrap(),
            &EnvInputs {
                base: &none,
                pass_env: pass,
                run_id: "r",
                ports: &ports,
                outputs: Some(outputs),
                strict_outputs: false,
            },
            &mut Vec::new(),
        )
        .unwrap()
        .own
    }

    fn token(store: &super::super::outputs::OutputStore, value: &str) {
        let entry = super::super::outputs::OutputEntry {
            value: value.into(),
            secret: false,
        };
        store.set("auth", [("TOKEN".to_string(), entry)].into());
    }

    const DEPS: &str = "schema_version: 1\nname: t\nstems:\n  auth: { type: process, command: x, ports: [{ name: http, port: 18100 }], outputs: { TOKEN: { command: 'echo t' } } }\n  api: { type: process, command: x, depends_on: [auth], env: { AUTH_TOKEN: '${stem.auth.outputs.TOKEN}', AUTH_URL: 'http://localhost:${stem.auth.port}/v1', PLAIN: p }, env_files: [api.env] }\n";

    #[test]
    fn dependency_outputs_are_never_drift() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join("api.env"), "FROM_FILE=1\n").unwrap();
        let ws = load_ws(&root, DEPS);
        let stem = ws.stem("api").unwrap();
        let none = BTreeMap::new();
        let store = super::super::outputs::OutputStore::default();
        token(&store, "t1");
        let live = env_with(&ws, "api", &none, &store);
        assert_eq!(live["AUTH_TOKEN"], "t1");
        assert_eq!(live["STEMS_AUTH_OUTPUT_TOKEN"], "t1");
        assert_eq!(live["AUTH_URL"], "http://localhost:18100/v1");
        let config = layers(&ws, "api", &none);
        assert!(drift(&ws, stem, &live, &config).is_empty());

        // `auth` restarted with a new token: not drift.
        token(&store, "t2");
        let now = env_with(&ws, "api", &none, &store);
        assert_eq!(now["STEMS_AUTH_OUTPUT_TOKEN"], "t2");
        assert!(drift(&ws, stem, &live, &config).is_empty());

        // `auth` is down (outputs cleared): unresolved now, not drift.
        store.clear("auth");
        let now = env_with(&ws, "api", &none, &store);
        assert_eq!(now["AUTH_TOKEN"], "${stem.auth.outputs.TOKEN}");
        assert!(!now.contains_key("STEMS_AUTH_OUTPUT_TOKEN"));
        assert!(drift(&ws, stem, &live, &config).is_empty());

        // An env-file change is drift; so is the literal text around a
        // reference.
        std::fs::write(root.join("api.env"), "FROM_FILE=2\n").unwrap();
        let config = layers(&ws, "api", &none);
        assert_eq!(drift(&ws, stem, &live, &config), vec!["FROM_FILE"]);
        let ws2 = load_ws(&root, &DEPS.replace("/v1", "/v2"));
        let config = layers(&ws2, "api", &none);
        assert_eq!(
            drift(&ws2, ws2.stem("api").unwrap(), &live, &config),
            vec!["AUTH_URL", "FROM_FILE"]
        );
    }

    #[test]
    fn unresolved_keys_keep_the_live_value() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join("api.env"), "FROM_FILE=1\n").unwrap();
        let ws = load_ws(&root, DEPS);
        let stem = ws.stem("api").unwrap();
        let none = BTreeMap::new();
        let store = super::super::outputs::OutputStore::default();
        token(&store, "t1");
        let live = env_with(&ws, "api", &none, &store);

        // `auth` restarting: its output reference and variable keep the
        // start-time value; the rest is computed now (the file changed).
        store.clear("auth");
        std::fs::write(root.join("api.env"), "FROM_FILE=2\n").unwrap();
        let mut now = env_with(&ws, "api", &none, &store);
        keep_live(&ws, stem, &mut now, &live);
        assert_eq!(now["AUTH_TOKEN"], "t1");
        assert_eq!(now["STEMS_AUTH_OUTPUT_TOKEN"], "t1");
        assert_eq!(now["FROM_FILE"], "2");
        assert!(now.values().all(|v| !v.contains("${stem.")), "{now:?}");

        // A resolved value is never replaced: the new token wins.
        token(&store, "t2");
        let mut now = env_with(&ws, "api", &none, &store);
        keep_live(&ws, stem, &mut now, &live);
        assert_eq!(now["AUTH_TOKEN"], "t2");
        assert_eq!(now["STEMS_AUTH_OUTPUT_TOKEN"], "t2");
        // A key removed from the config is not brought back.
        assert!(!now.contains_key("GONE"));
        let mut live_extra = live.clone();
        live_extra.insert("GONE".into(), "x".into());
        keep_live(&ws, stem, &mut now, &live_extra);
        assert!(!now.contains_key("GONE"));
    }

    #[test]
    fn pass_env_stems_names_are_not_drift() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join("api.env"), "FROM_FILE=1\n").unwrap();
        let ws = load_ws(&root, DEPS);
        let stem = ws.stem("api").unwrap();
        let store = super::super::outputs::OutputStore::default();
        token(&store, "t1");
        let pass: BTreeMap<String, String> =
            [("STEMS_HOME", "/h"), ("STEMS_STEM", "x"), ("K", "v")]
                .map(|(a, b)| (a.into(), b.into()))
                .into();
        let live = env_with(&ws, "api", &pass, &store);
        assert_eq!(live["STEMS_HOME"], "/h");
        // A marker always beats `--pass-env`.
        assert_eq!(live["STEMS_STEM"], "api");
        let config = layers(&ws, "api", &pass);
        assert!(drift(&ws, stem, &live, &config).is_empty());
    }

    #[test]
    fn template_matching() {
        assert!(matches_template("a", "a"));
        assert!(!matches_template("a", "b"));
        assert!(matches_template("${stem.a.port}", "123"));
        assert!(matches_template("${stem.a.port}", "${stem.a.port}"));
        assert!(matches_template(
            "http://h:${stem.a.port}/x",
            "http://h:1/x"
        ));
        assert!(!matches_template(
            "http://h:${stem.a.port}/x",
            "http://h:1/y"
        ));
        assert!(matches_template("${stem.a.port}-${stem.b.port}", "1-2"));
        assert!(!matches_template("${stem.a.port}-${stem.b.port}", "12"));
        assert!(!matches_template("ab${stem.a.port}ba", "aba"));
    }

    #[test]
    fn env_file_cache_rereads_a_changed_file() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        std::fs::write(root.join("api.env"), "A=1\n").unwrap();
        let ws = load_ws(&root, DEPS);
        let file = &ws.stem("api").unwrap().env_files[0];
        let cache = EnvFileCache::default();
        assert_eq!(
            cache.read(&root, "api", 0, file).unwrap(),
            vec![("A".into(), "1".into())]
        );
        std::fs::write(root.join("api.env"), "A=22\n").unwrap();
        assert_eq!(
            cache.read(&root, "api", 0, file).unwrap(),
            vec![("A".into(), "22".into())]
        );
        std::fs::remove_file(root.join("api.env")).unwrap();
        assert_eq!(
            cache.read(&root, "api", 0, file).unwrap_err().code,
            stems_core::ErrorCode::EnvFileNotFound
        );
    }
}
