//! Params and results of the git-codebase methods (`repos_sync`,
//! `repos_status`), deliverable 20. See `docs/repos.md`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_core::Error;

/// `repos_sync` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReposSyncParams {
    /// Stems to sync; empty = every enabled stem with a codebase.
    #[serde(default)]
    pub stems: Vec<String>,
    /// Fetch and check out existing clones (`repos sync`, `up --sync`);
    /// `false` only clones missing ones (what `up` does).
    #[serde(default = "yes")]
    pub force_fetch: bool,
    /// Pass `--recurse-submodules` to `git clone` / `git checkout`.
    #[serde(default)]
    pub recurse_submodules: bool,
}

fn yes() -> bool {
    true
}

/// What `repos sync` did to one stem's codebase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoAction {
    /// The managed directory was missing and was cloned.
    Cloned,
    /// Fetched; the checked-out branch moved to new commits.
    Fetched,
    /// Fetched and switched to the configured ref.
    CheckedOut,
    /// Already at the configured ref (nothing changed).
    UpToDate,
    /// Left alone: uncommitted changes, unpushed commits, or HEAD moved by
    /// the developer away from the ref stems checked out.
    SkippedDirty,
    /// The codebase is a local path (`source: local`); nothing to do.
    SkippedLocal,
    /// git failed (`GIT_CLONE_FAILED` / `GIT_NOT_INSTALLED` in `error`).
    Failed,
}

/// One stem's sync outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RepoSyncResult {
    /// Stem name.
    pub stem: String,
    /// Checkout directory.
    pub path: PathBuf,
    /// What happened.
    pub action: RepoAction,
    /// Configured ref (`null` = the remote's default branch).
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// HEAD after the sync (`null` when there is no checkout).
    pub sha: Option<String>,
    /// Human explanation.
    pub message: String,
    /// The error of a `failed` sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Value>")]
    pub error: Option<Error>,
}

/// `repos_sync` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReposSyncResult {
    /// No sync failed.
    pub ok: bool,
    /// One entry per selected stem with a codebase, in declaration order.
    pub repos: Vec<RepoSyncResult>,
}

/// `repos_status` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReposStatusParams {
    /// Stems to report; empty = every enabled stem with a codebase.
    #[serde(default)]
    pub stems: Vec<String>,
}

/// Where a codebase comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoSource {
    /// `codebase: { git: <url> }` (or a git URL string): managed by stems.
    Git,
    /// A local path (possibly a `stems.local.yaml` override of a git form).
    Local,
}

/// One stem's codebase state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoStatus {
    /// Stem name.
    pub stem: String,
    /// `git` or `local`.
    pub source: RepoSource,
    /// Checkout directory.
    pub path: PathBuf,
    /// Repository URL (git source only).
    pub url: Option<String>,
    /// Configured ref (git source only; `null` = remote default branch).
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Current branch (`null` when detached or not a git checkout).
    pub branch: Option<String>,
    /// HEAD commit (`null` when missing or not a git checkout).
    pub sha: Option<String>,
    /// Uncommitted changes (`git status --porcelain` not empty).
    pub dirty: bool,
    /// Commits on the branch not on its upstream (`null` without upstream).
    pub ahead: Option<u32>,
    /// Upstream commits not on the branch (`null` without upstream).
    pub behind: Option<u32>,
    /// The directory exists.
    pub exists: bool,
}

/// `repos_status` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReposStatusResult {
    /// One entry per selected stem with a codebase, in declaration order.
    pub repos: Vec<RepoStatus>,
}
