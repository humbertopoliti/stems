//! Params and results of the lifecycle-script methods (`build`, `reset`,
//! `stamps`), deliverable 16. See `docs/scripts.md`.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::lifecycle::StemFailure;

/// `build` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BuildParams {
    /// Stems whose `build` script runs; empty = every enabled stem that has one.
    #[serde(default)]
    pub stems: Vec<String>,
}

/// One finished script run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptRunSummary {
    /// Stem (`null` for workspace scripts).
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
    /// Exit code (`null` when killed by a signal).
    pub exit: Option<i32>,
    /// Wall time.
    pub duration_ms: u64,
}

/// `build` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuildResult {
    /// Every selected build succeeded.
    pub ok: bool,
    /// Scripts that ran successfully.
    pub built: Vec<ScriptRunSummary>,
    /// Selected stems without a `build` script.
    pub skipped: Vec<String>,
    /// Stems whose build failed (`SCRIPT_FAILED`).
    pub failed: Vec<StemFailure>,
}

/// `reset` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResetParams {
    /// Stems to reset; empty = every enabled stem.
    #[serde(default)]
    pub stems: Vec<String>,
}

/// `reset` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResetResult {
    /// Every reset succeeded.
    pub ok: bool,
    /// Stems that were running and were stopped first.
    pub stopped: Vec<String>,
    /// `reset` scripts that ran successfully.
    pub reset: Vec<ScriptRunSummary>,
    /// Stems whose stamps were cleared.
    pub cleared: Vec<String>,
    /// Stems whose `reset` failed (their stamps are kept).
    pub failed: Vec<StemFailure>,
}

/// `stamps` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StampsParams {
    /// Only this stem.
    #[serde(default)]
    pub stem: Option<String>,
    /// Clear the selected stamps (so `setup`/`seed` run again).
    #[serde(default)]
    pub clear: bool,
}

/// One recorded stamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StampEntry {
    /// Stem.
    pub stem: String,
    /// Script (`setup`, `seed`).
    pub script: String,
    /// sha256 (hex).
    pub hash: String,
    /// When the script last succeeded.
    pub computed_at: DateTime<Utc>,
    /// Input files hashed (relative to the codebase).
    #[schemars(with = "Vec<String>")]
    pub inputs: Vec<std::path::PathBuf>,
}

/// `stamps` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StampsResult {
    /// Stamps (sorted by stem, then script); with `clear`, the ones removed.
    pub stamps: Vec<StampEntry>,
    /// The stamps were cleared.
    #[serde(default)]
    pub cleared: bool,
}
