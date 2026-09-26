//! The server over an in-memory transport with a real rmcp client, against
//! the minimal example and no daemon: the daemonless tools, gating (which
//! never needs the daemon), resources, prompts and structured errors.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, GetPromptRequestParams,
    Implementation, ReadResourceRequestParams,
};
use serde_json::{Value, json};
use stems_mcp::{McpOptions, StemsMcp};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

type Client = rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>;

async fn connect(home: &Path) -> Client {
    let ws = repo().join("examples/workspaces/minimal");
    let server = StemsMcp::new(McpOptions {
        workspace: Some(ws.clone()),
        cwd: ws,
        env: HashMap::from([("HOME".to_string(), home.display().to_string())]),
        home: home.to_path_buf(),
        binary: PathBuf::from("/nonexistent/stems"),
        auto_start: false,
    });
    let (a, b) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        if let Ok(s) = server.serve(a).await {
            let _ = s.waiting().await;
        }
    });
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("unit-client", "1.0"),
    )
    .serve(b)
    .await
    .unwrap()
}

/// `(is_error, parsed JSON text)` of a tool call.
async fn call(c: &Client, name: &str, args: Value) -> (bool, Value) {
    let mut p = CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    let r = serde_json::to_value(c.call_tool(p).await.unwrap()).unwrap();
    let text = r["content"][0]["text"].as_str().unwrap_or_default();
    (
        r["isError"].as_bool().unwrap_or(false),
        serde_json::from_str(text).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn daemonless_tools_gating_resources_prompts() {
    let home = tempfile::tempdir().unwrap();
    let c = connect(home.path()).await;

    let tools = c.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"get_status") && names.contains(&"echo_svc__ping"));

    let (err, v) = call(&c, "list_stems", json!({})).await;
    assert!(!err, "{v}");
    assert_eq!(v["stems"][0]["name"], "echo-svc");
    assert_eq!(v["stems"][0]["custom_scripts"], json!(["ping"]));
    assert_eq!(v["daemon_running"], false);

    let (err, v) = call(&c, "get_config", json!({"stem": "echo-svc"})).await;
    assert!(!err, "{v}");
    assert_eq!(v["name"], "echo-svc");

    let (err, v) = call(&c, "get_graph", json!({})).await;
    assert!(!err, "{v}");
    assert_eq!(v["nodes"][0]["name"], "echo-svc");
    assert_eq!(v["live"], false);

    let (err, v) = call(&c, "get_status", json!({})).await;
    assert!(err);
    assert_eq!(v["code"], "DAEMON_NOT_RUNNING");
    assert!(v["hint"].as_str().unwrap().contains("--auto-start"), "{v}");

    let (err, v) = call(&c, "down", json!({"all": true})).await;
    assert!(err);
    assert_eq!(v["code"], "DESTRUCTIVE_NOT_CONFIRMED");
    let (err, v) = call(&c, "down", json!({"all": true, "confirm": true})).await;
    assert!(err);
    assert_eq!(v["code"], "DESTRUCTIVE_NOT_ALLOWED");
    assert!(
        v["hint"]
            .as_str()
            .unwrap()
            .contains("agent.allow_destructive")
    );

    let (err, v) = call(&c, "nope", json!({})).await;
    assert!(err);
    assert_eq!(v["code"], "USAGE");
    let (err, v) = call(&c, "get_logs", json!({"cursor": "bogus"})).await;
    assert!(err);
    assert!(
        v["code"] == "USAGE" || v["code"] == "DAEMON_NOT_RUNNING",
        "{v}"
    );

    let r = c
        .read_resource(ReadResourceRequestParams::new("stems://workspace"))
        .await
        .unwrap();
    let r = serde_json::to_value(r).unwrap();
    assert!(
        r["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("echo-svc")
    );
    assert_eq!(r["contents"][0]["mimeType"], "application/json");

    let res = c.list_all_resources().await.unwrap();
    let uris: Vec<String> = res.iter().map(|r| r.uri.clone()).collect();
    assert!(
        uris.contains(&"stems://echo-svc/logs?tail=200".to_string()),
        "{uris:?}"
    );

    let mut args = serde_json::Map::new();
    args.insert("stem".into(), json!("echo-svc"));
    let p = c
        .get_prompt(GetPromptRequestParams::new("diagnose_stem").with_arguments(args))
        .await
        .unwrap();
    let p = serde_json::to_value(p).unwrap();
    let text = p["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(
        text.contains("echo-svc") && text.contains("not running"),
        "{text}"
    );

    c.cancel().await.unwrap();
}
