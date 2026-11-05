//! Params and result of `stem_env` (deliverable 37, `docs/exec.md`): a
//! stem's context for `stems exec`, `stems shell` and `stems env`, which
//! run the command themselves.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stems_core::StemState;

/// `stem_env` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemEnvParams {
    /// The stem.
    pub stem: String,
    /// Do not redact secret outputs (the CLI asks only for a human on a
    /// terminal, like `stems outputs --reveal`).
    #[serde(default)]
    pub reveal: bool,
}

/// `stem_env` result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StemEnvResult {
    /// The stem.
    pub stem: String,
    /// Its type (`process`, `docker`, `compose`, `external`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Its default working directory (absolute): the codebase, else its
    /// state directory (`$STEMS_STATE_DIR`).
    pub cwd: PathBuf,
    /// What stems sets for it (layers 2–7 of `docs/lifecycle.md`: `PORT`,
    /// `env`, `env_files`, local env, `STEMS_*`), never the inherited base.
    /// Secrets of dependencies' outputs are redacted.
    pub env: BTreeMap<String, String>,
    /// Container id of a running docker/compose stem; `null` otherwise.
    pub container: Option<String>,
    /// Its state.
    #[schemars(with = "String")]
    pub state: StemState,
    /// It is running (`state` is a running state).
    pub running: bool,
    /// Variables whose value still holds a `${stem...}` reference (an auto
    /// port that is not allocated, an output not evaluated): kept literally.
    #[serde(default)]
    pub unresolved: Vec<String>,
}
