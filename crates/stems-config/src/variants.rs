//! Stem variants (FR-ST-8, `docs/config.md#variants-fr-st-8`).
//!
//! A stem may declare `variants: { <name>: { …any stem fields… } }`: partial
//! stem definitions deep-merged over the base stem when active. The active
//! variant is `stems.<n>.variant` (`stems.local.yaml` wins over `stems.yaml`);
//! absent, `local` or `base` means the base definition.
//!
//! Order: base (`stems.yaml` with its includes/extends) → active variant →
//! `stems.local.yaml`. The variant is merged with the usual rules
//! ([`crate::merge::merge`]: maps recursive, lists replace, scripts per key)
//! plus one: a variant that sets a different `type` first drops every
//! type-specific field of the base ([`TYPE_SPECIFIC_FIELDS`]), so a process
//! switching to docker keeps neither `command` nor `cwd`. Shared fields
//! (`env`, `ports`, `health`, `depends_on`, `scripts`, …) are kept. Strings
//! are substituted after the merge, so `${codebase}` in a variant is the
//! stem's codebase.
//!
//! This runs on the YAML trees before substitution; `variant`/`variants`
//! keys are removed from the merged tree afterwards ([`strip`]).

use indexmap::IndexMap;
use serde_yaml_ng::{Mapping, Value};

use crate::diagnostic::{Diagnostic, codes};
use crate::merge::{key_string, merge};
use crate::path::ConfigPath;

/// The name that selects the base definition (`stems switch <stem> local`).
pub const BASE_VARIANT: &str = "local";

/// Names that select the base definition; reserved as variant names.
pub const BASE_ALIASES: [&str; 2] = ["local", "base"];

/// True if `name` selects the base definition.
pub fn is_base(name: &str) -> bool {
    BASE_ALIASES.contains(&name)
}

/// Stem fields that only apply to some stem types (the flat
/// process/docker/compose fields of [`crate::raw::RawStem`]). A variant
/// changing `type` drops all of them from the base.
pub const TYPE_SPECIFIC_FIELDS: [&str; 15] = [
    "cwd",
    "command",
    "shell",
    "stdin",
    "image",
    "build",
    "volumes",
    "entrypoint",
    "network",
    "labels",
    "healthcheck",
    "file",
    "service",
    "project_name",
    "adopt",
];

/// A stem's variants: the active one (`None` = the base) and every name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StemVariants {
    pub active: Option<String>,
    pub names: Vec<String>,
}

fn stems_of(v: Option<&Value>) -> Option<&Mapping> {
    v?.get("stems")?.as_mapping()
}

fn stem_path(name: &str) -> ConfigPath {
    ConfigPath::root().key("stems").key(name)
}

/// The merged variant definitions of one stem: `stems.yaml`'s, with
/// `stems.local.yaml`'s merged over them (per variant, same rules).
fn definitions(name: &str, main: Option<&Value>, local: Option<&Value>) -> Mapping {
    let get = |s: Option<&Value>| -> Mapping {
        s.and_then(|s| s.get("variants"))
            .and_then(Value::as_mapping)
            .cloned()
            .unwrap_or_default()
    };
    let mut defs = get(main);
    for (k, v) in get(local) {
        match defs.get_mut(&k) {
            Some(existing) => merge(existing, v, &stem_path(name)),
            None => {
                defs.insert(k, v);
            }
        }
    }
    defs
}

/// `stems.<n>.variant`: the local file's when it sets the key (even to
/// null), else the committed one.
fn selection(main: Option<&Value>, local: Option<&Value>) -> Option<String> {
    let v = match local.and_then(Value::as_mapping) {
        Some(m) if m.contains_key("variant") => m.get("variant"),
        _ => main.and_then(|m| m.get("variant")),
    }?;
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(key_string(other)),
    }
}

/// Merge the variant `def` over the stem mapping `base` (see the module docs).
pub(crate) fn apply_variant(base: &mut Value, def: &Value, stem: &str) {
    if !base.is_mapping() {
        *base = Value::Mapping(Mapping::new());
    }
    let Value::Mapping(def) = def else { return };
    if let (Value::Mapping(b), Some(t)) = (&mut *base, def.get("type"))
        && b.get("type") != Some(t)
    {
        for f in TYPE_SPECIFIC_FIELDS {
            b.remove(f);
        }
        b.insert(Value::String("type".into()), t.clone());
    }
    let mut def = def.clone();
    def.remove("variants");
    def.remove("variant");
    merge(base, Value::Mapping(def), &stem_path(stem));
}

/// Apply every stem's active variant to `main` (the committed tree, before
/// `stems.local.yaml` is merged). Returns each stem's variants; problems
/// (`UNKNOWN_VARIANT`, reserved or nested variants) go to `diags`.
pub(crate) fn apply(
    main: &mut Value,
    local: Option<&Value>,
    diags: &mut Vec<Diagnostic>,
) -> IndexMap<String, StemVariants> {
    let mut names: Vec<String> = Vec::new();
    for m in [stems_of(Some(main)), stems_of(local)]
        .into_iter()
        .flatten()
    {
        for k in m.keys() {
            let k = key_string(k);
            if !names.contains(&k) {
                names.push(k);
            }
        }
    }
    let mut out = IndexMap::new();
    for name in names {
        let key = Value::String(name.clone());
        let main_stem = stems_of(Some(main)).and_then(|m| m.get(&key)).cloned();
        let local_stem = stems_of(local).and_then(|m| m.get(&key));
        let defs = definitions(&name, main_stem.as_ref(), local_stem);
        let p = stem_path(&name);
        let variant_names: Vec<String> = defs.keys().map(key_string).collect();
        for (k, def) in &defs {
            let vname = key_string(k);
            let vp = p.key("variants").key(&vname);
            if is_base(&vname) {
                diags.push(
                    Diagnostic::new(
                        codes::SCHEMA_INVALID,
                        format!(
                            "`{vname}` is reserved: it selects the base definition of `{name}`"
                        ),
                    )
                    .with_path(vp.clone())
                    .with_hint("rename the variant (e.g. `process`, `docker`)"),
                );
            }
            for nested in ["variants", "variant"] {
                if def.get(nested).is_some() {
                    diags.push(
                        Diagnostic::new(
                            codes::SCHEMA_INVALID,
                            format!(
                                "a variant cannot set `{nested}` (stem `{name}`, variant `{vname}`)"
                            ),
                        )
                        .with_path(vp.key(nested))
                        .with_hint(format!(
                            "select the variant with `stems.{name}.variant`, not inside a variant"
                        )),
                    );
                }
            }
        }
        let selected = selection(main_stem.as_ref(), local_stem);
        let active = match selected {
            None => None,
            Some(s) if is_base(&s) => None,
            Some(s) if variant_names.contains(&s) => Some(s),
            Some(s) => {
                let hint = if variant_names.is_empty() {
                    format!("`{name}` declares no `variants`; remove `stems.{name}.variant`")
                } else {
                    format!(
                        "variants of `{name}`: {} (or `{BASE_VARIANT}` for the base definition; see `stems switch {name}`)",
                        variant_names.join(", ")
                    )
                };
                diags.push(
                    Diagnostic::new(
                        codes::UNKNOWN_VARIANT,
                        format!("stem `{name}` has no variant `{s}`"),
                    )
                    .with_path(p.key("variant"))
                    .with_hint(hint)
                    .with_details(serde_json::json!({
                        "stem": name,
                        "variant": s,
                        "known": variant_names,
                    })),
                );
                None
            }
        };
        if let Some(v) = &active
            && let Some(def) = defs.get(v.as_str())
            && let Some(root) = main.as_mapping_mut()
        {
            let stems_key = Value::String("stems".into());
            if !matches!(root.get(&stems_key), Some(Value::Mapping(_))) {
                root.insert(stems_key.clone(), Value::Mapping(Mapping::new()));
            }
            if let Some(Value::Mapping(stems)) = root.get_mut(&stems_key) {
                if stems.get(&key).is_none() {
                    stems.insert(key.clone(), Value::Mapping(Mapping::new()));
                }
                if let Some(base) = stems.get_mut(&key) {
                    apply_variant(base, def, &name);
                }
            }
        }
        if !variant_names.is_empty() || active.is_some() {
            out.insert(
                name,
                StemVariants {
                    active,
                    names: variant_names,
                },
            );
        }
    }
    out
}

/// One choice of `stems switch <stem>`: `local` (the base) or a variant,
/// with the stem type it resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// `local` or the variant name.
    pub name: String,
    /// The `type` the stem has with this choice (`None` if unset).
    pub kind: Option<String>,
}

/// The choices of `stem` (`local` first, then its variants in declaration
/// order) from the committed tree and the local file's tree.
pub fn choices(main: &Value, local: Option<&Value>, stem: &str) -> Vec<Choice> {
    let key = Value::String(stem.to_string());
    let main_stem = stems_of(Some(main)).and_then(|m| m.get(&key));
    let local_stem = stems_of(local).and_then(|m| m.get(&key));
    let type_of = |v: Option<&Value>| v.and_then(|v| v.get("type")).map(key_string);
    let base = type_of(local_stem).or_else(|| type_of(main_stem));
    let mut out = vec![Choice {
        name: BASE_VARIANT.to_string(),
        kind: base.clone(),
    }];
    for (k, def) in definitions(stem, main_stem, local_stem) {
        out.push(Choice {
            name: key_string(&k),
            kind: type_of(Some(&def)).or_else(|| base.clone()),
        });
    }
    out
}

/// The variant `stems.yaml` selects for `stem` (its committed default).
pub fn committed_selection(main: &Value, stem: &str) -> Option<String> {
    let key = Value::String(stem.to_string());
    selection(stems_of(Some(main)).and_then(|m| m.get(&key)), None)
}

/// Remove `variant` / `variants` from every stem of the merged tree.
pub(crate) fn strip(merged: &mut Value) {
    let Some(Value::Mapping(stems)) = merged.get_mut("stems") else {
        return;
    };
    for (_, s) in stems.iter_mut() {
        if let Value::Mapping(m) = s {
            m.remove("variant");
            m.remove("variants");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    /// base → variant → local, then strip: the effective `stems.api`.
    fn effective(main: &str, local: Option<&str>) -> (Value, Vec<Diagnostic>) {
        let mut m = y(main);
        let l = local.map(y);
        let mut diags = Vec::new();
        apply(&mut m, l.as_ref(), &mut diags);
        if let Some(l) = l {
            merge(&mut m, l, &ConfigPath::root());
        }
        strip(&mut m);
        (m["stems"]["api"].clone(), diags)
    }

    const BASE: &str = r#"
stems:
  api:
    type: process
    description: the api
    codebase: ../api
    command: python3 app.py
    cwd: src
    env: { A: "1", B: "2" }
    ports: [{ name: http, port: 18080 }]
    health: { type: tcp, interval: 200ms }
    depends_on: [db]
    tags: [x]
    scripts: { start: "python3 app.py", seed: "echo seed", lint: { command: "echo lint", timeout: 5s } }
    variants:
      docker:
        type: docker
        build: { context: "${codebase}" }
        ports: [{ name: http, port: 18080, container_port: 8080 }]
        env: { PORT: "8080" }
      slow:
        env: { SLOW: "1" }
        scripts: { lint: "echo fast lint" }
"#;

    fn with(sel: &str) -> String {
        BASE.replacen(
            "    variants:\n",
            &format!("    variant: {sel}\n    variants:\n"),
            1,
        )
    }

    /// The merge semantics, as a table: (selection in stems.yaml, local
    /// file, expected fields of the effective stem).
    #[test]
    fn merge_semantics_table() {
        type Check = (&'static str, &'static str);
        let table: &[(Option<&str>, Option<&str>, &[Check])] = &[
            // No selection: the base, variants stripped.
            (
                None,
                None,
                &[
                    ("type", "process"),
                    ("command", "python3 app.py"),
                    ("env", "{A: '1', B: '2'}"),
                ],
            ),
            // `local` / `base` select the base.
            (Some("local"), None, &[("type", "process"), ("cwd", "src")]),
            (Some("base"), None, &[("type", "process")]),
            // A type change drops the process fields and keeps shared ones;
            // lists replace; maps merge; `${codebase}` is left for substitution.
            (
                Some("docker"),
                None,
                &[
                    ("type", "docker"),
                    ("command", "~"),
                    ("cwd", "~"),
                    ("build", "{context: '${codebase}'}"),
                    ("ports", "[{name: http, port: 18080, container_port: 8080}]"),
                    ("env", "{A: '1', B: '2', PORT: '8080'}"),
                    ("codebase", "../api"),
                    ("description", "the api"),
                    ("depends_on", "[db]"),
                    ("tags", "[x]"),
                    ("health", "{type: tcp, interval: 200ms}"),
                    ("variant", "~"),
                    ("variants", "~"),
                ],
            ),
            // Same type: a plain deep merge; scripts replace per key.
            (
                Some("slow"),
                None,
                &[
                    ("type", "process"),
                    ("command", "python3 app.py"),
                    ("env", "{A: '1', B: '2', SLOW: '1'}"),
                    (
                        "scripts",
                        "{start: python3 app.py, seed: echo seed, lint: echo fast lint}",
                    ),
                ],
            ),
            // The local file still wins over the variant; remapping only the
            // host `port` keeps the variant's `container_port` (by name).
            (
                Some("docker"),
                Some("stems: { api: { env: { PORT: '9090' }, ports: [{ name: http, port: 1 }] } }"),
                &[
                    ("type", "docker"),
                    ("env", "{A: '1', B: '2', PORT: '9090'}"),
                    ("ports", "[{name: http, port: 1, container_port: 8080}]"),
                ],
            ),
            // A local `container_port` wins over the variant's.
            (
                Some("docker"),
                Some("stems: { api: { ports: [{ name: http, port: 1, container_port: 9000 }] } }"),
                &[("ports", "[{name: http, port: 1, container_port: 9000}]")],
            ),
            // A renamed port inherits nothing.
            (
                Some("docker"),
                Some("stems: { api: { ports: [{ name: web, port: 1 }] } }"),
                &[("ports", "[{name: web, port: 1}]")],
            ),
            // Without the variant there is no container_port to keep.
            (
                None,
                Some("stems: { api: { ports: [{ name: http, port: 1 }] } }"),
                &[("ports", "[{name: http, port: 1}]")],
            ),
            // The local selection wins over the committed default...
            (
                Some("slow"),
                Some("stems: { api: { variant: docker } }"),
                &[("type", "docker")],
            ),
            // ...including `local` (and null) back to the base.
            (
                Some("docker"),
                Some("stems: { api: { variant: local } }"),
                &[("type", "process")],
            ),
            (
                Some("docker"),
                Some("stems: { api: { variant: ~ } }"),
                &[("type", "process")],
            ),
            // A variant defined or extended in the local file.
            (
                None,
                Some("stems: { api: { variant: slow, variants: { slow: { env: { C: '3' } } } } }"),
                &[("env", "{A: '1', B: '2', SLOW: '1', C: '3'}")],
            ),
        ];
        for (sel, local, checks) in table {
            let main = sel.map_or_else(|| BASE.to_string(), with);
            let (api, diags) = effective(&main, *local);
            assert!(diags.is_empty(), "{sel:?} {local:?}: {diags:?}");
            for (field, want) in *checks {
                let got = api.get(*field).cloned().unwrap_or(Value::Null);
                assert_eq!(got, y(want), "{sel:?} {local:?}: field `{field}`");
            }
        }
    }

    #[test]
    fn unknown_variant_is_reported_and_the_base_used() {
        let (api, diags) = effective(&with("nope"), None);
        assert_eq!(api["type"], y("process"));
        assert_eq!(diags.len(), 1, "{diags:?}");
        let d = &diags[0];
        assert_eq!(d.code, codes::UNKNOWN_VARIANT);
        assert_eq!(d.path.as_ref().unwrap().to_string(), "stems.api.variant");
        assert!(d.hint.as_ref().unwrap().contains("docker, slow"), "{d:?}");
        let (_, diags) = effective(
            "stems: { api: { type: process, command: x } }",
            Some("stems: { api: { variant: docker } }"),
        );
        assert_eq!(diags[0].code, codes::UNKNOWN_VARIANT);
        assert!(diags[0].hint.as_ref().unwrap().contains("declares no"));
    }

    #[test]
    fn reserved_and_nested_variants_are_schema_errors() {
        let (_, diags) = effective(
            "stems: { api: { type: process, variants: { local: {}, d: { variant: x } } } }",
            None,
        );
        let paths: Vec<String> = diags
            .iter()
            .map(|d| format!("{} {}", d.code, d.path.as_ref().unwrap()))
            .collect();
        assert_eq!(
            paths,
            [
                "SCHEMA_INVALID stems.api.variants.local",
                "SCHEMA_INVALID stems.api.variants.d.variant",
            ]
        );
    }

    #[test]
    fn apply_reports_names_and_active() {
        let mut m = y(&with("docker"));
        let mut diags = Vec::new();
        let r = apply(&mut m, None, &mut diags);
        assert_eq!(
            r["api"],
            StemVariants {
                active: Some("docker".into()),
                names: vec!["docker".into(), "slow".into()],
            }
        );
    }

    #[test]
    fn choices_list_local_then_variants_with_their_types() {
        let main = y(&with("slow"));
        let c = choices(&main, None, "api");
        let got: Vec<(String, Option<String>)> = c.into_iter().map(|c| (c.name, c.kind)).collect();
        assert_eq!(
            got,
            [
                ("local".to_string(), Some("process".to_string())),
                ("docker".to_string(), Some("docker".to_string())),
                ("slow".to_string(), Some("process".to_string())),
            ]
        );
        assert_eq!(committed_selection(&main, "api").as_deref(), Some("slow"));
        assert_eq!(committed_selection(&y(BASE), "api"), None);
    }

    /// Keep [`TYPE_SPECIFIC_FIELDS`] in step with the fields the resolver
    /// checks per type.
    #[test]
    fn type_specific_fields_match_the_resolver() {
        let mut names = crate::resolve::type_specific_field_names();
        names.sort_unstable();
        let mut ours = TYPE_SPECIFIC_FIELDS.to_vec();
        ours.sort_unstable();
        assert_eq!(names, ours);
    }
}
