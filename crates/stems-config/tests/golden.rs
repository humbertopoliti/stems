//! Goldens of the resolved example workspaces and the defaults table.
//! Absolute paths are masked: the workspace root becomes `<ws>`, the repo
//! root `<repo>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use stems_config::{DeferredRef, Diagnostic, LoadOptions, Workspace, load};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[derive(Serialize)]
struct Golden<'a> {
    sources: Vec<String>,
    diagnostics: &'a [Diagnostic],
    deferred: &'a [DeferredRef],
    workspace: &'a Workspace,
}

fn golden(ws: &str) -> String {
    let dir = repo_root().join("examples/workspaces").join(ws);
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), "/home/tester".to_string());
    let r = load(LoadOptions {
        workspace: Some(dir.clone()),
        cwd: repo_root(),
        env,
        // The committed example has only stems.local.yaml.example; skip any
        // developer-local overlay so the golden is machine independent.
        skip_local: true,
    })
    .unwrap_or_else(|e| panic!("{ws} failed to load: {e}"));
    let root = r.workspace.root.display().to_string();
    let repo = repo_root().display().to_string();
    let g = Golden {
        sources: r.sources.iter().map(|p| p.display().to_string()).collect(),
        diagnostics: &r.diagnostics,
        deferred: &r.deferred,
        workspace: &r.workspace,
    };
    serde_yaml_ng::to_string(&g)
        .unwrap()
        .replace(&root, "<ws>")
        .replace(&repo, "<repo>")
}

#[test]
fn hello_shop_resolved() {
    insta::assert_snapshot!(golden("hello-shop"));
}

#[test]
fn minimal_resolved() {
    insta::assert_snapshot!(golden("minimal"));
}

#[test]
fn defaults_table() {
    let rows: String = stems_config::defaults::defaults_table()
        .into_iter()
        .map(|(k, v)| format!("{k:<45} {v}\n"))
        .collect();
    insta::assert_snapshot!(rows);
}
