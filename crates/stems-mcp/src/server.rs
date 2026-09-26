//! The rmcp `ServerHandler`: `tools/list`, `tools/call`, resources,
//! prompts, and the transports.
//!
//! Every request's actor is `mcp:<client name>` from the client's
//! `initialize` (or, on the stateless 2026-07-28 lifecycle, the request's
//! `_meta` client info); [`UNKNOWN_CLIENT`] when the client sent none.

use std::net::SocketAddr;
use std::sync::Arc;

use futures::StreamExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, GetPromptRequestParams,
    GetPromptResponse, Implementation, ListPromptsResult, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProgressNotificationParam,
    ProgressToken, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData as McpError, Peer, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};
use stems_api::client::Client;
use stems_api::{DaemonStatus, Event, EventKind, Method};
use stems_core::Error;
use tokio::sync::Mutex;

use crate::McpOptions;
use crate::backend::Backend;
use crate::gating::{self, ToolFilter};
use crate::tools::{self, DownArgs, LONG, RunScriptArgs, UpArgs};
use crate::{prompts, resources};

/// Client name used when the client did not say who it is.
pub const UNKNOWN_CLIENT: &str = "unknown";

/// Server instructions sent in `initialize`.
pub const INSTRUCTIONS: &str = "stems manages this workspace's local services (stems). \
Read state with list_stems, get_status, get_graph, get_logs (paginate with next_cursor), \
get_events, get_health, get_metrics, get_config. Act with start, stop, restart, up, down, \
run_script and the <stem>__<script> tools (custom scripts). Destructive calls (down with \
all/volumes, up with fresh, reset, doctor with fix) need confirm: true after asking the user, \
and the workspace setting agent.allow_destructive. Every action is recorded with your client \
name as actor.";

struct Inner {
    backend: Backend,
    /// Peers seen in `notifications/initialized` (for `list_changed`).
    peers: Mutex<Vec<Peer<RoleServer>>>,
}

/// The MCP server. Cheap to clone (shared state).
#[derive(Clone)]
pub struct StemsMcp {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for StemsMcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StemsMcp")
            .field("backend", &self.inner.backend)
            .finish_non_exhaustive()
    }
}

/// `mcp:<client name>` for a request.
pub fn actor_of(ctx: &RequestContext<RoleServer>) -> String {
    let name = ctx
        .client_info()
        .map(|i| i.name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| UNKNOWN_CLIENT.to_string());
    format!("mcp:{name}")
}

/// A successful tool result: the JSON as pretty text.
pub fn ok_result(v: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(v).unwrap_or_default(),
    )])
}

/// A failed tool result: the stems error JSON as pretty text, `isError`.
pub fn err_result(e: &Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::to_string_pretty(e).unwrap_or_default(),
    )])
}

/// A result that is data but reports a failure (`ok: false`): `isError`.
pub fn failed_result(v: &Value) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::to_string_pretty(v).unwrap_or_default(),
    )])
}

/// One progress message for an event, or `None` for events not reported.
pub fn progress_message(e: &Event) -> Option<String> {
    let stem = e.stem.as_deref().unwrap_or("-");
    let k = e.kind.as_str();
    if k == EventKind::STEM_STATE {
        let mut s = format!(
            "{stem}: {} -> {}",
            e.from.as_deref().unwrap_or("-"),
            e.to.as_deref().unwrap_or("-")
        );
        if let Some(r) = &e.reason {
            s.push_str(&format!(" ({r})"));
        }
        Some(s)
    } else if k == EventKind::SCRIPT_STARTED || k == EventKind::SCRIPT_FINISHED {
        let script = e.data.get("script").and_then(Value::as_str).unwrap_or("?");
        Some(format!("{stem}: {k} {script}"))
    } else if k == EventKind::UP_STARTED
        || k == EventKind::UP_FINISHED
        || k == EventKind::DOWN_STARTED
        || k == EventKind::DOWN_FINISHED
        || k == EventKind::STEM_PORT_ALLOCATED
        || k == EventKind::PROCESS_EXITED
        || k == EventKind::STEM_HEALTH
        || k == EventKind::DOCKER_PULL
    {
        Some(format!("{stem}: {k}"))
    } else {
        None
    }
}

impl StemsMcp {
    /// A server for `opts`.
    pub fn new(opts: McpOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                backend: Backend::new(opts),
                peers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The daemon backend.
    pub fn backend(&self) -> &Backend {
        &self.inner.backend
    }

    /// Tell connected clients that the tool list changed
    /// (`notifications/tools/list_changed`). Deliverable 33 calls this when
    /// the config is reloaded; `tools/list` itself always rebuilds the list.
    pub async fn notify_tools_changed(&self) {
        let peers: Vec<Peer<RoleServer>> = {
            let mut p = self.inner.peers.lock().await;
            p.retain(|x| !x.is_transport_closed());
            p.clone()
        };
        for p in peers {
            let _ = p.notify_tool_list_changed().await;
        }
    }

    fn filter(&self) -> (ToolFilter, bool) {
        match self.backend().load_config() {
            Ok(r) => (
                ToolFilter::new(&r.workspace.agent),
                r.workspace.agent.allow_destructive,
            ),
            Err(_) => (ToolFilter::default(), false),
        }
    }

    /// The custom-script tools: the running daemon's catalogue, else the
    /// config on disk.
    async fn script_catalog(&self, actor: &str, start: bool) -> Vec<stems_api::CatalogScript> {
        let b = self.backend();
        let conn = if start {
            b.connect(actor).await
        } else {
            b.connect_existing(actor).await
        };
        if let Ok(c) = conn
            && let Ok(cat) = c
                .call::<stems_api::ScriptCatalogResult>(Method::SCRIPT_CATALOG, json!({}))
                .await
        {
            return tools::custom_scripts(cat);
        }
        match b.load_config() {
            Ok(r) => tools::custom_scripts(tools::local_catalog(&r.workspace)),
            Err(_) => Vec::new(),
        }
    }

    /// Every tool this server exposes right now (core + custom scripts,
    /// filtered by `agent.allowed_tools` / `denied_tools`).
    pub async fn tools(&self, actor: &str) -> Vec<Tool> {
        let (filter, _) = self.filter();
        let mut out: Vec<Tool> = tools::CORE_TOOLS
            .iter()
            .filter(|t| filter.allows(t.name))
            .map(tools::core_tool)
            .collect();
        for s in self.script_catalog(actor, false).await {
            if filter.allows(&s.mcp_tool) && !out.iter().any(|t| t.name == s.mcp_tool) {
                out.push(tools::script_tool(&s));
            }
        }
        out
    }

    /// Run one tool call and turn the outcome into a tool result.
    pub async fn call(
        &self,
        name: &str,
        args: Value,
        progress: Option<Progress>,
        actor: &str,
    ) -> CallToolResult {
        match self.call_inner(name, &args, progress, actor).await {
            Ok(Outcome::Ok(v)) => ok_result(&v),
            Ok(Outcome::Failed(v)) => failed_result(&v),
            Err(e) => err_result(&e),
        }
    }

    async fn call_inner(
        &self,
        name: &str,
        args: &Value,
        progress: Option<Progress>,
        actor: &str,
    ) -> Result<Outcome, Error> {
        let (filter, allow_destructive) = self.filter();
        if !filter.allows(name) {
            return Err(ToolFilter::denied_error(name));
        }
        let b = self.backend();
        let args = match args {
            Value::Null => Value::Object(Map::new()),
            v => v.clone(),
        };
        let is_core = tools::CORE_TOOLS.iter().any(|t| t.name == name);
        if is_core {
            gating::check(name, &args, allow_destructive)?;
        }
        let ok = |v: Value| Ok(Outcome::Ok(v));
        match name {
            "list_stems" => ok(tools::list_stems(b, actor).await?),
            "get_status" => ok(tools::get_status(b, actor, tools::parse(name, &args)?).await?),
            "get_graph" => ok(tools::get_graph(b, actor, tools::parse(name, &args)?).await?),
            "get_logs" => ok(tools::get_logs(b, actor, tools::parse(name, &args)?).await?),
            "get_metrics" => ok(tools::get_metrics(b, actor, tools::parse(name, &args)?).await?),
            "get_events" => ok(tools::get_events(b, actor, tools::parse(name, &args)?).await?),
            "get_config" => ok(tools::get_config(b, &tools::parse(name, &args)?)?),
            "doctor" => ok(tools::doctor(b, actor, tools::parse(name, &args)?).await?),
            "start" | "stop" | "restart" | "reset" | "watch_pause" | "watch_resume"
            | "get_health" | "get_outputs" => {
                let v = tools::simple(b, actor, name, &args).await?;
                Ok(if tools::result_failed(&v) {
                    Outcome::Failed(v)
                } else {
                    Outcome::Ok(v)
                })
            }
            "up" => {
                let a: UpArgs = tools::parse(name, &args)?;
                let p = tools::up_params(&a)?;
                self.long(actor, Method::UP, json!(p), progress).await
            }
            "down" => {
                let a: DownArgs = tools::parse(name, &args)?;
                let p = tools::down_params(&a);
                self.long(actor, Method::DOWN, json!(p), progress).await
            }
            "run_script" => {
                let a: RunScriptArgs = tools::parse(name, &args)?;
                let p = tools::run_script_params(a.stem, a.name, a.args, a.start_deps);
                self.long(actor, Method::RUN_SCRIPT, json!(p), progress)
                    .await
            }
            _ => {
                let scripts = self.script_catalog(actor, true).await;
                let Some(s) = scripts.into_iter().find(|s| s.mcp_tool == name) else {
                    return Err(Error::usage(
                        format!("unknown tool `{name}`"),
                        "call tools/list for the tools of this workspace",
                    )
                    .with_details(json!({ "tool": name })));
                };
                let obj = match args {
                    Value::Object(m) => m,
                    _ => {
                        return Err(Error::usage(
                            format!("the arguments of `{name}` must be an object"),
                            "see the tool's inputSchema",
                        ));
                    }
                };
                let p = tools::run_script_params(s.entry.stem, s.entry.name, obj, false);
                self.long(actor, Method::RUN_SCRIPT, json!(p), progress)
                    .await
            }
        }
    }

    /// A long daemon call (`up`, `down`, `run_script`): progress from the
    /// event stream while it runs, then the summary. `ok: false` results
    /// are tool errors.
    async fn long(
        &self,
        actor: &str,
        method: Method,
        params: Value,
        progress: Option<Progress>,
    ) -> Result<Outcome, Error> {
        let c = self.backend().connect(actor).await?;
        let events = match &progress {
            Some(_) => subscribe(&c).await.ok(),
            None => None,
        };
        let call = c.call_with_timeout::<Value>(method, &params, LONG);
        tokio::pin!(call);
        let v = match (events, progress) {
            (Some(mut events), Some(p)) => {
                let mut n = 0u32;
                loop {
                    tokio::select! {
                        r = &mut call => break r?,
                        ev = events.next() => match ev {
                            Some(e) => {
                                if let Some(msg) = progress_message(&e) {
                                    n += 1;
                                    let _ = p
                                        .peer
                                        .notify_progress(
                                            ProgressNotificationParam::new(p.token.clone(), f64::from(n))
                                                .with_message(msg),
                                        )
                                        .await;
                                }
                            }
                            None => break (&mut call).await?,
                        },
                    }
                }
            }
            _ => call.await?,
        };
        Ok(if tools::result_failed(&v) {
            Outcome::Failed(v)
        } else {
            Outcome::Ok(v)
        })
    }
}

async fn subscribe(c: &Client) -> Result<stems_api::client::EventStream, Error> {
    let st: DaemonStatus = c.call(Method::DAEMON_STATUS, json!({})).await?;
    c.subscribe_events(Some(st.last_seq)).await
}

/// Where progress notifications of a call go.
#[derive(Clone)]
pub struct Progress {
    /// The client.
    pub peer: Peer<RoleServer>,
    /// The request's progress token.
    pub token: ProgressToken,
}

/// A tool call's result.
enum Outcome {
    /// Success.
    Ok(Value),
    /// The call ran but failed (`ok: false`): data plus `isError`.
    Failed(Value),
}

fn mcp_error(e: &Error) -> McpError {
    let data = serde_json::to_value(e).ok();
    match e.code {
        stems_core::ErrorCode::Usage | stems_core::ErrorCode::UnknownStem => {
            McpError::invalid_params(e.message.clone(), data)
        }
        _ => McpError::internal_error(e.message.clone(), data),
    }
}

impl ServerHandler for StemsMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("stems", stems_api::VERSION))
        .with_instructions(INSTRUCTIONS)
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        self.inner.peers.lock().await.push(context.peer);
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let actor = actor_of(&context);
        Ok(ListToolsResult::with_all_items(self.tools(&actor).await))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let actor = actor_of(&context);
        let progress = context.meta.get_progress_token().map(|token| Progress {
            peer: context.peer.clone(),
            token,
        });
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        Ok(self
            .call(&request.name, args, progress, &actor)
            .await
            .into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(resources::list(
            self.backend(),
        )))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(
            resources::templates(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let actor = actor_of(&context);
        match resources::read(self.backend(), &actor, &request.uri).await {
            Ok(c) => Ok(ReadResourceResult::new(vec![c]).into()),
            Err(e) => Err(mcp_error(&e)),
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(prompts::list()))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        let actor = actor_of(&context);
        match prompts::get(self.backend(), &actor, &request.name, &request.arguments).await {
            Ok(r) => Ok(r.into()),
            Err(e) => Err(mcp_error(&e)),
        }
    }
}

/// Serve over stdin/stdout until the client disconnects.
pub async fn serve_stdio(server: StemsMcp) -> Result<(), Error> {
    let running = server
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| Error::internal(format!("MCP initialisation failed: {e}")))?;
    let _ = running.waiting().await;
    Ok(())
}

/// Serve streamable HTTP on `addr` (127.0.0.1 only) until SIGINT/SIGTERM.
pub async fn serve_http(server: StemsMcp, addr: SocketAddr) -> Result<(), Error> {
    use hyper_util::rt::TokioIo;
    use hyper_util::service::TowerToHyperService;
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        Error::usage(
            format!("cannot listen on {addr}: {e}"),
            "pick another port with --port",
        )
    })?;
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let stop = async {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            (Ok(mut i), Ok(mut t)) => {
                tokio::select! {
                    _ = i.recv() => {},
                    _ = t.recv() => {},
                }
            }
            _ => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(stop);
    loop {
        tokio::select! {
            () = &mut stop => return Ok(()),
            conn = listener.accept() => {
                let Ok((stream, _)) = conn else { continue };
                let svc = TowerToHyperService::new(service.clone());
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), svc)
                        .await;
                });
            }
        }
    }
}
