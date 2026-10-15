//! Named configurations (`docs/config.md#configurations`).
//!
//! `configurations: { <name>: <overlay> | <path> }` declares overlays with
//! the shape of `stems.local.yaml` (any workspace-level key, partial
//! `stems.<n>` definitions, `stems.<n>.variant`, `enabled: false`, …),
//! inline or in a separate YAML file (path relative to the file naming it;
//! [`crate::loader::Loader`] makes it absolute). `default_configuration`
//! picks one for everybody; `configuration:` (meant for `stems.local.yaml`)
//! wins; `none` (or `null` in the local file) selects no configuration even
//! when a default exists.
//!
//! Order: base (`stems.yaml` with its includes/extends) → the selected
//! configuration → the active variants → `stems.local.yaml`. The overlay is
//! merged with the usual rules ([`crate::merge::merge`]) before
//! [`crate::variants::apply`], so a `stems.<n>.variant` it sets selects the
//! variant (the local file still wins). The three keys are removed from the
//! merged tree afterwards ([`strip`]).

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_yaml_ng::{Mapping, Value};

use crate::diagnostic::{Diagnostic, codes};
use crate::loader::Loader;
use crate::merge::{key_string, merge};
use crate::model::{ConfigurationInfo, ConfigurationSource};
use crate::path::ConfigPath;

/// The name that selects no configuration (`stems configuration use none`).
pub const NONE: &str = "none";

/// True if `name` selects no configuration.
pub fn is_none(name: &str) -> bool {
    name == NONE
}

/// Keys an overlay may not set (they select or declare configurations, or
/// pull in files).
const NESTED: [&str; 5] = [
    "configuration",
    "configurations",
    "default_configuration",
    "include",
    "extends",
];

/// What [`apply`] decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Selected {
    /// The active configuration (`None`: the base config).
    pub active: Option<String>,
    /// `default_configuration`, when set to a name.
    pub default: Option<String>,
    /// Every declared configuration, in declaration order.
    pub infos: Vec<ConfigurationInfo>,
    /// The active configuration is inline: its spans under
    /// `configurations.<name>` are the root's.
    pub inline: Option<String>,
}

fn cfg_path(name: &str) -> ConfigPath {
    ConfigPath::root().key("configurations").key(name)
}

/// The declared configurations: `stems.yaml`'s (with its includes /
/// extends), with the local file's added or replacing them.
fn definitions(main: &Value, local: Option<&Value>) -> IndexMap<String, Value> {
    let mut defs: IndexMap<String, Value> = IndexMap::new();
    for tree in [Some(main), local].into_iter().flatten() {
        if let Some(Value::Mapping(m)) = tree.get("configurations") {
            for (k, v) in m {
                defs.insert(key_string(k), v.clone());
            }
        }
    }
    defs
}

fn info(name: &str, def: &Value) -> ConfigurationInfo {
    let source = match def {
        Value::String(p) => ConfigurationSource::File(PathBuf::from(p)),
        _ => ConfigurationSource::Inline,
    };
    ConfigurationInfo {
        name: name.to_string(),
        source,
    }
}

/// Every declared configuration (file paths absolute), in declaration
/// order, from the committed tree and the local file's tree.
pub fn declared(main: &Value, local: Option<&Value>) -> Vec<ConfigurationInfo> {
    definitions(main, local)
        .iter()
        .map(|(k, v)| info(k, v))
        .collect()
}

/// A selecting value: `null` / `none` select nothing.
fn name_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) if is_none(s) => None,
        Value::String(s) => Some(s.clone()),
        other => Some(key_string(other)),
    }
}

/// The key `key` of `tree`, if `tree` sets it (even to null).
fn key<'v>(tree: Option<&'v Value>, key: &str) -> Option<&'v Value> {
    tree?.as_mapping()?.get(key)
}

/// The effective selection and the key it comes from: `configuration` (the
/// local file's when it sets the key, else the committed one), else
/// `default_configuration` (same precedence).
fn selection(main: &Value, local: Option<&Value>) -> Option<(Option<String>, ConfigPath)> {
    for k in ["configuration", "default_configuration"] {
        let Some(v) = key(local, k).or_else(|| key(Some(main), k)) else {
            continue;
        };
        if matches!(v, Value::Null) && k == "default_configuration" {
            continue;
        }
        return Some((name_of(v), ConfigPath::root().key(k)));
    }
    None
}

/// What `stems.yaml` selects on its own (`configuration`, else
/// `default_configuration`; `None` for nothing or `none`).
pub fn committed_selection(main: &Value) -> Option<String> {
    selection(main, None).and_then(|(n, _)| n)
}

/// `default_configuration` (the local file's wins), if it names one.
pub fn default_of(main: &Value, local: Option<&Value>) -> Option<String> {
    key(local, "default_configuration")
        .or_else(|| key(Some(main), "default_configuration"))
        .and_then(name_of)
}

fn unknown(name: &str, at: ConfigPath, known: &[String]) -> Diagnostic {
    let hint = if known.is_empty() {
        format!("no `configurations` are declared; remove `{at}`")
    } else {
        format!(
            "configurations: {} (or `{NONE}` for the base config; see `stems configuration list`)",
            known.join(", ")
        )
    };
    Diagnostic::new(
        codes::UNKNOWN_CONFIGURATION,
        format!("no configuration named `{name}`"),
    )
    .with_path(at)
    .with_hint(hint)
    .with_details(serde_json::json!({
        "configuration": name,
        "known": known,
    }))
}

/// `SCHEMA_INVALID` for every key of [`NESTED`] the overlay `m` sets.
fn reject_nested(m: &Mapping, name: &str, file: Option<&Path>, diags: &mut Vec<Diagnostic>) {
    for k in NESTED {
        if m.contains_key(k) {
            let (path, what) = match file {
                Some(f) => (cfg_path(name), format!(" ({})", f.display())),
                None => (cfg_path(name).key(k), String::new()),
            };
            diags.push(
                Diagnostic::new(
                    codes::SCHEMA_INVALID,
                    format!("a configuration cannot set `{k}` (configuration `{name}`{what})"),
                )
                .with_path(path)
                .with_hint(format!(
                    "select a configuration with `configuration: {name}` (stems.local.yaml), not inside one"
                )),
            );
        }
    }
}

/// Merge the selected configuration over `main` (the committed tree, before
/// variants and `stems.local.yaml`). A file-form overlay is read through
/// `loader` (so it is a source and has spans); a missing one is a fatal
/// `CONFIGURATION_NOT_FOUND` in `loader.errors`. Problems
/// (`UNKNOWN_CONFIGURATION`, reserved names, nested keys) go to `diags`.
pub(crate) fn apply(
    main: &mut Value,
    local: Option<&Value>,
    loader: &mut Loader,
    diags: &mut Vec<Diagnostic>,
) -> Selected {
    let defs = definitions(main, local);
    let known: Vec<String> = defs.keys().cloned().collect();
    let mut out = Selected {
        infos: defs.iter().map(|(k, v)| info(k, v)).collect(),
        default: default_of(main, local),
        ..Default::default()
    };
    for (name, def) in &defs {
        if is_none(name) {
            diags.push(
                Diagnostic::new(
                    codes::SCHEMA_INVALID,
                    format!("`{NONE}` is reserved: it selects no configuration"),
                )
                .with_path(cfg_path(name))
                .with_hint("rename the configuration (e.g. `ci`, `docker`)"),
            );
        }
        if let Value::Mapping(m) = def {
            reject_nested(m, name, None, diags);
        }
    }
    let Some((selected, at)) = selection(main, local) else {
        return out;
    };
    let Some(name) = selected else {
        return out;
    };
    if let Some(d) = &out.default
        && d != &name
        && !known.contains(d)
    {
        diags.push(unknown(
            d,
            ConfigPath::root().key("default_configuration"),
            &known,
        ));
    }
    let Some(def) = defs.get(&name) else {
        diags.push(unknown(&name, at, &known));
        return out;
    };
    let overlay = match def {
        Value::String(p) => {
            let file = PathBuf::from(p);
            if !file.is_file() {
                loader.errors.push(
                    Diagnostic::new(
                        codes::CONFIGURATION_NOT_FOUND,
                        format!(
                            "configuration `{name}`: {} not found",
                            file.display()
                        ),
                    )
                    .with_path(cfg_path(&name))
                    .with_hint(
                        "fix the path (it is relative to the file that names it), create the file, or select another configuration",
                    )
                    .with_details(serde_json::json!({
                        "configuration": name,
                        "file": file,
                    })),
                );
                return out;
            }
            let Some(v) = loader.load_overlay(&file, None) else {
                return out;
            };
            if let Value::Mapping(m) = &v {
                reject_nested(m, &name, Some(&file), diags);
            }
            v
        }
        Value::Mapping(m) => {
            let mut m = m.clone();
            for k in NESTED {
                m.remove(k);
            }
            out.inline = Some(name.clone());
            Value::Mapping(m)
        }
        _ => return out,
    };
    merge(main, overlay, &ConfigPath::root());
    out.active = Some(name);
    out
}

/// Remove `configurations`, `default_configuration` and `configuration`
/// from the merged tree.
pub(crate) fn strip(merged: &mut Value) {
    if let Value::Mapping(m) = merged {
        for k in ["configurations", "default_configuration", "configuration"] {
            m.remove(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    const BASE: &str = r#"
default_configuration: ci
configurations:
  ci:
    stems: { api: { variant: slow, env: { CI: "1" } } }
  docker: { stems: { api: { variant: docker } } }
stems:
  api:
    type: process
    command: python3 app.py
    variants:
      slow: { env: { SLOW: "1" } }
      docker: { type: docker, image: api }
"#;

    fn run(main: &str, local: Option<&str>) -> (Value, Selected, Vec<Diagnostic>) {
        let mut m = y(main);
        let l = local.map(y);
        let mut loader = Loader::default();
        let mut diags = Vec::new();
        let s = apply(&mut m, l.as_ref(), &mut loader, &mut diags);
        assert!(loader.errors.is_empty(), "{:?}", loader.errors);
        (m, s, diags)
    }

    #[test]
    fn default_then_local_then_none() {
        let (m, s, d) = run(BASE, None);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(s.active.as_deref(), Some("ci"));
        assert_eq!(s.default.as_deref(), Some("ci"));
        assert_eq!(m["stems"]["api"]["variant"], y("slow"));
        assert_eq!(s.inline.as_deref(), Some("ci"));
        let (m, s, _) = run(BASE, Some("configuration: docker"));
        assert_eq!(s.active.as_deref(), Some("docker"));
        assert_eq!(m["stems"]["api"]["variant"], y("docker"));
        for local in ["configuration: none", "configuration: ~"] {
            let (m, s, d) = run(BASE, Some(local));
            assert!(d.is_empty(), "{d:?}");
            assert_eq!(s.active, None);
            assert_eq!(m["stems"]["api"].get("variant"), None);
        }
        let names: Vec<&str> = s.infos.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["ci", "docker"]);
        assert_eq!(committed_selection(&y(BASE)).as_deref(), Some("ci"));
    }

    #[test]
    fn unknown_selection_is_reported_and_the_base_used() {
        let (m, s, d) = run(BASE, Some("configuration: nope"));
        assert_eq!(s.active, None);
        assert_eq!(m["stems"]["api"].get("variant"), None);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].code, codes::UNKNOWN_CONFIGURATION);
        assert_eq!(d[0].path.as_ref().unwrap().to_string(), "configuration");
        assert!(d[0].hint.as_ref().unwrap().contains("ci, docker"));
        let (_, _, d) = run("default_configuration: x\nstems: {}", None);
        assert_eq!(d[0].code, codes::UNKNOWN_CONFIGURATION);
        assert_eq!(
            d[0].path.as_ref().unwrap().to_string(),
            "default_configuration"
        );
    }

    #[test]
    fn reserved_and_nested_keys_are_schema_errors() {
        let (_, _, d) = run(
            "configurations: { none: {}, a: { configuration: b, include: [x] } }",
            None,
        );
        let paths: Vec<String> = d
            .iter()
            .map(|d| format!("{} {}", d.code, d.path.as_ref().unwrap()))
            .collect();
        assert_eq!(
            paths,
            [
                "SCHEMA_INVALID configurations.none",
                "SCHEMA_INVALID configurations.a.configuration",
                "SCHEMA_INVALID configurations.a.include",
            ]
        );
    }

    #[test]
    fn strip_removes_the_keys() {
        let mut m = y(BASE);
        strip(&mut m);
        let m = m.as_mapping().unwrap();
        assert!(!m.contains_key("configurations"));
        assert!(!m.contains_key("default_configuration"));
        assert!(m.contains_key("stems"));
    }
}
