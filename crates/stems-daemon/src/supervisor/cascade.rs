//! Cascading restarts (FR-LC-9, `docs/restart.md#cascading-restarts`):
//! restarting a stem also restarts its running hard dependants,
//! transitively, in dependency order.
//!
//! ```text
//! origin(s) restarted (user `restart`, a watchdog, or already by the policy)
//!   ── healthy? no ──▶ `cascade.aborted {stem, error}`, dependants untouched
//!   ── yes ──▶ layer 1 (e.g. b, c): stop + start each (policy bypass), wait healthy
//!          ──▶ layer 2 (e.g. d) ...                     ──▶ `cascade.finished`
//! ```
//!
//! The dependants are [`dependants_closure`]: hard edges only (soft edges
//! never cascade), ordered by `start_order` restricted to the closure, so a
//! diamond's join restarts once, after both branches. Every dependant's
//! restart is a policy **bypass** ([`StemCell::restart_in_cascade`]): hooks
//! run, `setup`/`seed` do not, ports and overlays are kept, `restarts` and
//! `restart.max` do not count it; its `stem.restarting` carries `cascade:
//! <id>`.
//!
//! Loop guards (a cascade can never feed itself):
//!
//! 1. A restart caused by a cascade never starts one: the actor does not
//!    arm a policy cascade for a stem that is a dependant of the running
//!    cascade ([`after_policy_restart`]).
//! 2. `visited`: no stem is restarted twice within one cascade.
//! 3. The origins are never part of their own dependant closure.
//! 4. Watchdog triggers for the stems of the running cascade are dropped,
//!    not queued ([`CascadeHub::suppresses`], checked by the watch loop).
//!
//! One cascade runs at a time per daemon: a second one waits
//! (`cascade.queued`) for the gate, then for the ops lock `up` takes too.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use serde_json::json;
use stems_api::{CascadeRef, CascadeReport, EventKind, RestartParams, StemFailure, UpResult};
use stems_config::{Resolved, StemType, Workspace};
use stems_core::{Error, ErrorCode, StemState, WorkspaceGraph};
use tokio_util::sync::CancellationToken;

use super::actor::StemCell;
use super::{Core, Plan, Supervisor};
use crate::events::EventDraft;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Extra time a dependant gets on top of its `health.start_timeout` and
/// `stop_grace` before the cascade stops waiting for it (the probes fail
/// it with `HEALTH_TIMEOUT` first).
const MEMBER_SLACK: Duration = Duration::from_secs(10);

/// `health.start_timeout` of a stem without a health check.
const DEFAULT_START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a policy cascade waits for its origin to be healthy again.
const POLICY_BOUND: Duration = Duration::from_secs(600);

/// The transitive dependants of `origins` over **hard** edges among enabled
/// stems (soft edges are ignored; the origins themselves are never part of
/// it), as `start_order` layers restricted to that set.
pub fn dependants_closure(ws: &Workspace, origins: &[String]) -> Result<Vec<Vec<String>>, Error> {
    let origin_set: HashSet<&str> = origins.iter().map(String::as_str).collect();
    let mut set: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = origins.to_vec();
    while let Some(n) = frontier.pop() {
        for s in ws.stems().filter(|s| s.enabled) {
            if origin_set.contains(s.name.as_str()) || set.contains(&s.name) {
                continue;
            }
            if s.depends_on.iter().any(|d| !d.soft && d.stem == n) {
                set.insert(s.name.clone());
                frontier.push(s.name.clone());
            }
        }
    }
    if set.is_empty() {
        return Ok(Vec::new());
    }
    Ok(ws
        .start_order()?
        .into_iter()
        .map(|l| {
            l.into_iter()
                .filter(|n| set.contains(n))
                .collect::<Vec<_>>()
        })
        .filter(|l| !l.is_empty())
        .collect())
}

/// Which of `stems` a `restart` cascades from: all of them with
/// `--cascade`, none with `--no-cascade`, else those with
/// `restart.cascade: true`.
pub fn origins(ws: &Workspace, stems: &[String], flag: Option<bool>) -> Vec<String> {
    stems
        .iter()
        .filter(|n| match flag {
            Some(f) => f,
            None => ws.stem(n).is_some_and(|s| s.restart.cascade),
        })
        .cloned()
        .collect()
}

/// One cascading restart.
#[derive(Clone, Debug)]
pub struct Cascade {
    /// ULID; `cascade` in its events and in `status`.
    pub id: String,
    /// The first origin (events, `status`).
    pub origin: String,
    /// Every origin.
    pub origins: Vec<String>,
    /// Stems restarted (or being restarted) by this cascade, origins
    /// included (loop guard #2).
    pub visited: HashSet<String>,
    /// `restart`, `watch: app.py changed`, `policy`.
    pub reason: String,
    /// Who asked (events of the dependants' restarts carry it).
    pub actor: String,
    /// The dependants, in layers.
    pub layers: Vec<Vec<String>>,
}

impl Cascade {
    /// A cascade from `origins`; `exclude` (other stems restarted with them)
    /// are not restarted again as dependants.
    pub fn new(
        ws: &Workspace,
        origins: Vec<String>,
        exclude: &[String],
        reason: &str,
        actor: &str,
    ) -> Result<Self, Error> {
        let skip: HashSet<&str> = exclude.iter().map(String::as_str).collect();
        let layers: Vec<Vec<String>> = dependants_closure(ws, &origins)?
            .into_iter()
            .map(|l| {
                l.into_iter()
                    .filter(|n| !skip.contains(n.as_str()))
                    .collect::<Vec<_>>()
            })
            .filter(|l| !l.is_empty())
            .collect();
        let mut visited: HashSet<String> = origins.iter().cloned().collect();
        visited.extend(exclude.iter().cloned());
        Ok(Self {
            id: ulid::Ulid::generate().to_string(),
            origin: origins.first().cloned().unwrap_or_default(),
            origins,
            visited,
            reason: reason.to_string(),
            actor: actor.to_string(),
            layers,
        })
    }

    /// Every stem the cascade touches (origins and dependants).
    fn members(&self) -> HashSet<String> {
        let mut m: HashSet<String> = self.origins.iter().cloned().collect();
        m.extend(self.layers.iter().flatten().cloned());
        m
    }

    fn report(&self) -> CascadeReport {
        CascadeReport {
            id: self.id.clone(),
            origin: self.origin.clone(),
            origins: self.origins.clone(),
            ..CascadeReport::default()
        }
    }
}

/// The running cascade, as the rest of the daemon sees it.
#[derive(Clone, Debug)]
struct Active {
    id: String,
    origin: String,
    origins: HashSet<String>,
    members: HashSet<String>,
}

/// Daemon-wide cascade state; lives in [`Core`].
#[derive(Default)]
pub(crate) struct CascadeHub {
    sup: OnceLock<Weak<Supervisor>>,
    /// One cascade at a time.
    gate: tokio::sync::Mutex<()>,
    active: Mutex<Option<Active>>,
    /// Stems whose policy restart waits to become healthy before cascading.
    pub(crate) armed: Mutex<HashSet<String>>,
}

impl CascadeHub {
    /// Remember the supervisor policy cascades run through.
    pub(crate) fn bind(&self, sup: &Arc<Supervisor>) {
        let _ = self.sup.set(Arc::downgrade(sup));
    }

    fn supervisor(&self) -> Option<Arc<Supervisor>> {
        self.sup.get().and_then(Weak::upgrade)
    }

    /// `status`: the running cascade `stem` takes part in.
    pub(crate) fn of(&self, stem: &str) -> Option<CascadeRef> {
        lock(&self.active)
            .as_ref()
            .filter(|a| a.members.contains(stem))
            .map(|a| CascadeRef {
                id: a.id.clone(),
                origin: a.origin.clone(),
            })
    }

    /// Loop guard #4: a watchdog trigger of `stem` is dropped while a
    /// cascade restarts it (the id of that cascade).
    pub(crate) fn suppresses(&self, stem: &str) -> Option<String> {
        lock(&self.active)
            .as_ref()
            .filter(|a| a.members.contains(stem))
            .map(|a| a.id.clone())
    }

    /// Loop guard #1: `stem` is restarted as a dependant of the running
    /// cascade (its restarts must not cascade).
    pub(crate) fn restarting_dependant(&self, stem: &str) -> Option<String> {
        lock(&self.active)
            .as_ref()
            .filter(|a| a.members.contains(stem) && !a.origins.contains(stem))
            .map(|a| a.id.clone())
    }

    fn running_id(&self) -> Option<String> {
        lock(&self.active).as_ref().map(|a| a.id.clone())
    }
}

/// Clears the hub's running cascade when the cascade ends (however).
struct ActiveGuard<'a>(&'a CascadeHub);

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.0.active) = None;
    }
}

/// Wait for the gate (emitting `cascade.queued` when another cascade holds
/// it). The caller takes the ops lock next.
async fn enter<'a>(core: &'a Core, c: &Cascade) -> tokio::sync::MutexGuard<'a, ()> {
    if let Ok(g) = core.cascade.gate.try_lock() {
        return g;
    }
    core.events.emit(
        EventDraft::new(EventKind::CASCADE_QUEUED, &c.actor)
            .stem(&c.origin)
            .reason("another cascading restart is running")
            .data(json!({
                "id": c.id,
                "origin": c.origin,
                "origins": c.origins,
                "reason": c.reason,
                "behind": core.cascade.running_id(),
            })),
    );
    core.cascade.gate.lock().await
}

/// Publish `c` as the running cascade and emit `cascade.started`.
fn begin<'a>(core: &'a Core, c: &Cascade) -> ActiveGuard<'a> {
    *lock(&core.cascade.active) = Some(Active {
        id: c.id.clone(),
        origin: c.origin.clone(),
        origins: c.origins.iter().cloned().collect(),
        members: c.members(),
    });
    core.events.emit(
        EventDraft::new(EventKind::CASCADE_STARTED, &c.actor)
            .stem(&c.origin)
            .reason(c.reason.clone())
            .data(json!({
                "id": c.id,
                "origin": c.origin,
                "origins": c.origins,
                "reason": c.reason,
                "stems": c.layers,
            })),
    );
    ActiveGuard(&core.cascade)
}

fn aborted(core: &Core, c: &Cascade, f: &StemFailure) -> CascadeReport {
    core.events.emit(
        EventDraft::new(EventKind::CASCADE_ABORTED, &c.actor)
            .stem(&f.stem)
            .reason(format!(
                "`{}` did not become healthy: its dependants were not restarted",
                f.stem
            ))
            .data(json!({ "id": c.id, "origin": c.origin, "stem": f.stem, "error": f.error })),
    );
    let mut r = c.report();
    r.aborted = true;
    r.skipped = c.layers.iter().flatten().cloned().collect();
    r
}

fn finished(core: &Core, c: &Cascade, r: &CascadeReport) {
    let restarted: Vec<&String> = r.restarted.iter().flatten().collect();
    core.events.emit(
        EventDraft::new(EventKind::CASCADE_FINISHED, &c.actor)
            .stem(&c.origin)
            .data(json!({
                "id": c.id,
                "origin": c.origin,
                "restarted": restarted,
                "layers": r.restarted,
                "failed": r.failed.iter().map(|f| &f.stem).collect::<Vec<_>>(),
                "skipped": r.skipped,
                "ok": r.failed.is_empty(),
            })),
    );
}

/// Done starting: healthy with the start sequence finished, or not
/// running (any more).
fn settled(p: &super::Phase) -> bool {
    match p.state {
        StemState::Healthy => !p.pending,
        StemState::Unhealthy | StemState::Failed | StemState::Stopped | StemState::Unknown => true,
        _ => false,
    }
}

/// The stems of a cascade are restarted only while they run.
fn runs(state: StemState) -> bool {
    matches!(
        state,
        StemState::Healthy | StemState::Unhealthy | StemState::Starting
    )
}

impl Supervisor {
    /// Restart one dependant of `c` and wait until it is healthy again.
    async fn restart_member(
        &self,
        c: &Cascade,
        ws: &Resolved,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        let cell = self.cell(name);
        let stem = ws.workspace.stem(name);
        let bound = stem
            .and_then(|s| s.health.as_ref())
            .map_or(DEFAULT_START_TIMEOUT, |h| h.start_timeout.as_duration())
            + stem.map_or(Duration::from_secs(10), |s| s.stop_grace.as_duration())
            + MEMBER_SLACK;
        let why = format!("cascade from {}", c.origin);
        cell.restart_in_cascade(&c.actor, &why, &c.id).await?;
        let mut rx = cell.watch();
        tokio::select! {
            r = tokio::time::timeout(bound, rx.wait_for(settled)) => {
                if r.is_err() {
                    return Err(Error::new(
                        ErrorCode::StartTimeout,
                        format!("`{name}` did not become healthy within {}s after a cascading restart", bound.as_secs()),
                    )
                    .with_details(json!({ "stem": name, "cascade": c.id })));
                }
            }
            () = cancel.cancelled() => {
                return Err(Error::usage(
                    format!("the cascading restart of `{name}` was interrupted"),
                    "a `down`/`stop` or the daemon's shutdown stopped it",
                ));
            }
        }
        member_outcome(&cell)
    }

    /// Restart `c`'s dependants, layer by layer (each layer in parallel;
    /// a layer starts once the previous one is healthy).
    async fn run_layers(
        &self,
        c: &mut Cascade,
        ws: &Resolved,
        cancel: &CancellationToken,
    ) -> CascadeReport {
        let mut rep = c.report();
        // Dependants whose restart failed (their own dependants are skipped).
        let mut bad: HashSet<String> = HashSet::new();
        for layer in c.layers.clone() {
            let mut todo = Vec::new();
            for n in layer {
                let stem = ws.workspace.stem(&n);
                let dep_bad = stem.is_some_and(|s| {
                    s.depends_on
                        .iter()
                        .any(|d| !d.soft && bad.contains(&d.stem))
                });
                if cancel.is_cancelled() || dep_bad {
                    bad.insert(n.clone());
                    rep.skipped.push(n);
                    continue;
                }
                // Loop guard #2: never twice within one cascade.
                if !c.visited.insert(n.clone()) {
                    continue;
                }
                let external = stem.is_some_and(|s| s.kind() == StemType::External);
                if external || !runs(self.cell(&n).state()) {
                    rep.skipped.push(n);
                    continue;
                }
                todo.push(n);
            }
            let c2: &Cascade = c;
            let futs = todo
                .iter()
                .map(|n| async move { (n.clone(), self.restart_member(c2, ws, n, cancel).await) });
            let mut done = Vec::new();
            for (n, r) in futures::future::join_all(futs).await {
                match r {
                    Ok(()) => done.push(n),
                    Err(error) => {
                        bad.insert(n.clone());
                        rep.failed.push(StemFailure { stem: n, error });
                    }
                }
            }
            if !done.is_empty() {
                rep.restarted.push(done);
            }
        }
        rep
    }

    /// Run the cascade `c`: gate, ops lock, `cascade.started`, `origin`
    /// (the origins' own restart; `Some(failure)` aborts), then the
    /// dependants' layers and `cascade.finished`.
    async fn cascade_with<T>(
        &self,
        mut c: Cascade,
        ws: &Resolved,
        origin: impl Future<Output = (T, Option<StemFailure>)>,
    ) -> (T, Option<StemFailure>, CascadeReport) {
        let _gate = enter(&self.core, &c).await;
        let _ops = self.ops.lock().await;
        let cancel = self.token();
        let _active = begin(&self.core, &c);
        let (t, failure) = origin.await;
        if let Some(f) = failure {
            let rep = aborted(&self.core, &c, &f);
            return (t, Some(f), rep);
        }
        let rep = self.run_layers(&mut c, ws, &cancel).await;
        finished(&self.core, &c, &rep);
        (t, None, rep)
    }

    /// A watchdog `restart`/`rebuild` whose rule cascades: `origin` restarts
    /// the stem (and waits for it), then its dependants follow.
    pub(crate) async fn watch_cascade(
        &self,
        ws: &Arc<Resolved>,
        stem: &str,
        why: &str,
        actor: &str,
        origin: impl Future<Output = Result<(), Error>>,
    ) -> Result<(), Error> {
        let c = Cascade::new(&ws.workspace, vec![stem.to_string()], &[], why, actor)?;
        if c.layers.is_empty() {
            return origin.await;
        }
        let name = stem.to_string();
        let (_, failure, rep) = self
            .cascade_with(c, &ws.clone(), async move {
                match origin.await {
                    Ok(()) => ((), None),
                    Err(error) => ((), Some(StemFailure { stem: name, error })),
                }
            })
            .await;
        if let Some(f) = failure {
            return Err(f.error);
        }
        match rep.failed.into_iter().next() {
            Some(f) => Err(f.error),
            None => Ok(()),
        }
    }

    /// The policy restarted `origin` (with `restart.cascade`) and it is
    /// healthy again: restart its dependants.
    async fn policy_cascade(&self, origin: &str) {
        let cell = self.cell(origin);
        let ws = cell
            .info()
            .restart
            .workspace()
            .cloned()
            .or_else(|| self.host.resolved());
        let Some(ws) = ws else { return };
        let actor = stems_api::DAEMON_ACTOR;
        let c = match Cascade::new(
            &ws.workspace,
            vec![origin.to_string()],
            &[],
            "policy",
            actor,
        ) {
            Ok(c) if !c.layers.is_empty() => c,
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(stem = origin, error = %e.message, "policy cascade not started");
                return;
            }
        };
        let (_, _, rep) = self
            .cascade_with(c, &ws.clone(), async {
                // Queued behind another cascade, the origin may have been
                // stopped or have crashed again (which re-arms a cascade).
                let st = cell.state();
                let failure =
                    (!matches!(st, StemState::Healthy | StemState::Unhealthy)).then(|| {
                        StemFailure {
                            stem: origin.to_string(),
                            error: Error::usage(
                                format!("`{origin}` is {st}, not running healthy"),
                                "its dependants are restarted after its next policy restart",
                            ),
                        }
                    });
                ((), failure)
            })
            .await;
        tracing::debug!(stem = origin, ?rep, "policy cascade done");
    }
}

fn member_outcome(cell: &StemCell) -> Result<(), Error> {
    match cell.state() {
        StemState::Failed => Err(cell
            .error()
            .unwrap_or_else(|| Error::internal(format!("`{}` failed", cell.name)))),
        StemState::Stopped => Err(Error::usage(
            format!("`{}` was stopped during the cascading restart", cell.name),
            format!("start it with `stems start {}`", cell.name),
        )),
        _ => Ok(()),
    }
}

/// The `restart` RPC: a plain restart, or (FR-LC-9) the origins' restart
/// followed by their running dependants.
pub(crate) async fn restart(
    sup: &Supervisor,
    ws: &Arc<Resolved>,
    plan: &Plan,
    p: RestartParams,
    actor: &str,
) -> Result<UpResult, Error> {
    let origins = origins(&ws.workspace, &p.stems, p.cascade);
    let c = if origins.is_empty() {
        None
    } else {
        Some(Cascade::new(
            &ws.workspace,
            origins,
            &p.stems,
            "restart",
            actor,
        )?)
        .filter(|c| !c.layers.is_empty())
    };
    let Some(c) = c else {
        let _ops = sup.ops.lock().await;
        return sup.restart_stems(ws, plan, &p, actor).await;
    };
    let (res, _failure, rep) = sup
        .cascade_with(c, ws, async {
            let r = sup.restart_stems(ws, plan, &p, actor).await;
            let failure = match &r {
                Err(e) => Some(StemFailure {
                    stem: p.stems.first().cloned().unwrap_or_default(),
                    error: e.clone(),
                }),
                Ok(u) => u.failed.first().cloned().or_else(|| {
                    u.skipped.first().map(|s| StemFailure {
                        stem: s.clone(),
                        error: Error::new(ErrorCode::StartFailed, format!("`{s}` was not started")),
                    })
                }),
            };
            (r, failure)
        })
        .await;
    let mut res = res?;
    res.ok = res.ok && rep.failed.is_empty();
    res.cascade = Some(rep);
    Ok(res)
}

/// Called by the actor when the restart policy scheduled a restart of
/// `cell` (22): with `restart.cascade`, wait (in a task) until it is healthy
/// again, then cascade. Loop guard #1: not for a stem restarted as a
/// dependant of the running cascade. One waiter per stem.
pub(crate) fn after_policy_restart(core: &Arc<Core>, cell: &Arc<StemCell>) {
    if !cell.info().restart.cascades() {
        return;
    }
    let hub = &core.cascade;
    if let Some(id) = hub.restarting_dependant(&cell.name) {
        tracing::debug!(stem = %cell.name, cascade = %id, "a restart caused by a cascade does not cascade");
        return;
    }
    if !lock(&hub.armed).insert(cell.name.clone()) {
        return;
    }
    if tokio::runtime::Handle::try_current().is_err() {
        lock(&hub.armed).remove(&cell.name);
        return;
    }
    let (core, cell) = (core.clone(), cell.clone());
    tokio::spawn(async move {
        let mut rx = cell.watch();
        let done = |p: &super::Phase| {
            matches!(p.state, StemState::Failed | StemState::Stopped)
                || (p.state == StemState::Healthy && !p.pending)
        };
        let healthy = matches!(
            tokio::time::timeout(POLICY_BOUND, rx.wait_for(done))
                .await
                .map(|r| r.map(|p| p.state)),
            Ok(Ok(StemState::Healthy))
        );
        lock(&core.cascade.armed).remove(&cell.name);
        if !healthy || core.cascade.restarting_dependant(&cell.name).is_some() {
            return;
        }
        if let Some(sup) = core.cascade.supervisor() {
            sup.policy_cascade(&cell.name).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn ws(yaml: &str) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        let doc = format!("schema_version: 1\nname: t\nstems:\n{yaml}");
        std::fs::write(dir.path().join("stems.yaml"), &doc).unwrap();
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir.path().to_path_buf()),
            cwd: Path::new("/").to_path_buf(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap_or_else(|e| panic!("{e}\n{doc}"))
        .workspace
    }

    fn s(name: &str, deps: &str) -> String {
        format!("  {name}: {{ type: process, command: run, depends_on: [{deps}] }}\n")
    }

    fn names(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|l| l.iter().map(|x| x.to_string()).collect())
            .collect()
    }

    fn o(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn chain_restarts_everything_below_in_order() {
        let w = ws(&[s("a", ""), s("b", "a"), s("c", "b"), s("z", "")].concat());
        assert_eq!(
            dependants_closure(&w, &o(&["a"])).unwrap(),
            names(&[&["b"], &["c"]])
        );
        assert_eq!(
            dependants_closure(&w, &o(&["b"])).unwrap(),
            names(&[&["c"]])
        );
        assert!(dependants_closure(&w, &o(&["c"])).unwrap().is_empty());
        assert!(dependants_closure(&w, &o(&["z"])).unwrap().is_empty());
    }

    #[test]
    fn diamond_restarts_its_join_once_after_both_branches() {
        let w = ws(&[s("a", ""), s("b", "a"), s("c", "a"), s("d", "b, c")].concat());
        assert_eq!(
            dependants_closure(&w, &o(&["a"])).unwrap(),
            names(&[&["b", "c"], &["d"]])
        );
    }

    #[test]
    fn soft_edges_never_cascade() {
        let w = ws(&[
            s("a", ""),
            "  soft: { type: process, command: run, depends_on: [{ stem: a, soft: true }] }\n"
                .to_string(),
            s("below", "soft"),
            s("hard", "a"),
        ]
        .concat());
        assert_eq!(
            dependants_closure(&w, &o(&["a"])).unwrap(),
            names(&[&["hard"]])
        );
    }

    #[test]
    fn a_soft_cycle_back_to_the_origin_does_not_loop() {
        // b depends on a (hard), a on b (soft): allowed by validation.
        let w = ws(&[
            "  a: { type: process, command: run, depends_on: [{ stem: b, soft: true }] }\n"
                .to_string(),
            s("b", "a"),
        ]
        .concat());
        assert_eq!(
            dependants_closure(&w, &o(&["a"])).unwrap(),
            names(&[&["b"]])
        );
        // Loop guard #3: the origin is never its own dependant.
        assert!(dependants_closure(&w, &o(&["b"])).unwrap().is_empty());
    }

    #[test]
    fn several_origins_union_ordered_once_without_the_origins() {
        let w = ws(&[
            s("a", ""),
            s("x", ""),
            s("b", "a"),
            s("y", "x"),
            s("d", "b, y"),
        ]
        .concat());
        assert_eq!(
            dependants_closure(&w, &o(&["a", "x"])).unwrap(),
            names(&[&["b", "y"], &["d"]])
        );
        // An origin depending on another origin is not a dependant.
        assert_eq!(
            dependants_closure(&w, &o(&["a", "b"])).unwrap(),
            names(&[&["d"]])
        );
    }

    #[test]
    fn disabled_dependants_are_left_out() {
        let w = ws(&[
            s("a", ""),
            "  off: { type: process, command: run, enabled: false, depends_on: [a] }\n".to_string(),
        ]
        .concat());
        assert!(dependants_closure(&w, &o(&["a"])).unwrap().is_empty());
    }

    #[test]
    fn flag_overrides_config() {
        let w = ws(&[
            "  a: { type: process, command: run, restart: { cascade: true } }\n".to_string(),
            s("b", "a"),
        ]
        .concat());
        let both = o(&["a", "b"]);
        assert_eq!(origins(&w, &both, None), o(&["a"]));
        assert_eq!(origins(&w, &both, Some(true)), both);
        assert!(origins(&w, &both, Some(false)).is_empty());
    }

    #[test]
    fn a_cascade_excludes_the_other_restarted_stems() {
        let w = ws(&[s("a", ""), s("b", "a"), s("c", "b")].concat());
        let c = Cascade::new(&w, o(&["a"]), &o(&["a", "b"]), "restart", "cli:t").unwrap();
        assert_eq!(c.layers, names(&[&["c"]]));
        assert!(c.visited.contains("b"));
        assert_eq!(c.origin, "a");
    }
}
