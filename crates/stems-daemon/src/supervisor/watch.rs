//! Watchdogs (deliverable 24, FR-WD-1/2, `docs/watchdogs.md`): filesystem
//! changes in a stem's codebase (or the integration repo, `root:
//! workspace`) trigger `restart`, `rebuild`, `script:<name>` or
//! `signal:<SIG>`.
//!
//! ```text
//! notify (FSEvents / inotify) ──▶ notify-debouncer-full (100 ms, per path)
//!   ──▶ Matcher (paths/ignore globs, relative to the rule's root)
//!   ──▶ Scheduler: one Coalescer per rule
//!         debounce: a batch fires `debounce` after its first change
//!         settle:   ... and only once there was no change for `settle`
//!         busy:     while an action runs, changes pile up in ONE pending
//!                   batch per rule, fired when the action finished
//!   ──▶ action task (`watch.triggered` … `watch.action_finished`)
//! ```
//!
//! The pieces are generic so the config watcher (33) can reuse them:
//! [`Matcher`] and [`Scheduler`] know nothing about stems, and
//! [`spawn_fs_watcher`] turns a set of directories into a channel of
//! changed paths.
//!
//! A stem's watcher starts when its start begins (edits during a slow
//! start count) and stops when it becomes `stopped` or `failed`. A
//! watchdog restart goes through [`StemCell::restart_bypassing_policy`]
//! (22): hooks run, `restart.max` does not count it. A rule with `cascade`
//! (or a stem with `restart.cascade`) also restarts the stem's hard
//! dependants ([`super::cascade`], FR-LC-9); triggers of stems a cascade is
//! restarting are dropped (loop guard #4).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use globset::{Glob, GlobSet, GlobSetBuilder};
use indexmap::IndexSet;
use serde_json::{Value, json};
use stems_api::{
    EventKind, RunScriptParams, StemStatus, StemWatchStatus, WatchPauseParams, WatchPauseResult,
    WatchRuleStatus, WatchStatusParams, WatchStatusResult, WatchSummary,
};
use stems_config::{Resolved, Stem, StemType, Watch, WatchAction, WatchRoot};
use stems_core::{Error, ErrorCode, StemState};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

use super::actor::StemCell;
use super::{Core, StopReply, Supervisor, schedule};
use crate::events::EventDraft;

/// The actor of every watchdog action and event.
pub const WATCHDOG_ACTOR: &str = "watchdog";

/// Ignores applied on top of the config defaults (`stems_config::defaults::
/// WATCH_IGNORE`, already merged into every rule): virtualenvs, log files
/// and stems' own `.stems` directory.
pub const EXTRA_IGNORE: [&str; 3] = ["**/.venv/**", "*.log", "**/.stems/**"];

/// At most this many paths are listed in `watch.triggered`.
pub const MAX_SHOWN_PATHS: usize = 10;

/// notify-debouncer-full's per-path quiet time (our own debounce is on top).
const FS_DEBOUNCE: Duration = Duration::from_millis(100);

/// How long an action may take before the watchdog stops waiting for it.
const ACTION_BOUND: Duration = Duration::from_secs(300);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Which changed paths a rule cares about: `paths` globs minus `ignore`
/// globs (ignore wins), matched on the path relative to the rule's root with
/// `/` separators. `*` crosses directories (`*.py` matches `src/a.py`); an
/// ignore pattern also ignores everything below a matching directory
/// (`node_modules`, `**/dist/**`, `build/`).
#[derive(Debug)]
pub struct Matcher {
    roots: Vec<PathBuf>,
    paths: GlobSet,
    ignore: GlobSet,
    ignore_dirs: GlobSet,
}

/// A rule's ignore patterns: its own (config defaults + user entries) plus
/// [`EXTRA_IGNORE`], de-duplicated, in that order.
pub fn ignore_patterns(rule_ignore: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in rule_ignore
        .iter()
        .map(String::as_str)
        .chain(EXTRA_IGNORE.iter().copied())
    {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

fn glob(p: &str) -> Result<Glob, Error> {
    Glob::new(p).map_err(|e| {
        Error::new(ErrorCode::Usage, format!("invalid watch glob `{p}`: {e}"))
            .with_hint("fix the glob in the stem's `watch:` rule")
    })
}

fn normalise(p: &str) -> String {
    let p = p.trim();
    let p = p.strip_prefix("./").unwrap_or(p);
    p.to_string()
}

impl Matcher {
    /// Compile `paths` and `ignore` for a rule rooted at `root` (the root
    /// and its canonical form both work, so `/tmp` vs `/private/tmp`
    /// reports from FSEvents match).
    pub fn new(root: &Path, paths: &[String], ignore: &[String]) -> Result<Self, Error> {
        let mut roots = vec![root.to_path_buf()];
        if let Ok(c) = root.canonicalize()
            && c != root
        {
            roots.push(c);
        }
        let mut pb = GlobSetBuilder::new();
        if paths.is_empty() {
            pb.add(glob("**")?);
        }
        for p in paths {
            let p = normalise(p);
            match p.strip_suffix('/') {
                Some(dir) => pb.add(glob(&format!("{dir}/**"))?),
                None => pb.add(glob(&p)?),
            };
        }
        let (mut ib, mut db) = (GlobSetBuilder::new(), GlobSetBuilder::new());
        for p in ignore {
            let p = normalise(p);
            let p = p.trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            ib.add(glob(p)?);
            db.add(glob(p.strip_suffix("/**").unwrap_or(p))?);
            if !p.contains('/') {
                // A bare name (`node_modules`, `*.log`) anywhere.
                ib.add(glob(&format!("**/{p}"))?);
                db.add(glob(&format!("**/{p}"))?);
            }
        }
        let build = |b: GlobSetBuilder| {
            b.build()
                .map_err(|e| Error::internal(format!("watch globs: {e}")))
        };
        Ok(Self {
            roots,
            paths: build(pb)?,
            ignore: build(ib)?,
            ignore_dirs: build(db)?,
        })
    }

    /// `abs` relative to the root (`/` separators); `None` outside it.
    pub fn relative(&self, abs: &Path) -> Option<String> {
        self.roots.iter().find_map(|r| {
            let rel = abs.strip_prefix(r).ok()?;
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            (!parts.is_empty()).then(|| parts.join("/"))
        })
    }

    /// The relative path is ignored (itself, or a directory above it).
    pub fn ignored(&self, rel: &str) -> bool {
        if self.ignore.is_match(rel) {
            return true;
        }
        let mut prefix = String::new();
        for part in rel.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if self.ignore_dirs.is_match(&prefix) {
                return true;
            }
        }
        false
    }

    /// The relative path is watched: matches `paths` and is not ignored.
    pub fn matches_rel(&self, rel: &str) -> bool {
        self.paths.is_match(rel) && !self.ignored(rel)
    }

    /// `abs` relative to the root when the rule cares about it.
    pub fn matches(&self, abs: &Path) -> Option<String> {
        self.relative(abs).filter(|rel| self.matches_rel(rel))
    }
}

/// The directory a rule watches: the codebase (else the integration repo)
/// or, with `root: workspace`, the integration repo.
pub fn rule_root(ws: &Resolved, stem: &Stem, rule: &Watch) -> PathBuf {
    match rule.root {
        WatchRoot::Workspace => ws.workspace.root.clone(),
        WatchRoot::Codebase => stem.default_cwd(&ws.workspace.root).to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// Coalescing (pure; `now` is passed in, so tests use a fake clock)
// ---------------------------------------------------------------------------

/// The changes one firing covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Batch {
    /// Distinct changed paths, in the order first seen.
    pub paths: IndexSet<String>,
}

#[derive(Clone, Debug)]
struct Collecting {
    first: Instant,
    last: Instant,
    batch: Batch,
}

/// One rule's coalescing state: `Idle` or collecting a batch that fires at
/// `max(first + debounce, last + settle)`.
#[derive(Clone, Debug)]
pub struct Coalescer {
    debounce: Duration,
    settle: Duration,
    current: Option<Collecting>,
}

impl Coalescer {
    /// A coalescer with a debounce window and a settle (quiet) period.
    pub fn new(debounce: Duration, settle: Duration) -> Self {
        Self {
            debounce,
            settle,
            current: None,
        }
    }

    /// A change at `now`.
    pub fn event(&mut self, now: Instant, path: impl Into<String>) {
        let c = self.current.get_or_insert_with(|| Collecting {
            first: now,
            last: now,
            batch: Batch::default(),
        });
        c.last = c.last.max(now);
        c.batch.paths.insert(path.into());
    }

    /// When the current batch fires, if there is one.
    pub fn deadline(&self) -> Option<Instant> {
        self.current
            .as_ref()
            .map(|c| (c.first + self.debounce).max(c.last + self.settle))
    }

    /// The batch, if it is due at `now`.
    pub fn take_due(&mut self, now: Instant) -> Option<Batch> {
        if self.deadline()? <= now {
            self.current.take().map(|c| c.batch)
        } else {
            None
        }
    }

    /// A batch is being collected.
    pub fn pending(&self) -> bool {
        self.current.is_some()
    }

    /// Drop the current batch (paused).
    pub fn clear(&mut self) {
        self.current = None;
    }
}

/// A stem's rules: one [`Coalescer`] each and one action at a time. While
/// busy, changes keep collecting (at most one pending batch per rule) and
/// nothing fires until [`Scheduler::finished`].
#[derive(Clone, Debug)]
pub struct Scheduler {
    rules: Vec<Coalescer>,
    busy: bool,
}

impl Scheduler {
    /// `(debounce, settle)` per rule.
    pub fn new(rules: impl IntoIterator<Item = (Duration, Duration)>) -> Self {
        Self {
            rules: rules
                .into_iter()
                .map(|(d, s)| Coalescer::new(d, s))
                .collect(),
            busy: false,
        }
    }

    /// A change matching rule `rule` at `now`.
    pub fn event(&mut self, rule: usize, now: Instant, path: impl Into<String>) {
        if let Some(c) = self.rules.get_mut(rule) {
            c.event(now, path);
        }
    }

    /// When to wake up next (`None` while busy or idle).
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.busy {
            return None;
        }
        self.rules.iter().filter_map(Coalescer::deadline).min()
    }

    /// The first due rule's batch; marks the scheduler busy.
    pub fn poll(&mut self, now: Instant) -> Option<(usize, Batch)> {
        if self.busy {
            return None;
        }
        let fired = self
            .rules
            .iter_mut()
            .enumerate()
            .find_map(|(i, c)| c.take_due(now).map(|b| (i, b)));
        if fired.is_some() {
            self.busy = true;
        }
        fired
    }

    /// The running action finished.
    pub fn finished(&mut self) {
        self.busy = false;
    }

    /// An action is running.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Some rule has changes waiting.
    pub fn pending(&self) -> bool {
        self.rules.iter().any(Coalescer::pending)
    }

    /// Drop every waiting batch.
    pub fn clear(&mut self) {
        self.rules.iter_mut().for_each(Coalescer::clear);
    }
}

// ---------------------------------------------------------------------------
// Filesystem watching (generic: 33's config watcher reuses it)
// ---------------------------------------------------------------------------

/// A running notify watcher; dropping it stops watching.
pub struct FsWatcher {
    _debouncer: notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::NoCache,
    >,
}

/// Is this change a content/tree change (not a mere read)?
fn relevant(kind: &notify::EventKind) -> bool {
    use notify::EventKind as K;
    use notify::event::{MetadataKind, ModifyKind};
    match kind {
        K::Access(_) | K::Other => false,
        K::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        K::Any | K::Create(_) | K::Modify(_) | K::Remove(_) => true,
    }
}

/// Watch `dirs` recursively; changed paths (reads filtered out) arrive on
/// the returned channel, a batch per debouncer tick. Directories that do
/// not exist are skipped with a warning.
pub fn spawn_fs_watcher(
    dirs: &[PathBuf],
) -> Result<(FsWatcher, mpsc::UnboundedReceiver<Vec<PathBuf>>), Error> {
    spawn_fs_watcher_mode(dirs, notify::RecursiveMode::Recursive)
}

/// [`spawn_fs_watcher`] with an explicit mode (33's config watcher watches
/// the directories of the config files, not their subtrees).
pub fn spawn_fs_watcher_mode(
    dirs: &[PathBuf],
    mode: notify::RecursiveMode,
) -> Result<(FsWatcher, mpsc::UnboundedReceiver<Vec<PathBuf>>), Error> {
    use notify_debouncer_full::DebounceEventResult;
    let (tx, rx) = mpsc::unbounded_channel();
    let handler = move |r: DebounceEventResult| match r {
        Ok(events) => {
            let paths: Vec<PathBuf> = events
                .iter()
                .filter(|e| relevant(&e.event.kind))
                .flat_map(|e| e.event.paths.iter().cloned())
                .collect();
            if !paths.is_empty() {
                let _ = tx.send(paths);
            }
        }
        Err(errors) => {
            for e in errors {
                tracing::warn!(error = %e, "file watcher");
            }
        }
    };
    let mut debouncer = notify_debouncer_full::new_debouncer_opt::<
        _,
        notify::RecommendedWatcher,
        notify_debouncer_full::NoCache,
    >(
        FS_DEBOUNCE,
        None,
        handler,
        notify_debouncer_full::NoCache,
        notify::Config::default(),
    )
    .map_err(|e| Error::internal(format!("cannot start a file watcher: {e}")))?;
    let mut seen: Vec<&PathBuf> = Vec::new();
    for d in dirs {
        if seen.contains(&d) {
            continue;
        }
        seen.push(d);
        if !d.is_dir() {
            tracing::warn!(dir = %d.display(), "watch root does not exist; not watched");
            continue;
        }
        if let Err(e) = debouncer.watch(d, mode) {
            tracing::warn!(dir = %d.display(), error = %e, "cannot watch");
        }
    }
    Ok((
        FsWatcher {
            _debouncer: debouncer,
        },
        rx,
    ))
}

// ---------------------------------------------------------------------------
// The hub: per-stem watchers, pause flags, status
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StemFlags {
    paused: AtomicBool,
    busy: AtomicBool,
    pending: AtomicBool,
    last_triggered: Mutex<Option<DateTime<Utc>>>,
    /// Cancels the running watcher.
    running: Mutex<Option<CancellationToken>>,
    /// An action finished (wakes the watcher loop).
    idle: Notify,
}

/// Every stem's watchdog state; lives in [`Core`].
#[derive(Default)]
pub(crate) struct WatchHub {
    sup: OnceLock<Weak<Supervisor>>,
    global_paused: AtomicBool,
    /// `up --no-watch`.
    disabled: AtomicBool,
    stems: Mutex<HashMap<String, Arc<StemFlags>>>,
}

impl WatchHub {
    /// Remember the supervisor actions run through.
    pub(crate) fn bind(&self, sup: &Arc<Supervisor>) {
        let _ = self.sup.set(Arc::downgrade(sup));
    }

    fn supervisor(&self) -> Option<Arc<Supervisor>> {
        self.sup.get().and_then(Weak::upgrade)
    }

    fn flags(&self, stem: &str) -> Arc<StemFlags> {
        lock(&self.stems)
            .entry(stem.to_string())
            .or_default()
            .clone()
    }

    fn paused(&self, flags: &StemFlags) -> bool {
        self.global_paused.load(Ordering::SeqCst) || flags.paused.load(Ordering::SeqCst)
    }

    /// `up --no-watch` (true) or a plain `up` (false). Disabling stops every
    /// running watcher.
    pub(crate) fn set_disabled(&self, disabled: bool) {
        self.disabled.store(disabled, Ordering::SeqCst);
        if disabled {
            let names: Vec<String> = lock(&self.stems).keys().cloned().collect();
            for n in names {
                self.stop(&n);
            }
        }
    }

    /// Stop `stem`'s watcher (if any).
    pub(crate) fn stop(&self, stem: &str) {
        let flags = lock(&self.stems).get(stem).cloned();
        if let Some(t) = flags.and_then(|f| lock(&f.running).take()) {
            t.cancel();
        }
    }

    /// The `watch` summary for `status`.
    pub(crate) fn summary(&self, stem: &Stem) -> Option<WatchSummary> {
        if stem.watch.is_empty() {
            return None;
        }
        let flags = self.flags(&stem.name);
        Some(WatchSummary {
            paused: self.paused(&flags),
            rules: stem.watch.len(),
        })
    }
}

/// A stem's start began: start its watcher (no-op without rules, with `up
/// --no-watch`, or when one already runs).
pub(crate) fn on_start(core: &Arc<Core>, ws: &Arc<Resolved>, stem: &Stem) {
    let hub = &core.watch;
    if stem.watch.is_empty() || hub.disabled.load(Ordering::SeqCst) {
        return;
    }
    let flags = hub.flags(&stem.name);
    let cancel = {
        let mut running = lock(&flags.running);
        if running.as_ref().is_some_and(|t| !t.is_cancelled()) {
            return;
        }
        let t = CancellationToken::new();
        *running = Some(t.clone());
        t
    };
    let mut rules = Vec::new();
    for (i, rule) in stem.watch.iter().enumerate() {
        let root = rule_root(ws, stem, rule);
        match Matcher::new(&root, &rule.paths, &ignore_patterns(&rule.ignore)) {
            Ok(m) => rules.push((i, rule.clone(), root, m)),
            Err(e) => {
                tracing::warn!(stem = %stem.name, rule = i, error = %e.message, "watch rule skipped")
            }
        }
    }
    if rules.is_empty() {
        return;
    }
    tokio::spawn(watch_loop(
        core.clone(),
        stem.name.clone(),
        rules,
        flags,
        cancel,
    ));
}

/// A stem changed state: `stopped`/`failed` end its watcher.
pub(crate) fn on_transition(core: &Core, stem: &str, to: StemState) {
    if matches!(to, StemState::Stopped | StemState::Failed) {
        core.watch.stop(stem);
    }
}

/// `status`: fill every stem's `watch` summary.
pub(crate) fn decorate(core: &Core, ws: &Resolved, stems: &mut [StemStatus]) {
    for st in stems {
        if let Some(stem) = ws.workspace.stem(&st.name) {
            st.watch = core.watch.summary(stem);
        }
    }
}

type Rule = (usize, Watch, PathBuf, Matcher);

async fn watch_loop(
    core: Arc<Core>,
    stem: String,
    rules: Vec<Rule>,
    flags: Arc<StemFlags>,
    cancel: CancellationToken,
) {
    let dirs: Vec<PathBuf> = rules.iter().map(|r| r.2.clone()).collect();
    let (watcher, mut rx) = match spawn_fs_watcher(&dirs) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(stem, error = %e.message, "watchdog not started");
            return;
        }
    };
    tracing::debug!(stem, ?dirs, "watchdog started");
    let mut sched = Scheduler::new(
        rules
            .iter()
            .map(|r| (r.1.debounce.as_duration(), r.1.settle.as_duration())),
    );
    loop {
        // An action of a previous watcher of this stem (a docker rebuild
        // restarts the watcher) keeps this one busy too.
        let busy = flags.busy.load(Ordering::SeqCst);
        if !busy && sched.busy() {
            sched.finished();
        }
        // While any action runs nothing can fire: sleep until it finishes
        // (`idle`) or a change arrives.
        let deadline = (!busy)
            .then(|| sched.next_deadline())
            .flatten()
            .map(tokio::time::Instant::from_std)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            () = cancel.cancelled() => break,
            batch = rx.recv() => {
                let Some(paths) = batch else { break };
                if core.watch.paused(&flags) {
                    continue;
                }
                let now = Instant::now();
                for p in &paths {
                    for (k, r) in rules.iter().enumerate() {
                        if let Some(rel) = r.3.matches(p) {
                            sched.event(k, now, rel);
                        }
                    }
                }
            }
            () = tokio::time::sleep_until(deadline) => {}
            () = flags.idle.notified() => {}
        }
        if core.watch.paused(&flags) {
            sched.clear();
        }
        if !flags.busy.load(Ordering::SeqCst) && sched.busy() {
            sched.finished();
        }
        if flags.busy.load(Ordering::SeqCst) && !sched.busy() {
            // Busy through another watcher's action: wait for it.
            flags.pending.store(sched.pending(), Ordering::SeqCst);
            continue;
        }
        if let Some((k, batch)) = sched.poll(Instant::now()) {
            let (index, rule) = (rules[k].0, rules[k].1.clone());
            if let Some(id) = core.cascade.suppresses(&stem) {
                // Loop guard #4 (FR-LC-9): a cascading restart restarts this
                // stem right now; its own trigger is dropped, not queued.
                tracing::info!(stem, cascade = %id, paths = batch.paths.len(), "watch trigger dropped: the stem is part of a cascading restart");
                sched.finished();
            } else {
                fire(&core, &stem, index, &rule, batch, &flags);
            }
        }
        flags.pending.store(sched.pending(), Ordering::SeqCst);
    }
    drop(watcher);
    tracing::debug!(stem, "watchdog stopped");
}

/// Emit `watch.triggered` and run the action in its own task.
fn fire(
    core: &Arc<Core>,
    stem: &str,
    rule_index: usize,
    rule: &Watch,
    batch: Batch,
    flags: &Arc<StemFlags>,
) {
    let action = rule.action.clone();
    let cascade = rule.cascade;
    let count = batch.paths.len();
    let shown: Vec<&String> = batch.paths.iter().take(MAX_SHOWN_PATHS).collect();
    let why = match (count, shown.first()) {
        (1, Some(p)) => format!("watch: {p} changed"),
        _ => format!("watch: {count} files changed"),
    };
    flags.busy.store(true, Ordering::SeqCst);
    *lock(&flags.last_triggered) = Some(Utc::now());
    core.events.emit(
        EventDraft::new(EventKind::WATCH_TRIGGERED, WATCHDOG_ACTOR)
            .stem(stem)
            .reason(why.clone())
            .data(json!({
                "paths": shown,
                "path_count": count,
                "action": action.to_string(),
                "rule_index": rule_index,
            })),
    );
    let (core, stem, flags) = (core.clone(), stem.to_string(), flags.clone());
    tokio::spawn(async move {
        let r = match core.watch.supervisor() {
            Some(sup) => run_rule_action(&sup, &stem, &action, &why, cascade).await,
            None => Err(Error::internal("the supervisor is gone")),
        };
        let mut data = json!({
            "action": action.to_string(),
            "rule_index": rule_index,
            "ok": r.is_ok(),
        });
        if let Err(e) = &r {
            tracing::warn!(stem, action = %action, error = %e.message, "watchdog action failed");
            data["error"] = json!(e);
        }
        core.events.emit(
            EventDraft::new(EventKind::WATCH_ACTION_FINISHED, WATCHDOG_ACTOR)
                .stem(&stem)
                .data(data),
        );
        flags.busy.store(false, Ordering::SeqCst);
        flags.idle.notify_one();
    });
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Parse `HUP`, `SIGHUP`, `sighup` or `1`.
pub fn parse_signal(s: &str) -> Result<stems_runtime::os::Signal, Error> {
    use std::str::FromStr;
    let t = s.trim();
    let parsed = match t.parse::<i32>() {
        Ok(n) => stems_runtime::os::Signal::try_from(n).ok(),
        Err(_) => {
            let up = t.to_ascii_uppercase();
            let name = if up.starts_with("SIG") {
                up
            } else {
                format!("SIG{up}")
            };
            stems_runtime::os::Signal::from_str(&name).ok()
        }
    };
    parsed.ok_or_else(|| {
        Error::usage(
            format!("unknown signal `{s}` in a `signal:` watch action"),
            "use a signal name such as `signal:SIGHUP` or `signal:USR1`",
        )
    })
}

/// `signal:` needs a process group: process stems only.
pub fn check_signal_target(stem: &str, kind: StemType) -> Result<(), Error> {
    if kind == StemType::Process {
        return Ok(());
    }
    Err(
        Error::not_implemented(&format!("`signal:` watch actions for `{kind}` stems"), "24")
            .with_hint(format!(
                "use `action: restart` or `action: rebuild` for `{stem}`; signals only reach process stems"
            ))
            .with_details(json!({ "stem": stem, "type": kind.to_string() })),
    )
}

/// Wait until `cell` is done starting (healthy with its start sequence
/// finished, or unhealthy/stopped/failed); an error if it failed or stopped.
async fn settled(cell: &StemCell) -> Result<(), Error> {
    let mut rx = cell.watch();
    let done = |p: &super::Phase| match p.state {
        StemState::Healthy => !p.pending,
        StemState::Unhealthy | StemState::Failed | StemState::Stopped | StemState::Unknown => true,
        _ => false,
    };
    if tokio::time::timeout(ACTION_BOUND, rx.wait_for(done))
        .await
        .is_err()
    {
        return Err(Error::internal(format!(
            "`{}` did not settle within {}s after a watchdog action",
            cell.name,
            ACTION_BOUND.as_secs()
        )));
    }
    match cell.state() {
        StemState::Failed => Err(cell
            .error()
            .unwrap_or_else(|| Error::internal(format!("`{}` failed", cell.name)))),
        StemState::Stopped => Err(Error::usage(
            format!("`{}` was stopped during the watchdog action", cell.name),
            format!("start it with `stems start {}`", cell.name),
        )),
        _ => Ok(()),
    }
}

async fn restart(sup: &Supervisor, stem: &str, why: &str) -> Result<(), Error> {
    let cell = sup.cell(stem);
    cell.restart_bypassing_policy(WATCHDOG_ACTOR, why).await?;
    settled(&cell).await
}

/// `origin` (the stem's own restart), then, when the rule cascades
/// (FR-LC-9), its running hard dependants.
async fn cascading(
    sup: &Supervisor,
    ws: &Arc<Resolved>,
    stem: &str,
    why: &str,
    cascade: bool,
    origin: impl std::future::Future<Output = Result<(), Error>>,
) -> Result<(), Error> {
    if !cascade {
        return origin.await;
    }
    sup.watch_cascade(ws, stem, why, WATCHDOG_ACTOR, origin)
        .await
}

/// Docker/compose `rebuild`: stop (the container is removed), then start:
/// a fresh start always builds the image of a `build:` stem.
async fn recreate(
    sup: &Supervisor,
    ws: &Arc<Resolved>,
    stem: &Stem,
    why: &str,
) -> Result<(), Error> {
    let cell = sup.cell(&stem.name);
    if let StopReply::Failed(e) = cell
        .stop(
            stem.stop_grace.as_duration(),
            WATCHDOG_ACTOR,
            "watch rebuild",
        )
        .await
    {
        return Err(e);
    }
    cell.start(ws.clone(), WATCHDOG_ACTOR, why, BTreeMap::new())
        .await?;
    settled(&cell).await
}

/// Run one watchdog action for `stem` (`why` is the restart reason); a
/// restart cascades as the stem's `restart.cascade` says.
#[cfg(test)]
pub(crate) async fn run_action(
    sup: &Arc<Supervisor>,
    stem: &str,
    action: &WatchAction,
    why: &str,
) -> Result<(), Error> {
    run_rule_action(sup, stem, action, why, None).await
}

/// [`run_action`] for a rule: its `cascade` (else the stem's
/// `restart.cascade`) decides whether a `restart`/`rebuild` also restarts
/// the stem's running hard dependants (FR-LC-9).
pub(crate) async fn run_rule_action(
    sup: &Arc<Supervisor>,
    stem: &str,
    action: &WatchAction,
    why: &str,
    cascade: Option<bool>,
) -> Result<(), Error> {
    let ws = sup
        .host
        .resolved()
        .ok_or_else(|| Error::internal("no workspace is loaded"))?;
    let st = ws
        .workspace
        .stem(stem)
        .cloned()
        .ok_or_else(|| Error::new(ErrorCode::UnknownStem, format!("unknown stem `{stem}`")))?;
    let cascade = cascade.unwrap_or(st.restart.cascade);
    match action {
        WatchAction::Restart => {
            cascading(sup, &ws, stem, why, cascade, restart(sup, stem, why)).await
        }
        WatchAction::Rebuild => match st.kind() {
            StemType::Docker | StemType::Compose => {
                let origin = recreate(sup, &ws, &st, why);
                cascading(sup, &ws, stem, why, cascade, origin).await
            }
            _ => {
                let b = sup
                    .build_stems(&ws, &[stem.to_string()], WATCHDOG_ACTOR)
                    .await;
                if let Some(f) = b.failed.into_iter().next() {
                    return Err(f.error);
                }
                cascading(sup, &ws, stem, why, cascade, restart(sup, stem, why)).await
            }
        },
        WatchAction::Script(name) => {
            let v = sup
                .run_script(
                    RunScriptParams::new(Some(stem.to_string()), name.clone(), Vec::new()),
                    WATCHDOG_ACTOR,
                )
                .await?;
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                let e = v
                    .get("error")
                    .cloned()
                    .and_then(|e| serde_json::from_value::<Error>(e).ok())
                    .unwrap_or_else(|| {
                        Error::new(ErrorCode::ScriptFailed, format!("script `{name}` failed"))
                    });
                return Err(e);
            }
            Ok(())
        }
        WatchAction::Signal(sig) => {
            check_signal_target(stem, st.kind())?;
            let sig = parse_signal(sig)?;
            let pgid = sup
                .cell(stem)
                .info()
                .handle
                .as_ref()
                .map(stems_runtime::Handle::pgid)
                .ok_or_else(|| {
                    Error::usage(
                        format!("`{stem}` is not running: nothing to signal"),
                        format!("start it with `stems start {stem}`"),
                    )
                })?;
            stems_runtime::os::kill_group(pgid, Some(sig)).map_err(|e| {
                Error::internal(format!("cannot send {sig} to `{stem}` (pgid {pgid}): {e}"))
            })
        }
    }
}

// ---------------------------------------------------------------------------
// RPCs
// ---------------------------------------------------------------------------

impl Supervisor {
    /// `watch_pause` (`pause`) / `watch_resume`: no stems = global (a
    /// global resume also clears every per-stem pause).
    pub fn watch_pause(
        &self,
        p: WatchPauseParams,
        pause: bool,
        actor: &str,
    ) -> Result<WatchPauseResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            schedule::plan(&ws, &p.stems, true)?;
        }
        let hub = &self.core.watch;
        let kind = if pause {
            EventKind::WATCH_PAUSED
        } else {
            EventKind::WATCH_RESUMED
        };
        if p.stems.is_empty() {
            hub.global_paused.store(pause, Ordering::SeqCst);
            if !pause {
                for f in lock(&hub.stems).values() {
                    f.paused.store(false, Ordering::SeqCst);
                }
            }
            self.core
                .events
                .emit(EventDraft::new(kind, actor).data(json!({ "global": true })));
        } else {
            for s in &p.stems {
                hub.flags(s).paused.store(pause, Ordering::SeqCst);
                self.core.events.emit(
                    EventDraft::new(kind.clone(), actor)
                        .stem(s)
                        .data(json!({ "global": false })),
                );
            }
        }
        Ok(WatchPauseResult {
            stems: p.stems,
            global_paused: hub.global_paused.load(Ordering::SeqCst),
        })
    }

    /// `watch_status`.
    pub fn watch_status(
        &self,
        p: WatchStatusParams,
        actor: &str,
    ) -> Result<WatchStatusResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            schedule::plan(&ws, &p.stems, true)?;
        }
        let hub = &self.core.watch;
        let stems = ws
            .workspace
            .stems()
            .filter(|s| !s.watch.is_empty())
            .filter(|s| p.stems.is_empty() || p.stems.contains(&s.name))
            .map(|s| {
                let flags = hub.flags(&s.name);
                let active = lock(&flags.running)
                    .as_ref()
                    .is_some_and(|t| !t.is_cancelled());
                StemWatchStatus {
                    name: s.name.clone(),
                    rules: s
                        .watch
                        .iter()
                        .map(|r| WatchRuleStatus {
                            paths: r.paths.clone(),
                            ignore: ignore_patterns(&r.ignore),
                            action: r.action.to_string(),
                            debounce_ms: r.debounce.as_duration().as_millis() as u64,
                            settle_ms: r.settle.as_duration().as_millis() as u64,
                            root: match r.root {
                                WatchRoot::Codebase => "codebase".into(),
                                WatchRoot::Workspace => "workspace".into(),
                            },
                            dir: Some(rule_root(&ws, s, r).display().to_string()),
                        })
                        .collect(),
                    paused: hub.paused(&flags),
                    active,
                    last_triggered: *lock(&flags.last_triggered),
                    pending: flags.pending.load(Ordering::SeqCst),
                    busy: flags.busy.load(Ordering::SeqCst),
                }
            })
            .collect();
        Ok(WatchStatusResult {
            stems,
            global_paused: hub.global_paused.load(Ordering::SeqCst),
            disabled: hub.disabled.load(Ordering::SeqCst),
        })
    }
}

#[cfg(test)]
mod tests;
