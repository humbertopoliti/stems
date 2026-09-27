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
pub mod health;
pub mod lifecycle;
pub mod logs;
pub mod metrics;
pub mod overlays;
pub mod reload;
pub mod repos;
pub mod scripts;
pub mod watch;

pub use health::{HealthParams, HealthResult, HealthStatus, ProbeOutcome, ProbeRecord, StemHealth};
pub use lifecycle::{CascadeRef, CascadeReport};
pub use lifecycle::{
    DownParams, DownResult, PortStatus, RestartParams, StartParams, StatusParams, StatusResult,
    StatusSummary, StemFailure, StemStatus, StopParams, UpParams, UpResult,
};
pub use lifecycle::{OutputValue, OutputsParams, OutputsResult, REDACTED, StemOutputs};
pub use lifecycle::{SwitchVariantParams, SwitchVariantResult, VariantChoice};
pub use logs::{
    ExportLogsParams, ExportLogsResult, LogFilter, LogRecord, QUERY_LOGS_CAP, QueryLogsParams,
    QueryLogsResult, SubscribeLogsAck, SubscribeLogsParams,
};
pub use metrics::{
    DiskDir, DiskUsage, LimitStatus, MetricsParams, MetricsResult, MetricsSort, MetricsSummary,
    MetricsTotals, StemMetrics,
};
pub use overlays::{OverlayEntry, OverlayFileStatus, OverlaysParams, OverlaysResult};
pub use reload::{
    AppliedChange, ConfigApplyParams, ConfigApplyResult, ConfigDiffParams, ConfigDiffResult,
    FailedChange, ReloadAction, ReloadPlan, SkippedChange, StemChange,
};
pub use repos::{
    RepoAction, RepoSource, RepoStatus, RepoSyncResult, ReposStatusParams, ReposStatusResult,
    ReposSyncParams, ReposSyncResult,
};
pub use scripts::{
    BuildParams, BuildResult, CatalogScript, ResetParams, ResetResult, RunScriptAccepted,
    RunScriptParams, RunScriptResult, ScriptArgsInput, ScriptCatalogParams, ScriptCatalogResult,
    ScriptRunSummary, StampEntry, StampsParams, StampsResult,
};
pub use watch::{
    StemWatchStatus, WatchPauseParams, WatchPauseResult, WatchRuleStatus, WatchStatusParams,
    WatchStatusResult, WatchSummary,
};

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
        // --- lifecycle (10) ------------------------------------------------------
        /// Bring stems up ([`UpParams`] -> [`UpResult`]); long-running.
        UP = "up";
        /// Stop stems ([`DownParams`] -> [`DownResult`]).
        DOWN = "down";
        /// Start stems and their dependencies ([`StartParams`] -> [`UpResult`]).
        START = "start";
        /// Stop stems ([`StopParams`] -> [`DownResult`]).
        STOP = "stop";
        /// Stop then start, keeping ports ([`RestartParams`] -> [`UpResult`]).
        RESTART = "restart";
        /// Per-stem state ([`StatusParams`] -> [`StatusResult`]).
        STATUS = "status";
        // --- logs (12) ---------------------------------------------------------
        /// Query captured log records ([`QueryLogsParams`] -> [`QueryLogsResult`]).
        QUERY_LOGS = "query_logs";
        /// Stream log records ([`SubscribeLogsParams`]); ack ([`SubscribeLogsAck`])
        /// then `log` notifications, like `subscribe_events`.
        SUBSCRIBE_LOGS = "subscribe_logs";
        /// Write a support bundle ([`ExportLogsParams`] -> [`ExportLogsResult`]).
        EXPORT_LOGS = "export_logs";
        // --- lifecycle scripts (16) ---------------------------------------------
        /// Run `build` scripts ([`BuildParams`] -> [`BuildResult`]).
        BUILD = "build";
        /// Stop stems, run `reset`, clear stamps ([`ResetParams`] -> [`ResetResult`]).
        RESET = "reset";
        /// List or clear stamps ([`StampsParams`] -> [`StampsResult`]).
        STAMPS = "stamps";
        // --- overlays (18) -----------------------------------------------------
        /// Recorded overlays and their status ([`OverlaysParams`] -> [`OverlaysResult`]).
        OVERLAYS = "overlays";
        // --- custom scripts (17) ------------------------------------------------
        /// Run a stem or workspace script ([`RunScriptParams`] ->
        /// [`RunScriptResult`], or [`RunScriptAccepted`] with `wait: false`).
        RUN_SCRIPT = "run_script";
        /// Every runnable script ([`ScriptCatalogParams`] -> [`ScriptCatalogResult`]).
        SCRIPT_CATALOG = "script_catalog";
        // --- TUI (27) ------------------------------------------------------------
        /// One stem's resolved config as the daemon loaded it (`{stem}` ->
        /// the `stems_config::Stem` JSON, as `stems show` prints a stem).
        STEM_CONFIG = "stem_config";
        // --- git codebases (20) -----------------------------------------------
        /// Clone / fetch / check out git codebases ([`ReposSyncParams`] -> [`ReposSyncResult`]).
        REPOS_SYNC = "repos_sync";
        /// Per-stem codebase state ([`ReposStatusParams`] -> [`ReposStatusResult`]).
        REPOS_STATUS = "repos_status";
        // --- health (21) -------------------------------------------------------
        /// Last probe results per stem ([`HealthParams`] -> [`HealthResult`]).
        HEALTH = "health";
        // --- metrics (25) ------------------------------------------------------
        /// Latest samples, history, totals, disk ([`MetricsParams`] -> [`MetricsResult`]).
        METRICS = "metrics";
        // --- outputs (26) --------------------------------------------------------
        /// Evaluated stem outputs, secrets redacted ([`OutputsParams`] -> [`OutputsResult`]).
        OUTPUTS = "outputs";
        // --- watchdogs (24) ------------------------------------------------------
        /// Pause watchdogs, all or some stems' ([`WatchPauseParams`] -> [`WatchPauseResult`]).
        WATCH_PAUSE = "watch_pause";
        /// Resume watchdogs ([`WatchPauseParams`] -> [`WatchPauseResult`]).
        WATCH_RESUME = "watch_resume";
        /// Rules and state of every watchdog ([`WatchStatusParams`] -> [`WatchStatusResult`]).
        WATCH_STATUS = "watch_status";
        // --- config reload (33) --------------------------------------------------
        /// The pending reload plan ([`ConfigDiffParams`] -> [`ConfigDiffResult`]).
        CONFIG_DIFF = "config_diff";
        /// Apply the pending plan ([`ConfigApplyParams`] -> [`ConfigApplyResult`]).
        CONFIG_APPLY = "config_apply";
        // --- variants (FR-ST-8) --------------------------------------------------
        /// List a stem's variants, or switch it: edit `stems.local.yaml`,
        /// validate, apply to that stem ([`SwitchVariantParams`] ->
        /// [`SwitchVariantResult`]).
        SWITCH_VARIANT = "switch_variant";
        // --- crash recovery (11) ----------------------------------------------
        /// Adopt orphaned processes as their stems (`{orphans: [{stem, pid}]}`
        /// -> `{adopted: [{stem, pid, pgid}], failed: [{stem, pid, error}]}`).
        ADOPT_ORPHANS = "adopt_orphans";
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
        /// A cascading restart began (FR-LC-9; `data: {id, origin, origins,
        /// reason, stems}`, `stems` = the dependants' layers).
        CASCADE_STARTED = "cascade.started";
        /// A cascading restart waits for the running one (`data: {id,
        /// origin, origins, reason, behind}`).
        CASCADE_QUEUED = "cascade.queued";
        /// A cascading restart finished (`data: {id, origin, restarted,
        /// failed, skipped}`).
        CASCADE_FINISHED = "cascade.finished";
        /// The origin of a cascading restart did not become healthy; no
        /// dependant was touched (`stem` = the origin; `data: {id, origin, error}`).
        CASCADE_ABORTED = "cascade.aborted";
        /// A stem's health changed (`healthy` <-> `unhealthy`/`unknown`, 21):
        /// one per transition, never per probe. `data: {probe, detail, latency_ms, consecutive_failures}`.
        STEM_HEALTH = "stem.health";
        /// A metric threshold (`limits:`) was crossed or cleared (25; `data:
        /// {metric, value, limit, for_s, state: crossed|cleared}`).
        STEM_THRESHOLD = "stem.threshold";
        /// A stem's outputs were evaluated (26; `data: {names}`, never values).
        STEM_OUTPUTS = "stem.outputs";
        /// A process exited (`data: {code, signal}`).
        PROCESS_EXITED = "process.exited";
        /// A line of output of a debug-started process (`data: {stream, text}`).
        PROCESS_OUTPUT = "process.output";
        /// `up` began.
        UP_STARTED = "up.started";
        /// `up` finished.
        UP_FINISHED = "up.finished";
        /// A profile's hard dependencies were added to `up` (26; `data:
        /// {profile, added, requested}`).
        PROFILE_EXPANDED = "profile.expanded";
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
        /// A file watch fired (24; `data: {paths (≤ 10), path_count, action,
        /// rule_index}`).
        WATCH_TRIGGERED = "watch.triggered";
        /// Watching was paused (24; `stem` or `data: {global: true}`).
        WATCH_PAUSED = "watch.paused";
        /// Watching was resumed (24; `stem` or `data: {global: true}`).
        WATCH_RESUMED = "watch.resumed";
        /// A watchdog action finished (24; `data: {action, rule_index, ok, error?}`).
        WATCH_ACTION_FINISHED = "watch.action_finished";
        /// A running stem's watch rules were re-applied in place (33; `data:
        /// {rules, running}`).
        WATCH_RECONFIGURED = "watch.reconfigured";
        /// The config changed on disk (33; `data: {plan, sources}` with the
        /// [`ReloadPlan`]); nothing is applied yet.
        CONFIG_CHANGED = "config.changed";
        /// The config on disk is invalid; the applied one stays (33; `data:
        /// {errors}`).
        CONFIG_INVALID = "config.invalid";
        /// A reload plan was applied (33; `data: {applied, failed, skipped,
        /// workspace, auto}`).
        CONFIG_APPLIED = "config.applied";
        /// A command used the applied config while a change is pending (33;
        /// `data: {method, stems}`).
        CONFIG_PENDING = "config.pending";
        /// The script catalog (MCP tools) of the applied config changed (33;
        /// `data: {added, removed}` tool names). `stems mcp` sends
        /// `notifications/tools/list_changed`.
        TOOLS_CHANGED = "tools.changed";
        /// An overlay was written (`data: {dest, backup}`; 18).
        OVERLAY_MATERIALISED = "overlay.materialised";
        /// An unmodified overlay was removed on stop (`data: {dest}`).
        OVERLAY_REMOVED = "overlay.removed";
        /// A modified overlay was left on stop (`data: {dest}`, warning).
        OVERLAY_MODIFIED_LEFT_IN_PLACE = "overlay.modified_left_in_place";
        /// A `keep: true` overlay was left on stop (`data: {dest}`).
        OVERLAY_KEPT = "overlay.kept";
        /// A git codebase was cloned (20; `data: {url, ref, sha, path}`).
        REPO_CLONED = "repo.cloned";
        /// A git codebase was fetched (20; `data: {url, ref, sha, updated}`).
        REPO_FETCHED = "repo.fetched";
        /// A git codebase was switched to its configured ref (20).
        REPO_CHECKED_OUT = "repo.checked_out";
        /// A git codebase was left alone because of local work (20).
        REPO_SKIPPED_DIRTY = "repo.skipped_dirty";
        /// Cloning / fetching / checking out failed (20; `data: {error}`).
        REPO_FAILED = "repo.failed";
        /// Image pull progress of a docker stem, one per layer status change
        /// (14; `data: {stem, image, layer, status}`).
        DOCKER_PULL = "docker.pull";
        /// One line of a docker stem's image build output (14; `data: {stem, line}`).
        DOCKER_BUILD = "docker.build";
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

/// A JSON-RPC notification (no id): subscription frames (`event`, `log`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// [`NOTIFY_EVENT`] or [`NOTIFY_LOG`].
    pub method: String,
    /// Payload ([`Event`] for `event`, [`LogRecord`] for `log`).
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
    /// Only these kinds (`stem.state`; a trailing `*` matches a prefix:
    /// `stem.*`). Empty: every kind (31).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Only events caused by this actor (`mcp:claude-code`, `cli:alice`) (31).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

impl EventsParams {
    /// Whether `ev` passes the `kinds` / `actor` filters (not `since_seq`).
    pub fn matches(&self, ev: &Event) -> bool {
        let kind_ok = self.kinds.is_empty()
            || self.kinds.iter().any(|k| match k.strip_suffix('*') {
                Some(prefix) => ev.kind.as_str().starts_with(prefix),
                None => ev.kind == k.as_str(),
            });
        kind_ok && self.actor.as_ref().is_none_or(|a| &ev.actor == a)
    }
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
    fn events_filters() {
        let ev = sample_event();
        let p = |kinds: &[&str], actor: Option<&str>| EventsParams {
            kinds: kinds.iter().map(|k| k.to_string()).collect(),
            actor: actor.map(str::to_string),
            ..EventsParams::default()
        };
        assert!(p(&[], None).matches(&ev));
        assert!(p(&["stem.state"], Some("cli:alice")).matches(&ev));
        assert!(p(&["stem.*"], None).matches(&ev));
        assert!(!p(&["script.*"], None).matches(&ev));
        assert!(!p(&[], Some("mcp:x")).matches(&ev));
        // Old clients omit the new fields.
        let old: EventsParams = serde_json::from_value(json!({"since_seq": 3})).unwrap();
        assert!(old.matches(&ev));
    }

    #[test]
    fn schemas_generate() {
        let s = schemars::schema_for!(Event);
        assert!(serde_json::to_string(&s).unwrap().contains("seq"));
        let _ = schemars::schema_for!(Response);
        let _ = schemars::schema_for!(Request);
    }
}
