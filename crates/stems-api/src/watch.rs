//! Watchdog wire types (deliverable 24, `docs/watchdogs.md`): the
//! `watch_pause`, `watch_resume` and `watch_status` RPCs and the per-stem
//! summary in `status`.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `watch_pause` / `watch_resume` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchPauseParams {
    /// Only these stems; empty = every watchdog (global pause; a global
    /// resume also clears every per-stem pause).
    #[serde(default)]
    pub stems: Vec<String>,
}

/// `watch_pause` / `watch_resume` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchPauseResult {
    /// Stems whose pause flag changed (empty for a global pause/resume).
    pub stems: Vec<String>,
    /// Global pause after the call.
    pub global_paused: bool,
}

/// `watch_status` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchStatusParams {
    /// Only these stems (default: every stem with `watch:` rules).
    #[serde(default)]
    pub stems: Vec<String>,
}

/// One watch rule as the daemon applies it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchRuleStatus {
    /// Globs, relative to the root.
    pub paths: Vec<String>,
    /// Ignore globs (built-in defaults + user entries).
    pub ignore: Vec<String>,
    /// `restart`, `rebuild`, `script:<name>`, `signal:<SIG>`.
    pub action: String,
    /// Coalescing window.
    pub debounce_ms: u64,
    /// Quiet period required before acting.
    pub settle_ms: u64,
    /// `codebase` or `workspace`.
    pub root: String,
    /// The directory watched.
    pub dir: Option<String>,
}

/// One stem's watchdogs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemWatchStatus {
    /// Stem name.
    pub name: String,
    /// Its rules.
    pub rules: Vec<WatchRuleStatus>,
    /// Paused (per stem, or globally).
    pub paused: bool,
    /// A watcher is running (the stem runs and `up --no-watch` was not used).
    pub active: bool,
    /// When a rule last fired.
    pub last_triggered: Option<DateTime<Utc>>,
    /// Changes are queued behind a running action (at most one pending
    /// action per rule).
    pub pending: bool,
    /// An action is running.
    pub busy: bool,
}

/// `watch_status` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchStatusResult {
    /// Stems with `watch:` rules.
    pub stems: Vec<StemWatchStatus>,
    /// Every watchdog is paused.
    pub global_paused: bool,
    /// Watchdogs were disabled by `up --no-watch`.
    pub disabled: bool,
}

/// The `watch` summary of a stem in `status` (omitted without rules).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WatchSummary {
    /// Paused (per stem or globally).
    pub paused: bool,
    /// Number of rules.
    pub rules: usize,
}
