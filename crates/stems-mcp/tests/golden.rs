//! `schema/mcp-tools.json`: the `tools/list` of `examples/workspaces/minimal`
//! (every core tool plus `echo_svc__ping`) is a public contract. This test
//! fails on drift; `UPDATE_SCHEMA=1 cargo test -p stems-mcp --test golden`
//! rewrites the file (review the diff).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use stems_mcp::{McpOptions, StemsMcp};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A server on the minimal example with an empty STEMS_HOME (no daemon).
pub fn minimal_server(home: &Path) -> StemsMcp {
    let ws = repo().join("examples/workspaces/minimal");
    StemsMcp::new(McpOptions {
        workspace: Some(ws.clone()),
        cwd: ws,
        env: HashMap::from([("HOME".to_string(), home.display().to_string())]),
        home: home.to_path_buf(),
        binary: PathBuf::from("/nonexistent/stems"),
        auto_start: false,
    })
}

/// The golden document for a tool list.
pub fn document(tools: &[rmcp::model::Tool]) -> String {
    serde_json::to_string_pretty(&json!({ "tools": tools })).unwrap() + "\n"
}

#[tokio::test]
async fn tools_list_matches_the_golden() {
    let home = tempfile::tempdir().unwrap();
    let server = minimal_server(home.path());
    let tools = server.tools("mcp:golden").await;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for core in stems_mcp::tools::core_names() {
        assert!(names.contains(&core), "{core} missing from {names:?}");
    }
    assert_eq!(names.last(), Some(&"echo_svc__ping"));
    let got = document(&tools);
    let path = repo().join("schema/mcp-tools.json");
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    let (g, w): (Value, Value) = (
        serde_json::from_str(&got).unwrap(),
        serde_json::from_str(&want).unwrap_or(Value::Null),
    );
    assert!(
        g == w,
        "schema/mcp-tools.json is out of date: run `UPDATE_SCHEMA=1 cargo test -p stems-mcp --test golden` and review the diff\n--- generated\n{got}"
    );
}
