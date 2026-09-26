//! A stem's process environment (FR-ST-4) and start-time substitution of
//! deferred `${stem.<n>.port}` references (FR-WS-10, FR-ST-5).
//!
//! Layers, later wins:
//!
//! 1. the daemon's environment minus `STEMS_*` variables;
//! 2. `PORT` = the stem's primary host port, when it declares ports;
//! 3. workspace `env` → stem `env` (already merged by the loader);
//! 4. `env_files` (`KEY=value` dotenv files, in order);
//! 5. `stems.local.yaml` env (`Stem::local_env`);
//! 6. the shell environment passed with `stems up --pass-env`;
//! 7. `STEMS_STEM`, `STEMS_WORKSPACE`, `STEMS_CODEBASE`, `STEMS_RUN_ID` and
//!    `STEMS_<DEP>_PORT` for each dependency with a port.
//!
//! Every value is then re-rendered: `${stem.<n>.port}` and
//! `${stem.<n>.ports.<p>}` left in place by the loader (auto ports) become
//! the allocated ports.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use stems_config::{Stem, Workspace};
use stems_core::{Error, ErrorCode};

use super::ports::PortBook;

/// Inputs of [`build_env`] beyond the config.
pub struct EnvInputs<'a> {
    /// The daemon's own environment (layer 1).
    pub base: &'a BTreeMap<String, String>,
    /// `--pass-env` values.
    pub pass_env: &'a BTreeMap<String, String>,
    /// This daemon run's id.
    pub run_id: &'a str,
    /// Port book (auto ports).
    pub ports: &'a PortBook,
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

/// A stem's environment: what stems sets (layers 2–7, shown by `status
/// -v`) and the full environment given to the process (layer 1 below it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StemEnv {
    /// Layers 2–7 only (never the inherited daemon environment).
    pub own: BTreeMap<String, String>,
    /// Everything the process gets.
    pub full: BTreeMap<String, String>,
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
    for file in &stem.env_files {
        for (k, v) in parse_env_file(&stem.name, file)? {
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
    for dep in &stem.depends_on {
        if let Some(p) = inp.ports.resolve_ref(ws, &dep.stem, None, allocated) {
            env.insert(port_var(&dep.stem), p.to_string());
        }
    }
    for v in env.values_mut() {
        if v.contains("${stem.") {
            *v = render_refs(v, &stem.name, &mut |n, p| {
                inp.ports.resolve_ref(ws, n, p, allocated)
            });
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

/// Parse a dotenv file: `KEY=value` lines, optional `export `, `#` comments,
/// matching single/double quotes stripped. Missing file: `START_FAILED`.
pub fn parse_env_file(stem: &str, path: &Path) -> Result<Vec<(String, String)>, Error> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::new(
            ErrorCode::StartFailed,
            format!("cannot read env file {} of `{stem}`: {e}", path.display()),
        )
        .with_hint(format!(
            "create the file or remove it from `stems.{stem}.env_files`"
        ))
        .with_details(json!({ "stem": stem, "file": path }))
    })?;
    Ok(parse_dotenv(&text))
}

/// Parse dotenv text (see [`parse_env_file`]).
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            let v = v.trim();
            let v = ['"', '\'']
                .iter()
                .find_map(|q| {
                    v.strip_prefix(*q)
                        .and_then(|x| x.strip_suffix(*q))
                        .filter(|_| v.len() >= 2)
                })
                .unwrap_or(v);
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv() {
        let got = parse_dotenv(
            "# c\n\nA=1\nexport B = two words \nC=\"quoted # x\"\nD='s'\nnoeq\n=x\nE=a=b\n",
        );
        let want = [
            ("A", "1"),
            ("B", "two words"),
            ("C", "quoted # x"),
            ("D", "s"),
            ("E", "a=b"),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        );
    }

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
    fn port_vars() {
        assert_eq!(port_var("shop-api"), "STEMS_SHOP_API_PORT");
        assert_eq!(port_var("pg"), "STEMS_PG_PORT");
    }
}
