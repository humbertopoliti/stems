//! Params and results of the lifecycle methods (`up`, `down`, `start`,
//! `stop`, `restart`, `status`), deliverable 10. See `docs/lifecycle.md`.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_core::{Error, Glyph, StemState};

fn yes() -> bool {
    true
}

/// `up` params.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpParams {
    /// Stems to start (their hard dependencies are added); empty = every enabled stem.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Profile, used when `stems` is empty (the client resolves `--profile`
    /// / `STEMS_PROFILE`); `None` = the workspace default (`profile:`,
    /// `default_profile`, a profile named `default`), else every stem (26).
    #[serde(default)]
    pub profile: Option<String>,
    /// Informational: the client will not stay attached.
    #[serde(default)]
    pub detach: bool,
    /// Overall deadline in milliseconds (default: none; each stem is bounded
    /// by its `health.start_timeout`).
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Stop starting further stems after the first failure (default true).
    #[serde(default = "yes")]
    pub fail_fast: bool,
    /// Stems starting concurrently (default 4).
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// Shell environment passed through (`--pass-env`), applied last (FR-ST-4).
    #[serde(default)]
    pub pass_env: BTreeMap<String, String>,
    /// The client auto-started the daemon for this `up`: `down` of the last
    /// running stem then also shuts the daemon down.
    #[serde(default)]
    pub daemon_auto_started: bool,
    /// Ignore stamps: stop the planned stems, run their `reset` scripts and
    /// clear their stamps first, so `setup`/`seed` run again (deliverable 16).
    #[serde(default)]
    pub fresh: bool,
    /// Fetch and check out existing git codebases before starting (`up
    /// --sync`, deliverable 20). Missing clones are always cloned.
    #[serde(default)]
    pub sync: bool,
    /// Back up overlay destinations stems does not own and overwrite them
    /// (`up --force-overlays`, deliverable 18).
    #[serde(default)]
    pub force_overlays: bool,
    /// Start no watchdogs (`up --no-watch`, deliverable 24): watchers stay
    /// off until the next `up` without it.
    #[serde(default)]
    pub no_watch: bool,
}

impl Default for UpParams {
    fn default() -> Self {
        Self {
            stems: Vec::new(),
            profile: None,
            detach: false,
            timeout_ms: None,
            fail_fast: true,
            max_parallel: None,
            pass_env: BTreeMap::new(),
            daemon_auto_started: false,
            fresh: false,
            sync: false,
            force_overlays: false,
            no_watch: false,
        }
    }
}

/// A stem that failed, with why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StemFailure {
    /// Stem name.
    pub stem: String,
    /// The stems error (`START_FAILED`, `PORT_IN_USE`, `HEALTH_TIMEOUT`, ...).
    #[schemars(with = "Value")]
    pub error: Error,
}

/// `up` / `start` / `restart` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpResult {
    /// Every stem is ready.
    pub ok: bool,
    /// Stems asked for (empty = all).
    pub requested: Vec<String>,
    /// Stems that reached their ready state (`healthy`; `unknown` for external stems).
    pub ready: Vec<String>,
    /// Stems that failed.
    pub failed: Vec<StemFailure>,
    /// Stems not started (a dependency failed, or fail-fast aborted).
    pub skipped: Vec<String>,
}

/// `down` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DownParams {
    /// Stems to stop; empty = every running stem.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Stop everything and shut the daemon down.
    #[serde(default)]
    pub all: bool,
    /// Per-stem stop grace override in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Also remove the `<ws>_*` named volumes of the selected docker stems
    /// (destructive; the CLI asks for `--yes`) (14).
    #[serde(default)]
    pub volumes: bool,
}

/// `down` / `stop` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DownResult {
    /// Every selected stem is stopped.
    pub ok: bool,
    /// Stems stopped by this call.
    pub stopped: Vec<String>,
    /// Stems that were not running (or are external).
    pub skipped: Vec<String>,
    /// Stems whose stop failed.
    pub failed: Vec<StemFailure>,
    /// The daemon shuts down after this reply.
    #[serde(default)]
    pub daemon_stopping: bool,
    /// Docker volumes removed by `down --volumes` (14).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes_removed: Vec<String>,
}

/// `start` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StartParams {
    /// Stems to start.
    pub stems: Vec<String>,
    /// Do not start unstarted hard dependencies.
    #[serde(default)]
    pub no_deps: bool,
    /// Overall deadline in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `stop` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StopParams {
    /// Stems to stop.
    pub stems: Vec<String>,
    /// Also stop running dependants (otherwise `HAS_DEPENDANTS`).
    #[serde(default)]
    pub cascade: bool,
    /// Per-stem stop grace override in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `restart` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Stems to restart (allocated ports are kept).
    pub stems: Vec<String>,
    /// Do not start unstarted hard dependencies.
    #[serde(default)]
    pub no_deps: bool,
    /// Stop, run each stem's `build` script, then start (deliverable 16).
    #[serde(default)]
    pub build: bool,
    /// Overall deadline in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// `status` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {
    /// Only these stems (default: every enabled stem).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Add each running stem's resolved `env`.
    #[serde(default)]
    pub verbose: bool,
}

/// One port of a stem in `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PortStatus {
    /// Port name.
    pub name: String,
    /// Host port; `null` for an `auto` port not allocated yet.
    pub port: Option<u16>,
    /// Declared as `auto`.
    #[serde(default)]
    pub auto: bool,
}

/// One stem in `status` (FR-HS-2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StemStatus {
    /// Stem name.
    pub name: String,
    /// `process`, `docker`, `compose`, `external`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Lifecycle state.
    #[schemars(with = "String")]
    pub state: StemState,
    /// Glyph name (`healthy`, `failed`, `transitioning`, ...).
    #[schemars(with = "String")]
    pub glyph: Glyph,
    /// Why it is in this state; for a degraded stem, the degraded reasons
    /// (`dependency postgres unhealthy; flapping`). Empty for a plain
    /// healthy stem.
    pub reason: Option<String>,
    /// `healthy`, but with a warning (FR-GR-6, FR-HS-2): glyph `degraded`,
    /// derived for every snapshot, never stored (see `docs/health.md`).
    #[serde(default)]
    pub degraded: bool,
    /// Leader pid while running.
    pub pid: Option<i32>,
    /// Process group while running.
    pub pgid: Option<i32>,
    /// Declared ports with their host port.
    pub ports: Vec<PortStatus>,
    /// Seconds since the current process started.
    pub uptime_s: Option<u64>,
    /// When the current process started.
    pub started_at: Option<DateTime<Utc>>,
    /// Policy restarts since the daemon started (lifetime; a user
    /// `restart` does not reset it). Watchdog restarts (24) are not counted.
    pub restarts: u32,
    /// Policy restarts within the stem's `restart.window` (22); at 3 or
    /// more a healthy stem is degraded with reason `restarts` (FR-HS-4).
    #[serde(default)]
    pub restarts_in_window: u32,
    /// The stem has a `seed` script and it has run (or its stamp was
    /// current) since the stem last started: `condition: seeded` holds.
    #[serde(default)]
    pub seeded: bool,
    /// Health probe summary (deliverable 21); `null` without a health check.
    pub health: Option<crate::health::HealthStatus>,
    /// Latest metrics sample (25): `{cpu_pct, rss_bytes, children}`; absent
    /// when not running or not sampled yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<crate::metrics::MetricsSummary>,
    /// Last error, if the stem failed.
    #[schemars(with = "Option<Value>")]
    pub error: Option<Error>,
    /// Environment stems set for the process (`verbose` only): config env,
    /// env files, local overrides, `--pass-env`, `PORT` and `STEMS_*`; the
    /// inherited daemon environment is not shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    /// Evaluated outputs (FR-ST-6, 26) of the current start; secret ones
    /// are `"<redacted>"`. Empty (omitted) until the stem is healthy.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// Watchdogs (24): `{paused, rules}`; omitted for a stem without `watch:` rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<crate::watch::WatchSummary>,
}

/// What `status` and `outputs` show instead of a secret output's value.
pub const REDACTED: &str = "<redacted>";

/// `outputs` params (26).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputsParams {
    /// Only these stems (default: every stem that declares outputs).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Return secret values instead of `<redacted>` (the CLI only asks for
    /// this with `--reveal` in human mode on a terminal).
    #[serde(default)]
    pub reveal: bool,
}

/// One output of a stem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputValue {
    /// Output name.
    pub name: String,
    /// Value (`"<redacted>"` when secret); `null` while not evaluated
    /// (the stem is not healthy yet).
    pub value: Option<String>,
    /// Declared `secret: true`.
    pub secret: bool,
}

/// A stem's outputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemOutputs {
    /// Stem name.
    pub name: String,
    /// Its declared outputs, in declaration order.
    pub outputs: Vec<OutputValue>,
}

/// `outputs` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputsResult {
    /// Stems in declaration order.
    pub stems: Vec<StemOutputs>,
}

/// Counts per glyph in `status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusSummary {
    /// `healthy`.
    pub healthy: usize,
    /// `healthy` with a warning.
    pub degraded: usize,
    /// `failed` and `unhealthy`.
    pub failed: usize,
    /// `stopped`.
    pub stopped: usize,
    /// `unknown`.
    pub unknown: usize,
    /// Transitioning: `setup`, `starting`, `seeding`, `stopping`.
    pub starting: usize,
}

impl StatusSummary {
    /// Count `stems`.
    pub fn of(stems: &[StemStatus]) -> Self {
        let mut s = Self::default();
        for st in stems {
            match st.glyph {
                Glyph::Healthy => s.healthy += 1,
                Glyph::Degraded => s.degraded += 1,
                Glyph::Failed => s.failed += 1,
                Glyph::Stopped => s.stopped += 1,
                Glyph::Unknown => s.unknown += 1,
                Glyph::Transitioning => s.starting += 1,
            }
        }
        s
    }
}

/// `status` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusResult {
    /// Stems in declaration order.
    pub stems: Vec<StemStatus>,
    /// Counts.
    pub summary: StatusSummary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn up_params_defaults() {
        let p: UpParams = serde_json::from_value(json!({})).unwrap();
        assert!(p.fail_fast);
        assert_eq!(p, UpParams::default());
    }

    #[test]
    fn status_shape() {
        let st = StemStatus {
            name: "api".into(),
            kind: "process".into(),
            state: StemState::Healthy,
            glyph: Glyph::Healthy,
            reason: None,
            degraded: false,
            pid: Some(10),
            pgid: Some(10),
            ports: vec![PortStatus {
                name: "http".into(),
                port: Some(8080),
                auto: false,
            }],
            uptime_s: Some(3),
            started_at: None,
            restarts: 0,
            restarts_in_window: 0,
            seeded: false,
            health: None,
            metrics: None,
            error: None,
            env: None,
            outputs: BTreeMap::new(),
            watch: None,
        };
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v["type"], "process");
        assert_eq!(v["state"], "healthy");
        assert_eq!(v["glyph"], "healthy");
        assert!(v.get("env").is_none());
        assert_eq!(serde_json::from_value::<StemStatus>(v).unwrap(), st);
        let s = StatusSummary::of(&[st]);
        assert_eq!(s.healthy, 1);
    }
}
