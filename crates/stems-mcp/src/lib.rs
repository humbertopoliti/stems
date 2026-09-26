//! The stems MCP server (`stems mcp`, deliverable 31, FR-AI-1..4;
//! `docs/mcp.md`).
//!
//! [`StemsMcp`] implements rmcp's `ServerHandler`: every MCP tool call is
//! proxied to the workspace daemon over the local JSON-RPC socket
//! (`docs/protocol.md`) with the actor `mcp:<client name>` (FR-AI-4); a few
//! read-only tools (`list_stems`, `get_config`, config-only `get_graph`,
//! `doctor`) also work without a daemon. Custom scripts are published as
//! extra tools named `<stem>__<script>` (FR-AI-2), destructive calls are
//! gated by `confirm: true` **and** `agent.allow_destructive` (FR-AI-3).
//!
//! * [`tools`] — the core tool catalogue (names, schemas, annotations) and
//!   the dispatch of a call.
//! * [`gating`] — the destructive-action decision table and the
//!   `agent.allowed_tools` / `denied_tools` globs.
//! * [`backend`] — finding, connecting to and (with `--auto-start`)
//!   starting and stopping the workspace daemon.
//! * [`logs`] — `get_logs` pagination (opaque cursors, ≤ 500 lines a call).
//! * [`resources`], [`prompts`] — `stems://…` resources and the prompts.
//! * [`redact`] — secret redaction of config JSON.
//! * [`schema`] — JSON Schemas of tool inputs (schemars, field docs become
//!   descriptions).

pub mod backend;
pub mod gating;
pub mod logs;
pub mod prompts;
pub mod redact;
pub mod resources;
pub mod schema;
pub mod server;
pub mod tools;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use stems_core::Error;

pub use server::StemsMcp;

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

/// Default port of `--transport http`.
pub const DEFAULT_HTTP_PORT: u16 = 7070;

/// How `stems mcp` was invoked.
#[derive(Clone, Debug)]
pub struct McpOptions {
    /// `--workspace` (directory or `stems.yaml`), relative to `cwd`.
    pub workspace: Option<PathBuf>,
    /// Current directory (workspace discovery).
    pub cwd: PathBuf,
    /// Environment (`STEMS_WORKSPACE`, `${env.X}`, `HOME`, `DOCKER_HOST`).
    pub env: HashMap<String, String>,
    /// Stems home (absolute).
    pub home: PathBuf,
    /// The `stems` binary, spawned as `stems daemon …` by `--auto-start`.
    pub binary: PathBuf,
    /// Start the daemon when a tool needs it and it is not running; stop
    /// it again when the client disconnects if nothing runs.
    pub auto_start: bool,
}

impl McpOptions {
    /// Config loading options for this invocation.
    pub fn load_options(&self) -> stems_config::LoadOptions {
        stems_config::LoadOptions {
            workspace: self.workspace.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            skip_local: false,
        }
    }
}

/// The transport `stems mcp` serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// MCP over stdin/stdout (the default).
    Stdio,
    /// Streamable HTTP on `127.0.0.1:<port>` (local only, no auth).
    Http {
        /// TCP port.
        port: u16,
    },
}

/// Serve MCP until the client disconnects (stdio) or the process is
/// interrupted (HTTP). With `--auto-start`, a daemon this server started is
/// shut down afterwards when no stem is running.
pub async fn run(opts: McpOptions, transport: Transport) -> Result<(), Error> {
    let server = StemsMcp::new(opts);
    let result = match transport {
        Transport::Stdio => server::serve_stdio(server.clone()).await,
        Transport::Http { port } => {
            server::serve_http(server.clone(), SocketAddr::from(([127, 0, 0, 1], port))).await
        }
    };
    server.backend().stop_if_auto_started().await;
    result
}
