//! Health probes on the wire (deliverable 21, `docs/health.md`): the
//! `health` block of [`crate::StemStatus`] and the `health` RPC.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stems_core::StemState;

/// How many probe results the daemon keeps per stem.
pub const PROBE_HISTORY: usize = 50;

/// Default `last` of the `health` RPC.
pub const DEFAULT_LAST: usize = 10;

/// Outcome class of one probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// The check passed.
    Ok,
    /// The check ran and failed (connection refused, bad status, exit != 0, timeout).
    Fail,
    /// The check could not run (name resolution, spawn failure): an external
    /// stem goes `unknown`, a managed one counts it as a failure.
    Unknown,
}

/// One probe result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeRecord {
    /// When the probe finished.
    pub ts: DateTime<Utc>,
    /// `outcome == ok`.
    pub ok: bool,
    /// Outcome class.
    pub outcome: ProbeOutcome,
    /// Wall time of the probe.
    pub latency_ms: u64,
    /// What happened (`HTTP 200`, `connection refused`, `exit 1: ...`).
    pub detail: String,
}

/// The `health` block of a stem in `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HealthStatus {
    /// Probe type (`tcp`, `http`, `command`, `docker`, `process`, `grpc`).
    #[serde(rename = "type")]
    pub kind: String,
    /// The latest probe result (`null` before the first probe).
    pub last: Option<ProbeRecord>,
    /// Failed probes in a row.
    pub consecutive_failures: u32,
    /// Health transitions in the last 60 s (flapping at 3).
    pub transitions_60s: u32,
    /// Container stems (14/15): Docker's own `State.Health.Status`
    /// (`starting`, `healthy`, `unhealthy`) when the container has a healthcheck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// `health` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HealthParams {
    /// Only these stems (default: every stem).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Probe results per stem, newest last (default 10, at most 50).
    #[serde(default)]
    pub last: Option<usize>,
}

/// One stem in the `health` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemHealth {
    /// Stem name.
    pub name: String,
    /// Probe type; `null` when the stem has no health check.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Current state.
    #[schemars(with = "String")]
    pub state: StemState,
    /// Failed probes in a row.
    pub consecutive_failures: u32,
    /// Health transitions in the last 60 s.
    pub transitions_60s: u32,
    /// The last probe results, oldest first.
    pub results: Vec<ProbeRecord>,
}

/// `health` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HealthResult {
    /// Stems in declaration order.
    pub stems: Vec<StemHealth>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shapes() {
        let r = ProbeRecord {
            ts: DateTime::from_timestamp(0, 0).unwrap(),
            ok: false,
            outcome: ProbeOutcome::Unknown,
            latency_ms: 3,
            detail: "dns".into(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["outcome"], "unknown");
        let h = HealthStatus {
            kind: "http".into(),
            last: Some(r),
            consecutive_failures: 2,
            transitions_60s: 0,
            container: None,
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["type"], "http");
        assert_eq!(v["last"]["latency_ms"], 3);
        let p: HealthParams = serde_json::from_value(json!({})).unwrap();
        assert_eq!(p, HealthParams::default());
    }
}
