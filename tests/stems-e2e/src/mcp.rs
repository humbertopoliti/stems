//! The harness's MCP test client (deliverable 31): an rmcp client named
//! `stems-e2e` talking over stdio to a `stems mcp` child process started
//! with the scenario's environment (cwd = the workspace, `STEMS_HOME`, no
//! other `STEMS_*`).
//!
//! Every tool call carries a progress token; progress notifications are
//! counted. A call's result (the JSON text of its first content block) also
//! becomes the scenario's "last command" (`json` = the result, exit code 0
//! or 1 for `isError`), so the generic `Then the JSON at ...` steps work on
//! it too. The After hook calls [`shutdown`]: the server's process group is
//! killed and the leak check then stops any daemon it auto-started.

use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cucumber::step::{Context, Step};
use futures::future::LocalBoxFuture;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, GetPromptRequestParams,
    Implementation, NumberOrString, ProgressNotificationParam, ProgressToken,
    ReadResourceRequestParams, RequestMetaObject,
};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::Value;

use crate::world::{self, E2eWorld, repo_root, stems_bin};
use crate::{procs, util};

/// The client name every scenario uses: events carry `mcp:stems-e2e`.
pub const CLIENT_NAME: &str = "stems-e2e";

/// The client side: identity plus a progress counter.
#[derive(Clone)]
pub struct E2eClient {
    info: ClientConfig,
    progress: Arc<Mutex<Vec<String>>>,
}

impl ClientHandler for E2eClient {
    fn get_info(&self) -> ClientConfig {
        self.info.clone()
    }

    async fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(params.message.unwrap_or_default());
    }
}

/// One tool call's outcome.
#[derive(Clone, Debug)]
pub struct ToolResult {
    /// Tool name.
    pub name: String,
    /// `isError`.
    pub is_error: bool,
    /// The first text content parsed as JSON (`null` if it is not JSON).
    pub json: Value,
    /// The raw text.
    pub text: String,
}

/// A connected client and its server process.
pub struct McpSession {
    client: Option<RunningService<RoleClient, E2eClient>>,
    child: tokio::process::Child,
    /// The server's process group.
    pub pgid: i32,
    progress: Arc<Mutex<Vec<String>>>,
    next_token: AtomicU64,
    /// Every tool result, oldest first.
    pub results: Vec<ToolResult>,
    /// Tool names of the last `tools/list`.
    pub tools: Vec<Value>,
    /// Text of the last resource read.
    pub resource: Option<String>,
    /// Text of the last prompt.
    pub prompt: Option<String>,
}

impl std::fmt::Debug for McpSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpSession")
            .field("pgid", &self.pgid)
            .field("results", &self.results.len())
            .finish_non_exhaustive()
    }
}

impl McpSession {
    fn client(&self) -> &RunningService<RoleClient, E2eClient> {
        self.client
            .as_ref()
            .expect("the MCP client is disconnected (`When the MCP client disconnects` ran)")
    }

    /// Progress messages received so far.
    pub fn progress(&self) -> Vec<String> {
        self.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The last tool result.
    pub fn last(&self) -> &ToolResult {
        self.results
            .last()
            .expect("no MCP tool was called yet in this scenario")
    }
}

/// Start `stems mcp [--auto-start]` in the workspace and connect.
pub async fn connect(w: &mut E2eWorld, auto_start: bool) {
    assert!(w.mcp.is_none(), "an MCP client is already connected");
    let cwd = w.default_cwd();
    let mut cmd = tokio::process::Command::new(stems_bin());
    cmd.arg("mcp");
    if auto_start {
        cmd.arg("--auto-start");
        w.daemon_started = true;
    }
    cmd.current_dir(&cwd);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("STEMS_") {
            cmd.env_remove(k);
        }
    }
    cmd.env("STEMS_HOME", &w.home).env("STEMS_NO_COLOR", "1");
    let stderr = std::fs::File::create(w.root.join("mcp-server.stderr"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .process_group(0)
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {} mcp: {e}", stems_bin().display()));
    let pgid = child.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
    w.pgids.insert(pgid);
    let stdout = child.stdout.take().expect("piped stdout");
    let stdin = child.stdin.take().expect("piped stdin");
    let progress = Arc::new(Mutex::new(Vec::new()));
    let handler = E2eClient {
        info: ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(CLIENT_NAME, env!("CARGO_PKG_VERSION")),
        ),
        progress: progress.clone(),
    };
    let Ok(init) = tokio::time::timeout(w.remaining(), handler.serve((stdout, stdin))).await else {
        panic!("TIMEOUT: MCP initialisation with `stems mcp`");
    };
    let client = init.unwrap_or_else(|e| {
        let err = std::fs::read_to_string(w.root.join("mcp-server.stderr")).unwrap_or_default();
        panic!("MCP initialisation failed: {e}\n--- server stderr\n{err}")
    });
    w.mcp = Some(McpSession {
        client: Some(client),
        child,
        pgid,
        progress,
        next_token: AtomicU64::new(1),
        results: Vec::new(),
        tools: Vec::new(),
        resource: None,
        prompt: None,
    });
}

fn session(w: &mut E2eWorld) -> &mut McpSession {
    w.mcp
        .as_mut()
        .expect("no MCP client: use `Given an MCP client is connected`")
}

/// Call `name` with `args`; records the result (also as the last command).
pub async fn call(w: &mut E2eWorld, name: &str, args: Value) {
    let timeout = w.remaining();
    let started = Instant::now();
    let s = session(w);
    let mut p = CallToolRequestParams::new(name.to_string());
    match &args {
        Value::Object(m) => p = p.with_arguments(m.clone()),
        Value::Null => {}
        other => panic!("tool arguments must be a JSON object, got {other}"),
    }
    let token = s.next_token.fetch_add(1, Ordering::Relaxed);
    p.meta = Some(RequestMetaObject::with_progress_token(ProgressToken(
        NumberOrString::String(format!("e2e-{token}").into()),
    )));
    let r = tokio::time::timeout(timeout, s.client().call_tool(p))
        .await
        .unwrap_or_else(|_| panic!("TIMEOUT: MCP tool `{name}`"))
        .unwrap_or_else(|e| panic!("MCP tool call `{name}` failed at the protocol level: {e}"));
    let v = serde_json::to_value(&r).unwrap_or_default();
    let text = v["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let is_error = v["isError"].as_bool().unwrap_or(false);
    s.results.push(ToolResult {
        name: name.to_string(),
        is_error,
        json: json.clone(),
        text: text.clone(),
    });
    world::collect_pgids(&json, &mut w.pgids);
    w.last = Some(world::CmdOutput {
        argv: vec!["mcp-tool".into(), name.into(), args.to_string()],
        cwd: w.default_cwd(),
        code: i32::from(is_error),
        stdout: text,
        stderr: String::new(),
        json: Some(json),
        elapsed: started.elapsed(),
    });
}

/// The server's `tools/list`.
pub async fn list_tools(w: &mut E2eWorld) -> Vec<Value> {
    let timeout = w.remaining();
    let s = session(w);
    let tools = tokio::time::timeout(timeout, s.client().list_all_tools())
        .await
        .unwrap_or_else(|_| panic!("TIMEOUT: tools/list"))
        .unwrap_or_else(|e| panic!("tools/list failed: {e}"));
    s.tools = tools
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or_default())
        .collect();
    s.tools.clone()
}

/// Close the client (stdin EOF for the server) and wait up to 15 s for the
/// server process to exit; it stops a daemon it auto-started first.
pub async fn disconnect(w: &mut E2eWorld) {
    let s = session(w);
    if let Some(c) = s.client.take() {
        let _ = tokio::time::timeout(Duration::from_secs(5), c.cancel()).await;
    }
    let exited = tokio::time::timeout(Duration::from_secs(15), s.child.wait()).await;
    assert!(
        exited.is_ok(),
        "`stems mcp` did not exit within 15s after the client disconnected"
    );
}

/// After hook: kill the server's process group (a daemon it auto-started
/// is left to the leak check's `stems daemon stop`).
pub async fn shutdown(w: &mut E2eWorld) {
    if let Some(mut s) = w.mcp.take() {
        if let Some(c) = s.client.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), c.cancel()).await;
        }
        if tokio::time::timeout(Duration::from_secs(3), s.child.wait())
            .await
            .is_err()
        {
            procs::kill_group(s.pgid);
            let _ = s.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), s.child.wait()).await;
        }
    }
}

// ----------------------------------------------------------------------------
// Steps
// ----------------------------------------------------------------------------

macro_rules! step {
    ($name:ident($w:ident, $m:ident) $body:block) => {
        fn $name<'a>($w: &'a mut E2eWorld, ctx: Context) -> LocalBoxFuture<'a, ()> {
            #[allow(unused_variables)]
            let $m: Vec<String> = ctx.matches.into_iter().map(|(_, s)| s).collect();
            Box::pin(async move {
                let _ = $w.remaining();
                $body
            })
        }
    };
}

fn expected(w: &E2eWorld, raw: &str) -> Value {
    util::parse_expected(&w.expand(raw))
}

fn result_nodes(w: &mut E2eWorld, path: &str) -> (Vec<Value>, String) {
    let last = session(w).last().clone();
    let nodes = util::query(&last.json, path)
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .cloned()
        .collect();
    (
        nodes,
        format!(
            "tool `{}` result:\n{}",
            last.name,
            util::clip(&last.text, 3000)
        ),
    )
}

step!(given_mcp_connected(w, m) {
    connect(w, !m[1].is_empty()).await;
});

step!(when_mcp_call(w, m) {
    let raw = w.expand(&m[2]);
    let args: Value = serde_json::from_str(raw.trim())
        .unwrap_or_else(|e| panic!("tool arguments are not JSON ({e}): {raw}"));
    call(w, &m[1], args).await;
});

step!(then_tool_result_is(w, m) {
    let last = session(w).last().clone();
    let want_error = &m[1] == "error";
    assert!(
        last.is_error == want_error,
        "tool `{}` returned {} (expected {})\n{}",
        last.name,
        if last.is_error { "an error" } else { "success" },
        m[1],
        util::clip(&last.text, 3000)
    );
});

step!(then_tool_json_equals(w, m) {
    let want = expected(w, &m[2]);
    let (got, desc) = result_nodes(w, &m[1]);
    assert!(
        got.len() == 1 && got[0] == want,
        "tool result JSON at {} is {} (expected {want})\n{desc}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default()
    );
});

step!(then_tool_json_contains(w, m) {
    let want = expected(w, &m[2]);
    let (got, desc) = result_nodes(w, &m[1]);
    assert!(
        got.iter().any(|v| util::contains(v, &want)),
        "tool result JSON at {} ({}) does not contain {want}\n{desc}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default()
    );
});

step!(when_save_tool_json(w, m) {
    let (got, desc) = result_nodes(w, &m[1]);
    assert!(got.len() == 1, "tool result JSON at {} matched {} nodes\n{desc}", m[1], got.len());
    let v = match &got[0] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    w.vars.insert(m[2].clone(), v);
});

step!(then_across_results_distinct(w, m) {
    let n: usize = m[1].parse().expect("result count");
    let (distinct, total): (usize, usize) = (m[3].parse().expect("distinct"), m[4].parse().expect("total"));
    let s = session(w);
    assert!(s.results.len() >= n, "only {} tool results so far", s.results.len());
    let mut all = Vec::new();
    for r in &s.results[s.results.len() - n..] {
        for v in util::query(&r.json, &m[2]).unwrap_or_else(|e| panic!("{e}")) {
            all.push(v.to_string());
        }
    }
    let uniq: BTreeSet<&String> = all.iter().collect();
    assert!(
        uniq.len() == distinct && all.len() == total,
        "across the last {n} tool results {} has {} values, {} distinct (expected {distinct} distinct of {total})",
        m[2],
        all.len(),
        uniq.len()
    );
});

step!(then_tools_list(w, m) {
    let tools = list_tools(w).await;
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    let has = names.contains(&m[2].as_str());
    assert!(
        has == (&m[1] == "contains"),
        "tools/list {} \"{}\": {names:?}",
        if has { "contains" } else { "does not contain" },
        m[2]
    );
});

step!(then_tools_list_golden(w, m) {
    let tools = list_tools(w).await;
    let path = repo_root().join(&m[1]);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let golden: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let key = |t: &Value| (t["name"].clone(), t["description"].clone(), t["inputSchema"].clone());
    let got: Vec<_> = tools.iter().map(key).collect();
    let want: Vec<_> = golden["tools"].as_array().map(|a| a.iter().map(key).collect()).unwrap_or_default();
    if got != want {
        let names = |v: &[(Value, Value, Value)]| v.iter().map(|t| t.0.to_string()).collect::<Vec<_>>().join(", ");
        let differing: Vec<String> = got
            .iter()
            .filter(|g| !want.contains(g))
            .map(|g| g.0.to_string())
            .collect();
        panic!(
            "tools/list differs from {}\n  live:   {}\n  golden: {}\n  differing: {differing:?}\n(regenerate with `UPDATE_SCHEMA=1 cargo test -p stems-mcp --test golden` if intended)",
            m[1],
            names(&got),
            names(&want)
        );
    }
});

step!(when_read_resource(w, m) {
    let uri = w.expand(&m[1]);
    let timeout = w.remaining();
    let s = session(w);
    let r = tokio::time::timeout(timeout, s.client().read_resource(ReadResourceRequestParams::new(uri.clone())))
        .await
        .unwrap_or_else(|_| panic!("TIMEOUT: resources/read {uri}"))
        .unwrap_or_else(|e| panic!("resources/read {uri} failed: {e}"));
    let v = serde_json::to_value(&r).unwrap_or_default();
    s.resource = Some(v["contents"][0]["text"].as_str().unwrap_or_default().to_string());
});

step!(then_resource_contains(w, m) {
    let want = w.expand(&m[1]);
    let text = session(w).resource.clone().expect("no resource was read yet");
    assert!(text.contains(&want), "the resource content does not contain {want:?}:\n{}", util::clip(&text, 3000));
});

step!(when_get_prompt(w, m) {
    let raw = w.expand(&m[2]);
    let args: Value = serde_json::from_str(raw.trim())
        .unwrap_or_else(|e| panic!("prompt arguments are not JSON ({e}): {raw}"));
    let timeout = w.remaining();
    let s = session(w);
    let mut p = GetPromptRequestParams::new(m[1].clone());
    if let Value::Object(o) = args {
        p = p.with_arguments(o);
    }
    let r = tokio::time::timeout(timeout, s.client().get_prompt(p))
        .await
        .unwrap_or_else(|_| panic!("TIMEOUT: prompts/get {}", m[1]))
        .unwrap_or_else(|e| panic!("prompts/get {} failed: {e}", m[1]));
    let v = serde_json::to_value(&r).unwrap_or_default();
    let text: Vec<String> = v["messages"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x["content"]["text"].as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    s.prompt = Some(text.join("\n"));
});

step!(then_prompt_contains(w, m) {
    let want = w.expand(&m[1]);
    let text = session(w).prompt.clone().expect("no prompt was fetched yet");
    assert!(text.contains(&want), "the prompt text does not contain {want:?}:\n{}", util::clip(&text, 4000));
});

step!(then_progress_received(w, m) {
    let min: usize = m[1].parse().expect("count");
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        let got = session(w).progress();
        if got.len() >= min {
            return;
        }
        assert!(Instant::now() < until, "received {} progress notification(s) (expected at least {min}): {got:?}", got.len());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
});

step!(when_mcp_disconnects(w, m) {
    disconnect(w).await;
});

/// The MCP steps (chained after `steps::STEPS` in the collection).
pub const STEPS: &[(&str, Step<E2eWorld>)] = &[
    (
        r#"^an MCP client is connected( with auto-start)?$"#,
        given_mcp_connected,
    ),
    (
        r#"^the MCP client calls tool "([^"]+)" with (.+)$"#,
        when_mcp_call,
    ),
    (
        r#"^the tool result is (success|error)$"#,
        then_tool_result_is,
    ),
    (
        r#"^the tool result JSON at "([^"]+)" equals (.+)$"#,
        then_tool_json_equals,
    ),
    (
        r#"^the tool result JSON at "([^"]+)" contains (.+)$"#,
        then_tool_json_contains,
    ),
    (
        r#"^I save the tool result JSON at "([^"]+)" as "([\w-]+)"$"#,
        when_save_tool_json,
    ),
    (
        r#"^across the last (\d+) tool results the values at "([^"]+)" are (\d+) distinct of (\d+)$"#,
        then_across_results_distinct,
    ),
    (
        r#"^the tools list (contains|does not contain) "([^"]+)"$"#,
        then_tools_list,
    ),
    (
        r#"^the tools list matches "([^"]+)"$"#,
        then_tools_list_golden,
    ),
    (
        r#"^the MCP client reads resource "([^"]+)"$"#,
        when_read_resource,
    ),
    (
        r#"^the resource content contains "(.*)"$"#,
        then_resource_contains,
    ),
    (
        r#"^the MCP client gets prompt "([^"]+)" with (.+)$"#,
        when_get_prompt,
    ),
    (r#"^the prompt text contains "(.*)"$"#, then_prompt_contains),
    (
        r#"^the MCP client received at least (\d+) progress notifications?$"#,
        then_progress_received,
    ),
    (r#"^the MCP client disconnects$"#, when_mcp_disconnects),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_step_regexes_are_unambiguous() {
        let samples = [
            r#"an MCP client is connected"#,
            r#"an MCP client is connected with auto-start"#,
            r#"the MCP client calls tool "get_status" with {}"#,
            r#"the tool result is success"#,
            r#"the tool result JSON at "$.code" equals "USAGE""#,
            r#"the tool result JSON at "$.events" contains {"actor": "mcp:stems-e2e"}"#,
            r#"I save the tool result JSON at "$.next_cursor" as "c1""#,
            r#"across the last 3 tool results the values at "$.records[*].text" are 1200 distinct of 1200"#,
            r#"the tools list contains "echo_svc__ping""#,
            r#"the tools list matches "schema/mcp-tools.json""#,
            r#"the MCP client reads resource "stems://echo-svc/logs?tail=5""#,
            r#"the resource content contains "listening""#,
            r#"the MCP client gets prompt "diagnose_stem" with {"stem": "echo-svc"}"#,
            r#"the prompt text contains "healthy""#,
            r#"the MCP client received at least 1 progress notification"#,
            r#"the MCP client disconnects"#,
        ];
        let all: Vec<regex::Regex> = crate::steps::STEPS
            .iter()
            .chain(STEPS)
            .map(|(r, _)| regex::Regex::new(r).unwrap())
            .collect();
        for s in samples {
            let hits = all.iter().filter(|r| r.is_match(s)).count();
            assert_eq!(hits, 1, "{s:?} matched {hits} step regexes");
        }
        // Every MCP step has a sample (the optional auto-start group counts twice).
        assert_eq!(STEPS.len(), samples.len() - 1);
    }
}
