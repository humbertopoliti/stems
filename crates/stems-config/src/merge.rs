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

/// True for `stems.<n>.ports`, whose replacement keeps `container_port`s
/// by name (see [`inherit_container_ports`]).
fn is_stem_ports(path: &ConfigPath) -> bool {
    path.segments().len() == 3 && path.key_at(0) == Some("stems") && path.key_at(2) == Some("ports")
}

/// A `ports` list replacing `base`: each `{name, …}` entry of `overlay`
/// without a `container_port` takes the one of the replaced entry with the
/// same `name`, so remapping a host port (`stems.local.yaml`, a harness)
/// keeps the container side a variant or base declared. Entries without a
/// name, renamed ports and the shorthand forms (`5432`, `"15432:5432"`)
/// inherit nothing.
fn inherit_container_ports(base: &Value, overlay: &mut Value) {
    let (Value::Sequence(b), Value::Sequence(o)) = (base, overlay) else {
        return;
    };
    let name_of = |v: &Value| v.get("name").and_then(Value::as_str).map(str::to_owned);
    for entry in o.iter_mut() {
        let Some(name) = name_of(entry) else { continue };
        let Value::Mapping(m) = entry else { continue };
        if m.contains_key("container_port") {
            continue;
        }
        let inherited = b
            .iter()
            .find(|e| name_of(e).as_deref() == Some(name.as_str()))
            .and_then(|e| e.get("container_port"))
            .filter(|c| !c.is_null());
        if let Some(c) = inherited {
            m.insert(Value::String("container_port".into()), c.clone());
        }
    }
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
///   replaces the base; a replaced `stems.<n>.ports` list passes its
///   `container_port`s on by port name ([`inherit_container_ports`]).
///
/// A `null` overlay value **removes** the key (FR-ST-8): it is kept as a
/// `null` marker while layers are merged, so that it also wins over the
/// same key of a layer merged earlier into a later one (a variant defined in
/// both `stems.yaml` and `stems.local.yaml`, a local file's includes), and
/// [`prune_nulls`] drops every marker before the typed parse. Removed and
/// never-set keys are therefore the same to the typed model (the default
/// applies), and `scripts: { setup: ~ }` drops an inherited script.
pub fn merge(base: &mut Value, mut overlay: Value, path: &ConfigPath) {
    if is_stem_ports(path) {
        inherit_container_ports(base, &mut overlay);
    }
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

/// Remove every mapping entry whose value is `null` (the removal markers
/// [`merge`] leaves), recursively. List items are kept as they are.
pub fn prune_nulls(v: &mut Value) {
    match v {
        Value::Mapping(m) => {
            m.retain(|_, v| !v.is_null());
            for (_, child) in m.iter_mut() {
                prune_nulls(child);
            }
        }
        Value::Sequence(items) => items.iter_mut().for_each(prune_nulls),
        Value::Tagged(t) => prune_nulls(&mut t.value),
        _ => {}
    }
}

/// True if some mapping entry of `v` (at any depth) is `null`.
pub fn has_nulls(v: &Value) -> bool {
    match v {
        Value::Mapping(m) => m.iter().any(|(_, v)| v.is_null() || has_nulls(v)),
        Value::Sequence(items) => items.iter().any(has_nulls),
        Value::Tagged(t) => has_nulls(&t.value),
        _ => false,
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
    fn replaced_ports_keep_container_port_by_name() {
        // A later layer remapping the host port keeps the container side.
        let m = merged(
            "stems: { api: { ports: [{ name: http, port: 18080, container_port: 8080 }, { name: admin, port: 9000, container_port: 9001 }] } }",
            "stems: { api: { ports: [{ name: http, port: 28080 }, { name: debug, port: 9000 }, 5000] } }",
        );
        assert_eq!(
            m,
            y(
                "stems: { api: { ports: [{ name: http, port: 28080, container_port: 8080 }, { name: debug, port: 9000 }, 5000] } }"
            )
        );
        // An explicit container_port wins.
        let m = merged(
            "stems: { api: { ports: [{ name: http, port: 1, container_port: 8080 }] } }",
            "stems: { api: { ports: [{ name: http, port: 2, container_port: 9090 }] } }",
        );
        assert_eq!(
            m,
            y("stems: { api: { ports: [{ name: http, port: 2, container_port: 9090 }] } }")
        );
        // Only `stems.<n>.ports` behaves this way.
        let m = merged(
            "x: { ports: [{ name: http, container_port: 1 }] }",
            "x: { ports: [{ name: http }] }",
        );
        assert_eq!(m, y("x: { ports: [{ name: http }] }"));
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

    /// `merged`, then the removal markers dropped (what the typed parse sees).
    fn pruned(layers: &[&str]) -> Value {
        let mut b = y(layers[0]);
        for l in &layers[1..] {
            merge(&mut b, y(l), &ConfigPath::root());
        }
        prune_nulls(&mut b);
        b
    }

    #[test]
    fn null_removes_the_key() {
        // A map entry, a nested field, a list-valued field, a whole map.
        assert_eq!(
            pruned(&[
                "stems: { api: { env: { A: '1', B: '2' }, health: { type: tcp, port: 1 }, watch: [{ log: x }], tags: [a] } }",
                "stems: { api: { env: { A: ~ }, health: { port: ~ }, watch: ~ } }",
            ]),
            y("stems: { api: { env: { B: '2' }, health: { type: tcp }, tags: [a] } }")
        );
        assert_eq!(
            pruned(&[
                "stems: { api: { env: { A: '1' }, type: process } }",
                "stems: { api: { env: ~ } }"
            ]),
            y("stems: { api: { type: process } }")
        );
        // Removing a key nobody set is a no-op.
        assert_eq!(
            pruned(&[
                "stems: { api: { type: process } }",
                "stems: { api: { env: { X: ~ } } }"
            ]),
            y("stems: { api: { type: process, env: {} } }")
        );
        // A later layer can set the key again.
        assert_eq!(
            pruned(&[
                "stems: { api: { env: { A: '1', B: '2' } } }",
                "stems: { api: { env: ~ } }",
                "stems: { api: { env: { C: '3' } } }",
            ]),
            y("stems: { api: { env: { C: '3' } } }")
        );
        // List items are not touched.
        assert_eq!(pruned(&["x: [1]", "x: [~, 2]"]), y("x: [~, 2]"));
    }

    #[test]
    fn null_removes_a_script_per_key() {
        assert_eq!(
            pruned(&[
                "stems: { api: { scripts: { setup: { command: a }, seed: b } } }",
                "stems: { api: { scripts: { setup: ~ } } }",
            ]),
            y("stems: { api: { scripts: { seed: b } } }")
        );
        // The marker survives a merge into a layer without the key, so a
        // pre-merged overlay (a local file with includes, a variant defined
        // twice) still removes it from the base.
        let mut overlay = y("stems: { api: { scripts: { seed: c } } }");
        merge(
            &mut overlay,
            y("stems: { api: { scripts: { setup: ~ } } }"),
            &ConfigPath::root(),
        );
        let mut base = y("stems: { api: { scripts: { setup: a, seed: b } } }");
        merge(&mut base, overlay, &ConfigPath::root());
        prune_nulls(&mut base);
        assert_eq!(base, y("stems: { api: { scripts: { seed: c } } }"));
        let mut s = y("scripts: { nuke: a, keep: b }");
        merge(&mut s, y("scripts: { nuke: ~ }"), &ConfigPath::root());
        prune_nulls(&mut s);
        assert_eq!(s, y("scripts: { keep: b }"));
    }

    #[test]
    fn null_ports_and_container_port_inheritance() {
        // `ports: ~` removes the ports; a later list inherits nothing.
        assert_eq!(
            pruned(&[
                "stems: { api: { ports: [{ name: http, port: 1, container_port: 8080 }] } }",
                "stems: { api: { ports: ~ } }",
                "stems: { api: { ports: [{ name: http, port: 2 }] } }",
            ]),
            y("stems: { api: { ports: [{ name: http, port: 2 }] } }")
        );
        // The container_port rule is untouched by pruning.
        assert_eq!(
            pruned(&[
                "stems: { api: { ports: [{ name: http, port: 1, container_port: 8080 }] } }",
                "stems: { api: { ports: [{ name: http, port: 2 }] } }",
            ]),
            y("stems: { api: { ports: [{ name: http, port: 2, container_port: 8080 }] } }")
        );
    }

    #[test]
    fn has_nulls_finds_markers_at_any_depth() {
        assert!(has_nulls(&y("a: { b: { c: ~ } }")));
        assert!(has_nulls(&y("a: [{ b: ~ }]")));
        assert!(!has_nulls(&y("a: [~, 1]")));
        assert!(!has_nulls(&y("a: { b: 1 }")));
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
