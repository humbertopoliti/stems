//! Wire types for the local JSON-RPC API and event stream (`docs/protocol.md`).
//!
//! The protocol is JSON-RPC 2.0, one JSON object per line, over the daemon's
//! Unix socket. Every request carries a [`RequestMeta`] (actor, client version,
//! API version). Subscriptions (`subscribe_events`) answer with a normal
//! [`Response`] and then stream [`Notification`] frames on the same connection
//! until the client closes it.
//!
//! This crate does no I/O except in the optional [`client`] module
//! (feature `client`, on by default).

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_core::{Error, ErrorCode};

#[cfg(feature = "client")]
pub mod client;

/// Version of the wire protocol. Bumped on any incompatible change.
pub const API_VERSION: u32 = 1;

/// The `jsonrpc` member of every frame.
pub const JSONRPC_VERSION: &str = "2.0";

/// The stems version this crate was built as (client and daemon compare it).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

macro_rules! string_newtype {
    ($(#[$doc:meta])* $name:ident { $( $(#[$cdoc:meta])* $konst:ident = $val:literal; )* }) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(pub Cow<'static, str>);

        impl $name {
            $( $(#[$cdoc])* pub const $konst: $name = $name(Cow::Borrowed($val)); )*

            /// Every name defined by this build, in declaration order.
            pub const KNOWN: &'static [&'static str] = &[ $( $val, )* ];

            /// Any name (known or not).
            pub fn new(s: impl Into<String>) -> Self {
                Self(Cow::Owned(s.into()))
            }

            /// The string form.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&'static str> for $name {
            fn from(s: &'static str) -> Self {
                Self(Cow::Borrowed(s))
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(Cow::Owned(s))
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<$name> for str {
            fn eq(&self, other: &$name) -> bool {
                *self == *other.0
            }
        }

        impl PartialEq<$name> for &str {
            fn eq(&self, other: &$name) -> bool {
                **self == *other.0
            }
        }
    };
}

string_newtype! {
    /// An RPC method name. Open: later deliverables add methods without
    /// changing this type; unknown methods get `NOT_IMPLEMENTED`.
    Method {
        /// Liveness check; returns `{"pong": true}`.
        PING = "ping";
        /// [`DaemonInfo`].
        INFO = "info";
        /// Stream events ([`SubscribeEventsParams`]); ack then `event` notifications.
        SUBSCRIBE_EVENTS = "subscribe_events";
        /// Replay buffered events ([`EventsParams`] -> [`EventsResult`]).
        EVENTS = "events";
        /// Orderly daemon shutdown ([`ShutdownResult`]).
        SHUTDOWN = "shutdown";
        /// Load + validate a workspace ([`LoadWorkspaceParams`] -> [`WorkspaceLoaded`]).
        LOAD_WORKSPACE = "load_workspace";
        /// [`DaemonStatus`].
        DAEMON_STATUS = "daemon_status";
        // --- reserved for later deliverables ---------------------------------
        /// Reserved (10).
        UP = "up";
        /// Reserved (10).
        DOWN = "down";
        /// Reserved (10).
        START = "start";
        /// Reserved (10).
        STOP = "stop";
        /// Reserved (10).
        RESTART = "restart";
        /// Reserved (10).
        STATUS = "status";
        /// Reserved (12).
        LOGS = "logs";
        /// Reserved (12): streaming, like `subscribe_events`, with `log` notifications.
        SUBSCRIBE_LOGS = "subscribe_logs";
        /// Reserved (13).
        RUN_SCRIPT = "run_script";
        // --- debug (only with STEMS_DEBUG_RPC=1) -------------------------------
        /// Start a raw process through the runtime.
        DEBUG_START_RAW = "_debug.start_raw";
        /// Stop a raw process.
        DEBUG_STOP_RAW = "_debug.stop_raw";
        /// Describe a raw process.
        DEBUG_DESCRIBE = "_debug.describe";
    }
}

string_newtype! {
    /// The kind of an [`Event`]. Open: later deliverables add kinds.
    EventKind {
        /// The daemon is listening.
        DAEMON_STARTED = "daemon.started";
        /// Orderly shutdown began.
        DAEMON_STOPPING = "daemon.stopping";
        /// Last event of a daemon (best effort).
        DAEMON_STOPPED = "daemon.stopped";
        /// A workspace was loaded and validated.
        WORKSPACE_LOADED = "workspace.loaded";
        /// A stem changed state (`from` -> `to`).
        STEM_STATE = "stem.state";
        /// An `auto` port was allocated.
        STEM_PORT_ALLOCATED = "stem.port_allocated";
        /// A stem from a previous daemon run was re-attached.
        STEM_ADOPTED = "stem.adopted";
        /// A stem recorded in state was found dead on recovery.
        STEM_RECOVERED_DEAD = "stem.recovered_dead";
        /// A stem is being restarted by policy.
        STEM_RESTARTING = "stem.restarting";
        /// A stem exhausted its restart budget.
        STEM_GAVE_UP = "stem.gave_up";
        /// A health probe result changed.
        STEM_HEALTH = "stem.health";
        /// A process exited (`data: {code, signal}`).
        PROCESS_EXITED = "process.exited";
        /// A line of output of a debug-started process (`data: {stream, text}`).
        PROCESS_OUTPUT = "process.output";
        /// `up` began.
        UP_STARTED = "up.started";
        /// `up` finished.
        UP_FINISHED = "up.finished";
        /// `down` began.
        DOWN_STARTED = "down.started";
        /// `down` finished.
        DOWN_FINISHED = "down.finished";
        /// A script was queued.
        SCRIPT_QUEUED = "script.queued";
        /// A script started.
        SCRIPT_STARTED = "script.started";
        /// A script finished.
        SCRIPT_FINISHED = "script.finished";
        /// A file watch fired.
        WATCH_TRIGGERED = "watch.triggered";
        /// Watching was paused.
        WATCH_PAUSED = "watch.paused";
        /// Watch config changed.
        WATCH_RECONFIGURED = "watch.reconfigured";
        /// The config changed on disk.
        CONFIG_CHANGED = "config.changed";
        /// The config on disk is invalid.
        CONFIG_INVALID = "config.invalid";
    }
}

/// JSON-RPC error codes used by the daemon.
pub mod rpc_code {
    /// Invalid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Unknown method (`NOT_IMPLEMENTED`).
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Bad params (`USAGE`).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Any other stems error (see `error.data.code`).
    pub const APPLICATION: i64 = -32000;
    /// Client/daemon API version skew (`DAEMON_VERSION_MISMATCH`).
    pub const VERSION_MISMATCH: i64 = -32001;

    use stems_core::ErrorCode;

    /// The JSON-RPC code for a stems error code.
    pub fn for_error(code: ErrorCode) -> i64 {
        match code {
            ErrorCode::NotImplemented => METHOD_NOT_FOUND,
            ErrorCode::Usage => INVALID_PARAMS,
            ErrorCode::DaemonVersionMismatch => VERSION_MISMATCH,
            _ => APPLICATION,
        }
    }
}

/// Who is calling and with which versions. Required on every request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestMeta {
    /// `cli:<user>`, `tui:<user>`, `mcp:<client>`.
    pub actor: String,
    /// stems version of the client.
    pub client_version: String,
    /// [`API_VERSION`] of the client.
    pub api_version: u32,
}

impl RequestMeta {
    /// Meta for this build with the given actor.
    pub fn new(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            client_version: VERSION.to_string(),
            api_version: API_VERSION,
        }
    }
}

/// A JSON-RPC 2.0 request with stems' `meta` extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Request id (number or string), echoed in the response.
    pub id: Value,
    /// Method name.
    pub method: Method,
    /// Method parameters (an object; `{}` when there are none).
    #[serde(default = "empty_object")]
    pub params: Value,
    /// Caller identity and versions.
    pub meta: RequestMeta,
}

impl Request {
    /// A request with `jsonrpc: "2.0"`.
    pub fn new(id: impl Into<Value>, method: Method, params: Value, meta: RequestMeta) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method,
            params,
            meta,
        }
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

/// JSON-RPC error object; `data` is always the full stems [`Error`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    /// JSON-RPC code ([`rpc_code`]).
    pub code: i64,
    /// Same as `data.message`.
    pub message: String,
    /// The stems error (`code`, `message`, `path`, `location`, `hint`, `details`).
    #[schemars(with = "Value")]
    pub data: Error,
}

impl From<Error> for RpcError {
    fn from(e: Error) -> Self {
        Self {
            code: rpc_code::for_error(e.code),
            message: e.message.clone(),
            data: e,
        }
    }
}

/// A JSON-RPC 2.0 response: exactly one of `result` / `error`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The request id (`null` when the request could not be parsed).
    pub id: Value,
    /// Success value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// Success response.
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: Value, error: impl Into<RpcError>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error.into()),
        }
    }

    /// Error response with an explicit JSON-RPC code (parse / invalid request).
    pub fn err_with_code(id: Value, code: i64, error: Error) -> Self {
        Self::err(
            id,
            RpcError {
                code,
                message: error.message.clone(),
                data: error,
            },
        )
    }

    /// `Ok(result)` (`null` when absent) or the stems error.
    pub fn into_result(self) -> Result<Value, Error> {
        match self.error {
            Some(e) => Err(e.data),
            None => Ok(self.result.unwrap_or(Value::Null)),
        }
    }
}

/// A JSON-RPC notification (no id): subscription frames (`event`, later `log`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// [`NOTIFY_EVENT`] or (later) `log`.
    pub method: String,
    /// Payload ([`Event`] for `event`).
    pub params: Value,
}

/// Notification method carrying one [`Event`].
pub const NOTIFY_EVENT: &str = "event";
/// Notification method carrying one log line (deliverable 12).
pub const NOTIFY_LOG: &str = "log";

impl Notification {
    /// An `event` notification.
    pub fn event(ev: &Event) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: NOTIFY_EVENT.to_string(),
            params: serde_json::to_value(ev).unwrap_or(Value::Null),
        }
    }
}

/// One entry of the daemon's event log (REQUIREMENTS §6.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// When it happened (UTC, RFC 3339).
    pub ts: DateTime<Utc>,
    /// Monotonic per daemon run, starting at 1.
    pub seq: u64,
    /// What happened.
    pub kind: EventKind,
    /// The stem concerned, if any.
    #[serde(default)]
    pub stem: Option<String>,
    /// Previous state (state transitions).
    #[serde(default)]
    pub from: Option<String>,
    /// New state (state transitions).
    #[serde(default)]
    pub to: Option<String>,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
    /// Who caused it (`cli:<user>`, `daemon`, ...).
    pub actor: String,
    /// Kind-specific payload (`{}` when none).
    #[serde(default = "empty_object")]
    pub data: Value,
}

/// Actor of events the daemon causes itself.
pub const DAEMON_ACTOR: &str = "daemon";

/// `info` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfo {
    /// stems version of the daemon.
    pub version: String,
    /// [`API_VERSION`] of the daemon.
    pub api_version: u32,
    /// Workspace root the daemon owns.
    pub workspace: Option<PathBuf>,
    /// Daemon pid.
    pub pid: u32,
    /// Opaque process start time (same value as in the lock file).
    pub start_time: u64,
    /// Seconds since start.
    pub uptime_s: u64,
    /// Wall-clock start.
    pub started_at: DateTime<Utc>,
}

/// `daemon_status` result: [`DaemonInfo`] plus runtime counters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonStatus {
    /// Identity and versions.
    #[serde(flatten)]
    pub info: DaemonInfo,
    /// Name of the loaded workspace (`None` until `load_workspace` succeeds).
    pub workspace_name: Option<String>,
    /// Number of stems in the loaded workspace.
    pub stem_count: usize,
    /// Highest event seq emitted so far.
    pub last_seq: u64,
    /// Open event subscriptions.
    pub subscribers: usize,
    /// Whether `_debug.*` RPCs are enabled.
    pub debug_rpc: bool,
}

/// `subscribe_events` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeEventsParams {
    /// Replay buffered events with `seq > since_seq` first. `None`: live only.
    #[serde(default)]
    pub since_seq: Option<u64>,
}

/// `subscribe_events` ack (the response before the notifications).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeAck {
    /// Always true.
    pub subscribed: bool,
    /// Highest seq at subscription time.
    pub last_seq: u64,
}

/// `events` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EventsParams {
    /// Only events with `seq > since_seq` (default: all buffered).
    #[serde(default)]
    pub since_seq: Option<u64>,
    /// At most this many (the oldest first).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `events` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventsResult {
    /// Matching buffered events, oldest first.
    pub events: Vec<Event>,
    /// Highest seq emitted so far.
    pub last_seq: u64,
}

/// `load_workspace` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoadWorkspaceParams {
    /// Directory or `stems.yaml` path (absolute; relative paths resolve against the daemon's cwd).
    pub path: PathBuf,
}

/// `load_workspace` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceLoaded {
    /// Workspace root.
    pub root: PathBuf,
    /// Workspace name.
    pub name: String,
    /// Stem names, in declaration order.
    pub stems: Vec<String>,
    /// Config files read, in merge order.
    pub sources: Vec<PathBuf>,
}

/// `shutdown` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ShutdownResult {
    /// Always true: the orderly shutdown path has been triggered.
    pub stopping: bool,
}

/// `DAEMON_VERSION_MISMATCH` error for the given versions.
pub fn version_mismatch(
    client_version: &str,
    client_api: u32,
    daemon_version: &str,
    daemon_api: u32,
) -> Error {
    Error::new(
        ErrorCode::DaemonVersionMismatch,
        format!(
            "the running daemon is stems {daemon_version} (api {daemon_api}) but this client is stems {client_version} (api {client_api})"
        ),
    )
    .with_hint("restart the daemon: stems daemon stop && stems daemon start")
    .with_details(serde_json::json!({
        "client_version": client_version,
        "client_api_version": client_api,
        "daemon_version": daemon_version,
        "daemon_api_version": daemon_api,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn sample_event() -> Event {
        Event {
            ts: Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap(),
            seq: 7,
            kind: EventKind::STEM_STATE,
            stem: Some("api".into()),
            from: Some("starting".into()),
            to: Some("healthy".into()),
            reason: Some("health check passed".into()),
            actor: "cli:alice".into(),
            data: json!({"pid": 4242}),
        }
    }

    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-api");
    }

    #[test]
    fn event_golden() {
        insta::assert_json_snapshot!("event", sample_event());
        let minimal = Event {
            stem: None,
            from: None,
            to: None,
            reason: None,
            kind: EventKind::DAEMON_STARTED,
            actor: DAEMON_ACTOR.into(),
            data: json!({}),
            ..sample_event()
        };
        assert_eq!(
            serde_json::to_string(&minimal).unwrap(),
            r#"{"ts":"2026-09-26T12:00:00Z","seq":7,"kind":"daemon.started","stem":null,"from":null,"to":null,"reason":null,"actor":"daemon","data":{}}"#
        );
    }

    #[test]
    fn event_roundtrip_and_defaults() {
        let ev = sample_event();
        let s = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&s).unwrap(), ev);
        let sparse: Event = serde_json::from_str(
            r#"{"ts":"2026-09-26T12:00:00Z","seq":1,"kind":"x.new","actor":"daemon"}"#,
        )
        .unwrap();
        assert_eq!(sparse.kind, EventKind::new("x.new"));
        assert_eq!(sparse.data, json!({}));
    }

    #[test]
    fn request_golden() {
        let req = Request::new(
            1,
            Method::PING,
            json!({}),
            RequestMeta {
                actor: "cli:alice".into(),
                client_version: "0.1.0".into(),
                api_version: 1,
            },
        );
        insta::assert_json_snapshot!("request", req);
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{},"meta":{"actor":"cli:alice","client_version":"0.1.0","api_version":1}}"#
        );
        assert_eq!(serde_json::from_str::<Request>(&s).unwrap(), req);
    }

    #[test]
    fn request_params_default_to_empty_object() {
        let r: Request = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":"a","method":"info","meta":{"actor":"x","client_version":"0.1.0","api_version":1}}"#,
        )
        .unwrap();
        assert_eq!(r.params, json!({}));
        assert_eq!(r.method, Method::INFO);
        assert_eq!(r.id, json!("a"));
    }

    #[test]
    fn response_golden() {
        let ok = Response::ok(json!(1), json!({"pong": true}));
        insta::assert_json_snapshot!("response_ok", ok);
        let err = Response::err(
            json!(2),
            Error::new(ErrorCode::NotImplemented, "method `up` is not implemented")
                .with_hint("see docs/protocol.md"),
        );
        insta::assert_json_snapshot!("response_error", err);
        let back: Response = serde_json::from_str(&serde_json::to_string(&err).unwrap()).unwrap();
        assert_eq!(
            back.error.as_ref().unwrap().code,
            rpc_code::METHOD_NOT_FOUND
        );
        let e = back.into_result().unwrap_err();
        assert_eq!(e.code, ErrorCode::NotImplemented);
        assert_eq!(e.hint.as_deref(), Some("see docs/protocol.md"));
        assert_eq!(ok.into_result().unwrap(), json!({"pong": true}));
    }

    #[test]
    fn notification_golden() {
        insta::assert_json_snapshot!("notification_event", Notification::event(&sample_event()));
    }

    #[test]
    fn rpc_codes() {
        assert_eq!(
            rpc_code::for_error(ErrorCode::Usage),
            rpc_code::INVALID_PARAMS
        );
        assert_eq!(
            rpc_code::for_error(ErrorCode::DaemonVersionMismatch),
            rpc_code::VERSION_MISMATCH
        );
        assert_eq!(
            rpc_code::for_error(ErrorCode::LockHeld),
            rpc_code::APPLICATION
        );
    }

    #[test]
    fn open_newtypes() {
        assert_eq!(Method::new("ping"), Method::PING);
        assert!(Method::KNOWN.contains(&"subscribe_events"));
        assert_eq!(
            serde_json::to_string(&EventKind::PROCESS_EXITED).unwrap(),
            r#""process.exited""#
        );
        assert_eq!(EventKind::UP_FINISHED, "up.finished");
    }

    #[test]
    fn daemon_status_flattens_info() {
        let st = DaemonStatus {
            info: DaemonInfo {
                version: "0.1.0".into(),
                api_version: 1,
                workspace: Some("/ws".into()),
                pid: 10,
                start_time: 5,
                uptime_s: 3,
                started_at: Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap(),
            },
            workspace_name: Some("minimal".into()),
            stem_count: 1,
            last_seq: 2,
            subscribers: 0,
            debug_rpc: false,
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["pid"], json!(10));
        assert_eq!(v["workspace_name"], json!("minimal"));
        assert_eq!(serde_json::from_value::<DaemonStatus>(v).unwrap(), st);
    }

    #[test]
    fn version_mismatch_error_has_hint() {
        let e = version_mismatch("0.0.1", 1, "0.1.0", 1);
        assert_eq!(e.code, ErrorCode::DaemonVersionMismatch);
        assert_eq!(
            e.hint.as_deref(),
            Some("restart the daemon: stems daemon stop && stems daemon start")
        );
        assert_eq!(e.exit_code(), 4);
    }

    #[test]
    fn schemas_generate() {
        let s = schemars::schema_for!(Event);
        assert!(serde_json::to_string(&s).unwrap().contains("seq"));
        let _ = schemars::schema_for!(Response);
        let _ = schemars::schema_for!(Request);
    }
}
