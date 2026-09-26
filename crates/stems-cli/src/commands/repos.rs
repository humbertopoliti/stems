//! `stems repos sync | status` (deliverable 20, FR-WS-3, `docs/repos.md`).
//!
//! * **`repos sync [stems…]`** clones missing git codebases and fetches +
//!   checks out the configured ref of existing ones, never touching a
//!   working tree with local work (`skipped_dirty`). `data = { ok, repos:
//!   [{stem, path, action, ref, sha, message, error?}] }`; a failed entry is
//!   also in `errors` (`GIT_CLONE_FAILED` / `GIT_NOT_INSTALLED`, exit 1).
//! * **`repos status [stems…]`** reports per stem `{stem, source: git|local,
//!   path, url, ref, branch, sha, dirty, ahead, behind, exists}`.
//!
//! Both go through the daemon when one runs (so `repo.*` events are
//! emitted and git output lands in the stem's log), else run in-process.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use stems_api::{
    Method, RepoAction, RepoSource, ReposStatusParams, ReposStatusResult, ReposSyncParams,
    ReposSyncResult,
};
use stems_config::Workspace;
use stems_core::validate::ValidateOptions;
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::repos::{self, RepoCtx, SyncOptions};

use crate::cli::ReposArgs;
use crate::client::{self, block_on, connect_to};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Clones may be slow.
const LONG: Duration = Duration::from_secs(3600);

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The workspace, loaded and validated like the daemon does.
fn workspace(ctx: &Ctx) -> Result<Workspace, Errors> {
    let opts = ValidateOptions {
        skip_requires: true,
        ..ValidateOptions::default()
    };
    Ok(stems_core::load_and_validate(ctx.load_options(), &opts)?.workspace)
}

/// `Some(result)` from the daemon, `None` when no daemon runs.
async fn via_daemon<P: Serialize, R: serde::de::DeserializeOwned>(
    ctx: &Ctx,
    method: Method,
    p: &P,
) -> Result<Option<R>, Errors> {
    let t = client::target(ctx)?;
    match connect_to(&t, client::options(ctx)).await {
        Ok(c) => Ok(Some(c.call_with_timeout(method, p, LONG).await?)),
        Err(e) if e.code == ErrorCode::DaemonNotRunning => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn short(sha: Option<&str>) -> &str {
    sha.map_or("-", |s| &s[..s.len().min(12)])
}

fn action_name(a: RepoAction) -> String {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn sync_human(r: &ReposSyncResult) -> String {
    if r.repos.is_empty() {
        return "no stem has a codebase\n".into();
    }
    let w = r
        .repos
        .iter()
        .map(|x| x.stem.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut out = format!("{:<w$}  {:<13}  {:<12}  MESSAGE\n", "STEM", "ACTION", "SHA");
    for x in &r.repos {
        out.push_str(&format!(
            "{:<w$}  {:<13}  {:<12}  {}\n",
            x.stem,
            action_name(x.action),
            short(x.sha.as_deref()),
            x.message
        ));
    }
    out
}

fn status_human(r: &ReposStatusResult) -> String {
    if r.repos.is_empty() {
        return "no stem has a codebase\n".into();
    }
    let w = r
        .repos
        .iter()
        .map(|x| x.stem.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut out = format!(
        "{:<w$}  {:<6}  {:<14}  {:<12}  {:<5}  {:<9}  PATH\n",
        "STEM", "SOURCE", "REF", "SHA", "DIRTY", "AHEAD/BEH"
    );
    for x in &r.repos {
        let source = match x.source {
            RepoSource::Git => "git",
            RepoSource::Local => "local",
        };
        let r = x
            .git_ref
            .clone()
            .or_else(|| x.branch.clone())
            .unwrap_or_else(|| "-".into());
        let ab = match (x.ahead, x.behind) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => "-".into(),
        };
        let sha = if x.exists {
            short(x.sha.as_deref()).to_string()
        } else {
            "(missing)".into()
        };
        out.push_str(&format!(
            "{:<w$}  {:<6}  {:<14}  {:<12}  {:<5}  {:<9}  {}\n",
            x.stem,
            source,
            r,
            sha,
            if x.dirty { "yes" } else { "no" },
            ab,
            x.path.display()
        ));
    }
    out
}

/// `stems repos sync`.
pub fn sync(ctx: &Ctx, args: &ReposArgs) -> CommandOutput {
    block_on(async {
        let p = ReposSyncParams {
            stems: args.stems.clone(),
            force_fetch: true,
            recurse_submodules: args.recurse_submodules,
        };
        let r: ReposSyncResult = match via_daemon(ctx, Method::REPOS_SYNC, &p).await? {
            Some(r) => r,
            None => {
                let ws = workspace(ctx)?;
                let opts = SyncOptions {
                    force_fetch: true,
                    recurse_submodules: args.recurse_submodules,
                };
                repos::sync(&ws, &p.stems, &opts, &RepoCtx::offline(client::actor(ctx))).await?
            }
        };
        let errors: Vec<Error> = r.repos.iter().filter_map(|x| x.error.clone()).collect();
        Ok::<_, Errors>(
            CommandOutput::data(to_value(&r))
                .with_human(sync_human(&r))
                .with_errors(Errors(errors)),
        )
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems repos status`.
pub fn status(ctx: &Ctx, args: &ReposArgs) -> CommandOutput {
    block_on(async {
        let p = ReposStatusParams {
            stems: args.stems.clone(),
        };
        let r: ReposStatusResult = match via_daemon(ctx, Method::REPOS_STATUS, &p).await? {
            Some(r) => r,
            None => {
                let ws = stems_config::load(ctx.load_options()).map_err(Errors::from)?;
                repos::status(
                    &ws.workspace,
                    &p.stems,
                    &RepoCtx::offline(client::actor(ctx)),
                )
                .await?
            }
        };
        Ok::<_, Errors>(CommandOutput::data(to_value(&r)).with_human(status_human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}
