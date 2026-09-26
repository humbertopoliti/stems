//! `stems mcp [--transport stdio|http] [--port 7070] [--auto-start]`
//! (deliverable 31, FR-AI-1..4): runs the [`stems_mcp`] server for the
//! workspace (`--workspace`, `STEMS_WORKSPACE` or the cwd). Nothing is
//! printed on stdout besides the MCP protocol; the command returns when the
//! client disconnects (stdio) or on SIGINT/SIGTERM (http).

use stems_core::Error;
use stems_mcp::{DEFAULT_HTTP_PORT, McpOptions, Transport};

use crate::cli::{McpArgs, McpTransport};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Run `stems mcp`.
pub fn run(ctx: &Ctx, args: &McpArgs) -> CommandOutput {
    let binary = match std::env::current_exe() {
        Ok(b) => b,
        Err(e) => {
            return CommandOutput::failed(Error::internal(format!(
                "cannot locate the stems binary: {e}"
            )));
        }
    };
    let opts = McpOptions {
        workspace: ctx.global.workspace.clone(),
        cwd: ctx.cwd.clone(),
        env: ctx.env.clone(),
        home: ctx.home(),
        binary,
        auto_start: args.auto_start,
    };
    let transport = match args.transport {
        McpTransport::Stdio => Transport::Stdio,
        McpTransport::Http => Transport::Http {
            port: args.port.unwrap_or(DEFAULT_HTTP_PORT),
        },
    };
    // A multi-threaded runtime: tool calls run concurrently with the
    // protocol loop and event subscriptions.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let result = match rt {
        Ok(rt) => rt.block_on(stems_mcp::run(opts, transport)),
        Err(e) => Err(Error::internal(format!("tokio runtime: {e}"))),
    };
    match result {
        Ok(()) => CommandOutput::data(serde_json::Value::Null).with_raw(""),
        Err(e) => CommandOutput::failed(e),
    }
}
