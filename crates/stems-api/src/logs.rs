//! Log RPC types (deliverable 12, `docs/logs.md`): `query_logs`,
//! `subscribe_logs` (streamed: ack, then `log` notifications) and
//! `export_logs`. Records are [`stems_core::logs::LogRecord`].

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use stems_core::logs::LogRecord;

use crate::{JSONRPC_VERSION, NOTIFY_LOG, Notification};

/// Most records one `query_logs` call returns (the newest are kept and
/// `truncated` is set).
pub const QUERY_LOGS_CAP: usize = 10_000;

/// Filters shared by `query_logs` and `subscribe_logs`. Times are a
/// duration before the daemon's clock (`10m`, `1.5s`) or an RFC 3339 instant.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LogFilter {
    /// Only these stems (empty: all).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Records with `ts >= since`.
    #[serde(default)]
    pub since: Option<String>,
    /// Records with `ts <= until`.
    #[serde(default)]
    pub until: Option<String>,
    /// Records whose `text` matches this regex.
    #[serde(default)]
    pub grep: Option<String>,
    /// Level filter: `warn` (exactly) or `warn+` (warn and above).
    #[serde(default)]
    pub level: Option<String>,
    /// Only output of this script (`tag`).
    #[serde(default)]
    pub script: Option<String>,
    /// Only the last `n` matching records.
    #[serde(default)]
    pub tail: Option<usize>,
}

/// `query_logs` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QueryLogsParams {
    /// Filters.
    #[serde(flatten)]
    pub filter: LogFilter,
    /// Read the rotated files even when the in-memory ring covers the window.
    #[serde(default)]
    pub from_files: bool,
}

/// `query_logs` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryLogsResult {
    /// Matching records, oldest first, interleaved across stems by `ts`.
    #[schemars(with = "Vec<Value>")]
    pub records: Vec<LogRecord>,
    /// More than [`QUERY_LOGS_CAP`] records matched; only the newest are returned.
    pub truncated: bool,
}

/// `subscribe_logs` params: the filters (`until` is ignored); `since`
/// replays history first, `tail` limits that replay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeLogsParams {
    /// Filters.
    #[serde(flatten)]
    pub filter: LogFilter,
}

/// `subscribe_logs` ack (the response before the `log` notifications).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeLogsAck {
    /// Always true.
    pub subscribed: bool,
    /// Records about to be replayed before live ones.
    pub replay: usize,
}

/// `export_logs` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportLogsParams {
    /// Where the daemon writes the `.tar.gz` (absolute).
    pub path: PathBuf,
    /// Only log records and events at or after this time.
    #[serde(default)]
    pub since: Option<String>,
}

/// `export_logs` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportLogsResult {
    /// The bundle written.
    pub path: PathBuf,
    /// Archive entries, in order.
    pub entries: Vec<String>,
    /// Bundle size in bytes.
    pub bytes: u64,
}

impl Notification {
    /// A `log` notification carrying one record.
    pub fn log(rec: &LogRecord) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: NOTIFY_LOG.to_string(),
            params: serde_json::to_value(rec).unwrap_or(Value::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn params_are_flat_and_default() {
        let p: QueryLogsParams = serde_json::from_value(json!({
            "stems": ["a"], "level": "warn+", "tail": 3
        }))
        .unwrap();
        assert_eq!(p.filter.stems, vec!["a"]);
        assert_eq!(p.filter.tail, Some(3));
        assert!(!p.from_files);
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["level"], "warn+");
        let s: SubscribeLogsParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(s, SubscribeLogsParams::default());
    }
}
