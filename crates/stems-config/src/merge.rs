//! Deep merge of YAML trees (see the crate README for the rules).

use serde_yaml_ng::{Mapping, Value};

use crate::path::ConfigPath;

/// True for maps whose entries are replaced whole rather than merged:
/// workspace `scripts` and `stems.<n>.scripts`.
fn replaces_per_key(path: &ConfigPath) -> bool {
    let segs = path.segments().len();
    (segs == 1 && path.key_at(0) == Some("scripts"))
        || (segs == 3 && path.key_at(0) == Some("stems") && path.key_at(2) == Some("scripts"))
}

fn type_of(m: &Mapping) -> Option<&Value> {
    m.get("type")
}

/// Merge `overlay` into `base` in place.
///
/// - mapping + mapping: merged key by key, recursively; keys new in the
///   overlay are appended in overlay order;
/// - under `scripts` (workspace and per stem) each key is replaced whole;
/// - two mappings that both carry a `type` key with different values (a stem
///   or health check changing kind) are replaced, not merged;
/// - anything else (lists, scalars, null, mapping vs non-mapping): the overlay
///   replaces the base.
pub fn merge(base: &mut Value, overlay: Value, path: &ConfigPath) {
    match (base, overlay) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            if let (Some(bt), Some(ot)) = (type_of(b), type_of(&o))
                && bt != ot
            {
                *b = o;
                return;
            }
            let per_key = replaces_per_key(path);
            for (k, v) in o {
                let child = path.key(key_string(&k));
                match b.get_mut(&k) {
                    Some(existing) if !per_key => merge(existing, v, &child),
                    _ => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Render a mapping key as a path segment.
pub fn key_string(k: &Value) -> String {
    match k {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "~".into(),
        other => serde_yaml_ng::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn y(s: &str) -> Value {
        serde_yaml_ng::from_str(s).unwrap()
    }

    fn merged(base: &str, overlay: &str) -> Value {
        let mut b = y(base);
        merge(&mut b, y(overlay), &ConfigPath::root());
        b
    }

    #[test]
    fn maps_merge_recursively() {
        let m = merged(
            "stems: { api: { env: { A: '1', B: '2' }, type: process } }",
            "stems: { api: { env: { B: 'x', C: '3' } } }",
        );
        assert_eq!(
            m,
            y("stems: { api: { env: { A: '1', B: 'x', C: '3' }, type: process } }")
        );
    }

    #[test]
    fn new_keys_are_appended_in_order() {
        let m = merged("stems: { a: {}, b: {} }", "stems: { c: {}, a: { x: 1 } }");
        let keys: Vec<_> = m["stems"].as_mapping().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec![y("a"), y("b"), y("c")]);
    }

    #[test]
    fn lists_replace() {
        let m = merged(
            "stems: { api: { ports: [1, 2], depends_on: [db] } }",
            "stems: { api: { ports: [3] } }",
        );
        assert_eq!(m, y("stems: { api: { ports: [3], depends_on: [db] } }"));
    }

    #[test]
    fn stem_scripts_replace_per_key() {
        let m = merged(
            "stems: { api: { scripts: { setup: { command: a, inputs: [x] }, start: s } } }",
            "stems: { api: { scripts: { setup: { command: b }, seed: c } } }",
        );
        assert_eq!(
            m,
            y("stems: { api: { scripts: { setup: { command: b }, start: s, seed: c } } }")
        );
    }

    #[test]
    fn workspace_scripts_replace_per_key() {
        let m = merged(
            "scripts: { nuke: { file: a.sh, description: d } }",
            "scripts: { nuke: { file: b.sh } }",
        );
        assert_eq!(m, y("scripts: { nuke: { file: b.sh } }"));
    }

    #[test]
    fn scalars_and_mismatched_shapes_replace() {
        let m = merged(
            "stems: { api: { codebase: { git: 'x', ref: main }, enabled: true } }",
            "stems: { api: { codebase: ~/work/api, enabled: false } }",
        );
        assert_eq!(
            m,
            y("stems: { api: { codebase: ~/work/api, enabled: false } }")
        );
    }

    #[test]
    fn enabled_false_keeps_the_rest_of_the_stem() {
        let m = merged(
            "stems: { web: { type: process, ports: [1] } }",
            "stems: { web: { enabled: false } }",
        );
        assert_eq!(
            m,
            y("stems: { web: { type: process, ports: [1], enabled: false } }")
        );
    }

    #[test]
    fn changing_type_replaces_the_mapping() {
        let m = merged(
            "stems: { api: { type: process, health: { type: http, url: u, interval: 1s } } }",
            "stems: { api: { health: { type: tcp, port: 1 } } }",
        );
        assert_eq!(
            m,
            y("stems: { api: { type: process, health: { type: tcp, port: 1 } } }")
        );
        let m = merged(
            "stems: { api: { type: process, command: x } }",
            "stems: { api: { type: docker, image: i } }",
        );
        assert_eq!(m, y("stems: { api: { type: docker, image: i } }"));
    }

    #[test]
    fn null_overlay_clears_a_value() {
        let m = merged(
            "stems: { api: { health: { type: tcp } } }",
            "stems: { api: { health: ~ } }",
        );
        assert_eq!(m, y("stems: { api: { health: ~ } }"));
    }

    #[test]
    fn vars_and_profiles_merge_per_key() {
        let m = merged(
            "vars: { a: '1', b: '2' }\nprofiles: { default: [a, b], backend: [a] }",
            "vars: { b: '3' }\nprofiles: { default: [b] }",
        );
        assert_eq!(
            m,
            y("vars: { a: '1', b: '3' }\nprofiles: { default: [b], backend: [a] }")
        );
    }
}
