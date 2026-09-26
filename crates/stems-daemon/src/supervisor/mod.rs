//! The supervisor (deliverable 10): `up`, `down`, `start`, `stop`,
//! `restart` and `status` over per-stem actors (see `docs/lifecycle.md`).
//!
//! * [`actor`] — one task per stem owning its runtime handle and state.
//! * [`state`] — the legal state transitions (§6.4).
//! * [`schedule`] — the layer scheduler (parallel starts, edge conditions, fail-fast).
//! * [`waiter`] — when a started stem counts as `started` / `healthy` (21 swaps in probes).
//! * [`env`] — a stem's environment (FR-ST-4) and deferred `${stem.x.port}` rendering.
//! * [`ports`] — `PORT_IN_USE` checks and sticky `port: auto` allocation.
//!
//! Hooks for later deliverables: [`RuntimeRegistry`] (docker 14, compose
//! 15), [`OutputSink`] (logs 12), [`Waiter`] (probes 21), the actor's exit
//! handler (restart policies 22), [`Host`] (its `state_store` is 11's state file).

pub mod actor;
pub mod env;
pub mod ports;
pub mod schedule;
pub mod state;
pub mod waiter;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use stems_api::{
    DownParams, DownResult, EventKind, Method, RestartParams, StartParams, StatusParams,
    StatusResult, StatusSummary, StemFailure, StopParams, UpParams, UpResult,
};
use stems_config::{Resolved, StemType};
use stems_core::{Error, ErrorCode, WorkspaceGraph};
use stems_runtime::{OutputStream, ProcessRuntime, Runtime};
use tokio_util::sync::CancellationToken;

pub use actor::{Phase, StemCell, StopReply};
pub use ports::PortBook;
pub use schedule::Plan;
pub use waiter::{AliveWaiter, WaitTarget, Waiter};

use crate::daemon::Daemon;
use crate::events::{EventBus, EventDraft};
use crate::handler::{RequestCtx, SupervisorHooks};
use crate::state::{StateFile, StateStore, StemRecord};

/// What [`Supervisor::recover`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoverReport {
    /// Stems adopted (alive, same process).
    pub adopted: Vec<String>,
    /// Records dropped (dead or not verifiable).
    pub dead: Vec<String>,
}

/// Default `max_parallel` for `up`.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

/// Runtimes by stem type. Docker (14) and compose (15) register here.
#[derive(Clone, Default)]
pub struct RuntimeRegistry {
    by_type: HashMap<StemType, Arc<dyn Runtime>>,
}

impl RuntimeRegistry {
    /// Just the process runtime.
    pub fn with_process() -> Self {
        let mut r = Self::default();
        r.register(StemType::Process, Arc::new(ProcessRuntime::new()));
        r
    }

    /// Register (or replace) the runtime for `kind`.
    pub fn register(&mut self, kind: StemType, rt: Arc<dyn Runtime>) {
        self.by_type.insert(kind, rt);
    }

    /// The runtime for `kind`, or `NOT_IMPLEMENTED` naming its deliverable.
    pub fn get(&self, kind: StemType) -> Result<Arc<dyn Runtime>, Error> {
        self.by_type.get(&kind).cloned().ok_or_else(|| {
            let nn = match kind {
                StemType::Docker => "14",
                StemType::Compose => "15",
                _ => "13",
            };
            Error::not_implemented(&format!("running `{kind}` stems"), nn)
                .with_details(json!({ "type": kind.to_string(), "deliverable": nn }))
        })
    }
}

/// Receives each started unit's output (deliverable 12 installs the log sink).
pub trait OutputSink: Send + Sync {
    /// Called once per start with the unit's output stream (if any).
    fn attach(&self, stem: &str, stream: Option<OutputStream>);
}

/// Drops output (until 12).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullSink;

impl OutputSink for NullSink {
    fn attach(&self, _stem: &str, _stream: Option<OutputStream>) {}
}

/// What the supervisor needs from its daemon.
pub trait Host: Send + Sync {
    /// The loaded workspace.
    fn resolved(&self) -> Option<Arc<Resolved>>;
    /// Re-load the workspace from disk (validation errors are returned).
    fn reload(&self, actor: &str) -> Result<Arc<Resolved>, Error>;
    /// Trigger the daemon's orderly shutdown.
    fn request_shutdown(&self, actor: &str, reason: &str);
    /// The daemon was auto-started by `up`.
    fn started_by_up(&self) -> bool;
    /// Record that the daemon was auto-started by `up`.
    fn set_started_by_up(&self);
    /// The state store the supervisor persists running stems to (11).
    fn state_store(&self) -> Option<Arc<StateStore>> {
        None
    }
    /// Where started units' output goes (12: the daemon's log hub); `None`
    /// drops it ([`NullSink`]).
    fn output_sink(&self) -> Option<Arc<dyn OutputSink>> {
        None
    }
}

struct DaemonHost(Weak<Daemon>);

impl DaemonHost {
    fn daemon(&self) -> Result<Arc<Daemon>, Error> {
        self.0
            .upgrade()
            .ok_or_else(|| Error::internal("the daemon is shutting down"))
    }
}

impl Host for DaemonHost {
    fn resolved(&self) -> Option<Arc<Resolved>> {
        self.0.upgrade()?.resolved()
    }
    fn reload(&self, actor: &str) -> Result<Arc<Resolved>, Error> {
        let d = self.daemon()?;
        let Some(root) = d.info().workspace else {
            return Err(Error::usage(
                "this daemon has no workspace",
                "start it with `stems daemon start` inside a workspace, or call `load_workspace`",
            ));
        };
        d.load_workspace(&root, actor)?;
        d.resolved()
            .ok_or_else(|| Error::internal("workspace vanished after loading"))
    }
    fn request_shutdown(&self, actor: &str, reason: &str) {
        if let Some(d) = self.0.upgrade() {
            d.request_shutdown(actor, reason);
        }
    }
    fn started_by_up(&self) -> bool {
        self.0.upgrade().is_some_and(|d| d.started_by_up())
    }
    fn set_started_by_up(&self) {
        if let Some(d) = self.0.upgrade() {
            d.set_started_by_up();
        }
    }
    fn output_sink(&self) -> Option<Arc<dyn OutputSink>> {
        self.0
            .upgrade()
            .map(|d| d.logs().clone() as Arc<dyn OutputSink>)
    }
    fn state_store(&self) -> Option<Arc<StateStore>> {
        self.0.upgrade().map(|d| d.state_store().clone())
    }
}

/// Shared by the supervisor and every actor.
pub struct Core {
    pub(crate) events: Arc<EventBus>,
    pub(crate) runtimes: RuntimeRegistry,
    pub(crate) waiter: Arc<dyn Waiter>,
    pub(crate) ports: PortBook,
    pub(crate) run_id: String,
    pub(crate) base_env: BTreeMap<String, String>,
    pub(crate) sink: Arc<dyn OutputSink>,
    /// Durable state (deliverable 11); `None` in unit tests.
    pub(crate) state: Option<Arc<StateStore>>,
}

/// The supervisor. Installed into the daemon by [`Daemon::run`].
pub struct Supervisor {
    core: Arc<Core>,
    host: Arc<dyn Host>,
    cells: Mutex<IndexMap<String, Arc<StemCell>>>,
    /// Serialises lifecycle operations (`status` never takes it).
    ops: tokio::sync::Mutex<()>,
    /// Cancelled by `down`/shutdown so an in-flight `up` stops waiting.
    cancel: Mutex<CancellationToken>,
}

/// A fresh run id (a ULID, unique per daemon run).
fn new_run_id() -> String {
    crate::state::new_run_id()
}

fn params<T: DeserializeOwned>(method: &str, v: &Value) -> Result<T, Error> {
    let v = if v.is_null() { json!({}) } else { v.clone() };
    serde_json::from_value(v).map_err(|e| {
        Error::usage(
            format!("invalid params for `{method}`: {e}"),
            "see docs/lifecycle.md for each method's params",
        )
    })
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(|e| Error::internal(e.to_string()))
}

fn ms(v: Option<u64>) -> Option<Duration> {
    v.map(Duration::from_millis)
}

impl Supervisor {
    /// A supervisor with explicit parts (tests use fake runtimes/waiters).
    pub fn new(
        events: Arc<EventBus>,
        host: Arc<dyn Host>,
        runtimes: RuntimeRegistry,
        waiter: Arc<dyn Waiter>,
    ) -> Arc<Self> {
        Arc::new(Self {
            core: Arc::new(Core {
                events,
                runtimes,
                waiter,
                ports: PortBook::default(),
                run_id: host.state_store().map_or_else(new_run_id, |s| s.run_id()),
                base_env: std::env::vars().collect(),
                sink: host.output_sink().unwrap_or_else(|| Arc::new(NullSink)),
                state: host.state_store(),
            }),
            host,
            cells: Mutex::new(IndexMap::new()),
            ops: tokio::sync::Mutex::new(()),
            cancel: Mutex::new(CancellationToken::new()),
        })
    }

    /// The production supervisor for `daemon`: process runtime, alive waiter.
    pub fn for_daemon(daemon: &Arc<Daemon>) -> Arc<Self> {
        Self::new(
            daemon.events().clone(),
            Arc::new(DaemonHost(Arc::downgrade(daemon))),
            RuntimeRegistry::with_process(),
            Arc::new(AliveWaiter),
        )
    }

    /// This daemon run's id (`STEMS_RUN_ID`).
    pub fn run_id(&self) -> &str {
        &self.core.run_id
    }

    /// The cell of `name`, created (with its actor) on first use.
    pub fn cell(&self, name: &str) -> Arc<StemCell> {
        let mut cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
        cells
            .entry(name.to_string())
            .or_insert_with(|| StemCell::spawn(self.core.clone(), name))
            .clone()
    }

    fn existing_cells(&self) -> Vec<Arc<StemCell>> {
        self.cells
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn cells_for(&self, names: &[String]) -> HashMap<String, Arc<StemCell>> {
        names.iter().map(|n| (n.clone(), self.cell(n))).collect()
    }

    fn workspace(&self, reload: bool, actor: &str) -> Result<Arc<Resolved>, Error> {
        if reload {
            return self.host.reload(actor);
        }
        match self.host.resolved() {
            Some(r) => Ok(r),
            None => self.host.reload(actor),
        }
    }

    fn emit(&self, d: EventDraft) {
        self.core.events.emit(d);
    }

    /// Cancel an in-flight `up` and return a fresh token for the next one.
    fn cancel_running(&self) {
        let mut c = self.cancel.lock().unwrap_or_else(|e| e.into_inner());
        c.cancel();
        *c = CancellationToken::new();
    }

    fn token(&self) -> CancellationToken {
        self.cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `up`.
    pub async fn up(&self, p: UpParams, actor: &str) -> Result<UpResult, Error> {
        if let Some(profile) = &p.profile {
            return Err(Error::not_implemented("`up --profile`", "26")
                .with_details(json!({ "profile": profile, "deliverable": "26" })));
        }
        if p.daemon_auto_started {
            self.host.set_started_by_up();
        }
        let ws = self.workspace(true, actor)?;
        let plan = schedule::plan(&ws, &p.stems, false)?;
        let _ops = self.ops.lock().await;
        self.emit(EventDraft::new(EventKind::UP_STARTED, actor).data(json!({
            "requested": p.stems,
            "stems": plan.order,
            "layers": plan.layers,
            "detach": p.detach,
        })));
        let mut res = self.run_plan(&plan, &ws, &p, actor, "up").await;
        res.requested = p.stems.clone();
        self.emit(EventDraft::new(EventKind::UP_FINISHED, actor).data(json!({
            "requested": res.requested,
            "started": res.ready,
            "failed": res.failed.iter().map(|f| &f.stem).collect::<Vec<_>>(),
            "skipped": res.skipped,
            "ok": res.ok,
        })));
        Ok(res)
    }

    async fn run_plan(
        &self,
        plan: &Plan,
        ws: &Arc<Resolved>,
        p: &UpParams,
        actor: &str,
        reason: &str,
    ) -> UpResult {
        let cells = self.cells_for(&plan.order);
        let opts = schedule::RunOptions {
            ws: ws.clone(),
            actor: actor.to_string(),
            reason: reason.to_string(),
            fail_fast: p.fail_fast,
            max_parallel: p.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL),
            deadline: ms(p.timeout_ms).map(|d| tokio::time::Instant::now() + d),
            pass_env: p.pass_env.clone(),
            cancel: self.token(),
        };
        schedule::run(plan, &cells, &opts).await
    }

    fn not_managed(ws: &Resolved, stems: &[String], op: &str) -> Result<(), Error> {
        for s in stems {
            if ws
                .workspace
                .stem(s)
                .is_some_and(|x| x.kind() == StemType::External)
            {
                return Err(Error::new(
                    ErrorCode::NotManaged,
                    format!("`{s}` is an external stem; stems never starts or stops it"),
                )
                .with_hint("start it yourself; stems only monitors it")
                .with_details(json!({ "stem": s, "operation": op })));
            }
        }
        Ok(())
    }

    /// `start`.
    pub async fn start(&self, p: StartParams, actor: &str) -> Result<UpResult, Error> {
        let ws = self.workspace(true, actor)?;
        schedule::plan(&ws, &p.stems, p.no_deps)?;
        Self::not_managed(&ws, &p.stems, "start")?;
        let plan = schedule::plan(&ws, &p.stems, p.no_deps)?;
        let _ops = self.ops.lock().await;
        let up = UpParams {
            stems: p.stems.clone(),
            timeout_ms: p.timeout_ms,
            ..UpParams::default()
        };
        let mut res = self.run_plan(&plan, &ws, &up, actor, "start").await;
        res.requested = p.stems;
        Ok(res)
    }

    /// `restart`: stop the stems, then start them (and missing deps), keeping ports.
    pub async fn restart(&self, p: RestartParams, actor: &str) -> Result<UpResult, Error> {
        if p.build {
            return Err(Error::not_implemented("`restart --build`", "16")
                .with_details(json!({ "deliverable": "16" })));
        }
        let ws = self.workspace(true, actor)?;
        schedule::plan(&ws, &p.stems, p.no_deps)?;
        Self::not_managed(&ws, &p.stems, "restart")?;
        let plan = schedule::plan(&ws, &p.stems, p.no_deps)?;
        let _ops = self.ops.lock().await;
        let stop = self.stop_set(&ws, &p.stems, None, actor, "restart").await;
        if let Some(f) = stop.failed.into_iter().next() {
            return Err(f.error);
        }
        let up = UpParams {
            stems: p.stems.clone(),
            timeout_ms: p.timeout_ms,
            ..UpParams::default()
        };
        let mut res = self.run_plan(&plan, &ws, &up, actor, "restart").await;
        res.requested = p.stems;
        Ok(res)
    }

    /// Stop `names` in reverse dependency order (parallel within a layer).
    async fn stop_set(
        &self,
        ws: &Resolved,
        names: &[String],
        grace: Option<Duration>,
        actor: &str,
        reason: &str,
    ) -> DownResult {
        let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
        let mut layers: Vec<Vec<String>> = ws
            .workspace
            .stop_order()
            .unwrap_or_default()
            .into_iter()
            .map(|l| {
                l.into_iter()
                    .filter(|n| wanted.contains(n.as_str()))
                    .collect::<Vec<_>>()
            })
            .filter(|l| !l.is_empty())
            .collect();
        let placed: HashSet<String> = layers.iter().flatten().cloned().collect();
        let rest: Vec<String> = names
            .iter()
            .filter(|n| !placed.contains(*n))
            .cloned()
            .collect();
        if !rest.is_empty() {
            layers.insert(0, rest);
        }
        let mut res = DownResult::default();
        for layer in layers {
            let futs = layer.iter().map(|n| {
                let cell = self.cell(n);
                let g = grace.unwrap_or_else(|| {
                    ws.workspace
                        .stem(n)
                        .map_or(Duration::from_secs(10), |s| s.stop_grace.as_duration())
                });
                async move { (n.clone(), cell.stop(g, actor, reason).await) }
            });
            for (n, r) in futures::future::join_all(futs).await {
                match r {
                    StopReply::Stopped => res.stopped.push(n),
                    StopReply::NotRunning => res.skipped.push(n),
                    StopReply::Failed(error) => res.failed.push(StemFailure { stem: n, error }),
                }
            }
        }
        res.ok = res.failed.is_empty();
        res
    }

    fn any_running(&self) -> bool {
        self.existing_cells().iter().any(|c| c.state().is_running())
    }

    /// `down`.
    pub async fn down(&self, p: DownParams, actor: &str) -> Result<DownResult, Error> {
        self.cancel_running();
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            schedule::plan(&ws, &p.stems, true)?;
        }
        let _ops = self.ops.lock().await;
        let mut names: Vec<String> = if p.stems.is_empty() {
            self.existing_cells()
                .iter()
                .filter(|c| {
                    let s = c.state();
                    s.is_running() || s == stems_core::StemState::Failed
                })
                .map(|c| c.name.clone())
                .collect()
        } else {
            p.stems.clone()
        };
        let externals: Vec<String> = names
            .iter()
            .filter(|n| {
                ws.workspace
                    .stem(n)
                    .is_some_and(|s| s.kind() == StemType::External)
            })
            .cloned()
            .collect();
        names.retain(|n| !externals.contains(n));
        self.emit(EventDraft::new(EventKind::DOWN_STARTED, actor).data(json!({
            "requested": p.stems,
            "all": p.all,
        })));
        let mut res = self
            .stop_set(&ws, &names, ms(p.timeout_ms), actor, "down")
            .await;
        res.skipped.extend(externals);
        let shutdown = p.all || (!self.any_running() && self.host.started_by_up());
        res.daemon_stopping = shutdown;
        self.emit(
            EventDraft::new(EventKind::DOWN_FINISHED, actor).data(json!({
                "stopped": res.stopped,
                "skipped": res.skipped,
                "failed": res.failed.iter().map(|f| &f.stem).collect::<Vec<_>>(),
                "daemon_stopping": shutdown,
            })),
        );
        if shutdown {
            let host = self.host.clone();
            let actor = actor.to_string();
            // After the reply has been written.
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                host.request_shutdown(&actor, "down");
            });
        }
        Ok(res)
    }

    /// Running stems depending (transitively, hard edges) on `names`, not in `names`.
    fn running_dependants(&self, ws: &Resolved, names: &[String]) -> Vec<String> {
        let mut set: HashSet<String> = names.iter().cloned().collect();
        let mut out = Vec::new();
        loop {
            let more: Vec<String> = ws
                .workspace
                .stems()
                .filter(|s| !set.contains(&s.name))
                .filter(|s| {
                    s.depends_on
                        .iter()
                        .any(|d| !d.soft && set.contains(&d.stem))
                })
                .filter(|s| {
                    let cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
                    cells.get(&s.name).is_some_and(|c| c.state().is_running())
                })
                .map(|s| s.name.clone())
                .collect();
            if more.is_empty() {
                return out;
            }
            for m in more {
                set.insert(m.clone());
                out.push(m);
            }
        }
    }

    /// `stop`.
    pub async fn stop(&self, p: StopParams, actor: &str) -> Result<DownResult, Error> {
        let ws = self.workspace(false, actor)?;
        schedule::plan(&ws, &p.stems, true)?;
        Self::not_managed(&ws, &p.stems, "stop")?;
        let _ops = self.ops.lock().await;
        let dependants = self.running_dependants(&ws, &p.stems);
        if !dependants.is_empty() && !p.cascade {
            return Err(Error::new(
                ErrorCode::HasDependants,
                format!(
                    "cannot stop {}: running stems depend on it: {}",
                    p.stems.join(", "),
                    dependants.join(", ")
                ),
            )
            .with_hint(format!(
                "stop them too with `stems stop {} --cascade`, or stop {} first",
                p.stems.join(" "),
                dependants.join(" ")
            ))
            .with_details(json!({ "stems": p.stems, "dependants": dependants })));
        }
        let mut names = p.stems.clone();
        names.extend(dependants);
        Ok(self
            .stop_set(&ws, &names, ms(p.timeout_ms), actor, "stop")
            .await)
    }

    /// `status` (never waits for a lifecycle operation).
    pub fn status(&self, p: &StatusParams, actor: &str) -> Result<StatusResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            schedule::plan(&ws, &p.stems, true)?;
        }
        let stems: Vec<_> = ws
            .workspace
            .stems()
            .filter(|s| p.stems.is_empty() || p.stems.contains(&s.name))
            .map(|s| self.cell(&s.name).status(s, &self.core, p.verbose))
            .collect();
        let summary = StatusSummary::of(&stems);
        Ok(StatusResult { stems, summary })
    }

    /// Crash recovery (deliverable 11): adopt every stem the previous run
    /// recorded that is still alive (same pid *and* start time) as
    /// `healthy` (`stem.adopted`), drop the others (`stem.recovered_dead`),
    /// restore sticky `auto` ports, carry the stamps over, and rewrite the
    /// state file. Runs before the daemon serves requests.
    pub async fn recover(&self, previous: Option<StateFile>, actor: &str) -> RecoverReport {
        let _ops = self.ops.lock().await;
        let mut report = RecoverReport::default();
        let ws = self.host.resolved();
        let previous = previous.unwrap_or_else(|| StateFile::new("", Default::default()));
        if let Some(store) = &self.core.state {
            let stamps = previous.stamps.clone();
            store.update(|f| f.stamps = stamps);
        }
        for (name, rec) in previous.stems {
            let stem = ws.as_ref().and_then(|w| w.workspace.stem(&name).cloned());
            let kind = match (&stem, &rec.container_id) {
                (Some(s), _) => s.kind(),
                (None, Some(_)) => StemType::Docker,
                (None, None) => StemType::Process,
            };
            let grace = stem
                .as_ref()
                .map_or(Duration::from_secs(10), |s| s.stop_grace.as_duration());
            let adopted = match self.core.runtimes.get(kind) {
                Ok(rt) => rt.adopt(&rec.adopt_record()).await.map(|h| (rt, h)),
                Err(_) => None,
            };
            match adopted {
                Some((rt, h)) => {
                    for p in rec.ports.iter().filter(|p| p.auto) {
                        self.core.ports.restore(&name, &p.name, p.port);
                    }
                    actor::adopt(&self.core, &self.cell(&name), rt, h, &rec, grace, actor);
                    report.adopted.push(name);
                }
                None => {
                    self.emit(
                        EventDraft::new(EventKind::STEM_RECOVERED_DEAD, actor)
                            .stem(&name)
                            .reason("not alive (or a different process) any more; record cleared")
                            .data(json!({
                                "pid": rec.pid,
                                "pgid": rec.pgid,
                                "start_time": rec.start_time,
                                "container_id": rec.container_id,
                            })),
                    );
                    report.dead.push(name);
                }
            }
        }
        if let Some(store) = &self.core.state {
            store.flush();
        }
        tracing::info!(adopted = ?report.adopted, dead = ?report.dead, "crash recovery done");
        report
    }

    /// `adopt_orphans` (deliverable 11): register running processes found by
    /// the orphan scan as their stems. Params `{ orphans: [{stem, pid}] }`;
    /// result `{ adopted: [{stem, pid, pgid}], failed: [{stem, pid, error}] }`.
    pub async fn adopt_orphans(&self, p: &Value, actor: &str) -> Result<Value, Error> {
        #[derive(serde::Deserialize)]
        struct Item {
            stem: String,
            pid: i32,
        }
        #[derive(serde::Deserialize)]
        struct Params {
            orphans: Vec<Item>,
        }
        let p: Params = params("adopt_orphans", p)?;
        let ws = self.workspace(false, actor)?;
        let _ops = self.ops.lock().await;
        let mut adopted = Vec::new();
        let mut failed = Vec::new();
        for it in p.orphans {
            let r: Result<(i32, i32), Error> = async {
                let stem = ws.workspace.stem(&it.stem).ok_or_else(|| {
                    Error::new(
                        ErrorCode::UnknownStem,
                        format!("stem `{}` does not exist", it.stem),
                    )
                })?;
                let cell = self.cell(&it.stem);
                if cell.state().is_running() {
                    return Err(Error::usage(
                        format!("`{}` is already running under stems", it.stem),
                        "stop it first, or kill the orphan instead",
                    ));
                }
                let orphan = crate::orphans::Orphan {
                    kind: crate::orphans::OrphanKind::Process,
                    port: None,
                    pid: Some(it.pid),
                    pgid: None,
                    command: String::new(),
                    container_id: None,
                    stem: Some(it.stem.clone()),
                    matches_start_command: false,
                };
                let ar = crate::orphans::adopt_record(&orphan)
                    .ok_or_else(|| Error::internal(format!("process {} is gone", it.pid)))?;
                let rt = self.core.runtimes.get(stem.kind())?;
                let h = rt.adopt(&ar).await.ok_or_else(|| {
                    Error::internal(format!("process {} cannot be adopted", it.pid))
                })?;
                let started_at = ar
                    .start_time
                    .approx_system_time()
                    .map_or_else(chrono::Utc::now, chrono::DateTime::<chrono::Utc>::from);
                let rec = StemRecord {
                    pid: ar.pid,
                    pgid: ar.pgid,
                    start_time: ar.start_time,
                    container_id: None,
                    ports: stem
                        .ports
                        .iter()
                        .filter_map(|pt| match pt.port {
                            stems_config::PortRef::Fixed(n) => Some(crate::state::PortRecord {
                                name: pt.name.clone(),
                                port: n,
                                auto: false,
                            }),
                            stems_config::PortRef::Auto => None,
                        })
                        .collect(),
                    overlays: Vec::new(),
                    state: stems_core::StemState::Healthy,
                    started_at,
                    log_file: None,
                };
                actor::adopt(
                    &self.core,
                    &cell,
                    rt,
                    h,
                    &rec,
                    stem.stop_grace.as_duration(),
                    actor,
                );
                Ok((ar.pid, ar.pgid))
            }
            .await;
            match r {
                Ok((pid, pgid)) => {
                    adopted.push(json!({ "stem": it.stem, "pid": pid, "pgid": pgid }))
                }
                Err(e) => failed.push(json!({ "stem": it.stem, "pid": it.pid, "error": e })),
            }
        }
        Ok(json!({ "adopted": adopted, "failed": failed }))
    }

    /// Stop everything that runs, in reverse dependency order (daemon exit path).
    pub async fn stop_all(&self, actor: &str, reason: &str) {
        self.cancel_running();
        let _ops = self.ops.lock().await;
        let names: Vec<String> = self
            .existing_cells()
            .iter()
            .filter(|c| c.state().is_running())
            .map(|c| c.name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        let ws = self.host.resolved();
        let res = match ws {
            Some(ws) => self.stop_set(&ws, &names, None, actor, reason).await,
            None => {
                let futs = names.iter().map(|n| {
                    let c = self.cell(n);
                    async move { c.stop(Duration::from_secs(10), actor, reason).await }
                });
                futures::future::join_all(futs).await;
                DownResult::default()
            }
        };
        tracing::info!(stopped = ?res.stopped, failed = res.failed.len(), "supervisor stopped all stems");
    }
}

#[async_trait::async_trait]
impl SupervisorHooks for Supervisor {
    async fn handle(
        &self,
        ctx: &RequestCtx,
        method: &str,
        p: &Value,
    ) -> Option<Result<Value, Error>> {
        let actor = ctx.actor.as_str();
        let r = match method {
            m if m == Method::UP => match params(m, p) {
                Ok(p) => self.up(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::DOWN => match params(m, p) {
                Ok(p) => self.down(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::START => match params(m, p) {
                Ok(p) => self.start(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::STOP => match params(m, p) {
                Ok(p) => self.stop(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::RESTART => match params(m, p) {
                Ok(p) => self.restart(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::STATUS => match params::<StatusParams>(m, p) {
                Ok(p) => self.status(&p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::ADOPT_ORPHANS => self.adopt_orphans(p, actor).await,
            _ => return None,
        };
        Some(r)
    }

    async fn shutdown(&self) {
        self.stop_all(stems_api::DAEMON_ACTOR, "daemon shutdown")
            .await;
    }

    async fn recover(&self, previous: Option<StateFile>) {
        Supervisor::recover(self, previous, stems_api::DAEMON_ACTOR).await;
    }
}

#[cfg(test)]
mod tests;
