//! The request-dispatch seam: [`Handler`] (what the server calls) and
//! [`SupervisorHooks`] (the slot deliverable 10 fills).

use serde_json::Value;
use stems_core::Error;

/// Per-request context passed to handlers.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestCtx {
    /// `cli:<user>`, `tui:<user>`, `mcp:<client>`.
    pub actor: String,
    /// stems version the client reported.
    pub client_version: String,
    /// API version the client reported (already checked by the server).
    pub api_version: u32,
    /// JSON-RPC id of the request.
    pub request_id: Value,
}

impl RequestCtx {
    /// Context for work the daemon does on its own behalf.
    pub fn daemon() -> Self {
        Self {
            actor: stems_api::DAEMON_ACTOR.to_string(),
            client_version: stems_api::VERSION.to_string(),
            api_version: stems_api::API_VERSION,
            request_id: Value::Null,
        }
    }
}

/// Answers one request. The server calls it for every method except
/// `subscribe_events` (which it streams itself).
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Handle `method` with `params`; the `Ok` value becomes `result`, the
    /// `Err` becomes a JSON-RPC error whose `data` is the stems error.
    async fn handle(&self, ctx: RequestCtx, method: &str, params: Value) -> Result<Value, Error>;
}

/// Hooks the supervisor (deliverable 10) installs into the daemon.
#[async_trait::async_trait]
pub trait SupervisorHooks: Send + Sync {
    /// Handle a method the daemon's built-ins do not know (`up`, `down`, ...).
    /// `None` means "not mine" (the daemon then answers `NOT_IMPLEMENTED`).
    async fn handle(
        &self,
        ctx: &RequestCtx,
        method: &str,
        params: &Value,
    ) -> Option<Result<Value, Error>>;

    /// Called once on the orderly shutdown path, after `daemon.stopping` and
    /// before the socket and lock are removed (run `down` here).
    async fn shutdown(&self);

    /// Called once at startup, after the workspace loaded and before the
    /// socket serves requests, with the previous run's state file (crash
    /// recovery, deliverable 11).
    async fn recover(&self, _previous: Option<crate::state::StateFile>) {}
}
