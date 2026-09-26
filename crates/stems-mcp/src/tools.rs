//! The core MCP tools: argument types (their schemas are the tools'
//! `inputSchema`s), the catalogue published by `tools/list`, and what each
//! call does.
//!
//! Tool names and schemas are a public contract, snapshotted in
//! `schema/mcp-tools.json` (see the drift test in `tests/golden.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use stems_api::client::Client;
use stems_api::{
    DownParams, EventsParams, EventsResult, HealthParams, HealthResult, Method, MetricsParams,
    OutputsParams, QueryLogsParams, QueryLogsResult, ResetParams, RestartParams, RunScriptParams,
    ScriptArgsInput, ScriptCatalogResult, StartParams, StatusParams, StatusResult, StopParams,
    UpParams, WatchPauseParams,
};
use stems_core::scriptargs::ScriptKind;
use stems_core::selection::{SelectOptions, Selection};
use stems_core::{Error, ErrorCode, Glyph};

use crate::backend::Backend;
use crate::logs::{Cursor, PAGE_MAX, paginate, result_json};
use crate::redact::redacted;
use crate::schema::input_schema;

/// Deadline of long daemon calls (`up`, `down`, `run_script`, …).
pub const LONG: Duration = Duration::from_secs(15 * 60);
/// Default and maximum `limit` of `get_events`.
pub const EVENTS_DEFAULT: usize = 100;
/// Most events one `get_events` call returns.
pub const EVENTS_MAX: usize = 500;

// ---------------------------------------------------------------------------
// Argument types
// ---------------------------------------------------------------------------

/// `list_stems`: no arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct ListStemsArgs {}

/// `get_status` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetStatusArgs {
    /// Only these stems (default: every enabled stem).
    #[serde(default)]
    pub stems: Vec<String>,
}

/// `get_graph` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetGraphArgs {
    /// Only this stem, its dependencies and its dependants.
    #[serde(default)]
    pub focus: Option<String>,
    /// Only the stems of this profile (with their hard dependencies).
    #[serde(default)]
    pub profile: Option<String>,
}

/// `up` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct UpArgs {
    /// Stems to start (their hard dependencies are added); empty = the
    /// profile's stems.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Profile to bring up when `stems` is empty (default: the workspace's
    /// default profile, else every enabled stem).
    #[serde(default)]
    pub profile: Option<String>,
    /// Destructive: stop the planned stems, run their `reset` scripts and
    /// clear their stamps first so `setup`/`seed` run again. Needs
    /// `confirm: true` and `agent.allow_destructive`.
    #[serde(default)]
    pub fresh: bool,
    /// Confirms a destructive call (`fresh`), after asking the user.
    #[serde(default)]
    pub confirm: bool,
    /// Overall deadline in milliseconds (default: each stem's start timeout).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `down` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct DownArgs {
    /// Stems to stop; empty = every running stem.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Destructive: stop everything and shut the daemon down. Needs
    /// `confirm: true` and `agent.allow_destructive`.
    #[serde(default)]
    pub all: bool,
    /// Destructive: also remove the docker volumes of the selected stems.
    /// Needs `confirm: true` and `agent.allow_destructive`.
    #[serde(default)]
    pub volumes: bool,
    /// Confirms a destructive call (`all`, `volumes`), after asking the user.
    #[serde(default)]
    pub confirm: bool,
}

/// `run_script` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct RunScriptArgs {
    /// Owning stem; omit for a workspace-level script.
    #[serde(default)]
    pub stem: Option<String>,
    /// Script name (see the `<stem>__<script>` tools for custom scripts).
    pub name: String,
    /// Arguments by name, validated against the script's declared `args`
    /// (`{"email": "a@b.c"}`); invalid ones fail with `SCRIPT_ARGS_INVALID`.
    #[serde(default)]
    pub args: Map<String, Value>,
    /// Start the script's `requires:` stems first if they are not healthy.
    #[serde(default)]
    pub start_deps: bool,
}

/// `get_logs` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetLogsArgs {
    /// Only these stems (default: all).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Records at or after this time: a duration before now (`10m`, `1h30m`)
    /// or an RFC 3339 instant.
    #[serde(default)]
    pub since: Option<String>,
    /// Level: `error` (exactly) or `warn+` (warn and above).
    #[serde(default)]
    pub level: Option<String>,
    /// Only records whose text matches this regex.
    #[serde(default)]
    pub grep: Option<String>,
    /// Only the last `n` matching records (then paged oldest first).
    #[serde(default)]
    pub tail: Option<usize>,
    /// `next_cursor` of a previous call: continue after it (the other
    /// filters are taken from the cursor).
    #[serde(default)]
    pub cursor: Option<String>,
    /// Records per call (default and maximum 500).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `get_metrics` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetMetricsArgs {
    /// Only these stems (default: every enabled stem).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Include the samples of this last period (`5m`, `30s`).
    #[serde(default)]
    pub history: Option<String>,
}

/// `get_events` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetEventsArgs {
    /// Only events with a greater `seq` (use the previous result's
    /// `next_since_seq`); default: every buffered event.
    #[serde(default)]
    pub since_seq: Option<u64>,
    /// Only these kinds (`stem.state`, or a prefix with `*`: `script.*`).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only events caused by this actor (`mcp:<client>`, `cli:<user>`, `daemon`).
    #[serde(default)]
    pub actor: Option<String>,
    /// Events per call, oldest first (default 100, at most 500).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `get_config` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetConfigArgs {
    /// Only this stem's resolved config.
    #[serde(default)]
    pub stem: Option<String>,
    /// Add the table of defaults applied to unset fields.
    #[serde(default)]
    pub effective: bool,
}

/// `doctor` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct DoctorArgs {
    /// Destructive: apply the safe fixes (stale locks, orphans, ...), then
    /// check again. Needs `confirm: true` and `agent.allow_destructive`.
    #[serde(default)]
    pub fix: bool,
    /// Confirms a destructive call (`fix`), after asking the user.
    #[serde(default)]
    pub confirm: bool,
}

/// `reset` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct ResetArgs {
    /// Stems to reset: stopped, their `reset` scripts run, stamps cleared.
    pub stems: Vec<String>,
    /// Required (destructive): confirms the call, after asking the user.
    /// The workspace must also set `agent.allow_destructive`.
    #[serde(default)]
    pub confirm: bool,
}

/// `get_outputs` arguments.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct GetOutputsArgs {
    /// Only these stems (default: every stem that declares outputs).
    #[serde(default)]
    pub stems: Vec<String>,
}

// ---------------------------------------------------------------------------
// Catalogue
// ---------------------------------------------------------------------------

/// How a core tool behaves (for annotations and dispatch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Reads only.
    Read,
    /// Changes state, not destructive.
    Write,
    /// Destructive with some arguments (gated).
    Destructive,
}

/// One core tool.
pub struct CoreTool {
    /// Tool name.
    pub name: &'static str,
    /// What an agent reads.
    pub description: &'static str,
    /// Read / write / destructive.
    pub kind: Kind,
    /// The input schema.
    pub schema: fn() -> Map<String, Value>,
}

/// Every core tool, in `tools/list` order.
pub const CORE_TOOLS: &[CoreTool] = &[
    CoreTool {
        name: "list_stems",
        description: "List the workspace's stems (services): type, description, enabled, dependencies, ports, custom scripts, and the live state when the daemon runs. Works without a daemon.",
        kind: Kind::Read,
        schema: input_schema::<ListStemsArgs>,
    },
    CoreTool {
        name: "get_status",
        description: "Live state of each stem: lifecycle state, glyph (healthy/degraded/failed/stopped/...), reason, pid, ports, uptime, restarts, health summary, latest metrics. Secrets are redacted.",
        kind: Kind::Read,
        schema: input_schema::<GetStatusArgs>,
    },
    CoreTool {
        name: "get_graph",
        description: "The dependency graph: nodes {name, type, status, glyph, reason} and edges {from, to, condition, soft, protocol, via}. Live status when the daemon runs, config only otherwise.",
        kind: Kind::Read,
        schema: input_schema::<GetGraphArgs>,
    },
    CoreTool {
        name: "start",
        description: "Start stems (and their unstarted hard dependencies unless no_deps) and wait until they are ready. Returns {ok, ready, failed, skipped}.",
        kind: Kind::Write,
        schema: input_schema::<StartParams>,
    },
    CoreTool {
        name: "stop",
        description: "Stop stems. Running dependants make this fail with HAS_DEPENDANTS unless cascade is true.",
        kind: Kind::Write,
        schema: input_schema::<StopParams>,
    },
    CoreTool {
        name: "restart",
        description: "Restart stems keeping their ports; with build, run each stem's build script between stop and start.",
        kind: Kind::Write,
        schema: input_schema::<RestartParams>,
    },
    CoreTool {
        name: "up",
        description: "Bring stems up (a profile, or named stems with their dependencies) and wait until ready; sends progress notifications. fresh is destructive (needs confirm and agent.allow_destructive).",
        kind: Kind::Destructive,
        schema: input_schema::<UpArgs>,
    },
    CoreTool {
        name: "down",
        description: "Stop stems (default: every running stem). all (also stops the daemon) and volumes (removes docker volumes) are destructive (need confirm and agent.allow_destructive).",
        kind: Kind::Destructive,
        schema: input_schema::<DownArgs>,
    },
    CoreTool {
        name: "run_script",
        description: "Run a stem or workspace script with arguments by name and wait for it: {ok, exit, duration_ms, tail (last output lines), error}. Custom scripts are also published as <stem>__<script> tools.",
        kind: Kind::Write,
        schema: input_schema::<RunScriptArgs>,
    },
    CoreTool {
        name: "get_logs",
        description: "Captured log records (oldest first, at most 500 per call) filtered by stems, time, level, regex; paginate with next_cursor. truncated is true when not every matching record is in the result.",
        kind: Kind::Read,
        schema: input_schema::<GetLogsArgs>,
    },
    CoreTool {
        name: "get_metrics",
        description: "CPU, memory and process counts per stem (latest sample, optional history), with totals.",
        kind: Kind::Read,
        schema: input_schema::<GetMetricsArgs>,
    },
    CoreTool {
        name: "get_events",
        description: "The daemon's event log (state changes, scripts, health, watchdogs, ...) with who caused each event (actor), oldest first; filter by kinds and actor, page with since_seq.",
        kind: Kind::Read,
        schema: input_schema::<GetEventsArgs>,
    },
    CoreTool {
        name: "get_config",
        description: "The resolved workspace config (or one stem's), secrets redacted. Works without a daemon.",
        kind: Kind::Read,
        schema: input_schema::<GetConfigArgs>,
    },
    CoreTool {
        name: "doctor",
        description: "Check that this machine can run the workspace (tools, ports, Docker, daemon, orphans). fix is destructive (needs confirm and agent.allow_destructive). Works without a daemon.",
        kind: Kind::Destructive,
        schema: input_schema::<DoctorArgs>,
    },
    CoreTool {
        name: "reset",
        description: "Destructive: stop stems, run their reset scripts and clear their stamps (data is lost). Needs confirm and agent.allow_destructive.",
        kind: Kind::Destructive,
        schema: input_schema::<ResetArgs>,
    },
    CoreTool {
        name: "watch_pause",
        description: "Pause file watchdogs (all, or some stems').",
        kind: Kind::Write,
        schema: input_schema::<WatchPauseParams>,
    },
    CoreTool {
        name: "watch_resume",
        description: "Resume file watchdogs (all, or some stems').",
        kind: Kind::Write,
        schema: input_schema::<WatchPauseParams>,
    },
    CoreTool {
        name: "get_health",
        description: "The last health probe results per stem (outcome, latency, detail), failures in a row and recent transitions.",
        kind: Kind::Read,
        schema: input_schema::<HealthParams>,
    },
    CoreTool {
        name: "get_outputs",
        description: "Evaluated stem outputs (URLs, connection strings, ...); secret outputs are always redacted.",
        kind: Kind::Read,
        schema: input_schema::<GetOutputsArgs>,
    },
];

/// Names of the core tools.
pub fn core_names() -> Vec<&'static str> {
    CORE_TOOLS.iter().map(|t| t.name).collect()
}

/// The MCP definition of a core tool.
pub fn core_tool(t: &CoreTool) -> Tool {
    let ann = match t.kind {
        Kind::Read => ToolAnnotations::new().read_only(true).open_world(false),
        Kind::Write => ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .open_world(false),
        Kind::Destructive => ToolAnnotations::new()
            .read_only(false)
            .destructive(true)
            .open_world(false),
    };
    Tool::new(t.name, t.description, Arc::new((t.schema)())).with_annotations(ann)
}

/// The tool of a custom script from the catalogue.
pub fn script_tool(s: &stems_api::CatalogScript) -> Tool {
    let owner = match &s.entry.stem {
        Some(stem) => format!("custom script `{}` of stem `{stem}`", s.entry.name),
        None => format!("workspace script `{}`", s.entry.name),
    };
    let description = match &s.entry.description {
        Some(d) => format!("{d} ({owner}; returns the run_script result)"),
        None => format!("Run the {owner} (returns the run_script result)"),
    };
    let schema = match &s.input_schema {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    Tool::new(s.mcp_tool.clone(), description, Arc::new(schema)).with_annotations(
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .open_world(false),
    )
}

/// The custom scripts of a catalogue (lifecycle scripts are not tools).
pub fn custom_scripts(c: ScriptCatalogResult) -> Vec<stems_api::CatalogScript> {
    c.scripts
        .into_iter()
        .filter(|s| s.entry.kind == ScriptKind::Custom)
        .collect()
}

/// The script catalogue built from the config on disk (no daemon).
pub fn local_catalog(ws: &stems_config::Workspace) -> ScriptCatalogResult {
    ScriptCatalogResult::from_entries(stems_core::scriptargs::build_catalog(ws), None)
}

// ---------------------------------------------------------------------------
// Calls
// ---------------------------------------------------------------------------

/// Parse tool arguments; `USAGE` naming the tool on mismatch.
pub fn parse<T: DeserializeOwned>(tool: &str, args: &Value) -> Result<T, Error> {
    serde_json::from_value(args.clone()).map_err(|e| {
        Error::usage(
            format!("invalid arguments for `{tool}`: {e}"),
            "see the tool's inputSchema in tools/list",
        )
        .with_details(json!({ "tool": tool }))
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::internal(format!("encoding a result: {e}")))
}

/// `list_stems`.
pub async fn list_stems(b: &Backend, actor: &str) -> Result<Value, Error> {
    let resolved = b.load_config()?;
    let ws = &resolved.workspace;
    let live: Option<StatusResult> = match b.connect_existing(actor).await {
        Ok(c) => c.call(Method::STATUS, StatusParams::default()).await.ok(),
        Err(_) => None,
    };
    let stems: Vec<Value> = ws
        .stems
        .values()
        .map(|s| {
            let st = live
                .as_ref()
                .and_then(|l| l.stems.iter().find(|x| x.name == s.name));
            let scripts: Vec<&String> = s
                .scripts
                .keys()
                .filter(|n| !stems_config::is_lifecycle_script(n))
                .collect();
            json!({
                "name": s.name,
                "type": s.kind().to_string(),
                "description": s.description,
                "enabled": s.enabled,
                "depends_on": s.depends_on.iter().map(|d| &d.stem).collect::<Vec<_>>(),
                "ports": s.ports.iter().map(|p| json!({"name": p.name, "port": p.port})).collect::<Vec<_>>(),
                "custom_scripts": scripts,
                "state": st.map(|x| x.state.to_string()),
                "glyph": st.map(|x| x.glyph.name()),
            })
        })
        .collect();
    Ok(json!({
        "workspace": ws.name,
        "root": ws.root,
        "daemon_running": live.is_some(),
        "stems": stems,
    }))
}

/// Fresh daemon connection for `actor` (auto-start when enabled).
async fn client(b: &Backend, actor: &str) -> Result<Client, Error> {
    b.connect(actor).await
}

/// A plain proxied call.
pub async fn proxy<P: serde::Serialize>(
    b: &Backend,
    actor: &str,
    method: Method,
    params: &P,
    timeout: Duration,
) -> Result<Value, Error> {
    let c = client(b, actor).await?;
    c.call_with_timeout::<Value>(method, params, timeout).await
}

/// `get_status`.
pub async fn get_status(b: &Backend, actor: &str, a: GetStatusArgs) -> Result<Value, Error> {
    let p = StatusParams {
        stems: a.stems,
        verbose: false,
    };
    proxy(
        b,
        actor,
        Method::STATUS,
        &p,
        stems_api::client::CALL_TIMEOUT,
    )
    .await
}

/// `get_graph`: the layout from the config, glyphs from the daemon if any.
pub async fn get_graph(b: &Backend, actor: &str, a: GetGraphArgs) -> Result<Value, Error> {
    let resolved = b.load_config()?;
    let ws = &resolved.workspace;
    let mut l = stems_core::layout::layout(ws)
        .map_err(|e| crate::backend::first_error(stems_core::Errors::from(e)))?;
    if let Some(p) = &a.profile {
        let sel = Selection::resolve(
            ws,
            Some(p),
            &[],
            SelectOptions {
                include_deps: true,
                strict_profiles: Some(false),
            },
        )
        .map_err(|e| crate::backend::first_error(stems_core::Errors::from(e)))?;
        l = l.restrict(&sel.closure);
    }
    if let Some(f) = &a.focus {
        if l.node(f).is_none() {
            let known: Vec<&str> = l.nodes.iter().map(|n| n.name.as_str()).collect();
            return Err(Error::new(
                ErrorCode::UnknownStem,
                format!("no stem named `{f}` in the graph"),
            )
            .with_hint(format!("stems in the graph: {}", known.join(", ")))
            .with_details(json!({ "stem": f, "known": known })));
        }
        l = l.focus(f);
    }
    let live: Option<StatusResult> = match b.connect_existing(actor).await {
        Ok(c) => c.call(Method::STATUS, StatusParams::default()).await.ok(),
        Err(_) => None,
    };
    let full = |name: &str| match live
        .as_ref()
        .and_then(|st| st.stems.iter().find(|s| s.name == name))
    {
        Some(s) => (s.glyph, s.reason.clone()),
        None => (Glyph::Stopped, None),
    };
    let mut v = stems_core::render::to_json(
        &l,
        live.as_ref()
            .map(|_| &full as &dyn Fn(&str) -> (Glyph, Option<String>)),
    );
    v["live"] = json!(live.is_some());
    Ok(v)
}

/// `get_logs`.
pub async fn get_logs(b: &Backend, actor: &str, a: GetLogsArgs) -> Result<Value, Error> {
    let limit = a.limit.unwrap_or(PAGE_MAX).clamp(1, PAGE_MAX);
    let c = client(b, actor).await?;
    let cursor = a.cursor.as_deref().map(Cursor::decode).transpose()?;
    let mut p = QueryLogsParams::default();
    let (after, newest_only) = match &cursor {
        Some(cur) => {
            p.filter.stems = cur.stems.clone();
            p.filter.level = cur.level.clone();
            p.filter.grep = cur.grep.clone();
            p.filter.since = Some(cur.since());
            (Some((cur.ts, cur.skip)), false)
        }
        None => {
            p.filter.stems = a.stems.clone();
            p.filter.level = a.level.clone();
            p.filter.grep = a.grep.clone();
            p.filter.since = a.since.clone();
            p.filter.tail = a.tail;
            let newest = a.since.is_none() && a.tail.is_none();
            if newest {
                p.filter.tail = Some(limit + 1);
            }
            (None, newest)
        }
    };
    let res: QueryLogsResult = c
        .call_with_timeout(Method::QUERY_LOGS, &p, stems_api::client::CALL_TIMEOUT)
        .await?;
    let mut records = res.records;
    let mut older_left_out = res.truncated;
    if newest_only && records.len() > limit {
        records.drain(..records.len() - limit);
        older_left_out = true;
    }
    let page = paginate(records, after, limit);
    let base = cursor.unwrap_or(Cursor {
        ts: chrono::DateTime::<chrono::Utc>::MIN_UTC,
        skip: 0,
        stems: a.stems,
        level: a.level,
        grep: a.grep,
    });
    Ok(result_json(&page, Some(&base), page.more || older_left_out))
}

/// `get_metrics`.
pub async fn get_metrics(b: &Backend, actor: &str, a: GetMetricsArgs) -> Result<Value, Error> {
    let history_ms = a
        .history
        .as_deref()
        .map(|h| {
            stems_core::logs::parse_duration(h)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                .map_err(|e| {
                    Error::usage(
                        format!("invalid history `{h}`: {e}"),
                        "use a duration like 30s, 5m or 1h",
                    )
                })
        })
        .transpose()?;
    let p = MetricsParams {
        stems: a.stems,
        history_ms,
        ..MetricsParams::default()
    };
    proxy(
        b,
        actor,
        Method::METRICS,
        &p,
        stems_api::client::CALL_TIMEOUT,
    )
    .await
}

/// `get_events`.
pub async fn get_events(b: &Backend, actor: &str, a: GetEventsArgs) -> Result<Value, Error> {
    let limit = a.limit.unwrap_or(EVENTS_DEFAULT).clamp(1, EVENTS_MAX);
    let p = EventsParams {
        since_seq: a.since_seq,
        limit: Some(limit + 1),
        kinds: a.kinds,
        actor: a.actor,
    };
    let c = client(b, actor).await?;
    let mut r: EventsResult = c.call(Method::EVENTS, &p).await?;
    let truncated = r.events.len() > limit;
    r.events.truncate(limit);
    let next = r.events.last().map(|e| e.seq).or(a.since_seq);
    Ok(json!({
        "events": r.events,
        "returned": r.events.len(),
        "truncated": truncated,
        "last_seq": r.last_seq,
        "next_since_seq": next,
    }))
}

/// `get_config`: the resolved config (or a stem's), redacted.
pub fn get_config(b: &Backend, a: &GetConfigArgs) -> Result<Value, Error> {
    let resolved = b.load_config()?;
    let ws = &resolved.workspace;
    let mut v = match &a.stem {
        None => to_json(ws)?,
        Some(name) => match ws.stem(name) {
            Some(s) => to_json(s)?,
            None => {
                let known: Vec<&String> = ws.stems.keys().collect();
                return Err(
                    Error::new(ErrorCode::UnknownStem, format!("no stem named `{name}`"))
                        .with_hint(format!(
                            "stems in this workspace: {}",
                            known
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .with_details(json!({ "stem": name, "known": known })),
                );
            }
        },
    };
    if a.effective
        && let Value::Object(obj) = &mut v
    {
        let defaults: Map<String, Value> = stems_config::defaults::defaults_table()
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v)))
            .collect();
        obj.insert("defaults".into(), Value::Object(defaults));
    }
    Ok(redacted(v))
}

/// `doctor`, run in-process (there is no doctor RPC), like `stems doctor`.
pub async fn doctor(b: &Backend, actor: &str, a: DoctorArgs) -> Result<Value, Error> {
    use stems_daemon::doctor::{self, DaemonProbe, DoctorCtx, DoctorInput, fix};
    let t = b.target()?;
    let probe = match tokio::time::timeout(doctor::CHECK_TIMEOUT, b.connect_existing(actor)).await {
        Err(_) => DaemonProbe::Unreachable(Error::new(
            ErrorCode::DaemonNotRunning,
            "the daemon did not answer in time",
        )),
        Ok(Err(e)) if e.code == ErrorCode::DaemonNotRunning => DaemonProbe::NotRunning,
        Ok(Err(e)) if e.code == ErrorCode::DaemonVersionMismatch => DaemonProbe::Incompatible(e),
        Ok(Err(e)) => DaemonProbe::Unreachable(e),
        Ok(Ok(c)) => match c
            .call::<stems_api::DaemonStatus>(Method::DAEMON_STATUS, json!({}))
            .await
        {
            Ok(st) => DaemonProbe::Running(Box::new(st)),
            Err(e) => DaemonProbe::Unreachable(e),
        },
    };
    let opts = b.options();
    let input = DoctorInput {
        paths: t.paths.clone(),
        home: opts.home.clone(),
        load: opts.load_options(),
        daemon: probe,
        docker_host: opts.env.get("DOCKER_HOST").cloned(),
        ignore_pids: vec![i32::try_from(std::process::id()).unwrap_or(0)],
    };
    let mut report = doctor::run(input.clone(), false).await;
    if a.fix && report.fixable().next().is_some() {
        let cx = Arc::new(DoctorCtx::load(input.clone()));
        let fixed = fix::apply(
            &cx,
            &report,
            fix::FixOptions {
                kill_foreign: false,
            },
        )
        .await;
        if fixed.iter().any(|f| f.action == "kill_orphan") {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        report = doctor::run(input, false).await;
        report.fixed = fixed;
    }
    Ok(redacted(to_json(&report)?))
}

/// `start` / `stop` / `restart` / `reset` / `watch_*` / `get_health` /
/// `get_outputs`: typed params, one RPC.
pub async fn simple(b: &Backend, actor: &str, tool: &str, args: &Value) -> Result<Value, Error> {
    match tool {
        "start" => {
            let p: StartParams = parse(tool, args)?;
            proxy(b, actor, Method::START, &p, LONG).await
        }
        "stop" => {
            let p: StopParams = parse(tool, args)?;
            proxy(b, actor, Method::STOP, &p, LONG).await
        }
        "restart" => {
            let p: RestartParams = parse(tool, args)?;
            proxy(b, actor, Method::RESTART, &p, LONG).await
        }
        "reset" => {
            let a: ResetArgs = parse(tool, args)?;
            if a.stems.is_empty() {
                return Err(Error::usage(
                    "`reset` needs the stems to reset",
                    "name them in `stems` (resetting everything at once is not offered to agents)",
                ));
            }
            let p = ResetParams { stems: a.stems };
            proxy(b, actor, Method::RESET, &p, LONG).await
        }
        "watch_pause" | "watch_resume" => {
            let p: WatchPauseParams = parse(tool, args)?;
            let m = if tool == "watch_pause" {
                Method::WATCH_PAUSE
            } else {
                Method::WATCH_RESUME
            };
            proxy(b, actor, m, &p, stems_api::client::CALL_TIMEOUT).await
        }
        "get_health" => {
            let p: HealthParams = parse(tool, args)?;
            let v = proxy(
                b,
                actor,
                Method::HEALTH,
                &p,
                stems_api::client::CALL_TIMEOUT,
            )
            .await?;
            let _: HealthResult = serde_json::from_value(v.clone())
                .map_err(|e| Error::internal(format!("invalid health result: {e}")))?;
            Ok(v)
        }
        "get_outputs" => {
            let a: GetOutputsArgs = parse(tool, args)?;
            let p = OutputsParams {
                stems: a.stems,
                reveal: false,
            };
            proxy(
                b,
                actor,
                Method::OUTPUTS,
                &p,
                stems_api::client::CALL_TIMEOUT,
            )
            .await
        }
        other => Err(Error::internal(format!("no simple handler for `{other}`"))),
    }
}

/// `up` params from the tool arguments.
pub fn up_params(a: &UpArgs) -> Result<UpParams, Error> {
    if a.profile.is_some() && !a.stems.is_empty() {
        return Err(Error::usage(
            "`profile` cannot be combined with `stems`",
            "name the stems to start, or pass the profile alone",
        ));
    }
    Ok(UpParams {
        stems: a.stems.clone(),
        profile: a.profile.clone(),
        detach: true,
        timeout_ms: a.timeout_ms,
        fresh: a.fresh,
        ..UpParams::default()
    })
}

/// `down` params from the tool arguments.
pub fn down_params(a: &DownArgs) -> DownParams {
    DownParams {
        stems: a.stems.clone(),
        all: a.all,
        volumes: a.volumes,
        timeout_ms: None,
    }
}

/// `run_script` params from the tool arguments.
pub fn run_script_params(
    stem: Option<String>,
    name: String,
    args: Map<String, Value>,
    start_deps: bool,
) -> RunScriptParams {
    RunScriptParams {
        stem,
        name,
        args: ScriptArgsInput::Object(args),
        wait: true,
        start_deps,
        ready_timeout_ms: None,
    }
}

/// Whether a long-running call's result is a failure the agent must see as
/// a tool error (`ok: false`).
pub fn result_failed(v: &Value) -> bool {
    v.get("ok").and_then(Value::as_bool) == Some(false)
}

/// Every core tool name mapped to its kind (for tests and docs).
pub fn kinds() -> BTreeMap<&'static str, Kind> {
    CORE_TOOLS.iter().map(|t| (t.name, t.kind)).collect()
}
