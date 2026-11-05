//! Where each field of a stem comes from (FR-ST-8): `Stem::inherited_from`,
//! shown by `stems show <stem> --json`.
//!
//! Paths are the stem's top-level fields, plus one per key for the maps
//! that merge per key: `env.<KEY>`, `scripts.<name>`, `outputs.<NAME>`.
//! Each path is attributed to the last layer that sets it — `base`
//! (`stems.yaml` with its includes/extends), `configuration`, `variant`,
//! `local` (`stems.local.yaml`) — or `removed` when the merged value is a
//! `null` removal marker ([`crate::merge::merge`]). Paths that are not in
//! the merged stem (dropped by a type change or `inherit: false`) are not
//! listed, nor are defaults no layer sets.
//!
//! The base, variant and local layers are walked as written. The
//! configuration layer is the difference the selected configuration made
//! to the committed tree (a configuration value equal to the base's counts
//! as `base`).

use std::collections::BTreeMap;

use serde_yaml_ng::Value;

use crate::merge::key_string;
use crate::variants::SELECTION_KEYS;

/// The maps reported per key.
const PER_KEY: [&str; 3] = ["env", "scripts", "outputs"];

/// Layer names, as reported.
pub const BASE: &str = "base";
/// The selected configuration.
pub const CONFIGURATION: &str = "configuration";
/// The active variant.
pub const VARIANT: &str = "variant";
/// `stems.local.yaml`.
pub const LOCAL: &str = "local";
/// A `null` removed the key.
pub const REMOVED: &str = "removed";

/// The paths a stem mapping sets, with their values.
fn fields(stem: Option<&Value>) -> BTreeMap<String, &Value> {
    let mut out = BTreeMap::new();
    let Some(Value::Mapping(m)) = stem else {
        return out;
    };
    for (k, v) in m {
        let k = key_string(k);
        if SELECTION_KEYS.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::Mapping(inner) if PER_KEY.contains(&k.as_str()) => {
                for (ik, iv) in inner {
                    out.insert(format!("{k}.{}", key_string(ik)), iv);
                }
            }
            _ => {
                out.insert(k, v);
            }
        }
    }
    out
}

/// The layers of one stem, lowest first. Each is the stem's mapping in
/// that layer (`None` when the layer does not mention the stem).
#[derive(Default)]
pub(crate) struct Layers<'a> {
    /// The committed tree's stem.
    pub base: Option<&'a Value>,
    /// The stem after the selected configuration was merged.
    pub configured: Option<&'a Value>,
    /// The active variant's definition.
    pub variant: Option<&'a Value>,
    /// The local file's stem.
    pub local: Option<&'a Value>,
    /// The merged stem (removal markers still in).
    pub merged: Option<&'a Value>,
}

/// `inherited_from` for one stem (see the module docs).
pub(crate) fn of(layers: &Layers<'_>) -> BTreeMap<String, String> {
    let base = fields(layers.base);
    // The last layer that sets each path, and whether a `null` there removed
    // a value some lower layer had (a `null` with nothing below it removes
    // nothing: the default applies, and defaults are not listed).
    let mut out: BTreeMap<String, (&'static str, bool)> = BTreeMap::new();
    let mut had: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut set = |k: String, v: &Value, layer: &'static str| {
        let removes = v.is_null() && had.contains(&k);
        if !v.is_null() {
            had.insert(k.clone());
        }
        out.insert(k, (layer, removes));
    };
    for (k, v) in &base {
        set(k.clone(), v, BASE);
    }
    for (k, v) in fields(layers.configured) {
        if base.get(&k) != Some(&v) {
            set(k, v, CONFIGURATION);
        }
    }
    for (layer, tree) in [(VARIANT, layers.variant), (LOCAL, layers.local)] {
        for (k, v) in fields(tree) {
            set(k, v, layer);
        }
    }
    let merged = fields(layers.merged);
    out.into_iter()
        .filter_map(|(k, (layer, removes))| {
            let v = merged.get(&k)?;
            let layer = match (v.is_null(), removes) {
                (false, _) => layer,
                (true, true) => REMOVED,
                (true, false) => return None,
            };
            Some((k, layer.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::merge;
    use crate::path::ConfigPath;
    use crate::variants;

    fn y(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    /// Run the layers the way `load` does and compute `inherited_from` for
    /// `stems.api`.
    fn provenance(main: &str, config: Option<&str>, local: Option<&str>) -> Vec<(String, String)> {
        let committed = y(main);
        let mut tree = committed.clone();
        if let Some(c) = config {
            merge(&mut tree, y(c), &ConfigPath::root());
        }
        let configured = tree.clone();
        let local = local.map(y);
        let mut diags = Vec::new();
        let vs = variants::apply(&mut tree, local.as_ref(), &BTreeMap::new(), &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        if let Some(l) = &local {
            merge(&mut tree, l.clone(), &ConfigPath::root());
        }
        variants::strip(&mut tree);
        let stem = |t: &Value| t.get("stems").and_then(|s| s.get("api")).cloned();
        let (b, c, m) = (stem(&committed), stem(&configured), stem(&tree));
        let l = local.as_ref().and_then(stem);
        let def = vs.get("api").and_then(|v| v.def.clone());
        of(&Layers {
            base: b.as_ref(),
            configured: c.as_ref(),
            variant: def.as_ref(),
            local: l.as_ref(),
            merged: m.as_ref(),
        })
        .into_iter()
        .collect()
    }

    fn table(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The docs example (docs/config.md#variants-fr-st-8), docker active.
    const DOCS: &str = r#"
stems:
  api:
    type: process
    codebase: ../repos/api
    command: python3 app.py
    env: { SHOP_CHAOS: "1" }
    ports: [{ name: http, port: 18080 }]
    health: { type: http, url: "http://127.0.0.1:${stem.api.port}/healthz" }
    scripts: { setup: "pip install -r requirements.txt" }
    variant: docker
    variants:
      docker:
        type: docker
        build: { context: "${codebase}" }
        ports: [{ name: http, port: 18080, container_port: 8080 }]
        env: { PORT: "8080" }
      bare:
        scripts: { setup: ~ }
        env: { SHOP_CHAOS: ~ }
"#;

    #[test]
    fn docs_example_with_the_docker_variant() {
        assert_eq!(
            provenance(
                DOCS,
                None,
                Some("stems: { api: { ports: [{ name: http, port: 1 }] } }")
            ),
            table(&[
                ("build", "variant"),
                ("codebase", "base"),
                ("env.PORT", "variant"),
                ("env.SHOP_CHAOS", "base"),
                ("health", "base"),
                ("ports", "local"),
                ("scripts.setup", "base"),
                ("type", "variant"),
            ])
        );
    }

    #[test]
    fn null_is_reported_as_removed() {
        assert_eq!(
            provenance(DOCS, None, Some("stems: { api: { variant: bare } }")),
            table(&[
                ("codebase", "base"),
                ("command", "base"),
                ("env.SHOP_CHAOS", "removed"),
                ("health", "base"),
                ("ports", "base"),
                ("scripts.setup", "removed"),
                ("type", "base"),
            ])
        );
        // A later layer setting it again wins.
        let p = provenance(
            DOCS,
            None,
            Some("stems: { api: { variant: bare, env: { SHOP_CHAOS: '2' } } }"),
        );
        assert!(
            p.contains(&("env.SHOP_CHAOS".into(), "local".into())),
            "{p:?}"
        );
    }

    /// A `null` with no value below it removes nothing: not `removed`, and
    /// not listed (the default applies).
    #[test]
    fn null_without_a_lower_value_is_not_removed() {
        let p = provenance(
            "stems: { api: { type: process, command: x, health: ~ } }",
            None,
            Some("stems: { api: { env: { NEW: ~ } } }"),
        );
        assert_eq!(p, table(&[("command", "base"), ("type", "base")]));
    }

    #[test]
    fn configuration_layer_is_what_the_configuration_changed() {
        let p = provenance(
            DOCS,
            Some(
                "stems: { api: { variant: local, env: { SHOP_CHAOS: '1', EXTRA: x }, command: other } }",
            ),
            None,
        );
        assert_eq!(
            p,
            table(&[
                ("codebase", "base"),
                ("command", "configuration"),
                ("env.EXTRA", "configuration"),
                ("env.SHOP_CHAOS", "base"),
                ("health", "base"),
                ("ports", "base"),
                ("scripts.setup", "base"),
                ("type", "base"),
            ])
        );
    }

    #[test]
    fn inherit_false_lists_only_what_the_variant_and_local_set() {
        let p = provenance(
            DOCS,
            None,
            Some(
                "stems: { api: { enabled: true, variant: alone, variants: { alone: { inherit: false, type: docker, image: i } } } }",
            ),
        );
        assert_eq!(
            p,
            table(&[
                ("enabled", "local"),
                ("image", "variant"),
                ("type", "variant")
            ])
        );
    }
}
