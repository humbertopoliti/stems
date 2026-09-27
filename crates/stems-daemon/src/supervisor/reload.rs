//! Config reload on change (deliverable 33, FR-WD-3, `docs/config.md#reload`).
//!
//! ```text
//! notify (the directories of stems.yaml, stems.local.yaml, includes, extends)
//!   ──▶ Coalescer (300 ms debounce, 300 ms settle)
//!   ──▶ load + validate (no `requires:` probing)
//!         error ──▶ `config.invalid {errors}`; the applied config stays
//!         ok    ──▶ compute_plan(applied, new, what running stems run with)
//!                   ──▶ pending + `config.changed {plan}`
//!                   ──▶ auto_apply: true  → apply what restarts nothing
//!                       auto_apply: all   → apply everything
//! `config apply` ──▶ stop (reverse dependency order) what restarts or is
//!   removed ──▶ install the new config ──▶ hot changes in place ──▶ start
//!   (dependency order) what restarted and what was added
//! ```
//!
//! The plan is a pure diff of each stem's resolved config serialised to JSON,
//! field group by field group ([`field_action`], unit-tested as a table). A
//! running stem is compared with the config it *runs with* (the workspace of
//! its last start, which a hot apply rebases), so a stem skipped by a
//! partial apply stays pending until it is restarted.
//!
//! `up`, `start`, `restart`, `run`, ... use the latest *applied* config
//! ([`refresh`]): a disk change that restarts nothing running is applied in
//! place first; one that would restart a running stem stays pending
//! (`config.pending`) until `stems config apply`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use stems_api::{
    AppliedChange, ConfigApplyParams, ConfigApplyResult, ConfigDiffResult, Event, EventKind,
    FailedChange, ReloadAction, ReloadPlan, SkippedChange, StemChange, UpParams,
};
use stems_config::{AutoApply, Resolved, Stem, StemType, Workspace};
use stems_core::scriptargs::{ScriptKind, build_catalog};
use stems_core::{Error, ErrorCode, Errors, SelectOptions, Selection, StemState};
use tokio::sync::{broadcast, mpsc};

use super::watch::{Coalescer, FsWatcher, spawn_fs_watcher_mode};
use super::{Supervisor, probes, schedule, watch};
use crate::events::EventDraft;

/// Quiet time before a config change is loaded.
pub const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

/// The actor of the watcher's events and of auto-applied changes.
pub const RELOAD_ACTOR: &str = stems_api::DAEMON_ACTOR;

/// The `stem.state` reason of stops and starts done by `config apply`.
pub const APPLY_REASON: &str = "config reload";

use ReloadAction as A;

// ---------------------------------------------------------------------------
// The plan (pure)
// ---------------------------------------------------------------------------

/// What a change of a stem's top-level config field (its key in the
/// resolved stem's JSON) implies. `scripts` is split by [`diff_stem`]:
/// `scripts.start` (the start command) restarts, other scripts swap.
pub fn field_action(field: &str) -> ReloadAction {
    match field {
        "name" | "enabled" => A::Unchanged,
        "health" => A::HealthChanged,
        "watch" => A::WatchChanged,
        "scripts" => A::ScriptsChanged,
        "limits" => A::LimitsChanged,
        "restart" => A::RestartPolicyChanged,
        "outputs" => A::OutputsChanged,
        "description" | "tags" | "depends_on" | "stop_grace" => A::MetadataChanged,
        // The variant's name (FR-ST-8): what it changes (`type`, `env`, ...)
        // is diffed field by field; a `type` change is `restart_required`.
        "variant" | "variants" => A::MetadataChanged,
        // env, local_env, env_files, ports, codebase, overlays, type,
        // command, cwd, shell, stdin, image, build, volumes, entrypoint,
        // network, labels, healthcheck, compose fields, and anything new.
        _ => A::RestartRequired,
    }
}

/// The field name shown in a plan (`local_env` is part of `env`).
fn shown_field(key: &str) -> &str {
    if key == "local_env" { "env" } else { key }
}

/// Every change between two versions of a stem: the actions (sorted, the
/// strongest first) and the changed fields.
pub fn diff_stem(old: &Stem, new: &Stem) -> (Vec<ReloadAction>, Vec<String>) {
    let (Ok(Value::Object(a)), Ok(Value::Object(b))) =
        (serde_json::to_value(old), serde_json::to_value(new))
    else {
        return (vec![A::RestartRequired], vec!["config".into()]);
    };
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut actions = BTreeSet::new();
    let mut fields: Vec<String> = Vec::new();
    let push = |f: &str, fields: &mut Vec<String>| {
        if !fields.iter().any(|x| x == f) {
            fields.push(f.to_string());
        }
    };
    for k in keys {
        let (x, y) = (a.get(k), b.get(k));
        if x == y {
            continue;
        }
        if k == "scripts" {
            let start = |v: Option<&Value>| v.and_then(|v| v.get("start")).cloned();
            if start(x) != start(y) {
                actions.insert(A::RestartRequired);
                push("scripts.start", &mut fields);
            }
            let rest = |v: Option<&Value>| {
                v.and_then(Value::as_object).map(|m| {
                    let mut m = m.clone();
                    m.remove("start");
                    m
                })
            };
            if rest(x) != rest(y) {
                actions.insert(A::ScriptsChanged);
                push("scripts", &mut fields);
            }
            continue;
        }
        let act = field_action(k);
        if act == A::Unchanged {
            continue;
        }
        actions.insert(act);
        push(shown_field(k), &mut fields);
    }
    (actions.into_iter().collect(), fields)
}

fn entry(name: &str, changes: Vec<ReloadAction>, fields: Vec<String>, running: bool) -> StemChange {
    StemChange {
        name: name.to_string(),
        action: changes.iter().copied().min().unwrap_or(A::Unchanged),
        hot: changes.iter().all(|a| a.is_hot()),
        changes,
        fields,
        running,
    }
}

/// Workspace-level changes (the workspace minus its stems):
/// `profiles_changed` (profiles, default_profile, profile,
/// strict_profiles), else `<key>_changed`.
pub fn diff_workspace(old: &Workspace, new: &Workspace) -> Vec<String> {
    let (Ok(Value::Object(mut a)), Ok(Value::Object(mut b))) =
        (serde_json::to_value(old), serde_json::to_value(new))
    else {
        return Vec::new();
    };
    a.remove("stems");
    b.remove("stems");
    let keys: BTreeSet<String> = a.keys().chain(b.keys()).cloned().collect();
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        if a.get(&k) == b.get(&k) {
            continue;
        }
        let name = match k.as_str() {
            "profiles" | "default_profile" | "profile" | "strict_profiles" => {
                "profiles_changed".to_string()
            }
            other => format!("{other}_changed"),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// The MCP tools of a workspace's custom scripts, by tool name.
pub fn tools(ws: &Workspace) -> BTreeMap<String, Value> {
    build_catalog(ws)
        .into_iter()
        .filter(|e| e.kind == ScriptKind::Custom)
        .map(|e| {
            let v = serde_json::to_value(&e).unwrap_or(Value::Null);
            (e.mcp_tool_name(), v)
        })
        .collect()
}

/// The plan from `old` (the applied config) to `new`. `running` maps each
/// running stem to the config it runs with (its baseline; usually `old`).
pub fn compute_plan(
    old: &Resolved,
    new: &Resolved,
    running: &BTreeMap<String, Arc<Resolved>>,
) -> ReloadPlan {
    let base = |n: &str| -> Option<&Stem> {
        running
            .get(n)
            .map_or(&old.workspace, |w| &w.workspace)
            .stem(n)
            .filter(|s| s.enabled)
    };
    let mut names: Vec<String> = new.workspace.stems.keys().cloned().collect();
    for n in old.workspace.stems.keys().chain(running.keys()) {
        if !names.contains(n) {
            names.push(n.clone());
        }
    }
    let mut stems = Vec::new();
    for name in names {
        let n = new.workspace.stem(&name).filter(|s| s.enabled);
        let (changes, fields) = match (base(&name), n) {
            (None, None) => continue,
            (None, Some(_)) => (vec![A::Added], Vec::new()),
            (Some(_), None) => (vec![A::Removed], Vec::new()),
            (Some(b), Some(n)) => diff_stem(b, n),
        };
        let running = running.contains_key(&name);
        stems.push(entry(&name, changes, fields, running));
    }
    // `${stem.<x>.port}` references: a stem whose port moves restarts the
    // stems that point at it (their env/health is rendered at start).
    let moved: Vec<String> = stems
        .iter()
        .filter(|s| s.fields.iter().any(|f| f == "ports"))
        .map(|s| s.name.clone())
        .collect();
    if !moved.is_empty() {
        for s in stems
            .iter_mut()
            .filter(|s| !matches!(s.action, A::Added | A::Removed))
        {
            let Some(text) = new
                .workspace
                .stem(&s.name)
                .and_then(|st| serde_json::to_string(st).ok())
            else {
                continue;
            };
            let name = s.name.clone();
            for m in moved.iter().filter(|m| **m != name) {
                if text.contains(&format!("${{stem.{m}.")) {
                    let mut changes = s.changes.clone();
                    if !changes.contains(&A::RestartRequired) {
                        changes.push(A::RestartRequired);
                        changes.sort();
                    }
                    let mut fields = s.fields.clone();
                    fields.push(format!("stem.{m}.ports"));
                    *s = entry(&name, changes, fields, s.running);
                }
            }
        }
    }
    ReloadPlan {
        stems,
        workspace: diff_workspace(&old.workspace, &new.workspace),
        catalog_changed: tools(&old.workspace) != tools(&new.workspace),
    }
}

/// Which entries an apply covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// `stems config apply`: everything.
    All,
    /// `stems config apply <stems…>`: these stems (and every removal of a
    /// running stem: it is no longer in the config).
    Stems(Vec<String>),
    /// `config.reload.auto_apply` (`true` = [`AutoApply::Hot`], `all`).
    Auto(AutoApply),
}

impl Filter {
    /// Is `e` applied?
    pub fn selects(&self, e: &StemChange) -> bool {
        match self {
            Filter::All | Filter::Auto(AutoApply::All) => true,
            Filter::Stems(list) => list.contains(&e.name) || (e.action == A::Removed && e.running),
            // Nothing restarts, stops or starts: hot changes, and stopped
            // stems (their next start uses the new config).
            Filter::Auto(_) => e.hot || !e.running,
        }
    }

    /// Automatic application of `plan` must wait altogether: `auto_apply:
    /// true` never stops a running stem that was removed from the config.
    pub fn defers(&self, plan: &ReloadPlan) -> bool {
        matches!(self, Filter::Auto(AutoApply::Hot))
            && plan.affected().any(|e| e.running && e.action == A::Removed)
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A detected change not (fully) applied yet.
#[derive(Clone)]
struct Pending {
    detected_at: DateTime<Utc>,
}

#[derive(Default)]
struct Inner {
    /// The detected, not (fully) applied config.
    pending: Option<Pending>,
    /// The errors of the last reload attempt (the config on disk is invalid).
    last_error: Vec<Error>,
    /// When the applied config was loaded (`workspace.loaded`).
    loaded_at: Option<DateTime<Utc>>,
    /// Profile and stems of the last `up` (an added stem it covers starts).
    last_up: Option<(Option<String>, Vec<String>)>,
    /// The plan last announced in `config.changed` (no duplicates).
    announced: Option<Value>,
}

/// The supervisor's reload state.
#[derive(Default)]
pub(crate) struct ReloadState {
    inner: Mutex<Inner>,
}

impl ReloadState {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `up` ran with this selection.
    pub(crate) fn note_up(&self, profile: Option<String>, stems: Vec<String>) {
        self.lock().last_up = Some((profile, stems));
    }
}

/// The first error, the others in its `details.errors` (like `load_workspace`).
fn flatten(errs: &Errors) -> Error {
    let mut first = errs
        .0
        .first()
        .cloned()
        .unwrap_or_else(|| Error::internal("the workspace failed to load without an error"));
    if errs.0.len() > 1 {
        first.details = json!({ "errors": errs.0 });
    }
    first
}

// ---------------------------------------------------------------------------
// The config watcher
// ---------------------------------------------------------------------------

/// The files the config watcher watches: every source of `ws` plus
/// `stems.local.yaml` (also before it exists).
pub fn watched_files(ws: &Resolved) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let local = ws.workspace.root.join(stems_config::LOCAL_FILE);
    for p in ws.sources.iter().chain([&ws.workspace.config_file, &local]) {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// `p` with its directory canonicalised (FSEvents reports `/private/tmp`
/// for `/tmp`); `p` itself when the directory does not exist.
fn canonical(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(d), Some(f)) => d
            .canonicalize()
            .map_or_else(|_| p.to_path_buf(), |d| d.join(f)),
        _ => p.to_path_buf(),
    }
}

/// Start the config watcher of a daemon's supervisor. It follows
/// `workspace.loaded` (the watched files are the loaded config's sources).
pub(crate) fn spawn_watcher(sup: &Arc<Supervisor>) {
    let events = sup.core.events.subscribe();
    tokio::spawn(watch_loop(Arc::downgrade(sup), events));
}

type Fs = (FsWatcher, mpsc::UnboundedReceiver<Vec<PathBuf>>);

async fn next_batch(fs: &mut Option<Fs>) -> Option<Vec<PathBuf>> {
    match fs.as_mut() {
        Some((_, rx)) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn watch_loop(weak: Weak<Supervisor>, mut events: broadcast::Receiver<Event>) {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut keys: HashSet<PathBuf> = HashSet::new();
    let mut fs: Option<Fs> = None;
    let mut coalescer = Coalescer::new(RELOAD_DEBOUNCE, RELOAD_DEBOUNCE);
    let mut resync = true;
    loop {
        if resync {
            resync = false;
            let Some(sup) = weak.upgrade() else { return };
            if let Some(ws) = sup.host.resolved() {
                let want = watched_files(&ws);
                if want != files || fs.is_none() {
                    let mut dirs: Vec<PathBuf> = Vec::new();
                    for d in want.iter().filter_map(|p| p.parent()) {
                        if !dirs.iter().any(|x| x == d) {
                            dirs.push(d.to_path_buf());
                        }
                    }
                    drop(sup);
                    // Starting an FSEvents stream takes seconds: not on a
                    // runtime thread.
                    let started = tokio::task::spawn_blocking(move || {
                        spawn_fs_watcher_mode(&dirs, notify::RecursiveMode::NonRecursive)
                    })
                    .await
                    .unwrap_or_else(|e| Err(Error::internal(e.to_string())));
                    match started {
                        Ok(w) => {
                            fs = Some(w);
                            // A change made while the watcher started is
                            // not reported: compare with the disk once.
                            coalescer.event(Instant::now(), "(watcher started)");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e.message, "config watcher not started");
                            fs = None;
                        }
                    }
                    keys = want
                        .iter()
                        .flat_map(|p| [p.clone(), canonical(p)])
                        .collect();
                    tracing::debug!(files = ?want, "config watcher");
                    files = want;
                }
            }
        }
        let deadline = coalescer
            .deadline()
            .map(tokio::time::Instant::from_std)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            ev = events.recv() => match ev {
                Ok(e) if e.kind == EventKind::WORKSPACE_LOADED => {
                    resync = true;
                    if let Some(sup) = weak.upgrade() {
                        sup.reload.lock().loaded_at = Some(e.ts);
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => resync = true,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            batch = next_batch(&mut fs) => match batch {
                Some(paths) => {
                    let now = Instant::now();
                    for p in paths {
                        if keys.contains(&p) || keys.contains(&canonical(&p)) {
                            coalescer.event(now, p.display().to_string());
                        }
                    }
                }
                None => {
                    fs = None;
                    files.clear();
                    resync = true;
                }
            },
            () = tokio::time::sleep_until(deadline) => {
                if coalescer.take_due(Instant::now()).is_some() {
                    let Some(sup) = weak.upgrade() else { return };
                    sup.detect(RELOAD_ACTOR).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The supervisor side
// ---------------------------------------------------------------------------

/// `workspace(reload: true)`: the latest applied config for `up`, `start`,
/// `restart`, `run`, ... A disk change that restarts no running stem is
/// applied in place first; one that would is left pending
/// (`config.pending`) and the applied config is used.
pub(crate) fn refresh(sup: &Supervisor, actor: &str) -> Result<Arc<Resolved>, Error> {
    let Some(applied) = sup.host.resolved() else {
        return sup.host.reload(actor);
    };
    let new = match sup.host.load_candidate(actor, true) {
        Ok(r) => r,
        Err(errs) => {
            sup.reload.lock().last_error = errs.0.clone();
            return Err(flatten(&errs));
        }
    };
    if Arc::ptr_eq(&applied, &new) {
        return Ok(applied);
    }
    sup.reload.lock().last_error.clear();
    let plan = sup.plan_for(&applied, &new);
    if plan.is_empty() {
        sup.clear_pending();
        sup.install_new(&applied, new.clone(), actor);
        return Ok(new);
    }
    if !plan.affected().any(|e| e.running && !e.hot) {
        // Nothing running restarts: apply it now.
        sup.install_new(&applied, new.clone(), actor);
        let mut applied_changes = Vec::new();
        for e in plan.affected() {
            if e.running {
                sup.hot(&new, e, actor);
            }
            applied_changes.push(AppliedChange {
                stem: e.name.clone(),
                action: e.action,
                result: if e.running { "reconfigured" } else { "swapped" }.into(),
            });
        }
        sup.rebase_running(&new, &HashSet::new());
        sup.clear_pending();
        sup.emit(
            EventDraft::new(EventKind::CONFIG_APPLIED, actor).data(json!({
                "applied": applied_changes,
                "failed": [],
                "skipped": [],
                "workspace": plan.workspace,
                "implicit": true,
            })),
        );
        return Ok(new);
    }
    sup.set_pending(new, &plan, actor);
    let waiting: Vec<&String> = plan
        .affected()
        .filter(|e| e.running && !e.hot)
        .map(|e| &e.name)
        .collect();
    sup.emit(
        EventDraft::new(EventKind::CONFIG_PENDING, actor)
            .reason("the config changed; `stems config apply` restarts the affected stems")
            .data(json!({ "stems": waiting })),
    );
    Ok(applied)
}

impl Supervisor {
    /// The config each running stem runs with.
    fn baselines(&self, applied: &Arc<Resolved>) -> BTreeMap<String, Arc<Resolved>> {
        self.existing_cells()
            .into_iter()
            .filter(|c| c.state().is_running())
            .map(|c| {
                let ws = c
                    .info()
                    .restart
                    .workspace()
                    .cloned()
                    .unwrap_or_else(|| applied.clone());
                (c.name.clone(), ws)
            })
            .collect()
    }

    /// The plan from the applied config to `new`.
    fn plan_for(&self, applied: &Arc<Resolved>, new: &Resolved) -> ReloadPlan {
        compute_plan(applied, new, &self.baselines(applied))
    }

    fn clear_pending(&self) {
        let mut i = self.reload.lock();
        i.pending = None;
        i.announced = None;
    }

    /// Remember `new` as pending; `config.changed` unless this very plan was
    /// announced already.
    fn set_pending(&self, new: Arc<Resolved>, plan: &ReloadPlan, actor: &str) {
        let v = serde_json::to_value(plan).unwrap_or(Value::Null);
        let announce = {
            let mut i = self.reload.lock();
            i.pending = Some(Pending {
                detected_at: i.pending.as_ref().map_or_else(Utc::now, |p| p.detected_at),
            });
            let fresh = i.announced.as_ref() != Some(&v);
            i.announced = Some(v.clone());
            fresh
        };
        if announce {
            self.announce(&new, v, actor);
        }
    }

    fn announce(&self, new: &Resolved, plan: Value, actor: &str) {
        let n = plan.get("stems").and_then(Value::as_array).map_or(0, |a| {
            a.iter()
                .filter(|s| s.get("action").and_then(Value::as_str) != Some("unchanged"))
                .count()
        });
        self.emit(
            EventDraft::new(EventKind::CONFIG_CHANGED, actor)
                .reason(format!("config changed: {n} stems affected"))
                .data(json!({
                    "plan": plan,
                    "sources": watched_files(new),
                    "auto_apply": new.workspace.config.reload.auto_apply,
                })),
        );
    }

    /// The watcher saw a change: load, validate, plan, maybe auto-apply.
    pub(crate) async fn detect(&self, actor: &str) {
        let host = self.host.clone();
        let loaded =
            tokio::task::spawn_blocking(move || host.load_candidate(RELOAD_ACTOR, false)).await;
        let Ok(loaded) = loaded else { return };
        let new = match loaded {
            Ok(r) => r,
            Err(errs) => {
                {
                    let mut i = self.reload.lock();
                    i.last_error = errs.0.clone();
                    i.pending = None;
                    i.announced = None;
                }
                let codes: Vec<String> = errs.0.iter().map(|e| e.code.to_string()).collect();
                let reason = errs
                    .0
                    .first()
                    .map_or_else(String::new, |e| e.message.clone());
                tracing::warn!(
                    ?codes,
                    "config changed but is invalid; the applied config stays"
                );
                self.emit(
                    EventDraft::new(EventKind::CONFIG_INVALID, actor)
                        .reason(reason)
                        .data(json!({ "errors": errs.0, "codes": codes })),
                );
                return;
            }
        };
        let recovered = !std::mem::take(&mut self.reload.lock().last_error).is_empty();
        let Some(applied) = self.host.resolved() else {
            // Nothing was loaded yet (it was invalid at startup): load it.
            self.install_new(&new, new.clone(), actor);
            return;
        };
        let plan = self.plan_for(&applied, &new);
        if plan.is_empty() {
            self.clear_pending();
            if applied.sources != new.sources {
                self.install_new(&applied, new.clone(), actor);
            }
            if recovered {
                let v = serde_json::to_value(&plan).unwrap_or(Value::Null);
                self.announce(&new, v, actor);
            }
            return;
        }
        if recovered {
            self.reload.lock().announced = None;
        }
        self.set_pending(new.clone(), &plan, actor);
        let mode = new.workspace.config.reload.auto_apply;
        if mode != AutoApply::Off {
            let filter = Filter::Auto(mode);
            if filter.defers(&plan) {
                tracing::info!(
                    "auto_apply: a running stem was removed; waiting for `stems config apply`"
                );
                return;
            }
            if let Err(e) = self.apply_to(new, filter, actor).await {
                tracing::warn!(error = %e.message, "auto-applying the config failed");
            }
        }
    }

    /// Make `new` the applied config; `tools.changed` when the custom
    /// scripts (MCP tools) or the agent's tool filters changed.
    fn install_new(&self, old: &Resolved, new: Arc<Resolved>, actor: &str) {
        let (a, b) = (tools(&old.workspace), tools(&new.workspace));
        let agent = old.workspace.agent != new.workspace.agent;
        self.host.install(new, actor);
        if a != b || agent {
            let added: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
            let removed: Vec<&String> = a.keys().filter(|k| !b.contains_key(*k)).collect();
            let changed: Vec<&String> = b
                .iter()
                .filter(|(k, v)| a.get(*k).is_some_and(|o| o != *v))
                .map(|(k, _)| k)
                .collect();
            self.emit(
                EventDraft::new(EventKind::TOOLS_CHANGED, actor).data(json!({
                    "added": added,
                    "removed": removed,
                    "changed": changed,
                    "agent_changed": agent,
                })),
            );
        }
    }

    /// Point a running stem at the applied config: policy restarts, stop
    /// scripts and `stop_grace` use it from now on (22's open question:
    /// policy restarts use the latest applied config).
    fn rebase(&self, name: &str, new: &Arc<Resolved>) {
        let Some(stem) = new.workspace.stem(name) else {
            return;
        };
        let cell = self.cell(name);
        let mut info = cell.info();
        info.restart.rebase(new, stem);
        if let Some(l) = info.launch.as_mut() {
            l.ws = new.clone();
        }
        info.grace = stem.stop_grace.as_duration();
    }

    /// [`Supervisor::rebase`] every running stem except `stale` ones.
    fn rebase_running(&self, new: &Arc<Resolved>, stale: &HashSet<String>) {
        for c in self.existing_cells() {
            if c.state().is_running() && !stale.contains(&c.name) {
                self.rebase(&c.name, new);
            }
        }
    }

    /// Apply the hot changes of a running stem in place.
    fn hot(&self, new: &Arc<Resolved>, e: &StemChange, actor: &str) {
        let Some(stem) = new.workspace.stem(&e.name) else {
            return;
        };
        let cell = self.cell(&e.name);
        for c in &e.changes {
            match c {
                A::WatchChanged => {
                    self.core.watch.stop(&e.name);
                    watch::on_start(&self.core, new, stem);
                    let rules: Vec<Value> = stem
                        .watch
                        .iter()
                        .map(|r| {
                            json!({
                                "paths": r.paths,
                                "action": r.action.to_string(),
                                "debounce_ms": r.debounce.as_duration().as_millis() as u64,
                                "settle_ms": r.settle.as_duration().as_millis() as u64,
                            })
                        })
                        .collect();
                    self.emit(
                        EventDraft::new(EventKind::WATCH_RECONFIGURED, actor)
                            .stem(&e.name)
                            .data(json!({ "rules": rules, "running": true })),
                    );
                }
                A::HealthChanged if stem.kind() == StemType::External => {
                    probes::unmonitor(&self.core, &cell, actor, APPLY_REASON);
                    let (cell, ws, actor) = (cell.clone(), new.clone(), actor.to_string());
                    tokio::spawn(async move {
                        let _ = cell.start(ws, &actor, APPLY_REASON, BTreeMap::new()).await;
                    });
                }
                A::HealthChanged => {
                    // A starting stem keeps its readiness probe until its
                    // next start; a running one gets a fresh prober.
                    if matches!(cell.state(), StemState::Healthy | StemState::Unhealthy) {
                        cell.abort_ready_task();
                        probes::monitor_adopted(&self.core, &cell, new);
                    }
                }
                // Scripts, limits, restart policy, metadata: the rebase
                // (and the swapped config) is all it takes.
                _ => {}
            }
        }
    }

    /// Forget a removed stem: watchdog, outputs, overlays, cell.
    fn forget(&self, name: &str, actor: &str) {
        self.core.watch.stop(name);
        self.core.outputs.clear(name);
        super::overlays::cleanup(&self.core, name, actor);
        if let Some(store) = &self.core.state {
            store.set_stem(name, None);
        }
        self.cells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shift_remove(name);
    }

    /// Does the last `up`'s selection (on `new`) cover `name`?
    fn covered_by_last_up(&self, new: &Resolved, name: &str) -> bool {
        let Some((profile, stems)) = self.reload.lock().last_up.clone() else {
            return false;
        };
        Selection::resolve(
            &new.workspace,
            profile.as_deref(),
            &stems,
            SelectOptions::default(),
        )
        .is_ok_and(|s| s.closure.iter().any(|n| n == name))
    }

    /// Apply the plan from the applied config to `new` (the entries
    /// `filter` selects), transactionally per stem: failures are reported,
    /// the other stems proceed.
    pub(crate) async fn apply_to(
        &self,
        new: Arc<Resolved>,
        filter: Filter,
        actor: &str,
    ) -> Result<ConfigApplyResult, Error> {
        let _ops = self.ops.lock().await;
        let applied = self
            .host
            .resolved()
            .ok_or_else(|| Error::internal("no workspace is loaded"))?;
        let any_running = self.any_running();
        let plan = self.plan_for(&applied, &new);
        let mut res = ConfigApplyResult::default();
        let (sel, unsel): (Vec<StemChange>, Vec<StemChange>) =
            plan.affected().cloned().partition(|e| filter.selects(e));
        let action_of = |n: &str| {
            sel.iter()
                .find(|e| e.name == n)
                .map_or(A::RestartRequired, |e| e.action)
        };

        // 1. Stop what restarts or was removed (reverse dependency order,
        //    the config they run with).
        let stopping: Vec<&StemChange> = sel
            .iter()
            .filter(|e| e.running && (!e.hot || e.action == A::Removed))
            .collect();
        let kind_of = |n: &str| {
            applied
                .workspace
                .stem(n)
                .or_else(|| new.workspace.stem(n))
                .map(Stem::kind)
        };
        let managed: Vec<String> = stopping
            .iter()
            .filter(|e| kind_of(&e.name) != Some(StemType::External))
            .map(|e| e.name.clone())
            .collect();
        let mut failed: HashSet<String> = HashSet::new();
        let stop = self
            .stop_set(&applied, &managed, None, actor, APPLY_REASON)
            .await;
        for f in stop.failed {
            failed.insert(f.stem.clone());
            res.failed.push(FailedChange {
                action: action_of(&f.stem),
                stem: f.stem,
                error: f.error,
            });
        }
        for e in stopping
            .iter()
            .filter(|e| kind_of(&e.name) == Some(StemType::External))
        {
            probes::unmonitor(&self.core, &self.cell(&e.name), actor, APPLY_REASON);
        }

        // 2. Removed stems leave for good.
        for e in sel.iter().filter(|e| e.action == A::Removed) {
            if failed.contains(&e.name) {
                continue;
            }
            self.forget(&e.name, actor);
            res.applied.push(AppliedChange {
                stem: e.name.clone(),
                action: e.action,
                result: if e.running { "stopped" } else { "removed" }.into(),
            });
        }

        // 3. The new config is the applied one; hot changes in place.
        self.install_new(&applied, new.clone(), actor);
        let stale: HashSet<String> = unsel
            .iter()
            .filter(|e| e.running)
            .map(|e| e.name.clone())
            .chain(failed.iter().cloned())
            .collect();
        for e in sel
            .iter()
            .filter(|e| e.running && e.hot && e.action != A::Removed)
        {
            self.hot(&new, e, actor);
            res.applied.push(AppliedChange {
                stem: e.name.clone(),
                action: e.action,
                result: "reconfigured".into(),
            });
        }
        self.rebase_running(&new, &stale);

        // 4. Start what restarts, and added stems the last `up` covers (or
        //    named on the command line), in dependency order.
        let mut start: Vec<String> = stopping
            .iter()
            .filter(|e| e.action != A::Removed && !failed.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
        for e in sel.iter().filter(|e| e.action == A::Added) {
            let named = matches!(&filter, Filter::Stems(l) if l.contains(&e.name));
            let auto_hot = filter == Filter::Auto(AutoApply::Hot);
            if !auto_hot && (named || (any_running && self.covered_by_last_up(&new, &e.name))) {
                start.push(e.name.clone());
            } else {
                res.applied.push(AppliedChange {
                    stem: e.name.clone(),
                    action: e.action,
                    result: "registered".into(),
                });
            }
        }
        for e in sel
            .iter()
            .filter(|e| !e.running && !matches!(e.action, A::Added | A::Removed))
        {
            res.applied.push(AppliedChange {
                stem: e.name.clone(),
                action: e.action,
                result: "swapped".into(),
            });
        }
        if !start.is_empty() {
            match schedule::plan(&new, &start, true) {
                Err(error) => {
                    for n in &start {
                        res.failed.push(FailedChange {
                            stem: n.clone(),
                            action: action_of(n),
                            error: error.clone(),
                        });
                    }
                }
                Ok(p) => {
                    let up = self
                        .run_plan(&p, &new, &UpParams::default(), actor, APPLY_REASON)
                        .await;
                    for n in up.ready {
                        let action = action_of(&n);
                        res.applied.push(AppliedChange {
                            result: if action == A::Added {
                                "started"
                            } else {
                                "restarted"
                            }
                            .into(),
                            stem: n,
                            action,
                        });
                    }
                    for f in up.failed {
                        res.failed.push(FailedChange {
                            action: action_of(&f.stem),
                            stem: f.stem,
                            error: f.error,
                        });
                    }
                    for n in up.skipped {
                        res.failed.push(FailedChange {
                            action: action_of(&n),
                            error: Error::new(
                                ErrorCode::StartFailed,
                                format!("`{n}` was not started: a dependency failed"),
                            )
                            .with_hint("fix the failed stem, then `stems start` it"),
                            stem: n,
                        });
                    }
                }
            }
        }

        // 5. What is left pending.
        res.workspace = plan.workspace.clone();
        res.skipped = unsel
            .iter()
            .map(|e| SkippedChange {
                stem: e.name.clone(),
                action: e.action,
                reason: match &filter {
                    Filter::Auto(_) => "needs a restart: run `stems config apply`".into(),
                    _ => "not selected".into(),
                },
            })
            .collect();
        let after = self.plan_for(&new, &new);
        res.pending = !after.is_empty();
        if res.pending {
            let mut i = self.reload.lock();
            i.pending = Some(Pending {
                detected_at: i.pending.as_ref().map_or_else(Utc::now, |p| p.detected_at),
            });
            i.announced = serde_json::to_value(&after).ok();
        } else {
            self.clear_pending();
        }
        res.ok = res.failed.is_empty();
        self.emit(
            EventDraft::new(EventKind::CONFIG_APPLIED, actor).data(json!({
                "applied": res.applied,
                "failed": res.failed.iter().map(|f| json!({
                    "stem": f.stem,
                    "action": f.action,
                    "code": f.error.code,
                })).collect::<Vec<_>>(),
                "skipped": res.skipped,
                "workspace": res.workspace,
                "auto": matches!(filter, Filter::Auto(_)),
                "pending": res.pending,
            })),
        );
        Ok(res)
    }

    /// `config_diff`: the plan from the applied config to the one on disk
    /// (read now, never applied), or the last error when it is invalid.
    pub fn config_diff(&self, actor: &str) -> Result<ConfigDiffResult, Error> {
        let applied = self.workspace(false, actor)?;
        let (loaded_at, pending) = {
            let i = self.reload.lock();
            (i.loaded_at, i.pending.clone())
        };
        let mut out = ConfigDiffResult {
            loaded_at,
            sources: watched_files(&applied),
            ..ConfigDiffResult::default()
        };
        match self.host.load_candidate(actor, false) {
            Ok(new) => {
                let plan = self.plan_for(&applied, &new);
                out.pending = !plan.is_empty();
                out.detected_at = pending
                    .filter(|_| out.pending)
                    .map(|p| p.detected_at)
                    .or_else(|| out.pending.then(Utc::now));
                out.plan = plan;
            }
            Err(errs) => {
                self.reload.lock().last_error = errs.0.clone();
                // The applied config stays; only stems still running an
                // older config are pending.
                let plan = self.plan_for(&applied, &applied);
                out.pending = !plan.is_empty();
                out.plan = plan;
                out.last_error = errs.0;
            }
        }
        Ok(out)
    }

    /// `config_apply`: apply the config on disk now.
    pub async fn config_apply(
        &self,
        p: ConfigApplyParams,
        actor: &str,
    ) -> Result<ConfigApplyResult, Error> {
        if !p.yes {
            return Err(Error::new(
                ErrorCode::DestructiveNotConfirmed,
                "`config apply` restarts, starts and stops stems",
            )
            .with_hint("review `stems config diff`, then rerun with `--yes`")
            .with_details(json!({ "command": "config apply", "stems": p.stems })));
        }
        let applied = self.workspace(false, actor)?;
        let new = match self.host.load_candidate(actor, true) {
            Ok(r) => r,
            Err(errs) => {
                self.reload.lock().last_error = errs.0.clone();
                return Err(flatten(&errs));
            }
        };
        self.reload.lock().last_error.clear();
        if !p.stems.is_empty() {
            let plan = self.plan_for(&applied, &new);
            for s in &p.stems {
                if !plan.stems.iter().any(|e| &e.name == s) {
                    return Err(Error::new(
                        ErrorCode::UnknownStem,
                        format!("`{s}` is not a stem of the old or the new config"),
                    )
                    .with_hint("see `stems config diff` for the stems a change affects")
                    .with_details(json!({ "stem": s })));
                }
            }
        }
        let filter = if p.stems.is_empty() {
            Filter::All
        } else {
            Filter::Stems(p.stems)
        };
        self.apply_to(new, filter, actor).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// One workspace dir per test: the versions compared differ only
        /// by their `stems.yaml`.
        static DIR: tempfile::TempDir = tempfile::tempdir().unwrap();
    }

    fn load(doc: &str) -> (PathBuf, Resolved) {
        let dir = DIR.with(|d| d.path().to_path_buf());
        std::fs::write(dir.join("stems.yaml"), doc).unwrap();
        let r = stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir.clone()),
            cwd: dir.clone(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap_or_else(|e| panic!("{e}\n{doc}"));
        (dir, r)
    }

    const BASE: &str = r#"
schema_version: 1
name: t
stems:
  api:
    type: process
    command: run api
    env: { A: "1" }
    ports: [{ name: http, port: 18001 }]
    health: { type: tcp, interval: 200ms }
    watch: [{ paths: ["*.py"], action: restart, debounce: 200ms }]
    scripts: { lint: "echo lint", seed: "echo seed" }
  web:
    type: process
    command: run web
    depends_on: [api]
    env: { API: "http://127.0.0.1:${stem.api.port}" }
"#;

    fn plan_of(edit: &str) -> ReloadPlan {
        let (_a, old) = load(BASE);
        let (_b, new) = load(&BASE.replacen("    command: run api\n", edit, 1));
        compute_plan(&old, &new, &BTreeMap::new())
    }

    fn api(plan: &ReloadPlan) -> &StemChange {
        plan.stems.iter().find(|s| s.name == "api").unwrap()
    }

    /// The mapping table: which field change maps to which action (hot?).
    #[test]
    fn field_changes_map_to_actions() {
        let table: &[(&str, ReloadAction, &str, bool)] = &[
            (
                "    command: run api2\n",
                A::RestartRequired,
                "command",
                false,
            ),
            (
                "    command: run api\n    cwd: /tmp\n",
                A::RestartRequired,
                "cwd",
                false,
            ),
            (
                "    command: run api\n    stop_grace: 1s\n",
                A::MetadataChanged,
                "stop_grace",
                true,
            ),
            (
                "    command: run api\n    description: hi\n",
                A::MetadataChanged,
                "description",
                true,
            ),
            (
                "    command: run api\n    tags: [x]\n",
                A::MetadataChanged,
                "tags",
                true,
            ),
            (
                "    command: run api\n    restart: { max: 9 }\n",
                A::RestartPolicyChanged,
                "restart",
                true,
            ),
            (
                "    command: run api\n    limits: { memory: 1GB }\n",
                A::LimitsChanged,
                "limits",
                true,
            ),
            (
                "    command: run api\n    outputs: { X: \"1\" }\n",
                A::OutputsChanged,
                "outputs",
                false,
            ),
            (
                "    command: run api\n    env_files: [.env]\n",
                A::RestartRequired,
                "env_files",
                false,
            ),
            (
                "    command: run api\n    codebase: /tmp\n",
                A::RestartRequired,
                "codebase",
                false,
            ),
            // FR-ST-8: switching a process to a docker variant.
            (
                "    command: run api\n    variant: docker\n    variants: { docker: { type: docker, image: \"shop:1\" } }\n",
                A::RestartRequired,
                "type",
                false,
            ),
            (
                "    command: run api\n    variants: { docker: { type: docker, image: \"shop:1\" } }\n",
                A::MetadataChanged,
                "variants",
                true,
            ),
            (
                "    command: run api\n    overlays: [{ template: x, dest: /tmp/stems-reload-x }]\n",
                A::RestartRequired,
                "overlays",
                false,
            ),
        ];
        for (edit, action, field, hot) in table {
            let p = plan_of(edit);
            let e = api(&p);
            assert_eq!(e.action, *action, "{edit}");
            assert!(
                e.fields.iter().any(|f| f == field),
                "{edit}: {:?}",
                e.fields
            );
            assert_eq!(e.hot, *hot, "{edit}");
            assert_eq!(e.hot, action.is_hot(), "{edit}");
        }
    }

    #[test]
    fn env_ports_health_watch_scripts() {
        let (_a, old) = load(BASE);
        let edits: &[(&str, &str, ReloadAction, bool)] = &[
            ("A: \"1\"", "A: \"2\"", A::RestartRequired, false),
            ("port: 18001", "port: 18009", A::RestartRequired, false),
            (
                "interval: 200ms }",
                "interval: 300ms }",
                A::HealthChanged,
                true,
            ),
            ("debounce: 200ms", "debounce: 500ms", A::WatchChanged, true),
            (
                "lint: \"echo lint\"",
                "lint: \"echo lint2\"",
                A::ScriptsChanged,
                true,
            ),
            (
                "seed: \"echo seed\"",
                "seed: \"echo seed\", start: \"x\"",
                A::RestartRequired,
                false,
            ),
        ];
        for (from, to, action, hot) in edits {
            let (_b, new) = load(&BASE.replacen(from, to, 1));
            let p = compute_plan(&old, &new, &BTreeMap::new());
            let e = api(&p);
            assert_eq!((e.action, e.hot), (*action, *hot), "{from} -> {to}: {e:?}");
        }
        // env is reported as `env`.
        let (_b, new) = load(&BASE.replacen("A: \"1\"", "A: \"2\"", 1));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert_eq!(api(&p).fields, ["env"]);
        // A health + watch change: both hot, the strongest named first.
        let (_c, new) = load(
            &BASE
                .replacen("interval: 200ms }", "interval: 300ms }", 1)
                .replacen("debounce: 200ms", "debounce: 500ms", 1),
        );
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert_eq!(api(&p).changes, [A::HealthChanged, A::WatchChanged]);
        assert!(api(&p).hot);
        // env + watch: restart wins, not hot.
        let (_d, new) = load(&BASE.replacen("A: \"1\"", "A: \"2\"", 1).replacen(
            "debounce: 200ms",
            "debounce: 500ms",
            1,
        ));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert_eq!(api(&p).action, A::RestartRequired);
        assert!(!api(&p).hot);
    }

    #[test]
    fn a_moved_port_restarts_the_stems_referring_to_it() {
        let (_a, old) = load(BASE);
        let (_b, new) = load(&BASE.replacen("port: 18001", "port: 18009", 1));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        let web = p.stems.iter().find(|s| s.name == "web").unwrap();
        // A fixed port is substituted at load: `web`'s env changed.
        assert_eq!(web.action, A::RestartRequired);
        assert_eq!(web.fields, ["env"]);
        // `port: auto` is rendered at start: the reference itself is compared.
        let auto = BASE.replacen("port: 18001", "port: auto", 1);
        let (_c, old) = load(&auto);
        let (_d, new) = load(&auto.replacen("name: http, port: auto", "name: web, port: auto", 1));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        let web = p.stems.iter().find(|s| s.name == "web").unwrap();
        assert_eq!(web.action, A::RestartRequired);
        assert_eq!(web.fields, ["stem.api.ports"]);
    }

    #[test]
    fn added_removed_disabled_workspace_and_catalog() {
        let (_a, old) = load(BASE);
        let doc = format!("{BASE}  extra:\n    type: process\n    command: run extra\n");
        let (_b, new) = load(&doc);
        let p = compute_plan(&old, &new, &BTreeMap::new());
        let extra = p.stems.iter().find(|s| s.name == "extra").unwrap();
        assert_eq!((extra.action, extra.hot), (A::Added, false));
        let p = compute_plan(&new, &old, &BTreeMap::new());
        let extra = p.stems.iter().find(|s| s.name == "extra").unwrap();
        assert_eq!(extra.action, A::Removed);
        assert_eq!(
            p.stems.last().unwrap().name,
            "extra",
            "removed stems come last"
        );
        // enabled: false is a removal.
        let (_c, new) = load(&BASE.replacen(
            "    command: run web\n",
            "    command: run web\n    enabled: false\n",
            1,
        ));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert_eq!(
            p.stems.iter().find(|s| s.name == "web").unwrap().action,
            A::Removed
        );
        // Unchanged: an empty plan, every stem listed.
        let (_d, same) = load(BASE);
        let p = compute_plan(&old, &same, &BTreeMap::new());
        assert!(p.is_empty(), "{p:?}");
        assert_eq!(p.stems.len(), 2);
        // Workspace-level changes and the catalog.
        let doc = BASE.replacen(
            "name: t\n",
            "name: t\nprofiles: { p: [api] }\nagent: { allow_destructive: true }\n",
            1,
        );
        let (_e, new) = load(&doc);
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert_eq!(p.workspace, ["agent_changed", "profiles_changed"]);
        assert!(!p.catalog_changed);
        let (_f, new) = load(&BASE.replacen(
            "seed: \"echo seed\"",
            "seed: \"echo seed\", fmt: \"echo fmt\"",
            1,
        ));
        let p = compute_plan(&old, &new, &BTreeMap::new());
        assert!(p.catalog_changed);
        // `seed` is a lifecycle script: not a tool.
        let (_g, new) = load(&BASE.replacen("seed: \"echo seed\"", "seed: \"echo seed2\"", 1));
        assert!(!compute_plan(&old, &new, &BTreeMap::new()).catalog_changed);
    }

    #[test]
    fn a_running_stem_is_compared_with_what_it_runs() {
        let (_a, old) = load(BASE);
        let (_b, new) = load(&BASE.replacen("A: \"1\"", "A: \"2\"", 1));
        let new = Arc::new(new);
        // `api` still runs the old config although `new` is applied.
        let running: BTreeMap<String, Arc<Resolved>> =
            [("api".to_string(), Arc::new(old))].into_iter().collect();
        let p = compute_plan(&new, &new, &running);
        assert_eq!(api(&p).action, A::RestartRequired);
        assert!(api(&p).running);
        let web = p.stems.iter().find(|s| s.name == "web").unwrap();
        assert!(!web.running);
        assert_eq!(web.action, A::Unchanged);
    }

    #[test]
    fn filters() {
        let e = |action: ReloadAction, running: bool| StemChange {
            name: "s".into(),
            action,
            changes: vec![action],
            fields: vec![],
            hot: action.is_hot(),
            running,
        };
        let hot = Filter::Auto(AutoApply::Hot);
        assert!(hot.selects(&e(A::WatchChanged, true)));
        assert!(!hot.selects(&e(A::RestartRequired, true)));
        assert!(hot.selects(&e(A::RestartRequired, false)));
        assert!(Filter::Auto(AutoApply::All).selects(&e(A::RestartRequired, true)));
        let named = Filter::Stems(vec!["x".into()]);
        assert!(!named.selects(&e(A::RestartRequired, true)));
        assert!(named.selects(&e(A::Removed, true)), "removals always apply");
        let plan = ReloadPlan {
            stems: vec![e(A::Removed, true)],
            ..ReloadPlan::default()
        };
        assert!(hot.defers(&plan));
        assert!(!Filter::All.defers(&plan));
    }

    #[test]
    fn watched_files_include_the_local_file() {
        let (_dir, r) = load(BASE);
        let files = watched_files(&r);
        let root = &r.workspace.root;
        assert!(files.contains(&root.join("stems.yaml")), "{files:?}");
        assert!(files.contains(&root.join("stems.local.yaml")), "{files:?}");
        assert_eq!(files.len(), 2);
    }

    /// Config changes are debounced: a burst of writes is one reload, 300 ms
    /// after the first and once 300 ms passed without a write.
    #[test]
    fn config_changes_are_debounced() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut c = Coalescer::new(RELOAD_DEBOUNCE, RELOAD_DEBOUNCE);
        c.event(at(0), "stems.local.yaml");
        c.event(at(50), "stems.local.yaml");
        c.event(at(100), "stems.yaml");
        assert!(c.take_due(at(300)).is_none(), "still settling");
        assert_eq!(c.deadline(), Some(at(400)));
        let batch = c.take_due(at(400)).expect("one reload");
        assert_eq!(batch.paths.len(), 2);
        assert!(c.take_due(at(10_000)).is_none());
    }
}
