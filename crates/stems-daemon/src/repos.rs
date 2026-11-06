//! Git codebases (deliverable 20, FR-WS-3, `docs/repos.md`): clone, fetch
//! and check out `codebase: { git: <url>, ref: <branch|tag|sha> }` into the
//! managed directory (`<repos_dir>/<stem>`, default `.stems/repos/<stem>`).
//!
//! Everything goes through the `git` CLI (never libgit2) so the developer's
//! credentials, SSH config and helpers apply unchanged. The module needs no
//! supervisor: the CLI calls it directly when no daemon runs; the daemon
//! calls it for the `repos_sync` / `repos_status` RPCs and, through
//! [`ensure_clone`], before starting a stem whose clone is missing.
//!
//! Rules ([`sync`]):
//! * missing directory → `git clone [--branch <ref>]` (a sha: clone, then
//!   `checkout --detach <sha>`) → `cloned`;
//! * existing clone, `force_fetch` false → left alone (`up` never fetches);
//! * existing clone, `force_fetch` → never touch developer work: uncommitted
//!   changes, HEAD moved away from what stems last checked out, or commits
//!   that are on no remote branch/tag → `skipped_dirty`. Otherwise `git
//!   fetch --tags --force --prune origin`, then check out the ref
//!   (branches only fast-forward) → `checked_out` / `fetched` / `up_to_date`;
//! * a local path (e.g. a `stems.local.yaml` override) → `skipped_local`.
//!
//! `reset` (`repos sync --reset`, 49) drops the "HEAD moved" rule, so a
//! tree moved by the developer (or by [`checkout`]) returns to the declared
//! ref; uncommitted changes and unpushed commits are still left alone.
//!
//! [`checkout`] (`repos checkout <stem> <ref>`, 49) fetches and checks out
//! any branch (as `origin/<branch>`), tag or sha, detached, and records
//! nothing: to `sync` the tree has been moved by the developer. [`status`]
//! reports what is `checked_out` and whether it `drift`s from the declared
//! ref.
//!
//! git output of the mutating commands goes to the stem's log as `stream:
//! script, tag: git` when a sink is given. Events: `repo.cloned`,
//! `repo.fetched`, `repo.checked_out`, `repo.skipped_dirty`, `repo.failed`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{Value, json};
use stems_api::{
    EventKind, RepoAction, RepoSource, RepoStatus, RepoSyncResult, ReposStatusResult,
    ReposSyncResult,
};
use stems_config::{Codebase, Stem, Workspace};
use stems_core::{Error, ErrorCode};

use crate::events::{EventBus, EventDraft};
use crate::supervisor::OutputSink;

/// Lines of git output kept for `details.tail`.
pub const TAIL_LINES: usize = 20;
/// Log tag of git output.
pub const LOG_TAG: &str = "git";
/// File in the clone's git dir recording what stems last checked out.
const MARKER: &str = "stems-sync";

/// Knobs of [`sync`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncOptions {
    /// Fetch + check out existing clones (`repos sync`, `up --sync`);
    /// `false` only clones missing ones (plain `up`).
    pub force_fetch: bool,
    /// Pass `--recurse-submodules` to clone and checkout.
    pub recurse_submodules: bool,
    /// Return a tree the developer moved to the declared ref (49).
    pub reset: bool,
}

/// Where events and git output go, and which `git` to run.
#[derive(Clone)]
pub struct RepoCtx {
    /// Event bus (`None` without a daemon).
    pub events: Option<Arc<EventBus>>,
    /// Log sink (`None` without a daemon).
    pub sink: Option<Arc<dyn OutputSink>>,
    /// Who asked.
    pub actor: String,
    /// The git binary (default `git`, looked up on `PATH`).
    pub git: PathBuf,
}

impl RepoCtx {
    /// No events, no logs (daemonless CLI).
    pub fn offline(actor: impl Into<String>) -> Self {
        Self {
            events: None,
            sink: None,
            actor: actor.into(),
            git: PathBuf::from("git"),
        }
    }

    /// The supervisor's events and log sink.
    pub(crate) fn for_core(core: &crate::supervisor::Core, actor: &str) -> Self {
        Self {
            events: Some(core.events.clone()),
            sink: Some(core.sink.clone()),
            actor: actor.to_string(),
            git: PathBuf::from("git"),
        }
    }

    fn emit(&self, kind: EventKind, stem: &str, data: Value) {
        if let Some(ev) = &self.events {
            ev.emit(EventDraft::new(kind, &self.actor).stem(stem).data(data));
        }
    }
}

/// True for a (possibly abbreviated) commit sha: 7–40 lowercase hex digits.
pub fn is_sha(r: &str) -> bool {
    (7..=40).contains(&r.len())
        && r.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The shape of a `ref:` (without asking the remote).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// No ref: the remote's default branch.
    Default,
    /// Looks like a commit sha.
    Sha,
    /// A branch or tag name (resolved against the remote after fetching).
    Name,
}

/// Classify a configured ref.
pub fn ref_kind(r: Option<&str>) -> RefKind {
    match r {
        None | Some("") => RefKind::Default,
        Some(s) if is_sha(s) => RefKind::Sha,
        Some(_) => RefKind::Name,
    }
}

// ---------------------------------------------------------------------------
// git runner
// ---------------------------------------------------------------------------

struct Out {
    ok: bool,
    stdout: String,
    tail: Vec<String>,
}

impl Out {
    fn line(&self) -> String {
        self.stdout.trim().to_string()
    }
}

struct Git<'a> {
    ctx: &'a RepoCtx,
    stem: &'a str,
}

impl Git<'_> {
    /// Run `git <args>` in `cwd`. `log`: write the command and its output to
    /// the stem's log. `Err` only when git cannot be started.
    async fn run(&self, cwd: &Path, args: &[&str], log: bool) -> Result<Out, Error> {
        let mut cmd = tokio::process::Command::new(&self.ctx.git);
        cmd.args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                git_not_installed(&self.ctx.git)
            } else {
                Error::internal(format!("cannot run git: {e}"))
            }
        })?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let mut tail: VecDeque<String> = VecDeque::new();
        for l in stderr
            .lines()
            .chain(if log { stdout.lines() } else { "".lines() })
        {
            // git progress uses `\r`; keep the last state of each line.
            let l = l.rsplit('\r').next().unwrap_or(l).trim_end();
            if l.is_empty() {
                continue;
            }
            if tail.len() == TAIL_LINES {
                tail.pop_front();
            }
            tail.push_back(l.to_string());
        }
        if log
            && let Some(w) = self
                .ctx
                .sink
                .as_ref()
                .and_then(|s| s.script_writer(self.stem, LOG_TAG))
        {
            w.line(format!("$ git {}", args.join(" "))).await;
            for l in stdout.lines().chain(stderr.lines()) {
                let l = l.rsplit('\r').next().unwrap_or(l).trim_end();
                if !l.is_empty() {
                    w.line(l.to_string()).await;
                }
            }
        }
        Ok(Out {
            ok: out.status.success(),
            stdout,
            tail: tail.into(),
        })
    }

    /// `Some(trimmed stdout)` when the command succeeded.
    async fn query(&self, cwd: &Path, args: &[&str]) -> Result<Option<String>, Error> {
        let o = self.run(cwd, args, false).await?;
        Ok(o.ok.then(|| o.line()))
    }
}

fn git_not_installed(git: &Path) -> Error {
    Error::new(
        ErrorCode::GitNotInstalled,
        format!(
            "`{}` was not found on PATH; git codebases need it",
            git.display()
        ),
    )
    .with_hint("install git (e.g. `xcode-select --install` or your package manager), or point the codebase at a local checkout in stems.local.yaml")
    .with_details(json!({ "git": git }))
}

fn git_failed(
    stem: &str,
    url: &str,
    git_ref: Option<&str>,
    what: &str,
    tail: Vec<String>,
) -> Error {
    let last = tail.last().cloned().unwrap_or_default();
    Error::new(
        ErrorCode::GitCloneFailed,
        if last.is_empty() {
            format!("{what} for `{stem}` ({url}) failed")
        } else {
            format!("{what} for `{stem}` ({url}) failed: {last}")
        },
    )
    .with_hint(format!(
        "check the URL and ref and that `git clone {url}` works in your shell; or point `stems.{stem}.codebase` at a local checkout in stems.local.yaml"
    ))
    .with_details(json!({ "stem": stem, "url": url, "ref": git_ref, "tail": tail }))
}

// ---------------------------------------------------------------------------
// selection
// ---------------------------------------------------------------------------

/// The stems to act on: `names` (each must exist; disabled ones are
/// allowed when named), else every enabled stem with a codebase.
pub fn select<'a>(ws: &'a Workspace, names: &[String]) -> Result<Vec<&'a Stem>, Error> {
    if names.is_empty() {
        return Ok(ws.stems().filter(|s| s.codebase.is_some()).collect());
    }
    let mut out = Vec::new();
    for n in names {
        let Some(s) = ws.stem(n) else {
            return Err(Error::new(
                ErrorCode::UnknownStem,
                format!("stem `{n}` does not exist in workspace `{}`", ws.name),
            )
            .with_hint(format!(
                "known stems: {}",
                ws.stems.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            .with_details(json!({ "stem": n })));
        };
        if s.codebase.is_some() {
            out.push(s);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// sync
// ---------------------------------------------------------------------------

/// Sync the codebases of `names` (empty = every enabled stem with one), one
/// after the other. `Err` only for an unknown stem; git failures are
/// `failed` entries (`ok: false`).
pub async fn sync(
    ws: &Workspace,
    names: &[String],
    opts: &SyncOptions,
    ctx: &RepoCtx,
) -> Result<ReposSyncResult, Error> {
    let stems = select(ws, names)?;
    let mut repos = Vec::new();
    for s in stems {
        repos.push(sync_stem(s, opts, ctx).await);
    }
    Ok(ReposSyncResult {
        ok: repos.iter().all(|r| r.action != RepoAction::Failed),
        repos,
    })
}

/// Before starting `stem`: clone its git codebase if the managed directory
/// is missing (never fetches). `Err` = the clone failed.
pub async fn ensure_clone(
    stem: &Stem,
    opts: &SyncOptions,
    ctx: &RepoCtx,
) -> Result<Option<RepoSyncResult>, Error> {
    let Some(Codebase::Git { path, .. }) = &stem.codebase else {
        return Ok(None);
    };
    if is_populated(path) {
        return Ok(None);
    }
    let opts = SyncOptions {
        force_fetch: false,
        ..opts.clone()
    };
    let r = sync_stem(stem, &opts, ctx).await;
    match &r.error {
        Some(e) => Err(e.clone()),
        None => Ok(Some(r)),
    }
}

/// A directory that exists and is not empty.
fn is_populated(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_some())
}

/// Sync one stem's codebase.
pub async fn sync_stem(stem: &Stem, opts: &SyncOptions, ctx: &RepoCtx) -> RepoSyncResult {
    let name = stem.name.as_str();
    let (url, git_ref, path) = match &stem.codebase {
        Some(Codebase::Git { url, git_ref, path }) => (url.as_str(), git_ref.clone(), path.clone()),
        Some(Codebase::Local { path }) => {
            return RepoSyncResult {
                stem: name.to_string(),
                path: path.clone(),
                action: RepoAction::SkippedLocal,
                git_ref: None,
                sha: None,
                message: "local codebase (not managed by stems)".into(),
                error: None,
                detached: false,
            };
        }
        None => {
            return RepoSyncResult {
                stem: name.to_string(),
                path: PathBuf::new(),
                action: RepoAction::SkippedLocal,
                git_ref: None,
                sha: None,
                message: "no codebase".into(),
                error: None,
                detached: false,
            };
        }
    };
    let git = Git { ctx, stem: name };
    let base = RepoSyncResult {
        stem: name.to_string(),
        path: path.clone(),
        action: RepoAction::UpToDate,
        git_ref: git_ref.clone(),
        sha: None,
        message: String::new(),
        error: None,
        detached: false,
    };
    let r = if is_populated(&path) {
        if opts.force_fetch {
            update(&git, url, git_ref.as_deref(), &path, opts, base).await
        } else {
            let sha = git
                .query(&path, &["rev-parse", "HEAD"])
                .await
                .ok()
                .flatten();
            Ok(RepoSyncResult {
                sha,
                message:
                    "already cloned; not fetched (use `stems repos sync` or `stems up --sync`)"
                        .into(),
                ..base
            })
        }
    } else {
        clone(&git, url, git_ref.as_deref(), &path, opts, base).await
    };
    match r {
        Ok(r) => r,
        Err(e) => {
            ctx.emit(
                EventKind::REPO_FAILED,
                name,
                json!({ "url": url, "ref": git_ref, "path": path, "error": e }),
            );
            RepoSyncResult {
                stem: name.to_string(),
                path,
                action: RepoAction::Failed,
                git_ref,
                sha: None,
                message: e.message.clone(),
                error: Some(e),
                detached: false,
            }
        }
    }
}

async fn clone(
    git: &Git<'_>,
    url: &str,
    git_ref: Option<&str>,
    path: &Path,
    opts: &SyncOptions,
    base: RepoSyncResult,
) -> Result<RepoSyncResult, Error> {
    let stem = git.stem;
    let parent = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent)
        .map_err(|e| Error::internal(format!("cannot create {}: {e}", parent.display())))?;
    let dest = path.display().to_string();
    let kind = ref_kind(git_ref);
    let mut args = vec!["clone"];
    if opts.recurse_submodules {
        args.push("--recurse-submodules");
    }
    if let (RefKind::Name, Some(r)) = (kind, git_ref) {
        args.extend(["--branch", r]);
    }
    args.extend(["--", url, dest.as_str()]);
    let o = git.run(parent, &args, true).await?;
    if !o.ok {
        return Err(git_failed(stem, url, git_ref, "git clone", o.tail));
    }
    if let (RefKind::Sha, Some(r)) = (kind, git_ref) {
        let mut args = vec!["checkout", "--detach"];
        if opts.recurse_submodules {
            args.push("--recurse-submodules");
        }
        args.push(r);
        let o = git.run(path, &args, true).await?;
        if !o.ok {
            // Do not leave a clone at the wrong commit behind.
            let _ = std::fs::remove_dir_all(path);
            return Err(git_failed(
                stem,
                url,
                git_ref,
                &format!("checking out `{r}`"),
                o.tail,
            ));
        }
    }
    let sha = git.query(path, &["rev-parse", "HEAD"]).await?;
    let branch = git
        .query(path, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;
    write_marker(git, path, branch.as_deref(), sha.as_deref()).await;
    git.ctx.emit(
        EventKind::REPO_CLONED,
        stem,
        json!({ "url": url, "ref": git_ref, "sha": sha, "path": path }),
    );
    Ok(RepoSyncResult {
        action: RepoAction::Cloned,
        sha,
        message: format!("cloned {url} into {}", path.display()),
        ..base
    })
}

/// What stems last checked out: `(branch or "-", sha)`.
async fn read_marker(git: &Git<'_>, path: &Path) -> Option<(Option<String>, String)> {
    let dir = git
        .query(path, &["rev-parse", "--absolute-git-dir"])
        .await
        .ok()??;
    let text = std::fs::read_to_string(Path::new(&dir).join(MARKER)).ok()?;
    let mut it = text.split_whitespace();
    let b = it.next()?;
    let sha = it.next()?.to_string();
    Some(((b != "-").then(|| b.to_string()), sha))
}

async fn write_marker(git: &Git<'_>, path: &Path, branch: Option<&str>, sha: Option<&str>) {
    let Ok(Some(dir)) = git.query(path, &["rev-parse", "--absolute-git-dir"]).await else {
        return;
    };
    let _ = std::fs::write(
        Path::new(&dir).join(MARKER),
        format!("{} {}\n", branch.unwrap_or("-"), sha.unwrap_or("-")),
    );
}

async fn skipped(
    git: &Git<'_>,
    url: &str,
    base: RepoSyncResult,
    sha: Option<String>,
    reason: &str,
    message: String,
) -> Result<RepoSyncResult, Error> {
    git.ctx.emit(
        EventKind::REPO_SKIPPED_DIRTY,
        git.stem,
        json!({ "url": url, "ref": base.git_ref, "sha": sha, "path": base.path, "reason": reason }),
    );
    Ok(RepoSyncResult {
        action: RepoAction::SkippedDirty,
        sha,
        message,
        ..base
    })
}

async fn update(
    git: &Git<'_>,
    url: &str,
    git_ref: Option<&str>,
    path: &Path,
    opts: &SyncOptions,
    base: RepoSyncResult,
) -> Result<RepoSyncResult, Error> {
    let stem = git.stem;
    let shown = path.display().to_string();
    if !path.join(".git").exists() {
        return Err(Error::new(
            ErrorCode::GitCloneFailed,
            format!("the codebase directory of `{stem}` ({shown}) exists but is not a git checkout"),
        )
        .with_hint(format!(
            "move it away so stems can clone {url} there, or point `stems.{stem}.codebase` at it in stems.local.yaml"
        ))
        .with_details(json!({ "stem": stem, "url": url, "ref": git_ref, "path": path, "tail": [] })));
    }
    let sha0 = git.query(path, &["rev-parse", "HEAD"]).await?;
    let branch0 = git
        .query(path, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;

    // 1. Uncommitted work.
    let porcelain = git
        .query(path, &["status", "--porcelain"])
        .await?
        .unwrap_or_default();
    if !porcelain.is_empty() {
        let n = porcelain.lines().count();
        return skipped(
            git,
            url,
            base,
            sha0,
            "uncommitted_changes",
            format!("{n} uncommitted change(s) in {shown}; left alone (commit, stash or discard them, then sync again)"),
        )
        .await;
    }
    // 2. HEAD moved away from what stems checked out (another branch, or
    // another commit of a detached HEAD: `repos checkout`), unless `reset`.
    let marker = read_marker(git, path).await;
    if let Some((mb, msha)) = &marker
        && !opts.reset
        && (*mb != branch0 || (mb.is_none() && sha0.as_deref() != Some(msha.as_str())))
    {
        let at = |b: Option<&str>, sha: Option<&str>| match (b, sha) {
            (Some(b), _) => format!("branch `{b}`"),
            (None, Some(s)) => format!("a detached HEAD at {}", &s[..s.len().min(12)]),
            (None, None) => "a detached HEAD".to_string(),
        };
        let now = at(branch0.as_deref(), sha0.as_deref());
        let was = at(mb.as_deref(), Some(msha.as_str()));
        return skipped(
            git,
            url,
            base,
            sha0,
            "moved",
            format!(
                "{shown} is on {now}, not on {was} that stems checked out; left alone (`stems repos sync --reset {stem}` returns it to {})",
                describe(git_ref)
            ),
        )
        .await;
    }

    // 3. Fetch.
    let o = git
        .run(
            path,
            &["fetch", "--tags", "--force", "--prune", "origin"],
            true,
        )
        .await?;
    if !o.ok {
        return Err(git_failed(stem, url, git_ref, "git fetch", o.tail));
    }

    // 4. Commits that exist nowhere else.
    let stems_sha = marker.as_ref().map(|(_, s)| s.as_str());
    if sha0.as_deref() != stems_sha && head_unreachable(git, path).await? {
        git.ctx.emit(
            EventKind::REPO_FETCHED,
            stem,
            json!({ "url": url, "ref": git_ref, "sha": sha0, "updated": false }),
        );
        return skipped(
            git,
            url,
            base,
            sha0,
            "unpushed_commits",
            format!("HEAD of {shown} has commits on no remote branch or tag; left alone (push them first)"),
        )
        .await;
    }

    // 5. Check out the ref.
    let recurse: &[&str] = if opts.recurse_submodules {
        &["--recurse-submodules"]
    } else {
        &[]
    };
    let fail = |what: String, tail: Vec<String>| git_failed(stem, url, git_ref, &what, tail);
    match (ref_kind(git_ref), git_ref) {
        (RefKind::Name, Some(r)) => {
            let remote_branch = format!("refs/remotes/origin/{r}");
            let tag = format!("refs/tags/{r}");
            if git
                .query(path, &["rev-parse", "-q", "--verify", &remote_branch])
                .await?
                .is_some()
            {
                let upstream = format!("origin/{r}");
                if branch0.as_deref() != Some(r) {
                    let local = format!("refs/heads/{r}");
                    let exists = git
                        .query(path, &["rev-parse", "-q", "--verify", &local])
                        .await?
                        .is_some();
                    let mut args: Vec<&str> = vec!["checkout"];
                    args.extend(recurse);
                    if exists {
                        args.push(r);
                    } else {
                        args.extend(["-b", r, "--track", upstream.as_str()]);
                    }
                    let o = git.run(path, &args, true).await?;
                    if !o.ok {
                        return Err(fail(format!("checking out `{r}`"), o.tail));
                    }
                }
                let o = git
                    .run(path, &["merge", "--ff-only", &upstream], true)
                    .await?;
                if !o.ok {
                    return Err(fail(
                        format!("fast-forwarding `{r}` to `{upstream}`"),
                        o.tail,
                    ));
                }
            } else {
                let target = if git
                    .query(path, &["rev-parse", "-q", "--verify", &tag])
                    .await?
                    .is_some()
                {
                    tag
                } else {
                    r.to_string()
                };
                let mut args: Vec<&str> = vec!["checkout", "--detach"];
                args.extend(recurse);
                args.push(&target);
                let o = git.run(path, &args, true).await?;
                if !o.ok {
                    return Err(fail(
                        format!(
                            "checking out `{r}` (no such branch, tag or commit on the remote?)"
                        ),
                        o.tail,
                    ));
                }
            }
        }
        (RefKind::Sha, Some(r)) => {
            let mut args: Vec<&str> = vec!["checkout", "--detach"];
            args.extend(recurse);
            args.push(r);
            let o = git.run(path, &args, true).await?;
            if !o.ok {
                return Err(fail(format!("checking out `{r}`"), o.tail));
            }
        }
        _ => {
            // The remote's default branch: fast-forward the current branch.
            if branch0.is_some()
                && git
                    .query(path, &["rev-parse", "-q", "--verify", "@{upstream}"])
                    .await?
                    .is_some()
            {
                let o = git
                    .run(path, &["merge", "--ff-only", "@{upstream}"], true)
                    .await?;
                if !o.ok {
                    return Err(fail(
                        "fast-forwarding to the upstream branch".into(),
                        o.tail,
                    ));
                }
            }
        }
    }

    let sha = git.query(path, &["rev-parse", "HEAD"]).await?;
    let branch = git
        .query(path, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?;
    write_marker(git, path, branch.as_deref(), sha.as_deref()).await;
    let updated = sha != sha0 || branch != branch0;
    git.ctx.emit(
        EventKind::REPO_FETCHED,
        stem,
        json!({ "url": url, "ref": git_ref, "sha": sha, "updated": updated }),
    );
    let (action, message) = if !updated {
        (
            RepoAction::UpToDate,
            format!("already at {}", describe(git_ref)),
        )
    } else if branch == branch0 && branch.is_some() {
        (
            RepoAction::Fetched,
            format!("fast-forwarded {}", describe(git_ref)),
        )
    } else {
        git.ctx.emit(
            EventKind::REPO_CHECKED_OUT,
            stem,
            json!({ "url": url, "ref": git_ref, "sha": sha, "from": sha0, "path": path }),
        );
        (
            RepoAction::CheckedOut,
            format!("checked out {}", describe(git_ref)),
        )
    };
    Ok(RepoSyncResult {
        action,
        sha,
        message,
        ..base
    })
}

fn describe(git_ref: Option<&str>) -> String {
    git_ref.map_or_else(|| "the default branch".to_string(), |r| format!("`{r}`"))
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Per-stem codebase state of `names` (empty = every enabled stem with a
/// codebase). Local codebases are reported with `source: local` (git fields
/// filled when the directory is a git checkout).
pub async fn status(
    ws: &Workspace,
    names: &[String],
    ctx: &RepoCtx,
) -> Result<ReposStatusResult, Error> {
    let stems = select(ws, names)?;
    let mut repos = Vec::new();
    for s in stems {
        let (source, url, git_ref, path) = match &s.codebase {
            Some(Codebase::Git { url, git_ref, path }) => (
                RepoSource::Git,
                Some(url.clone()),
                git_ref.clone(),
                path.clone(),
            ),
            Some(Codebase::Local { path }) => (RepoSource::Local, None, None, path.clone()),
            None => continue,
        };
        let git = Git { ctx, stem: &s.name };
        let exists = path.is_dir();
        let mut st = RepoStatus {
            stem: s.name.clone(),
            source,
            path: path.clone(),
            url,
            git_ref,
            branch: None,
            sha: None,
            dirty: false,
            ahead: None,
            behind: None,
            exists,
            checked_out: None,
            drift: false,
        };
        if exists && path.join(".git").exists() {
            st.sha = git.query(&path, &["rev-parse", "HEAD"]).await?;
            st.branch = git
                .query(&path, &["symbolic-ref", "-q", "--short", "HEAD"])
                .await?;
            st.dirty = !git
                .query(&path, &["status", "--porcelain"])
                .await?
                .unwrap_or_default()
                .is_empty();
            if st.branch.is_some()
                && let Some(counts) = git
                    .query(
                        &path,
                        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
                    )
                    .await?
            {
                let mut it = counts.split_whitespace().map(|n| n.parse::<u32>().ok());
                st.ahead = it.next().flatten();
                st.behind = it.next().flatten();
            }
            st.checked_out =
                checked_out(&git, &path, st.branch.as_deref(), st.sha.as_deref()).await?;
            if source == RepoSource::Git {
                st.drift = drift(
                    &git,
                    &path,
                    st.git_ref.as_deref(),
                    st.branch.as_deref(),
                    st.sha.as_deref(),
                )
                .await?;
            }
        }
        repos.push(st);
    }
    Ok(ReposStatusResult { repos })
}

/// What is checked out: the branch, else a tag at HEAD, else a remote
/// branch at HEAD (`origin/x`), else the sha.
async fn checked_out(
    git: &Git<'_>,
    path: &Path,
    branch: Option<&str>,
    sha: Option<&str>,
) -> Result<Option<String>, Error> {
    if let Some(b) = branch {
        return Ok(Some(b.to_string()));
    }
    let Some(sha) = sha else {
        return Ok(None);
    };
    if let Some(tag) = git
        .query(path, &["describe", "--tags", "--exact-match", "HEAD"])
        .await?
        .filter(|t| !t.is_empty())
    {
        return Ok(Some(tag));
    }
    let remotes = git
        .query(
            path,
            &[
                "for-each-ref",
                "--points-at",
                "HEAD",
                "--format=%(refname:short)",
                "refs/remotes",
            ],
        )
        .await?
        .unwrap_or_default();
    if let Some(r) = remotes
        .lines()
        .map(str::trim)
        .find(|r| r.contains('/') && !r.ends_with("/HEAD"))
    {
        return Ok(Some(r.to_string()));
    }
    Ok(Some(sha.to_string()))
}

/// HEAD is not the declared ref: for a branch, another branch or a detached
/// HEAD; for a tag or sha, another commit; for the default branch, a
/// detached HEAD or a branch other than `origin/HEAD`'s.
async fn drift(
    git: &Git<'_>,
    path: &Path,
    git_ref: Option<&str>,
    branch: Option<&str>,
    sha: Option<&str>,
) -> Result<bool, Error> {
    let Some(sha) = sha else {
        return Ok(false);
    };
    Ok(match (ref_kind(git_ref), git_ref) {
        (RefKind::Sha, Some(r)) => !sha.starts_with(r),
        (RefKind::Name, Some(r)) => {
            let remote_branch = format!("refs/remotes/origin/{r}");
            if git
                .query(path, &["rev-parse", "-q", "--verify", &remote_branch])
                .await?
                .is_some()
            {
                branch != Some(r)
            } else {
                let target = format!("{r}^{{commit}}");
                match git
                    .query(path, &["rev-parse", "-q", "--verify", &target])
                    .await?
                {
                    Some(t) => t != sha,
                    None => false,
                }
            }
        }
        _ => match branch {
            None => true,
            Some(b) => git
                .query(
                    path,
                    &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"],
                )
                .await?
                .and_then(|d| d.strip_prefix("origin/").map(str::to_string))
                .is_some_and(|d| d != b),
        },
    })
}

/// HEAD has commits on no remote branch or tag (after a fetch): moving it
/// would leave them reachable only from the reflog.
async fn head_unreachable(git: &Git<'_>, path: &Path) -> Result<bool, Error> {
    let containing = git
        .query(
            path,
            &[
                "for-each-ref",
                "--contains",
                "HEAD",
                "--count=1",
                "refs/remotes",
                "refs/tags",
            ],
        )
        .await?
        .unwrap_or_default();
    Ok(containing.is_empty())
}

/// Detached HEAD whose commits no local branch, remote branch or tag has
/// (an attached HEAD is a branch itself, so never unreachable).
async fn head_detached_unreachable(git: &Git<'_>, path: &Path) -> Result<bool, Error> {
    if git
        .query(path, &["symbolic-ref", "-q", "HEAD"])
        .await?
        .is_some_and(|r| !r.is_empty())
    {
        return Ok(false);
    }
    let containing = git
        .query(
            path,
            &[
                "for-each-ref",
                "--contains",
                "HEAD",
                "--count=1",
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ],
        )
        .await?
        .unwrap_or_default();
    Ok(containing.is_empty())
}

/// The remote a local work tree follows: the current branch's upstream
/// remote, else the only remote, else `origin` when it exists.
async fn local_remote(git: &Git<'_>, path: &Path) -> Result<Option<String>, Error> {
    let remotes: Vec<String> = git
        .query(path, &["remote"])
        .await?
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    if let Some(b) = git
        .query(path, &["symbolic-ref", "-q", "--short", "HEAD"])
        .await?
        && let Some(r) = git
            .query(path, &["config", &format!("branch.{b}.remote")])
            .await?
        && remotes.contains(&r)
    {
        return Ok(Some(r));
    }
    Ok(match remotes.as_slice() {
        [only] => Some(only.clone()),
        _ if remotes.iter().any(|r| r == "origin") => Some("origin".into()),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// checkout
// ---------------------------------------------------------------------------

/// `repos checkout <stem> <ref>` (49): clone the codebase if missing,
/// refuse uncommitted changes unless `force` (`REPO_DIRTY`; with `force`
/// they are carried over as `git checkout` does), fetch, then check out
/// `git_ref` detached: a branch as `origin/<branch>`, a tag, or a sha.
/// Nothing is recorded, so `sync` leaves the tree alone until `sync
/// --reset`. Event `repo.checked_out`.
pub async fn checkout(
    ws: &Workspace,
    name: &str,
    git_ref: &str,
    force: bool,
    ctx: &RepoCtx,
) -> Result<RepoSyncResult, Error> {
    // Never an option to git (`--orphan=…`, `-b`): a ref, tag or sha only.
    if git_ref.starts_with('-') || git_ref.trim().is_empty() {
        return Err(Error::usage(
            format!("`{git_ref}` is not a git ref"),
            "give a branch, tag or commit (it cannot start with `-`)",
        )
        .with_details(json!({ "stem": name, "ref": git_ref })));
    }
    let stem = select(ws, &[name.to_string()])?
        .into_iter()
        .next()
        .ok_or_else(|| {
            Error::usage(
                format!("stem `{name}` has no codebase"),
                "`stems repos checkout` needs a stem with a codebase",
            )
            .with_details(json!({ "stem": name }))
        })?;
    let (url, declared, path, local) = match &stem.codebase {
        Some(Codebase::Git { url, git_ref, path }) => {
            (url.as_str(), git_ref.clone(), path.clone(), false)
        }
        Some(Codebase::Local { path }) => ("", None, path.clone(), true),
        None => unreachable!("select() keeps stems with a codebase"),
    };
    if local {
        return checkout_local(name, &path, git_ref, force, ctx).await;
    }
    if !is_populated(&path) {
        let r = sync_stem(stem, &SyncOptions::default(), ctx).await;
        if let Some(e) = r.error {
            return Err(e);
        }
    }
    let git = Git { ctx, stem: name };
    let shown = path.display().to_string();
    let sha0 = git.query(&path, &["rev-parse", "HEAD"]).await?;
    let porcelain = git
        .query(&path, &["status", "--porcelain"])
        .await?
        .unwrap_or_default();
    if !porcelain.is_empty() && !force {
        let n = porcelain.lines().count();
        return Err(Error::new(
            ErrorCode::RepoDirty,
            format!("the codebase of `{name}` ({shown}) has {n} uncommitted change(s); `{git_ref}` was not checked out"),
        )
        .with_hint(format!(
            "commit, stash or discard them, or rerun with `--force` to carry them over (`stems repos checkout {name} {git_ref} --force`)"
        ))
        .with_details(json!({ "stem": name, "path": path, "ref": git_ref, "changes": n })));
    }
    let o = git
        .run(
            &path,
            &["fetch", "--tags", "--force", "--prune", "origin"],
            true,
        )
        .await?;
    if !o.ok {
        return Err(git_failed(name, url, Some(git_ref), "git fetch", o.tail));
    }
    // Commits on HEAD that no remote branch or tag has would be lost from
    // sight (only the reflog would still reach them).
    if !force && sha0.is_some() && head_unreachable(&git, &path).await? {
        return Err(Error::new(
            ErrorCode::RepoDirty,
            format!(
                "HEAD of the codebase of `{name}` ({shown}) has commits on no remote branch or tag; `{git_ref}` was not checked out"
            ),
        )
        .with_hint(format!(
            "push them (or create a branch for them) first: after the checkout they would only be reachable from the reflog; or rerun with `--force` (`stems repos checkout {name} {git_ref} --force`)"
        ))
        .with_details(json!({ "stem": name, "path": path, "ref": git_ref, "sha": sha0, "unpushed_commits": true })));
    }
    let remote_branch = format!("refs/remotes/origin/{git_ref}");
    let tag = format!("refs/tags/{git_ref}");
    let target = if git
        .query(&path, &["rev-parse", "-q", "--verify", &remote_branch])
        .await?
        .is_some()
    {
        format!("origin/{git_ref}")
    } else if git
        .query(&path, &["rev-parse", "-q", "--verify", &tag])
        .await?
        .is_some()
    {
        tag
    } else {
        git_ref.to_string()
    };
    let o = git
        .run(&path, &["checkout", "--detach", &target], true)
        .await?;
    if !o.ok {
        return Err(git_failed(
            name,
            url,
            Some(git_ref),
            &format!("checking out `{git_ref}` (no such branch, tag or commit on the remote?)"),
            o.tail,
        ));
    }
    let sha = git.query(&path, &["rev-parse", "HEAD"]).await?;
    ctx.emit(
        EventKind::REPO_CHECKED_OUT,
        name,
        json!({ "url": url, "ref": git_ref, "sha": sha, "from": sha0, "path": path, "declared": declared }),
    );
    Ok(RepoSyncResult {
        stem: name.to_string(),
        path,
        action: RepoAction::CheckedOut,
        git_ref: Some(git_ref.to_string()),
        sha,
        message: format!(
            "checked out `{git_ref}` (detached); `stems repos sync --reset {name}` returns to {}",
            describe(declared.as_deref())
        ),
        error: None,
        detached: true,
    })
}

/// What `repos checkout` does with a local-path codebase that is a git work
/// tree: the same dirty-tree refusal and fetch (when a remote exists) as for
/// a managed clone, but a local branch of that name is switched to (attached)
/// and nothing is cloned.
async fn checkout_local(
    name: &str,
    path: &Path,
    git_ref: &str,
    force: bool,
    ctx: &RepoCtx,
) -> Result<RepoSyncResult, Error> {
    let git = Git { ctx, stem: name };
    let shown = path.display().to_string();
    let inside = path.is_dir()
        && git
            .query(path, &["rev-parse", "--is-inside-work-tree"])
            .await?
            .as_deref()
            == Some("true");
    if !inside {
        return Err(Error::new(
            ErrorCode::RepoNotGit,
            format!("the codebase of `{name}` ({shown}) is not a git repository"),
        )
        .with_hint(format!(
            "run `git init` there, or point `stems.{name}.codebase` at a git work tree (or use `codebase: {{ git: <url> }}` to let stems clone it)"
        ))
        .with_details(json!({ "stem": name, "path": path, "ref": git_ref })));
    }
    let sha0 = git.query(path, &["rev-parse", "HEAD"]).await?;
    let porcelain = git
        .query(path, &["status", "--porcelain"])
        .await?
        .unwrap_or_default();
    if !porcelain.is_empty() && !force {
        let n = porcelain.lines().count();
        return Err(Error::new(
            ErrorCode::RepoDirty,
            format!("the codebase of `{name}` ({shown}) has {n} uncommitted change(s); `{git_ref}` was not checked out"),
        )
        .with_hint(format!(
            "commit, stash or discard them, or rerun with `--force` to carry them over (`stems repos checkout {name} {git_ref} --force`)"
        ))
        .with_details(json!({ "stem": name, "path": path, "ref": git_ref, "changes": n })));
    }
    // The remote this repository follows: the current branch's upstream
    // remote, else the only remote, else `origin` when it exists.
    let remote = local_remote(&git, path).await?;
    let has_remotes = git
        .query(path, &["remote"])
        .await?
        .is_some_and(|r| !r.is_empty());
    // Fetch only when there is a remote. Plain `--tags`: no `--force` (it
    // would overwrite the developer's own tags) and no `--prune`. A failure
    // (offline, rejected tag update) only matters if the ref is not already
    // known locally.
    let mut fetch_warning: Option<Vec<String>> = None;
    if has_remotes {
        let mut fetch: Vec<&str> = vec!["fetch", "--tags"];
        if let Some(r) = remote.as_deref() {
            fetch.push(r);
        }
        let o = git.run(path, &fetch, true).await?;
        if !o.ok {
            fetch_warning = Some(o.tail.clone());
        }
    }
    // Commits only on HEAD (no branch, remote branch or tag has them) would
    // be left to the reflog by a checkout.
    if !force && sha0.is_some() && head_detached_unreachable(&git, path).await? {
        return Err(Error::new(
            ErrorCode::RepoDirty,
            format!(
                "HEAD of the codebase of `{name}` ({shown}) has commits on no branch or tag; `{git_ref}` was not checked out"
            ),
        )
        .with_hint(format!(
            "push them (or create a branch for them) first: after the checkout they would only be reachable from the reflog; or rerun with `--force` (`stems repos checkout {name} {git_ref} --force`)"
        ))
        .with_details(json!({ "stem": name, "path": path, "ref": git_ref, "sha": sha0, "unpushed_commits": true })));
    }
    let local_branch = format!("refs/heads/{git_ref}");
    let tag = format!("refs/tags/{git_ref}");
    let commit = format!("{git_ref}^{{commit}}");
    let has_local = git
        .query(path, &["rev-parse", "-q", "--verify", &local_branch])
        .await?
        .is_some();
    let remote_target = match remote.as_deref() {
        Some(r) => {
            let rb = format!("refs/remotes/{r}/{git_ref}");
            git.query(path, &["rev-parse", "-q", "--verify", &rb])
                .await?
                .map(|_| format!("{r}/{git_ref}"))
        }
        None => None,
    };
    let has_tag = git
        .query(path, &["rev-parse", "-q", "--verify", &tag])
        .await?
        .is_some();
    if let Some(w) = &fetch_warning {
        let resolves = has_local
            || remote_target.is_some()
            || has_tag
            || git
                .query(path, &["rev-parse", "-q", "--verify", &commit])
                .await?
                .is_some();
        if !resolves {
            return Err(git_failed(
                name,
                &shown,
                Some(git_ref),
                &format!("git fetch (and `{git_ref}` is not known locally)"),
                w.clone(),
            ));
        }
    }
    let (args, detached): (Vec<String>, bool) = if has_local {
        (vec!["checkout".into(), git_ref.into()], false)
    } else {
        let target = remote_target.unwrap_or(if has_tag { tag } else { git_ref.to_string() });
        (vec!["checkout".into(), "--detach".into(), target], true)
    };
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = git.run(path, &argv, true).await?;
    if !o.ok {
        return Err(git_failed(
            name,
            &shown,
            Some(git_ref),
            &format!("checking out `{git_ref}` (no such branch, tag or commit?)"),
            o.tail,
        ));
    }
    let sha = git.query(path, &["rev-parse", "HEAD"]).await?;
    ctx.emit(
        EventKind::REPO_CHECKED_OUT,
        name,
        json!({ "ref": git_ref, "sha": sha, "from": sha0, "path": path, "detached": detached, "fetch_warning": fetch_warning.as_ref().and_then(|t| t.last()) }),
    );
    let mut message = if detached {
        format!("checked out `{git_ref}` (detached)")
    } else {
        format!("checked out branch `{git_ref}`")
    };
    if fetch_warning.is_some() {
        message.push_str("; warning: git fetch failed, used what is known locally");
    }
    Ok(RepoSyncResult {
        stem: name.to_string(),
        path: path.to_path_buf(),
        action: RepoAction::CheckedOut,
        git_ref: Some(git_ref.to_string()),
        sha,
        message,
        error: None,
        detached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_kinds() {
        assert_eq!(ref_kind(None), RefKind::Default);
        assert_eq!(ref_kind(Some("main")), RefKind::Name);
        assert_eq!(ref_kind(Some("v2")), RefKind::Name);
        assert_eq!(ref_kind(Some("deadbeef")), RefKind::Sha);
        assert_eq!(
            ref_kind(Some("0123456789abcdef0123456789abcdef01234567")),
            RefKind::Sha
        );
        // Too short, uppercase, or not hex: a name.
        assert_eq!(ref_kind(Some("abc12")), RefKind::Name);
        assert_eq!(ref_kind(Some("DEADBEEF")), RefKind::Name);
        assert_eq!(ref_kind(Some("feature/x")), RefKind::Name);
    }

    fn sh(dir: &Path, cmd: &str) -> String {
        let o = std::process::Command::new("/bin/sh")
            .args(["-c", cmd])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A bare repo `origin.git` with one commit on `main`; returns its URL.
    fn origin(d: &Path) -> String {
        sh(
            d,
            "git init -q -b main src && cd src && echo one > f && git add f && git commit -qm one \
             && git init -q --bare ../origin.git && git push -q ../origin.git main",
        );
        format!("file://{}", d.join("origin.git").display())
    }

    fn stem(url: &str, r: Option<&str>, path: &Path) -> Stem {
        let yaml = format!(
            "schema_version: 1\nname: t\nstems:\n  api:\n    type: external\n    codebase: {{ git: '{url}'{} , path: '{}' }}\n",
            r.map(|r| format!(", ref: '{r}'")).unwrap_or_default(),
            path.display()
        );
        let d = path.parent().unwrap().join("ws");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("stems.yaml"), yaml).unwrap();
        let r = stems_config::load(stems_config::LoadOptions {
            workspace: Some(d.clone()),
            cwd: d,
            env: Default::default(),
            skip_local: true,
        })
        .unwrap();
        r.workspace.stem("api").unwrap().clone()
    }

    #[tokio::test]
    async fn clone_sync_dirty_and_failures() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path().canonicalize().unwrap();
        let url = origin(&d);
        let path = d.join("clone");
        let ctx = RepoCtx::offline("test");
        let force = SyncOptions {
            force_fetch: true,
            ..SyncOptions::default()
        };

        let s = stem(&url, Some("main"), &path);
        let r = sync_stem(&s, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::Cloned, "{r:?}");
        let r = sync_stem(&s, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::UpToDate, "{r:?}");

        // A new tag upstream; switching the ref checks it out.
        sh(
            &d,
            "cd src && echo two > f && git commit -qam two && git tag v2 && git push -q ../origin.git main v2",
        );
        let v2 = sh(&d, "cd src && git rev-parse v2");
        let s2 = stem(&url, Some("v2"), &path);
        let r = sync_stem(&s2, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::CheckedOut, "{r:?}");
        assert_eq!(r.sha.as_deref(), Some(v2.as_str()));
        let ws = stems_config::load(stems_config::LoadOptions {
            workspace: Some(d.join("ws")),
            cwd: d.join("ws"),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap()
        .workspace;
        let st = status(&ws, &[], &ctx).await.unwrap();
        assert_eq!(st.repos[0].sha.as_deref(), Some(v2.as_str()));
        assert!(!st.repos[0].dirty);

        // Back to main (a branch that moved on): checked out again.
        let r = sync_stem(&s, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::CheckedOut, "{r:?}");
        assert_eq!(r.sha.as_deref(), Some(v2.as_str()));

        // Dirty: left alone, file untouched.
        std::fs::write(path.join("f"), "mine").unwrap();
        let r = sync_stem(&s2, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::SkippedDirty, "{r:?}");
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), "mine");
        std::fs::write(path.join("f"), "two\n").unwrap();

        // The developer switched branches: left alone.
        sh(&path, "git checkout -q -b feature");
        let r = sync_stem(&s2, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::SkippedDirty, "{r:?}");
        assert!(r.message.contains("feature"), "{}", r.message);

        // Without force_fetch an existing clone is not touched.
        let r = sync_stem(&s2, &SyncOptions::default(), &ctx).await;
        assert_eq!(r.action, RepoAction::UpToDate);

        // Clone failure carries the stderr tail.
        let bad = stem("file:///nonexistent/repo.git", Some("main"), &d.join("bad"));
        let r = sync_stem(&bad, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::Failed);
        let e = r.error.unwrap();
        assert_eq!(e.code, ErrorCode::GitCloneFailed);
        assert!(!e.details["tail"].as_array().unwrap().is_empty());
        assert!(!d.join("bad").exists());

        // A sha ref: clone then detach.
        let one = sh(&d, "cd src && git rev-parse HEAD~1");
        let s3 = stem(&url, Some(&one[..12]), &d.join("bysha"));
        let r = sync_stem(&s3, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::Cloned, "{r:?}");
        assert_eq!(r.sha.as_deref(), Some(one.as_str()));
    }

    #[tokio::test]
    async fn checkout_drift_and_reset() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path().canonicalize().unwrap();
        let url = origin(&d);
        let path = d.join("clone");
        let ctx = RepoCtx::offline("test");
        let force = SyncOptions {
            force_fetch: true,
            ..SyncOptions::default()
        };
        let s = stem(&url, Some("main"), &path);
        let ws = stems_config::load(stems_config::LoadOptions {
            workspace: Some(d.join("ws")),
            cwd: d.join("ws"),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap()
        .workspace;
        // Clones a missing codebase first, then checks out the branch.
        sh(
            &d,
            "cd src && git checkout -q -b feature && echo f > g && git add g && git commit -qm f \
             && git push -q ../origin.git feature && git checkout -q main",
        );
        let feature = sh(&d, "cd src && git rev-parse feature");
        let r = checkout(&ws, "api", "feature", false, &ctx).await.unwrap();
        assert_eq!(r.action, RepoAction::CheckedOut);
        assert_eq!(r.sha.as_deref(), Some(feature.as_str()));
        let st = status(&ws, &[], &ctx).await.unwrap();
        assert!(st.repos[0].drift);
        assert_eq!(st.repos[0].checked_out.as_deref(), Some("origin/feature"));
        assert_eq!(st.repos[0].branch, None);

        // Sync leaves the moved tree alone; --reset brings it back.
        let r = sync_stem(&s, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::SkippedDirty, "{r:?}");
        assert!(r.message.contains("--reset"), "{}", r.message);
        let reset = SyncOptions {
            reset: true,
            ..force.clone()
        };
        let r = sync_stem(&s, &reset, &ctx).await;
        assert_eq!(r.action, RepoAction::CheckedOut, "{r:?}");
        let st = status(&ws, &[], &ctx).await.unwrap();
        assert!(!st.repos[0].drift);
        assert_eq!(st.repos[0].checked_out.as_deref(), Some("main"));

        // Dirty: refused without force, carried over with it.
        std::fs::write(path.join("f"), "mine").unwrap();
        let e = checkout(&ws, "api", "feature", false, &ctx)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::RepoDirty);
        let r = checkout(&ws, "api", &feature[..10], true, &ctx)
            .await
            .unwrap();
        assert_eq!(r.sha.as_deref(), Some(feature.as_str()));
        assert_eq!(std::fs::read_to_string(path.join("f")).unwrap(), "mine");

        // A detached tree moved to another commit is `moved` too (tag ref).
        sh(&path, "git checkout -q -- f");
        sh(
            &d,
            "cd src && git tag v1 main && git push -q ../origin.git v1",
        );
        let t = stem(&url, Some("v1"), &d.join("bytag"));
        assert_eq!(sync_stem(&t, &force, &ctx).await.action, RepoAction::Cloned);
        sh(&d.join("bytag"), "git checkout -q --detach origin/feature");
        let r = sync_stem(&t, &force, &ctx).await;
        assert_eq!(r.action, RepoAction::SkippedDirty, "{r:?}");
        assert!(r.message.contains("detached HEAD at"), "{}", r.message);
    }

    #[tokio::test]
    async fn checkout_local_path() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path().canonicalize().unwrap();
        let ctx = RepoCtx::offline("test");
        let p = d.join("web");
        std::fs::create_dir(&p).unwrap();
        let e = checkout_local("web", &p, "main", false, &ctx)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::RepoNotGit);
        sh(
            &p,
            "git init -q -b main && git config user.email t@e.x && git config user.name t \
             && echo a > a && git add a && git commit -qm a && git tag v1 \
             && git checkout -q -b other && echo b > b && git add b && git commit -qm b \
             && git checkout -q main",
        );
        let r = checkout_local("web", &p, "other", false, &ctx)
            .await
            .unwrap();
        assert!(!r.detached);
        assert_eq!(sh(&p, "git symbolic-ref --short HEAD"), "other");
        let r = checkout_local("web", &p, "v1", false, &ctx).await.unwrap();
        assert!(r.detached && r.message.contains("(detached)"));
        std::fs::write(p.join("a"), "dirty").unwrap();
        let e = checkout_local("web", &p, "other", false, &ctx)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::RepoDirty);
        checkout_local("web", &p, "other", true, &ctx)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn missing_git_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let s = stem("file:///x/repo.git", None, &d.path().join("c"));
        let mut ctx = RepoCtx::offline("test");
        ctx.git = PathBuf::from("/nonexistent/bin/git");
        let e = ensure_clone(&s, &SyncOptions::default(), &ctx)
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::GitNotInstalled);
    }
}
