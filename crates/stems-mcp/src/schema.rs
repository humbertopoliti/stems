//! JSON Schemas of tool inputs, generated with schemars from the argument
//! types (their doc comments become the property descriptions agents read).
//!
//! Sub-schemas are inlined (MCP clients handle `$defs` unevenly) and the
//! `$schema` / `title` keys are dropped: the tool name already says it.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

/// The MCP `inputSchema` of `T` (always `"type": "object"`).
pub fn input_schema<T: JsonSchema>() -> Map<String, Value> {
    let generator = SchemaSettings::draft2020_12()
        .with(|s| {
            s.inline_subschemas = true;
        })
        .into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let mut v = serde_json::to_value(schema).unwrap_or_default();
    clean(&mut v);
    let mut m = match v {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    m.remove("$schema");
    m.remove("title");
    m.remove("description");
    m.insert("type".into(), Value::String("object".into()));
    if !m.contains_key("properties") {
        m.insert("properties".into(), Value::Object(Map::new()));
    }
    m
}

/// Normalise schemars output for MCP clients: `format: uint*` hints that
/// are not JSON Schema formats go, and descriptions become one paragraph
/// per blank-line-separated block (doc comments are wrapped at 80 columns).
fn clean(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(f)) = m.get("format")
                && (f.starts_with("uint") || f.starts_with("int"))
            {
                m.remove("format");
            }
            if let Some(Value::String(d)) = m.get_mut("description") {
                *d = unwrap_lines(d);
            }
            for (_, x) in m.iter_mut() {
                clean(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(clean),
        _ => {}
    }
}

/// Join wrapped lines with spaces, keeping blank-line paragraph breaks.
fn unwrap_lines(s: &str) -> String {
    s.split("\n\n")
        .map(|p| p.split('\n').map(str::trim).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    /// Example.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        /// Stems to act
        /// on.
        #[serde(default)]
        stems: Vec<String>,
        /// How many.
        limit: Option<u32>,
    }

    #[test]
    fn object_schema_with_descriptions() {
        let s = Value::Object(input_schema::<Args>());
        assert_eq!(s["type"], "object");
        assert_eq!(s["properties"]["stems"]["description"], "Stems to act on.");
        assert!(s.get("$schema").is_none());
        assert!(s["properties"]["limit"].get("format").is_none());
    }
}
