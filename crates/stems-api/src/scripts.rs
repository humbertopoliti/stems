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

// --- custom scripts (17) -----------------------------------------------------

/// Arguments of a `run_script` call: the raw `--` argv (CLI), validated
/// against the script's `args` like a command line, or a JSON object
/// (MCP / TUI) validated with type coercion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ScriptArgsInput {
    /// `["--email", "a@b.c"]`; passed through untouched when the script
    /// declares no `args`.
    Argv(Vec<String>),
    /// `{"email": "a@b.c", "role": "user"}`.
    Object(serde_json::Map<String, serde_json::Value>),
}

impl Default for ScriptArgsInput {
    fn default() -> Self {
        Self::Argv(Vec::new())
    }
}

fn default_true() -> bool {
    true
}

/// `run_script` params.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunScriptParams {
    /// Owning stem; `null` for a workspace-level script.
    #[serde(default)]
    pub stem: Option<String>,
    /// Script name.
    pub name: String,
    /// Arguments (argv or object).
    #[serde(default)]
    pub args: ScriptArgsInput,
    /// `true` (default): reply when the script finished ([`RunScriptResult`]);
    /// `false`: reply at once ([`RunScriptAccepted`]) and follow the
    /// `script.*` events carrying the same `run_id`.
    #[serde(default = "default_true")]
    pub wait: bool,
    /// Start the script's `requires:` stems if they are not healthy
    /// (otherwise `SCRIPT_REQUIRES_UNMET`).
    #[serde(default)]
    pub start_deps: bool,
    /// How long to wait for the owning stem while it is starting
    /// (`setup`/`starting`/`seeding`); default 30 000.
    #[serde(default)]
    pub ready_timeout_ms: Option<u64>,
}

impl RunScriptParams {
    /// Params for `stem`'s script `name` with argv `args`, waiting for the result.
    pub fn new(stem: Option<String>, name: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            stem,
            name: name.into(),
            args: ScriptArgsInput::Argv(args),
            wait: true,
            start_deps: false,
            ready_timeout_ms: None,
        }
    }
}

/// `run_script` reply with `wait: false`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunScriptAccepted {
    /// Id of this script run (in the `script.*` events' `data.run_id`).
    pub run_id: String,
    /// Stem (`null` for workspace scripts).
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
}

/// `run_script` reply with `wait: true`: how the (last attempt of the)
/// script ended. A failed script is still a successful RPC: `ok: false`
/// and `error` (`SCRIPT_FAILED`) describe it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunScriptResult {
    /// Id of this script run (in the `script.*` events' `data.run_id`).
    pub run_id: String,
    /// Stem (`null` for workspace scripts).
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
    /// Exit 0, in time.
    pub ok: bool,
    /// Exit code of the last attempt (`null` when killed by a signal).
    pub exit: Option<i32>,
    /// Signal that killed the last attempt.
    pub signal: Option<i32>,
    /// Wall time of all attempts together (backoff included).
    pub duration_ms: u64,
    /// The last attempt ran into the script's `timeout`.
    pub timed_out: bool,
    /// Attempts made (1 + retries used).
    pub attempts: u32,
    /// The argv the script received (`--name value` pairs or passthrough).
    pub argv: Vec<String>,
    /// The last output lines of the last attempt.
    pub tail: Vec<String>,
    /// The run waited for another run of a script of the same stem.
    pub queued: bool,
    /// `SCRIPT_FAILED` when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub error: Option<stems_core::Error>,
}

/// `script_catalog` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptCatalogParams {
    /// Only this stem's scripts (workspace scripts are then left out).
    #[serde(default)]
    pub stem: Option<String>,
}

/// One entry of the script catalogue: [`ScriptCatalogEntry`] plus the MCP
/// tool name and input schema derived from it (FR-AI-2).
///
/// [`ScriptCatalogEntry`]: stems_core::scriptargs::ScriptCatalogEntry
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogScript {
    /// The catalogue entry.
    #[serde(flatten)]
    pub entry: stems_core::scriptargs::ScriptCatalogEntry,
    /// `<stem>__<script>` / `workspace__<script>`.
    pub mcp_tool: String,
    /// JSON Schema of the arguments (an MCP tool's `inputSchema`).
    pub input_schema: serde_json::Value,
}

impl From<stems_core::scriptargs::ScriptCatalogEntry> for CatalogScript {
    fn from(entry: stems_core::scriptargs::ScriptCatalogEntry) -> Self {
        Self {
            mcp_tool: entry.mcp_tool_name(),
            input_schema: stems_core::scriptargs::json_schema_for(&entry.args),
            entry,
        }
    }
}

/// `script_catalog` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptCatalogResult {
    /// Workspace scripts first (sorted), then each enabled stem's (sorted).
    pub scripts: Vec<CatalogScript>,
}

impl ScriptCatalogResult {
    /// The catalogue from `entries` (`stems_core::scriptargs::build_catalog`),
    /// only `stem`'s scripts when given.
    pub fn from_entries(
        entries: Vec<stems_core::scriptargs::ScriptCatalogEntry>,
        stem: Option<&str>,
    ) -> Self {
        Self {
            scripts: entries
                .into_iter()
                .filter(|e| stem.is_none_or(|s| e.stem.as_deref() == Some(s)))
                .map(CatalogScript::from)
                .collect(),
        }
    }
}
