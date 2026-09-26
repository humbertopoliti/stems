//! Config reload wire types (deliverable 33, `docs/config.md#reload`): the
//! [`ReloadPlan`] the daemon computes when the config changes on disk, and
//! the `config_diff` / `config_apply` RPCs.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stems_core::Error;

/// What applying the new config does to one stem.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReloadAction {
    /// Nothing changed.
    Unchanged,
    /// The stem must restart to pick the change up (env, ports, command,
    /// image, codebase, overlays, ...; see `fields`).
    RestartRequired,
    /// Outputs changed: they are evaluated at start, so the stem restarts.
    OutputsChanged,
    /// New stem (or `enabled: true` again).
    Added,
    /// Stem gone (or `enabled: false`): stopped, state entry dropped,
    /// overlays cleaned.
    Removed,
    /// Health check changed: the prober restarts, the stem does not.
    HealthChanged,
    /// Watch rules changed: the watchdog is reconfigured in place.
    WatchChanged,
    /// Scripts changed (not `scripts.start`): the definition is swapped.
    ScriptsChanged,
    /// Metric thresholds changed: applied from the next sample.
    LimitsChanged,
    /// `restart:` policy changed: used from the next crash.
    RestartPolicyChanged,
    /// `description`, `tags`, `depends_on`, `stop_grace`: swapped in place.
    MetadataChanged,
}

impl ReloadAction {
    /// Applying it restarts nothing.
    pub fn is_hot(self) -> bool {
        !matches!(
            self,
            Self::RestartRequired | Self::OutputsChanged | Self::Added | Self::Removed
        )
    }

    /// The snake_case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::RestartRequired => "restart_required",
            Self::OutputsChanged => "outputs_changed",
            Self::Added => "added",
            Self::Removed => "removed",
            Self::HealthChanged => "health_changed",
            Self::WatchChanged => "watch_changed",
            Self::ScriptsChanged => "scripts_changed",
            Self::LimitsChanged => "limits_changed",
            Self::RestartPolicyChanged => "restart_policy_changed",
            Self::MetadataChanged => "metadata_changed",
        }
    }
}

impl std::fmt::Display for ReloadAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One stem in a [`ReloadPlan`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemChange {
    /// Stem name.
    pub name: String,
    /// The strongest change: `removed`/`added`, else `restart_required`,
    /// else `outputs_changed`, else the first hot change.
    pub action: ReloadAction,
    /// Every change of this stem (one per changed field group).
    #[serde(default)]
    pub changes: Vec<ReloadAction>,
    /// Changed top-level fields (`env`, `ports`, `command`, `image`,
    /// `codebase`, `scripts.start`, `health`, `watch`, ...).
    #[serde(default)]
    pub fields: Vec<String>,
    /// Applying it restarts nothing (every change is hot).
    pub hot: bool,
    /// The stem runs now (a stopped stem never restarts on apply).
    #[serde(default)]
    pub running: bool,
}

/// What changed between the applied config and the one on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReloadPlan {
    /// Every stem of the old and the new config (declaration order, removed
    /// stems last), `unchanged` ones included.
    pub stems: Vec<StemChange>,
    /// Workspace-level changes: `profiles_changed`, `agent_changed`,
    /// `logs_changed`, `metrics_changed`, `scripts_changed`,
    /// `requires_changed`, `config_changed`, ... (always hot).
    pub workspace: Vec<String>,
    /// The script catalog (MCP tools, `stems scripts`) changed.
    pub catalog_changed: bool,
}

impl ReloadPlan {
    /// Stems whose action is not `unchanged`.
    pub fn affected(&self) -> impl Iterator<Item = &StemChange> {
        self.stems
            .iter()
            .filter(|s| s.action != ReloadAction::Unchanged)
    }

    /// Nothing changed at all.
    pub fn is_empty(&self) -> bool {
        self.affected().next().is_none() && self.workspace.is_empty() && !self.catalog_changed
    }
}

/// `config_diff` params (none).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDiffParams {}

/// `config_diff` result.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDiffResult {
    /// The pending plan (empty when nothing is pending).
    pub plan: ReloadPlan,
    /// A changed config waits to be applied.
    pub pending: bool,
    /// When the applied config was loaded.
    pub loaded_at: Option<DateTime<Utc>>,
    /// When the pending change was detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_at: Option<DateTime<Utc>>,
    /// The config files the daemon watches.
    #[serde(default)]
    pub sources: Vec<std::path::PathBuf>,
    /// The errors of the last reload attempt, when the config on disk is
    /// invalid (the applied config stays in force).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(with = "Vec<serde_json::Value>")]
    pub last_error: Vec<Error>,
}

/// `config_apply` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigApplyParams {
    /// Only these stems' changes (default: all of them, and the
    /// workspace-level changes).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Confirmation (the CLI asks unless `--yes`).
    #[serde(default)]
    pub yes: bool,
}

/// One applied change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AppliedChange {
    /// Stem name.
    pub stem: String,
    /// What was done for it.
    pub action: ReloadAction,
    /// `restarted`, `started`, `stopped`, `registered`, `reconfigured`,
    /// `swapped` (a stopped stem: its next start uses the new config).
    pub result: String,
}

/// One change not applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedChange {
    /// Stem name.
    pub stem: String,
    /// Its action.
    pub action: ReloadAction,
    /// Why (`not selected`, `needs a restart (auto_apply: true)`).
    pub reason: String,
}

/// A change whose application failed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct FailedChange {
    /// Stem name.
    pub stem: String,
    /// Its action.
    pub action: ReloadAction,
    /// What went wrong.
    #[schemars(with = "serde_json::Value")]
    pub error: Error,
}

/// `config_apply` result.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ConfigApplyResult {
    /// Changes applied.
    pub applied: Vec<AppliedChange>,
    /// Changes that failed (the others proceeded).
    pub failed: Vec<FailedChange>,
    /// Changes left pending.
    pub skipped: Vec<SkippedChange>,
    /// Workspace-level changes applied.
    #[serde(default)]
    pub workspace: Vec<String>,
    /// A change is still pending afterwards.
    pub pending: bool,
    /// Nothing failed.
    pub ok: bool,
}
