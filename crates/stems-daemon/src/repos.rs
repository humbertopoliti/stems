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
    // 2. HEAD moved away from what stems checked out.
    let marker = read_marker(git, path).await;
    if let Some((mb, _)) = &marker
        && *mb != branch0
    {
        let now = branch0
            .as_deref()
            .map_or("a detached HEAD".to_string(), |b| format!("branch `{b}`"));
        let was = mb
            .as_deref()
            .map_or("a detached HEAD".to_string(), |b| format!("branch `{b}`"));
        return skipped(
            git,
            url,
            base,
            sha0,
            "moved",
            format!("{shown} is on {now}, not on {was} that stems checked out; left alone"),
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
    if sha0.as_deref() != stems_sha {
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
        if containing.is_empty() {
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
        }
        repos.push(st);
    }
    Ok(ReposStatusResult { repos })
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
