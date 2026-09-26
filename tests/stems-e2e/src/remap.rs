//! Port remapping of copied workspaces.
//!
//! Every scenario gets its own block of TCP ports so scenarios can run in
//! parallel. The harness never edits the copied `stems.yaml`; instead it
//! generates a `stems.local.yaml` (the documented per-developer overlay,
//! FR-WS-4) that overrides every numeric port the workspace declares.
//!
//! Rules (kept deliberately small and predictable):
//! - The *declared ports* are every numeric `stems.<name>.ports[*].port`
//!   (numbers or numeric strings). `port: auto` is left untouched.
//! - Declared ports are sorted and mapped to `base, base + 1, ...`. The same
//!   number declared twice maps to the same new port, so static conflicts
//!   (`PORT_CONFLICT`) are preserved.
//! - For every stem that mentions a declared port, the generated overlay
//!   contains: the full `ports` array with the new numbers (other fields such
//!   as `name` and `container_port` are copied unchanged, because arrays are
//!   replaced on merge); every `env` value that *equals* a declared port
//!   (`PORT: "18080"`, `SHOP_CONTROL_PORT: "18081"`) or contains it as a
//!   `:<port>` URL segment; `health.port`; and `health.url` `:<port>`
//!   segments. Workspace-level `env` gets the same env treatment.
//! - Container-side ports (`container_port`) are never touched.

use std::collections::{BTreeMap, BTreeSet};

use serde_yaml_ng::{Mapping, Value};

/// The outcome of remapping one workspace document.
#[derive(Debug, Clone, PartialEq)]
pub struct Remap {
    /// Old port -> new port.
    pub map: BTreeMap<u16, u16>,
    /// The overlay to write as `stems.local.yaml` (may be empty).
    pub local: Mapping,
}

/// Parses a port value: a YAML integer, or a string holding only digits.
fn as_port(v: &Value) -> Option<u16> {
    match v {
        Value::Number(n) => n
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|p| *p > 0),
        Value::String(s) => s.trim().parse::<u16>().ok().filter(|p| *p > 0),
        _ => None,
    }
}

fn key(s: &str) -> Value {
    Value::String(s.to_owned())
}

/// Every numeric port declared under `stems.*.ports[*].port`.
pub fn declared_ports(doc: &Value) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    let Some(stems) = doc.get("stems").and_then(Value::as_mapping) else {
        return out;
    };
    for (_, stem) in stems {
        let Some(ports) = stem.get("ports").and_then(Value::as_sequence) else {
            continue;
        };
        for item in ports {
            let port = match item {
                Value::Mapping(m) => m.get(key("port")).and_then(as_port),
                other => as_port(other),
            };
            if let Some(p) = port {
                out.insert(p);
            }
        }
    }
    out
}

/// Replaces `:<old>` segments (not followed by another digit) in `s`.
pub fn replace_port_refs(s: &str, map: &BTreeMap<u16, u16>) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b':' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start
                && let Ok(p) = s[start..end].parse::<u16>()
                && let Some(new) = map.get(&p)
            {
                out.push(':');
                out.push_str(&new.to_string());
                i = end;
                continue;
            }
        }
        // Push the whole UTF-8 char starting at i.
        let ch = s[i..].chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8().max(1);
    }
    out
}

/// Remaps one scalar env/health value; `None` when unchanged.
fn remap_scalar(v: &Value, map: &BTreeMap<u16, u16>) -> Option<Value> {
    match v {
        Value::Number(_) => {
            let p = as_port(v)?;
            map.get(&p).map(|n| Value::Number((*n).into()))
        }
        Value::String(s) => {
            if let Ok(p) = s.trim().parse::<u16>() {
                return map.get(&p).map(|n| Value::String(n.to_string()));
            }
            let replaced = replace_port_refs(s, map);
            (replaced != *s).then_some(Value::String(replaced))
        }
        _ => None,
    }
}

fn remap_env(env: &Mapping, map: &BTreeMap<u16, u16>) -> Mapping {
    let mut out = Mapping::new();
    for (k, v) in env {
        if let Some(new) = remap_scalar(v, map) {
            out.insert(k.clone(), new);
        }
    }
    out
}

fn remap_ports_seq(ports: &[Value], map: &BTreeMap<u16, u16>) -> Option<Value> {
    let mut changed = false;
    let new: Vec<Value> = ports
        .iter()
        .map(|item| match item {
            Value::Mapping(m) => {
                let mut m = m.clone();
                if let Some(p) = m.get(key("port")).and_then(as_port)
                    && let Some(n) = map.get(&p)
                {
                    m.insert(key("port"), Value::Number((*n).into()));
                    changed = true;
                }
                Value::Mapping(m)
            }
            other => match as_port(other).and_then(|p| map.get(&p)) {
                Some(n) => {
                    changed = true;
                    Value::Number((*n).into())
                }
                None => other.clone(),
            },
        })
        .collect();
    changed.then_some(Value::Sequence(new))
}

fn remap_health(health: &Mapping, map: &BTreeMap<u16, u16>) -> Mapping {
    let mut out = Mapping::new();
    if let Some(p) = health.get(key("port")).and_then(as_port)
        && let Some(n) = map.get(&p)
    {
        out.insert(key("port"), Value::Number((*n).into()));
    }
    if let Some(Value::String(url)) = health.get(key("url")) {
        let new = replace_port_refs(url, map);
        if new != *url {
            out.insert(key("url"), Value::String(new));
        }
    }
    out
}

/// Computes the port map and the overlay for a workspace YAML document.
///
/// `base` is the first port of the scenario's block and `capacity` the block
/// size; more distinct declared ports than `capacity` is an error.
pub fn remap_ports(yaml: &str, base: u16, capacity: u16) -> Result<Remap, String> {
    let doc: Value = serde_yaml_ng::from_str(yaml).map_err(|e| format!("invalid YAML: {e}"))?;
    let ports = declared_ports(&doc);
    if ports.len() > usize::from(capacity) {
        return Err(format!(
            "workspace declares {} distinct ports but the per-scenario block holds {capacity}",
            ports.len()
        ));
    }
    let map: BTreeMap<u16, u16> = ports
        .iter()
        .zip(base..)
        .map(|(old, new)| (*old, new))
        .collect();

    let mut local = Mapping::new();
    if map.is_empty() {
        return Ok(Remap { map, local });
    }

    if let Some(env) = doc.get("env").and_then(Value::as_mapping) {
        let env = remap_env(env, &map);
        if !env.is_empty() {
            local.insert(key("env"), Value::Mapping(env));
        }
    }

    let mut stems_out = Mapping::new();
    if let Some(stems) = doc.get("stems").and_then(Value::as_mapping) {
        for (name, stem) in stems {
            let Some(stem) = stem.as_mapping() else {
                continue;
            };
            let mut out = Mapping::new();
            if let Some(seq) = stem.get(key("ports")).and_then(Value::as_sequence)
                && let Some(new) = remap_ports_seq(seq, &map)
            {
                out.insert(key("ports"), new);
            }
            if let Some(env) = stem.get(key("env")).and_then(Value::as_mapping) {
                let env = remap_env(env, &map);
                if !env.is_empty() {
                    out.insert(key("env"), Value::Mapping(env));
                }
            }
            if let Some(health) = stem.get(key("health")).and_then(Value::as_mapping) {
                let health = remap_health(health, &map);
                if !health.is_empty() {
                    out.insert(key("health"), Value::Mapping(health));
                }
            }
            if !out.is_empty() {
                stems_out.insert(name.clone(), Value::Mapping(out));
            }
        }
    }
    if !stems_out.is_empty() {
        local.insert(key("stems"), Value::Mapping(stems_out));
    }
    Ok(Remap { map, local })
}

/// Sets `dotted.path=value` inside `root`, creating intermediate mappings.
/// `raw` is parsed as a YAML scalar/flow value (`false`, `18080`, `"x"`,
/// `[a, b]`); anything unparsable is kept as a string.
pub fn set_dotted(root: &mut Mapping, dotted: &str, raw: &str) -> Result<(), String> {
    let value: Value =
        serde_yaml_ng::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
    let segments: Vec<&str> = dotted.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(format!("invalid dotted path {dotted:?}"));
    }
    let mut cur = root;
    for seg in &segments[..segments.len() - 1] {
        let entry = cur
            .entry(key(seg))
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !entry.is_mapping() {
            *entry = Value::Mapping(Mapping::new());
        }
        cur = entry
            .as_mapping_mut()
            .ok_or_else(|| format!("cannot descend into {seg:?}"))?;
    }
    cur.insert(key(segments[segments.len() - 1]), value);
    Ok(())
}

/// Renders the overlay as the text of a `stems.local.yaml`.
pub fn render_local(local: &Mapping) -> String {
    let body = serde_yaml_ng::to_string(&Value::Mapping(local.clone())).unwrap_or_default();
    format!(
        "# Generated by the stems e2e harness (tests/stems-e2e): per-scenario port\n\
         # remapping and local overrides. Do not commit.\n{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Mapping {
        serde_yaml_ng::from_str(s).expect("valid test yaml")
    }

    #[test]
    fn minimal_workspace_is_remapped() {
        let input = r#"
schema_version: 1
name: minimal
stems:
  echo-svc:
    type: process
    codebase: ../../repos/shop-api
    env:
      PORT: "18090"
      SHOP_CHAOS: "1"
    ports: [{ name: http, port: 18090 }]
    health:
      type: tcp
      port: 18090
      interval: 200ms
"#;
        let r = remap_ports(input, 20000, 20).unwrap();
        assert_eq!(r.map, BTreeMap::from([(18090, 20000)]));
        let expected = yaml(
            r#"
stems:
  echo-svc:
    ports: [{ name: http, port: 20000 }]
    env:
      PORT: "20000"
    health:
      port: 20000
"#,
        );
        assert_eq!(r.local, expected);
    }

    #[test]
    fn hello_shop_like_workspace_keeps_auto_container_ports_and_substitutions() {
        let input = r#"
env:
  GLOBAL_API: "http://localhost:18080/api"
stems:
  postgres:
    type: docker
    ports: [{ name: pg, port: 15432, container_port: 5432 }]
    env: { POSTGRES_DB: shop }
  shop-api:
    env:
      PORT: "18080"
      REDIS_URL: "redis://localhost:${stem.redis.port}"
      NOT_A_PORT: "1808"
    ports: [{ name: http, port: 18080 }]
    health:
      type: http
      url: "http://localhost:${stem.shop-api.port}/healthz"
  shop-worker:
    env:
      SHOP_CONTROL_PORT: 18081
    ports: [{ name: control, port: "18081" }]
    health: { type: tcp, port: 18081 }
  shop-web:
    ports: [{ name: http, port: auto }]
    health:
      type: http
      url: "http://localhost:18080/"
  httpbin:
    type: external
    health: { type: http, url: "https://httpbin.org/get" }
"#;
        let r = remap_ports(input, 30000, 20).unwrap();
        assert_eq!(
            r.map,
            BTreeMap::from([(15432, 30000), (18080, 30001), (18081, 30002)])
        );
        let expected = yaml(
            r#"
env:
  GLOBAL_API: "http://localhost:30001/api"
stems:
  postgres:
    ports: [{ name: pg, port: 30000, container_port: 5432 }]
  shop-api:
    ports: [{ name: http, port: 30001 }]
    env:
      PORT: "30001"
  shop-worker:
    ports: [{ name: control, port: 30002 }]
    env:
      SHOP_CONTROL_PORT: 30002
    health:
      port: 30002
  shop-web:
    health:
      url: "http://localhost:30001/"
"#,
        );
        assert_eq!(r.local, expected);
    }

    #[test]
    fn duplicate_declarations_map_to_the_same_port() {
        let input = r#"
stems:
  a: { ports: [{ port: 5432 }] }
  b: { ports: [{ port: 5432 }] }
"#;
        let r = remap_ports(input, 21000, 20).unwrap();
        assert_eq!(r.map.len(), 1);
        let local = serde_yaml_ng::to_string(&r.local).unwrap();
        assert_eq!(local.matches("21000").count(), 2, "{local}");
    }

    #[test]
    fn no_ports_means_empty_overlay() {
        let r = remap_ports("stems:\n  x: { type: external }\n", 20000, 20).unwrap();
        assert!(r.map.is_empty());
        assert!(r.local.is_empty());
    }

    #[test]
    fn too_many_ports_is_an_error() {
        let input = "stems:\n  a: { ports: [{port: 1}, {port: 2}, {port: 3}] }\n";
        let err = remap_ports(input, 20000, 2).unwrap_err();
        assert!(err.contains("3 distinct ports"), "{err}");
    }

    #[test]
    fn replace_port_refs_respects_digit_boundaries() {
        let map = BTreeMap::from([(1808, 2000)]);
        assert_eq!(
            replace_port_refs("http://h:18080/x", &map),
            "http://h:18080/x"
        );
        assert_eq!(
            replace_port_refs("http://h:1808/x", &map),
            "http://h:2000/x"
        );
        assert_eq!(replace_port_refs("h:1808", &map), "h:2000");
        assert_eq!(replace_port_refs("é:1808é", &map), "é:2000é");
    }

    #[test]
    fn set_dotted_merges_into_the_generated_overlay() {
        let mut local = yaml("stems:\n  echo-svc:\n    env:\n      PORT: \"20000\"\n");
        set_dotted(&mut local, "stems.echo-svc.env.SHOP_CHAOS", "\"0\"").unwrap();
        set_dotted(&mut local, "stems.echo-svc.enabled", "false").unwrap();
        set_dotted(&mut local, "profiles.default", "backend").unwrap();
        let expected = yaml(
            r#"
stems:
  echo-svc:
    env:
      PORT: "20000"
      SHOP_CHAOS: "0"
    enabled: false
profiles:
  default: backend
"#,
        );
        assert_eq!(local, expected);
        assert!(set_dotted(&mut local, "a..b", "1").is_err());
    }

    #[test]
    fn render_local_round_trips() {
        let local = yaml("stems:\n  a:\n    ports: [{ port: 20000 }]\n");
        let text = render_local(&local);
        assert!(text.starts_with("# Generated by the stems e2e harness"));
        let back: Mapping = serde_yaml_ng::from_str(&text).unwrap();
        assert_eq!(back, local);
    }
}
