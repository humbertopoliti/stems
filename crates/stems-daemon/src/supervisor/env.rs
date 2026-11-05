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
//!    `required: false` entry is skipped, see `stems_core::envfiles`);
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
        .filter(|(k, _)| !k.starts_with("STEMS_"))
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
    for (k, v) in &stem.env {
        env.insert(k.clone(), v.clone());
    }
    for (i, file) in stem.env_files.iter().enumerate() {
        for (k, v) in parse_env_file(&ws.root, &stem.name, i, file)? {
            env.insert(k, v);
        }
    }
    for (k, v) in &stem.local_env {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in inp.pass_env {
        env.insert(k.clone(), v.clone());
    }
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
        .filter(|(k, _)| !k.starts_with("STEMS_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    full.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
    Ok(StemEnv { own: env, full })
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
            ("LC_ALL", "C"),
            ("SECRET", "s"),
        ]
        .map(|(a, b)| (a.into(), b.into()))
        .into();
        // No caller env: the daemon's.
        let none = CallEnv::default();
        assert_eq!(none.base(&daemon)["FOO"], "stale");
        // The caller's, minus STEMS_*.
        let call = CallEnv::new(Some(caller), BTreeMap::new());
        let base = call.base(&daemon);
        assert_eq!(base["FOO"], "fresh");
        assert!(!base.contains_key("STEMS_HOME"));
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
}
