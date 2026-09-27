//! The supervisor (deliverable 10): `up`, `down`, `start`, `stop`,
//! `restart` and `status` over per-stem actors (see `docs/lifecycle.md`).
//!
//! * [`actor`] — one task per stem owning its runtime handle and state.
//! * [`state`] — the legal state transitions (§6.4).
//! * [`schedule`] — the layer scheduler (parallel starts, edge conditions, fail-fast).
//! * [`waiter`] — when a started stem counts as `started` / `healthy` / `seeded`.
//! * [`probes`] — health probes, transitions, degraded derivation (21).
//! * [`env`] — a stem's environment (FR-ST-4) and deferred `${stem.x.port}` rendering.
//! * [`ports`] — `PORT_IN_USE` checks and sticky `port: auto` allocation.
//!
//! Hooks for later deliverables: [`RuntimeRegistry`] (docker 14, compose
//! 15), [`OutputSink`] (logs 12), [`Waiter`] (probes 21), the actor's exit
//! handler (restart policies 22), [`Host`] (its `state_store` is 11's state file).

pub mod actor;
pub mod cascade;
pub mod containers;
pub mod env;
pub mod hooks;
pub mod metrics;
pub mod outputs;
pub mod overlays;
pub mod ports;
pub mod probes;
pub mod reload;
pub mod restart;
pub mod run;
pub mod schedule;
pub mod state;
pub mod waiter;
pub mod watch;

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
use stems_runtime::{ExternalRuntime, OutputStream, ProcessRuntime, Runtime};
use tokio_util::sync::CancellationToken;

pub use actor::{Phase, StemCell, StopReply};
pub use ports::PortBook;
pub use schedule::Plan;
pub use waiter::{ProbeWaiter, WaitTarget, Waiter};

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

/// Runtimes by stem type. Docker (14) and compose (15) register here. The
/// no-op [`ExternalRuntime`] for `type: external` is always registered
/// (also in [`Default`]), so monitor-only stems never hit `NOT_IMPLEMENTED`.
#[derive(Clone)]
pub struct RuntimeRegistry {
    by_type: HashMap<StemType, Arc<dyn Runtime>>,
    /// Lazily connected docker/compose runtimes (14/15), when registered.
    containers: Option<Arc<containers::Containers>>,
}

impl Default for RuntimeRegistry {
    fn default() -> Self {
        let mut by_type: HashMap<StemType, Arc<dyn Runtime>> = HashMap::new();
        by_type.insert(StemType::External, Arc::new(ExternalRuntime::new()));
        Self {
            by_type,
            containers: None,
        }
    }
}

impl RuntimeRegistry {
    /// The process runtime (plus the external one).
    pub fn with_process() -> Self {
        let mut r = Self::default();
        r.register(StemType::Process, Arc::new(ProcessRuntime::new()));
        r
    }

    /// Register the lazily connected docker and compose runtimes (14/15):
    /// Docker is contacted only when a docker/compose stem is started.
    pub fn with_containers(mut self, c: Arc<containers::Containers>) -> Self {
        for kind in [StemType::Docker, StemType::Compose] {
            self.register(
                kind,
                Arc::new(containers::LazyRuntime::new(c.clone(), kind)),
            );
        }
        self.containers = Some(c);
        self
    }

    /// The docker/compose runtimes, if registered.
    pub fn containers(&self) -> Option<&Arc<containers::Containers>> {
        self.containers.as_ref()
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
    /// A writer for one script run's output (deliverable 16: records tagged
    /// `stream: script, tag: <script>`); `None` drops it.
    fn script_writer(&self, _stem: &str, _script: &str) -> Option<crate::logs::ScriptWriter> {
        None
    }
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
    /// Load + validate the workspace from disk *without* installing it
    /// (33: compared with the applied one first). Every error is returned;
    /// `requires:` tools are probed only with `check_requires` (commands,
    /// not the file watcher). Default: [`Host::reload`].
    fn load_candidate(
        &self,
        actor: &str,
        _check_requires: bool,
    ) -> Result<Arc<Resolved>, stems_core::Errors> {
        self.reload(actor).map_err(|e| stems_core::Errors(vec![e]))
    }
    /// Make `resolved` the applied workspace (33). Default: nothing.
    fn install(&self, _resolved: Arc<Resolved>, _actor: &str) {}
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
    /// The daemon's `<home>/<ws-hash>` directory; scripts' `STEMS_STATE_DIR`
    /// live below it (16). Default: the state file's directory.
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        self.state_store()?
            .path()?
            .parent()
            .map(std::path::Path::to_path_buf)
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
    fn load_candidate(
        &self,
        _actor: &str,
        check_requires: bool,
    ) -> Result<Arc<Resolved>, stems_core::Errors> {
        let d = self.daemon().map_err(|e| stems_core::Errors(vec![e]))?;
        let Some(root) = d.info().workspace else {
            return Err(stems_core::Errors(vec![Error::usage(
                "this daemon has no workspace",
                "start it with `stems daemon start` inside a workspace, or call `load_workspace`",
            )]));
        };
        d.load_candidate(&root, !check_requires).map(Arc::new)
    }
    fn install(&self, resolved: Arc<Resolved>, actor: &str) {
        if let Some(d) = self.0.upgrade() {
            d.install_workspace(resolved, actor);
        }
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
    /// Runs lifecycle and workspace scripts (deliverable 16).
    pub(crate) scripts: crate::scripts::ScriptRunner,
    /// `up --force-overlays` is in progress (18): overlay destinations stems
    /// does not own are backed up and overwritten instead of refused.
    pub(crate) force_overlays: std::sync::atomic::AtomicBool,
    /// Evaluated outputs of running stems (26, FR-ST-6).
    pub(crate) outputs: outputs::OutputStore,
    /// Samples, history and thresholds (25).
    pub(crate) metrics: metrics::MetricsStore,
    /// Watchdogs: per-stem watchers, pause flags (24).
    pub(crate) watch: watch::WatchHub,
    /// Cascading restarts: the running one, the queue gate (FR-LC-9).
    pub(crate) cascade: cascade::CascadeHub,
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
    /// Custom script runs: per-stem slots, shutdown (17).
    runs: run::ScriptRuns,
    /// Config reload: pending plan, last error, last `up` selection (33).
    reload: reload::ReloadState,
}

/// A fresh run id (a ULID, unique per daemon run).
/// The daemon's environment as inherited by scripts, health commands and
/// process stems, with the `docker` CLI's directory prepended to `PATH`
/// when `docker` is not on it (so `docker exec ...` works from a daemon
/// started without Docker Desktop's bin directory on `PATH`; docs/docker.md).
fn base_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    stems_runtime::docker::ensure_docker_on_path(&mut env);
    env
}

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
        let run_id = host.state_store().map_or_else(new_run_id, |s| s.run_id());
        // Stamps and script runs need a store even without a state file.
        let state = host
            .state_store()
            .unwrap_or_else(|| Arc::new(StateStore::in_memory(run_id.clone())));
        let sink = host.output_sink().unwrap_or_else(|| Arc::new(NullSink));
        let script_rt = runtimes
            .get(StemType::Process)
            .unwrap_or_else(|_| Arc::new(ProcessRuntime::new()));
        let data_dir = host
            .data_dir()
            .unwrap_or_else(|| std::env::temp_dir().join(format!("stems-{run_id}")));
        let scripts = crate::scripts::ScriptRunner::new(
            script_rt,
            events.clone(),
            sink.clone(),
            Some(state.clone()),
            data_dir,
            run_id.clone(),
        );
        let sup = Arc::new(Self {
            core: Arc::new(Core {
                events,
                runtimes,
                waiter,
                ports: PortBook::default(),
                run_id,
                base_env: base_env(),
                sink,
                state: Some(state),
                scripts,
                force_overlays: std::sync::atomic::AtomicBool::new(false),
                outputs: outputs::OutputStore::default(),
                metrics: metrics::MetricsStore::default(),
                watch: watch::WatchHub::default(),
                cascade: cascade::CascadeHub::default(),
            }),
            host,
            cells: Mutex::new(IndexMap::new()),
            ops: tokio::sync::Mutex::new(()),
            cancel: Mutex::new(CancellationToken::new()),
            runs: run::ScriptRuns::default(),
            reload: reload::ReloadState::default(),
        });
        // Watchdog actions run through the supervisor (24).
        sup.core.watch.bind(&sup);
        // Policy-triggered cascades run through it too (FR-LC-9).
        sup.core.cascade.bind(&sup);
        sup
    }

    /// The production supervisor for `daemon`: process runtime, lazily
    /// connected docker/compose runtimes (14/15), health probes (21).
    pub fn for_daemon(daemon: &Arc<Daemon>) -> Arc<Self> {
        let compose_dir = containers::compose_dir_for(daemon.state_store().path());
        let sup = Self::new(
            daemon.events().clone(),
            Arc::new(DaemonHost(Arc::downgrade(daemon))),
            RuntimeRegistry::with_process()
                .with_containers(Arc::new(containers::Containers::production(compose_dir))),
            Arc::new(ProbeWaiter),
        );
        metrics::spawn_sampler(&sup);
        // The config watcher (33).
        reload::spawn_watcher(&sup);
        sup
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
            // The latest *applied* config; a disk change that would restart
            // running stems stays pending (33, `config apply`).
            return reload::refresh(self, actor);
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
        if p.daemon_auto_started {
            self.host.set_started_by_up();
        }
        let ws = self.workspace(true, actor)?;
        // Stems, else the profile, else everything; plus hard deps (26).
        let (plan, selection) = schedule::plan_up(&ws, p.profile.as_deref(), &p.stems)?;
        let _ops = self.ops.lock().await;
        // `config apply` starts an added stem this selection covers (33).
        self.reload.note_up(p.profile.clone(), p.stems.clone());
        if let Some(profile) = &selection.profile
            && !selection.expanded.is_empty()
        {
            self.emit(
                EventDraft::new(EventKind::PROFILE_EXPANDED, actor).data(json!({
                    "profile": profile,
                    "added": selection.expanded,
                    "requested": selection.requested,
                })),
            );
        }
        self.emit(EventDraft::new(EventKind::UP_STARTED, actor).data(json!({
            "profile": selection.profile,
            "requested": p.stems,
            "stems": plan.order,
            "layers": plan.layers,
            "detach": p.detach,
            "fresh": p.fresh,
        })));
        // Workspace `bootstrap`, then (`--fresh`) reset + clear stamps (16).
        let pre = async {
            self.bootstrap(&ws.workspace, actor, &p.pass_env).await?;
            if p.fresh {
                let r = self.reset_stems(&ws, &plan.order, actor).await;
                if let Some(f) = r.failed.into_iter().next() {
                    return Err(f.error);
                }
            }
            Ok::<(), Error>(())
        };
        if let Err(e) = pre.await {
            self.emit(EventDraft::new(EventKind::UP_FINISHED, actor).data(json!({
                "requested": p.stems,
                "started": [],
                "failed": [],
                "skipped": plan.order,
                "ok": false,
                "error": e,
            })));
            return Err(e);
        }
        if p.sync {
            self.sync_existing_repos(&ws, &plan.order, actor).await;
        }
        self.core
            .force_overlays
            .store(p.force_overlays, std::sync::atomic::Ordering::SeqCst);
        // `up --no-watch` switches watchdogs off until the next `up` (24).
        self.core.watch.set_disabled(p.no_watch);
        let mut res = self.run_plan(&plan, &ws, &p, actor, "up").await;
        self.core
            .force_overlays
            .store(false, std::sync::atomic::Ordering::SeqCst);
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

    /// `up --sync` (20): fetch + check out the planned stems' existing git
    /// clones. Not fatal: a failure is a `repo.failed` event and the stem
    /// starts from the checkout it has (missing clones are cloned by the
    /// actor, which fails the stem on error).
    async fn sync_existing_repos(&self, ws: &Resolved, order: &[String], actor: &str) {
        let names: Vec<String> = order
            .iter()
            .filter(|n| {
                ws.workspace.stem(n).is_some_and(|s| {
                    matches!(&s.codebase, Some(stems_config::Codebase::Git { path, .. }) if path.is_dir())
                })
            })
            .cloned()
            .collect();
        if names.is_empty() {
            return;
        }
        let opts = crate::repos::SyncOptions {
            force_fetch: true,
            ..Default::default()
        };
        let ctx = crate::repos::RepoCtx::for_core(&self.core, actor);
        if let Err(e) = crate::repos::sync(&ws.workspace, &names, &opts, &ctx).await {
            tracing::warn!(error = %e.message, "up --sync");
        }
    }

    /// `repos_sync` / `repos_status` (20).
    async fn repos(&self, method: &str, p: &Value, actor: &str) -> Result<Value, Error> {
        let ws = self.workspace(true, actor)?;
        let ctx = crate::repos::RepoCtx::for_core(&self.core, actor);
        if method == Method::REPOS_SYNC {
            let p: stems_api::ReposSyncParams = params(method, p)?;
            let opts = crate::repos::SyncOptions {
                force_fetch: p.force_fetch,
                recurse_submodules: p.recurse_submodules,
            };
            let _ops = self.ops.lock().await;
            to_value(crate::repos::sync(&ws.workspace, &p.stems, &opts, &ctx).await?)
        } else {
            let p: stems_api::ReposStatusParams = params(method, p)?;
            to_value(crate::repos::status(&ws.workspace, &p.stems, &ctx).await?)
        }
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

    /// `restart`: stop the stems, then start them (and missing deps), keeping
    /// ports; with a cascade their running hard dependants follow (FR-LC-9,
    /// [`cascade::restart`]).
    pub async fn restart(&self, p: RestartParams, actor: &str) -> Result<UpResult, Error> {
        let ws = self.workspace(true, actor)?;
        schedule::plan(&ws, &p.stems, p.no_deps)?;
        Self::not_managed(&ws, &p.stems, "restart")?;
        let plan = schedule::plan(&ws, &p.stems, p.no_deps)?;
        cascade::restart(self, &ws, &plan, p, actor).await
    }

    /// The stop, build and start of `restart` (the ops lock is held).
    pub(crate) async fn restart_stems(
        &self,
        ws: &Arc<Resolved>,
        plan: &Plan,
        p: &RestartParams,
        actor: &str,
    ) -> Result<UpResult, Error> {
        let ws = ws.clone();
        let stop = self.stop_set(&ws, &p.stems, None, actor, "restart").await;
        if let Some(f) = stop.failed.into_iter().next() {
            return Err(f.error);
        }
        if p.build {
            let b = self.build_stems(&ws, &p.stems, actor).await;
            if let Some(f) = b.failed.into_iter().next() {
                return Err(f.error);
            }
        }
        let up = UpParams {
            stems: p.stems.clone(),
            timeout_ms: p.timeout_ms,
            ..UpParams::default()
        };
        let mut res = self.run_plan(plan, &ws, &up, actor, "restart").await;
        res.requested = p.stems.clone();
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
        // Externals are never stopped, but `down` ends their monitoring (21).
        let unmonitor: Vec<String> = if p.stems.is_empty() {
            ws.workspace
                .stems()
                .filter(|s| s.kind() == StemType::External)
                .map(|s| s.name.clone())
                .collect()
        } else {
            externals.clone()
        };
        for n in &unmonitor {
            probes::unmonitor(&self.core, &self.cell(n), actor, "down");
        }
        res.skipped.extend(externals);
        // Overlays of stems that are not running (exited on their own) (18).
        self.cleanup_idle_overlays(&p.stems, actor);
        // Stopped containers left by `stop`/crashes, and `--volumes` (14).
        let running = |n: &str| {
            let cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
            cells.get(n).is_some_and(|c| c.state().is_running())
        };
        let (volumes, vol_failed) =
            containers::after_down(&self.core, &ws.workspace, &p.stems, &running, p.volumes).await;
        res.volumes_removed = volumes;
        if !vol_failed.is_empty() {
            res.failed.extend(vol_failed);
            res.ok = false;
        }
        if p.all
            && let Err(error) = self.teardown(&ws.workspace, actor).await
        {
            // Workspace `teardown` (16) failed: report it, still shut down.
            res.failed.push(StemFailure {
                stem: crate::scripts::WORKSPACE_LOG_STEM.into(),
                error,
            });
            res.ok = false;
        }
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
        let mut stems: Vec<_> = ws
            .workspace
            .stems()
            .filter(|s| p.stems.is_empty() || p.stems.contains(&s.name))
            .map(|s| {
                let mut st = self.cell(&s.name).status(s, &self.core, p.verbose);
                containers::decorate_status(
                    self.core.runtimes.containers().map(|c| &**c),
                    s.kind(),
                    &mut st,
                );
                st
            })
            .collect();
        self.core.metrics.decorate(&mut stems);
        watch::decorate(&self.core, &ws, &mut stems);
        // Degraded is derived here, never stored (21; metric thresholds 25).
        probes::apply_degraded(
            &ws,
            &mut stems,
            |n| self.cell(n).state(),
            |n| self.core.metrics.crossed(n),
        );
        let summary = StatusSummary::of(&stems);
        Ok(StatusResult { stems, summary })
    }

    /// `stem_config {stem}` (27): the stem's resolved config as JSON, for the
    /// TUI's detail view.
    pub fn stem_config(&self, p: &Value, actor: &str) -> Result<Value, Error> {
        let name = p.get("stem").and_then(Value::as_str).ok_or_else(|| {
            Error::usage(
                "invalid params for `stem_config`: missing `stem`",
                "pass `{\"stem\": \"<name>\"}`",
            )
        })?;
        let ws = self.workspace(false, actor)?;
        let stem = ws.workspace.stem(name).ok_or_else(|| {
            Error::new(ErrorCode::UnknownStem, format!("unknown stem `{name}`"))
                .with_hint("run `stems status` for the stems of this workspace")
        })?;
        to_value(stem)
    }

    /// `switch_variant {stem, variant?}` (FR-ST-8): what `stems switch`
    /// does, in the daemon (the TUI's variant picker; the CLI when a daemon
    /// runs). Without `variant`: the stem's choices. With one: write
    /// `stems.<stem>.variant` to the workspace's `stems.local.yaml` with the
    /// comment-preserving editor (`local` removes the key unless
    /// `stems.yaml` selects a default variant), re-validate the workspace
    /// from disk (the file is restored when that fails), then `config_apply
    /// {stems: [stem], yes: true}`.
    pub async fn switch_variant(
        &self,
        p: stems_api::SwitchVariantParams,
        actor: &str,
    ) -> Result<stems_api::SwitchVariantResult, Error> {
        use stems_config::{ConfigPath, edit, variants};
        let applied = self.workspace(false, actor)?;
        let root = applied.workspace.root.clone();
        let first = |errs: stems_core::Errors| {
            let all = errs.0.clone();
            let mut e = errs.0.into_iter().next().unwrap_or_else(|| {
                Error::internal("the workspace failed to load without an error")
            });
            if all.len() > 1 {
                e.details = json!({ "errors": all });
            }
            e
        };
        let opts = stems_config::LoadOptions {
            workspace: Some(root.clone()),
            cwd: root.clone(),
            env: std::env::vars().collect(),
            skip_local: false,
        };
        let (config_file, committed) =
            stems_config::committed_tree(&opts).map_err(|e| first(stems_core::Errors::from(e)))?;
        let file = config_file
            .parent()
            .map_or_else(|| root.clone(), std::path::Path::to_path_buf)
            .join(stems_config::LOCAL_FILE);
        let original = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(Error::new(
                    ErrorCode::ConfigReadFailed,
                    format!("{} cannot be read: {e}", file.display()),
                )
                .with_hint("check that the file is readable by the daemon's user"));
            }
        };
        let current = self.host.load_candidate(actor, false).map_err(first)?;
        let Some(stem) = current.workspace.stem(&p.stem) else {
            let known: Vec<&String> = current.workspace.stems.keys().collect();
            return Err(Error::new(
                ErrorCode::UnknownStem,
                format!("no stem named `{}`", p.stem),
            )
            .with_hint("run `stems status` for the stems of this workspace")
            .with_details(json!({ "stem": p.stem, "known": known })));
        };
        let name = stem.name.clone();
        let from = stem
            .variant
            .clone()
            .unwrap_or_else(|| variants::BASE_VARIANT.to_string());
        let local_tree = edit::parse_value(&original).ok();
        let choices = variants::choices(&committed, local_tree.as_ref(), &name);
        let list: Vec<stems_api::VariantChoice> = choices
            .iter()
            .map(|c| stems_api::VariantChoice {
                name: c.name.clone(),
                kind: c.kind.clone(),
                active: c.name == from,
            })
            .collect();
        let path = ConfigPath::root().key("stems").key(&name).key("variant");
        let mut out = stems_api::SwitchVariantResult {
            stem: name.clone(),
            from: from.clone(),
            to: from.clone(),
            kind: choices
                .iter()
                .find(|c| c.name == from)
                .and_then(|c| c.kind.clone()),
            variants: list,
            path: path.to_string(),
            ..Default::default()
        };
        let Some(target) = p.variant else {
            return Ok(out);
        };
        let to = if variants::is_base(&target) {
            variants::BASE_VARIANT.to_string()
        } else {
            target.clone()
        };
        let Some(choice) = choices.iter().find(|c| c.name == to) else {
            let names: Vec<&str> = choices
                .iter()
                .map(|c| c.name.as_str())
                .filter(|n| *n != variants::BASE_VARIANT)
                .collect();
            let hint = if names.is_empty() {
                format!("`{name}` declares no `variants` (see docs/config.md#variants-fr-st-8)")
            } else {
                format!(
                    "variants of `{name}`: {} (or `{}` for the base definition)",
                    names.join(", "),
                    variants::BASE_VARIANT
                )
            };
            return Err(Error::new(
                ErrorCode::UnknownVariant,
                format!("stem `{name}` has no variant `{target}`"),
            )
            .with_path(path)
            .with_hint(hint)
            .with_details(json!({ "stem": name, "variant": target, "known": names })));
        };
        let committed_default =
            variants::committed_selection(&committed, &name).filter(|v| !variants::is_base(v));
        let no_base = |_: &ConfigPath| None;
        let edited = if to == variants::BASE_VARIANT && committed_default.is_none() {
            edit::unset(&original, &path).map(|(t, _)| t)
        } else {
            edit::set(&original, &path, &to, &no_base)
        };
        let new = edited.map_err(|e| {
            Error::new(
                ErrorCode::Usage,
                format!("cannot edit `{path}` in {}: {}", file.display(), e.0),
            )
            .with_path(path.clone())
            .with_hint(format!(
                "set `variant: {to}` under `stems.{name}` in {} by hand",
                file.display()
            ))
        })?;
        out.to = to.clone();
        out.kind = choice.kind.clone();
        out.file = Some(file.clone());
        out.changed = original != new;
        out.diff = edit::diff_lines(&original, &new);
        if out.changed {
            let check = || self.host.load_candidate(actor, false).map(|_| ());
            match edit::write_checked(&file, &new, check) {
                Ok(()) => {}
                Err(edit::WriteError::Io(e)) => {
                    return Err(Error::new(
                        ErrorCode::ConfigReadFailed,
                        format!("cannot write {}: {e}", file.display()),
                    )
                    .with_hint("check the permissions of the integration repo directory")
                    .with_details(json!({ "file": file })));
                }
                Err(edit::WriteError::Rejected(errs)) => {
                    let mut e = first(errs);
                    if e.hint.is_none() {
                        e.hint = Some(format!("{} was left unchanged", stems_config::LOCAL_FILE));
                    }
                    return Err(e);
                }
            }
        }
        let r = self
            .config_apply(
                stems_api::ConfigApplyParams {
                    stems: vec![name],
                    yes: true,
                },
                actor,
            )
            .await?;
        out.applied = Some(r);
        Ok(out)
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
            let runs = previous.script_runs.clone();
            let overlays = previous.overlays.clone();
            store.update(|f| {
                f.stamps = stamps;
                f.script_runs = runs;
                f.overlays = overlays;
            });
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
                // Containers are verified by id + labels (14/15).
                Ok(rt) if matches!(kind, StemType::Docker | StemType::Compose) => {
                    let w = ws.as_ref().map(|w| &w.workspace);
                    containers::adopt(&self.core, kind, &rec.adopt_record(), w, &name)
                        .await
                        .map(|h| (rt, h))
                }
                Ok(rt) => rt.adopt(&rec.adopt_record()).await.map(|h| (rt, h)),
                Err(_) => None,
            };
            match adopted {
                Some((rt, h)) => {
                    for p in rec.ports.iter().filter(|p| p.auto) {
                        self.core.ports.restore(&name, &p.name, p.port);
                    }
                    actor::adopt(&self.core, &self.cell(&name), rt, h, &rec, grace, actor);
                    if let Some(ws) = &ws {
                        probes::monitor_adopted(&self.core, &self.cell(&name), ws);
                        self.cell(&name).remember_workspace(ws);
                        outputs::after_adopt(&self.core, &self.cell(&name), ws);
                    }
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
        // Overlays of stems that did not survive (18).
        self.cleanup_idle_overlays(&[], actor);
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
                probes::monitor_adopted(&self.core, &cell, &ws);
                cell.remember_workspace(&ws);
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
            m if m == Method::BUILD => match params(m, p) {
                Ok(p) => self.build(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::RESET => match params(m, p) {
                Ok(p) => self.reset(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::OVERLAYS => match params(m, p) {
                Ok(p) => self.overlays(&p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::REPOS_SYNC || m == Method::REPOS_STATUS => {
                self.repos(m, p, actor).await
            }
            m if m == Method::STAMPS => match params(m, p) {
                Ok(p) => self.stamps(p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::RUN_SCRIPT => match params(m, p) {
                Ok(p) => self.run_script(p, actor).await,
                Err(e) => Err(e),
            },
            m if m == Method::SCRIPT_CATALOG => match params(m, p) {
                Ok(p) => self.script_catalog(p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::STEM_CONFIG => self.stem_config(p, actor),
            m if m == Method::HEALTH => match params(m, p) {
                Ok(p) => self.health(&p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::METRICS => match params(m, p) {
                Ok(p) => self.metrics(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::WATCH_PAUSE || m == Method::WATCH_RESUME => match params(m, p) {
                Ok(p) => self
                    .watch_pause(p, m == Method::WATCH_PAUSE, actor)
                    .and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::WATCH_STATUS => match params(m, p) {
                Ok(p) => self.watch_status(p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::OUTPUTS => match params(m, p) {
                Ok(p) => self.outputs(&p, actor).and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::CONFIG_DIFF => self.config_diff(actor).and_then(to_value),
            m if m == Method::CONFIG_APPLY => match params(m, p) {
                Ok(p) => self.config_apply(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            m if m == Method::SWITCH_VARIANT => match params(m, p) {
                Ok(p) => self.switch_variant(p, actor).await.and_then(to_value),
                Err(e) => Err(e),
            },
            _ => return None,
        };
        Some(r)
    }

    async fn shutdown(&self) {
        self.runs.shutdown().await;
        self.stop_all(stems_api::DAEMON_ACTOR, "daemon shutdown")
            .await;
        // A failed start (or a crash the restart policy gave up on) keeps
        // its stopped container for inspection until `down`; it does not
        // outlive the daemon either (only `stems stop` keeps one).
        let failed = containers::failed_selection(
            self.existing_cells()
                .iter()
                .map(|c| (c.name.clone(), c.state())),
        );
        if !failed.is_empty()
            && let Some(ws) = self.host.resolved()
        {
            let none_running = |_: &str| false;
            containers::after_down(&self.core, &ws.workspace, &failed, &none_running, false).await;
        }
    }

    async fn recover(&self, previous: Option<StateFile>) {
        Supervisor::recover(self, previous, stems_api::DAEMON_ACTOR).await;
    }
}

#[cfg(test)]
mod tests;
