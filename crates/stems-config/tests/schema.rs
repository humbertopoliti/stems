//! `schema/stems.schema.json` must match the code. Regenerate with
//! `UPDATE_SCHEMA=1 cargo test -p stems-config --test schema`.

use std::path::Path;

#[test]
fn schema_file_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/stems.schema.json");
    let generated = stems_config::json_schema_string();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "{} is out of date; run `UPDATE_SCHEMA=1 cargo test -p stems-config --test schema` and commit it",
        path.display()
    );
}

#[test]
fn example_workspaces_validate_against_the_schema_shape() {
    // Light sanity check without a JSON Schema validator: every top-level key
    // used by the examples is a declared property.
    let schema: serde_json::Value =
        serde_json::from_str(&stems_config::json_schema_string()).unwrap();
    let props = schema["properties"].as_object().unwrap();
    for key in [
        "schema_version",
        "name",
        "vars",
        "requires",
        "profiles",
        "agent",
        "scripts",
        "stems",
        "include",
        "extends",
    ] {
        assert!(props.contains_key(key), "missing {key}");
    }
    assert_eq!(schema["additionalProperties"], false);
}
