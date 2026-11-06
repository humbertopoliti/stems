//! Custom scripts (deliverable 17, `docs/scripts.md`): the `run_script` and
//! `script_catalog` methods.
//!
//! `run_script` runs any named script of a stem or of the workspace:
//!
//! 1. finds it (`UNKNOWN_STEM`, `SCRIPT_NOT_FOUND`);
//! 2. validates the arguments against its `args` schema
//!    ([`stems_core::scriptargs`], `SCRIPT_ARGS_INVALID`, exit 2);
//! 3. checks `requires_stopped:` — every listed stem must not run
//!    (`stopped`, `failed`); with `stop_deps` they are stopped first (like
//!    `stems stop`) — and `requires:` — every listed stem must be
//!    `healthy`; with `start_deps` they are started first (like `stems
//!    start`); else `SCRIPT_REQUIRES_UNMET` (49);
//! 4. waits (bounded by `ready_timeout_ms`, default 30 s) while the owning
//!    stem is in its start sequence (`setup`, `starting`, `seeding`, or
//!    `healthy` with `post_start`/`seed` pending), else `START_TIMEOUT`;
//! 5. takes the per-stem script slot (one script at a time per stem, or per
//!    workspace for workspace scripts) unless the script is `concurrent:
//!    true`; a run that has to wait emits `script.queued`;
//! 6. runs it through the [`crate::scripts::ScriptRunner`] with the argv
//!    `--name value…` (or the raw passthrough args) and `STEMS_ARG_<NAME>`
//!    env, retrying a failure `retries` times with exponential backoff
//!    ([`retry_backoff`]); every attempt emits `script.started` /
//!    `script.finished` with `data.run_id` and `data.attempt`.
//!
//! The run itself is a spawned task, so a client that disconnects never
//! leaves a half-supervised script behind; `wait: false` replies with the
//! `run_id` at once. Daemon shutdown cancels (kills) running scripts.
//!
//! `stem_env` (37, `docs/exec.md`) gives `stems exec`/`shell`/`env` a
//! stem's context ([`exec_env`]); they run the command themselves.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use stems_api::{
    EventKind, RunScriptAccepted, RunScriptParams, RunScriptResult, ScriptArgsInput,
    ScriptCatalogParams, ScriptCatalogResult, StartParams, StemEnvParams, StemEnvResult,
    StopParams,
};
use stems_config::{Backoff, Resolved, Script, Stem, StemRuntime, Workspace};
use stems_core::scriptargs::{ParsedArgs, build_catalog, parse_args, parse_args_json};
use stems_core::{Error, ErrorCode, StemState};
use tokio_util::sync::CancellationToken;

use super::env::{CallEnv, EnvInputs, StemEnv, build_env};
use super::hooks::{stem_env_with, workspace_env};
use super::outputs::OutputStore;
use super::ports::PortBook;
use super::{Core, Supervisor, schedule};
use crate::events::EventDraft;
use crate::scripts::{RunContext, ScriptRef, WORKSPACE_LOG_STEM};

/// Default bound for waiting on a stem that is still starting.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long daemon shutdown waits for cancelled scripts to be cleaned up.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

/// Backoff between script retries (FR-SC-8): 500 ms, doubling, capped at 10 s.
pub fn retry_backoff() -> Backoff {
    Backoff {
        initial: "500ms".parse().unwrap_or_default(),
        max: "10s".parse().unwrap_or_default(),
        factor: 2.0,
    }
}

/// Per-supervisor bookkeeping of custom script runs.
pub(crate) struct ScriptRuns {
    /// One slot per stem (and [`WORKSPACE_LOG_STEM`] for workspace scripts).
    slots: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Cancelled on daemon shutdown.
    cancel: CancellationToken,
    /// Every run task holds a read guard; shutdown takes the write lock.
    active: Arc<tokio::sync::RwLock<()>>,
}

impl Default for ScriptRuns {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            cancel: CancellationToken::new(),
            active: Arc::new(tokio::sync::RwLock::new(())),
        }
    }
}

impl ScriptRuns {
    fn slot(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    /// Kill running scripts and wait (bounded) until their runs ended.
    pub(crate) async fn shutdown(&self) {
        self.cancel.cancel();
        let _ = tokio::time::timeout(SHUTDOWN_WAIT, self.active.write()).await;
    }
}

/// The catalogue of `ws` (only `stem`'s scripts when given; `UNKNOWN_STEM`).
pub fn catalog(ws: &Resolved, stem: Option<&str>) -> Result<ScriptCatalogResult, Error> {
    if let Some(s) = stem {
        schedule::plan(ws, &[s.to_string()], true)?;
    }
    Ok(ScriptCatalogResult::from_entries(
        build_catalog(&ws.workspace),
        stem,
    ))
}

fn not_found(stem: Option<&str>, name: &str, known: Vec<&String>) -> Error {
    let mut known: Vec<&String> = known;
    known.sort();
    let known = if known.is_empty() {
        "none".to_string()
    } else {
        known
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (msg, hint) = match stem {
        Some(s) => (
            format!("stem `{s}` has no script `{name}`"),
            format!("its scripts: {known}; `stems scripts {s}` lists them"),
        ),
        None => (
            format!("the workspace has no script `{name}`"),
            format!(
                "workspace scripts: {known}; for a stem's script use `stems run <stem> {name}`"
            ),
        ),
    };
    Error::new(ErrorCode::ScriptNotFound, msg)
        .with_hint(hint)
        .with_details(json!({ "stem": stem, "script": name }))
}

fn with_context(e: Error, stem: Option<&str>, name: &str) -> Error {
    let mut e = e;
    if let Value::Object(m) = &mut e.details {
        m.insert("stem".into(), json!(stem));
        m.insert("script".into(), json!(name));
    }
    e
}

/// What `stems exec` / `stems env` add to the caller's environment for
/// `stem` (37): its own layers (never a base) with the ports of `ports`
/// (an `auto` port it has not allocated stays a `${stem...}` reference),
/// the outputs of `outputs`, `STEMS_STATE_DIR`; `run_id: None` (no daemon)
/// leaves out `STEMS_RUN_ID`. Returns the env and the names of variables
/// still holding a reference.
pub fn exec_env(
    ws: &Workspace,
    stem: &Stem,
    ports: &PortBook,
    outputs: Option<&OutputStore>,
    run_id: Option<&str>,
    state_dir: &std::path::Path,
) -> Result<(BTreeMap<String, String>, Vec<String>), Error> {
    exec_env_with(
        ws,
        stem,
        ports,
        outputs,
        run_id,
        state_dir,
        &BTreeMap::new(),
    )
}

/// [`exec_env`] with the `--pass-env` values `pass` as layer 6, applied
/// exactly like a start's ([`build_env`]): `STEMS_*` names included, under
/// the layer-7 markers.
fn exec_env_with(
    ws: &Workspace,
    stem: &Stem,
    ports: &PortBook,
    outputs: Option<&OutputStore>,
    run_id: Option<&str>,
    state_dir: &std::path::Path,
    pass: &BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, Vec<String>), Error> {
    let none = BTreeMap::new();
    let mut env = build_env(
        ws,
        stem,
        &EnvInputs {
            base: &none,
            pass_env: pass,
            run_id: run_id.unwrap_or_default(),
            ports,
            outputs,
            strict_outputs: false,
        },
        &mut Vec::new(),
    )?
    .own;
    if run_id.is_none() {
        env.remove("STEMS_RUN_ID");
    }
    env.insert("STEMS_STATE_DIR".into(), state_dir.display().to_string());
    let mut unresolved: Vec<String> = env
        .iter()
        .filter(|(_, v)| v.contains("${stem."))
        .map(|(k, _)| k.clone())
        .collect();
    // Port variables of an `auto` port nobody allocated yet are absent (42).
    for var in port_vars(ws, stem) {
        if !env.contains_key(&var) && !unresolved.contains(&var) {
            unresolved.push(var);
        }
    }
    unresolved.sort();
    Ok((env, unresolved))
}

/// Every port variable `build_env` sets for `stem` once its ports (and its
/// dependencies') are allocated: `PORT`, `PORT_<NAME>`, `STEMS_<DEP>_PORT`,
/// `STEMS_<DEP>_PORT_<NAME>`.
fn port_vars(ws: &Workspace, stem: &Stem) -> Vec<String> {
    let mut vars = Vec::new();
    if !stem.ports.is_empty() {
        vars.push("PORT".to_string());
    }
    for (i, port) in stem.ports.iter().enumerate() {
        if super::ports::is_named(i, port) {
            vars.push(super::ports::own_port_var(&port.name));
        }
    }
    for dep in &stem.depends_on {
        let Some(d) = ws.stem(&dep.stem) else {
            continue;
        };
        if !d.ports.is_empty() {
            vars.push(super::env::port_var(&dep.stem));
        }
        for (i, port) in d.ports.iter().enumerate() {
            if super::ports::is_named(i, port) {
                vars.push(super::ports::named_port_var(&dep.stem, &port.name));
            }
        }
    }
    vars
}

/// A stem's default working directory for `exec`: its codebase, else its
/// state directory.
pub fn exec_cwd(stem: &Stem, state_dir: &std::path::Path) -> std::path::PathBuf {
    match &stem.codebase {
        Some(cb) => cb.path().to_path_buf(),
        None => state_dir.to_path_buf(),
    }
}

/// A stem that does not run, as `requires_stopped:` wants it: `stopped`,
/// `failed` or `disabled` (49).
pub fn is_stopped(state: StemState) -> bool {
    matches!(
        state,
        StemState::Stopped | StemState::Failed | StemState::Disabled
    )
}

/// States in which a stem is still going through its start sequence.
fn in_start_sequence(state: StemState, pending: bool) -> bool {
    matches!(
        state,
        StemState::Setup | StemState::Starting | StemState::Seeding
    ) || (state == StemState::Healthy && pending)
}

/// Everything a run task needs, owned.
struct Job {
    core: Arc<Core>,
    ws: Arc<Resolved>,
    stem: Option<String>,
    name: String,
    script: Script,
    parsed: ParsedArgs,
    env: StemEnv,
    cwd_override: Option<std::path::PathBuf>,
    shell: Option<String>,
    actor: String,
    run_id: String,
    slot: Option<Arc<tokio::sync::Mutex<()>>>,
    cancel: CancellationToken,
}

impl Job {
    async fn run(self) -> Result<RunScriptResult, Error> {
        let t0 = Instant::now();
        let stem = self.stem.as_deref();
        let mut queued = false;
        let _slot = match &self.slot {
            None => None,
            Some(slot) => match slot.clone().try_lock_owned() {
                Ok(g) => Some(g),
                Err(_) => {
                    queued = true;
                    let mut ev =
                        EventDraft::new(EventKind::SCRIPT_QUEUED, &self.actor).data(json!({
                            "script": self.name,
                            "run_id": self.run_id,
                            "actor": self.actor,
                            "reason": "another script of the same stem is running",
                        }));
                    if let Some(s) = stem {
                        ev = ev.stem(s);
                    }
                    self.core.events.emit(ev);
                    tokio::select! {
                        g = slot.clone().lock_owned() => Some(g),
                        () = self.cancel.cancelled() => {
                            return Err(Error::new(
                                ErrorCode::ScriptFailed,
                                format!("script `{}` was cancelled while queued (daemon shutting down)", self.name),
                            )
                            .with_details(json!({ "stem": stem, "script": self.name, "reason": "cancelled" })));
                        }
                    }
                }
            },
        };
        let argv = self.parsed.to_argv();
        let backoff = retry_backoff();
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let mut extra = Map::new();
            extra.insert("run_id".into(), json!(self.run_id));
            extra.insert("attempt".into(), json!(attempt));
            let r = self
                .core
                .scripts
                .run(
                    &self.ws.workspace.root,
                    stem,
                    ScriptRef {
                        name: &self.name,
                        script: &self.script,
                    },
                    RunContext {
                        args: argv.clone(),
                        env: self.env.full.clone(),
                        own_env: Some(self.env.own.clone()),
                        extra_env: self.parsed.to_env(),
                        cwd_override: self.cwd_override.clone(),
                        shell: self.shell.clone(),
                        actor: self.actor.clone(),
                        cancel: Some(self.cancel.clone()),
                        event_extra: extra,
                        quiet: false,
                    },
                )
                .await?;
            let done = r.success() || r.cancelled || attempt > self.script.retries;
            if !done {
                let delay = stems_core::restart::backoff_delay(&backoff, attempt);
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = self.cancel.cancelled() => {}
                }
                if !self.cancel.is_cancelled() {
                    continue;
                }
            }
            let ok = r.success();
            return Ok(RunScriptResult {
                run_id: self.run_id.clone(),
                stem: self.stem.clone(),
                script: self.name.clone(),
                ok,
                exit: r.exit.code,
                signal: r.exit.signal,
                duration_ms: t0.elapsed().as_millis() as u64,
                timed_out: r.timed_out,
                attempts: attempt,
                argv,
                tail: r.tail.clone(),
                queued,
                // `soft_fail` softens the lifecycle, not an explicit run (41).
                soft: r.soft_failed(),
                error: (!ok).then(|| {
                    let mut e = r.error();
                    if let Value::Object(m) = &mut e.details {
                        m.insert("attempts".into(), json!(attempt));
                        m.insert("run_id".into(), json!(self.run_id));
                    }
                    e
                }),
            });
        }
    }
}

impl Supervisor {
    /// `stem_env` (37): `stem`'s context for `exec`/`shell`/`env`. Never
    /// allocates a port. Computed from the config and the env files as they
    /// are now (plus the `--pass-env` of the stem's last start), so a key
    /// removed from a file is gone. For a running stem, a key with no value
    /// now (a `${stem...}` reference left unresolved, a dependency port or
    /// output variable absent: the dependency is restarting or down) keeps
    /// the value the stem got at its start, per key ([`keep_live`]); a
    /// start-time environment whose config layers differ gets an
    /// `ENV_STALE` warning naming the keys ([`drift`]).
    ///
    /// [`keep_live`]: super::env::keep_live
    /// [`drift`]: super::env::drift
    pub fn stem_env(&self, p: &StemEnvParams, actor: &str) -> Result<StemEnvResult, Error> {
        let ws = self.workspace(false, actor)?;
        schedule::plan(&ws, std::slice::from_ref(&p.stem), true)?;
        let stem = ws
            .workspace
            .stem(&p.stem)
            .ok_or_else(|| Error::internal(format!("stem `{}` vanished", p.stem)))?;
        let state_dir = self.core.scripts.state_dir(Some(&stem.name));
        let _ = std::fs::create_dir_all(&state_dir);
        let (state, handle, live, pass) = {
            let cell = self.cell(&stem.name);
            let info = cell.info();
            (
                info.state,
                info.handle.clone(),
                info.env.clone(),
                info.restart.call_env().pass,
            )
        };
        let running = state.is_running();
        let mut env = self.config_env(&ws.workspace, stem, &pass, &state_dir)?;
        let mut warnings = Vec::new();
        if running && !live.is_empty() {
            let config = super::env::config_layers(
                &ws.workspace,
                stem,
                &pass,
                &mut super::env::parse_env_file,
            )?;
            let keys = super::env::drift(&ws.workspace, stem, &live, &config);
            if !keys.is_empty() {
                warnings.push(super::env::stale_warning(&stem.name, &keys));
            }
            super::env::keep_live(&ws.workspace, stem, &mut env, &live);
        }
        if !p.reveal {
            super::outputs::redact_env(&self.core, stem, &mut env);
        }
        let unresolved = env
            .iter()
            .filter(|(_, v)| v.contains("${stem."))
            .map(|(k, _)| k.clone())
            .collect();
        let container = match handle {
            Some(stems_runtime::Handle::Container { container_id, .. }) if running => {
                Some(container_id)
            }
            _ => None,
        };
        Ok(StemEnvResult {
            stem: stem.name.clone(),
            kind: stem.kind().to_string(),
            cwd: exec_cwd(stem, &state_dir),
            env,
            container,
            state,
            running,
            unresolved,
            warnings,
        })
    }

    /// What `stem`'s config yields now for `exec`/`env` (env files re-read):
    /// [`exec_env`] with the `--pass-env` values `pass` of its last start
    /// (layer 6, applied like the start applied them).
    fn config_env(
        &self,
        ws: &stems_config::Workspace,
        stem: &stems_config::Stem,
        pass: &BTreeMap<String, String>,
        state_dir: &std::path::Path,
    ) -> Result<BTreeMap<String, String>, Error> {
        let ports = self.core.ports.frozen();
        let (env, _) = exec_env_with(
            ws,
            stem,
            &ports,
            Some(&self.core.outputs),
            Some(&self.core.run_id),
            state_dir,
            pass,
        )?;
        Ok(env)
    }

    /// `ENV_STALE` keys of running `stem` for `status` ([`drift`]): where
    /// the config layers of the env it got at its start differ from its
    /// config, env files and `--pass-env` now. Dependency ports and outputs
    /// never count. Empty when it is not running, was adopted (its start env
    /// is unknown) or the env cannot be computed (a missing env file: the
    /// start would fail, not drift). Env files come from the
    /// [`EnvFileCache`](super::env::EnvFileCache) (one `stat` per file per
    /// call; re-read when the mtime or length changes): `status` is polled.
    ///
    /// [`drift`]: super::env::drift
    pub(crate) fn env_drift(
        &self,
        ws: &stems_config::Workspace,
        stem: &stems_config::Stem,
    ) -> Vec<String> {
        let (live, pass) = {
            let cell = self.cell(&stem.name);
            let info = cell.info();
            if !info.state.is_running() || info.env.is_empty() {
                return Vec::new();
            }
            (info.env.clone(), info.restart.call_env().pass)
        };
        let mut read = |r: &std::path::Path, s: &str, i: usize, f: &stems_config::EnvFile| {
            self.env_files.read(r, s, i, f)
        };
        match super::env::config_layers(ws, stem, &pass, &mut read) {
            Ok(config) => super::env::drift(ws, stem, &live, &config),
            Err(_) => Vec::new(),
        }
    }

    /// `script_catalog`.
    pub fn script_catalog(
        &self,
        p: ScriptCatalogParams,
        actor: &str,
    ) -> Result<ScriptCatalogResult, Error> {
        let ws = self.workspace(false, actor)?;
        catalog(&ws, p.stem.as_deref())
    }

    /// Stems of `requires` that are not `healthy`, with their state.
    fn unmet(&self, requires: &[String]) -> Vec<(String, StemState)> {
        requires
            .iter()
            .map(|r| (r.clone(), self.cell(r).state()))
            .filter(|(_, s)| *s != StemState::Healthy)
            .collect()
    }

    /// Stems of `requires_stopped` that run, with their state (49).
    fn unmet_stopped(&self, stopped: &[String]) -> Vec<(String, StemState)> {
        stopped
            .iter()
            .map(|r| (r.clone(), self.cell(r).state()))
            .filter(|(_, s)| !is_stopped(*s))
            .collect()
    }

    /// `requires_stopped:` of `p`'s script do not run (`stop_deps`: stopped
    /// first), then its `requires:` are healthy (`start_deps`: started
    /// first, in the caller's environment).
    async fn ensure_requires(
        &self,
        ws: &Resolved,
        p: &RunScriptParams,
        script: &Script,
        actor: &str,
    ) -> Result<(), Error> {
        let (stem, name) = (p.stem.as_deref(), p.name.as_str());
        let (requires, stopped) = (&script.requires, &script.requires_stopped);
        for list in [requires, stopped] {
            if !list.is_empty() {
                schedule::plan(ws, list, true)?;
            }
        }
        let mut unmet_stopped = self.unmet_stopped(stopped);
        let mut stop_error = None;
        if !unmet_stopped.is_empty() && p.stop_deps {
            let names: Vec<String> = unmet_stopped.iter().map(|(n, _)| n.clone()).collect();
            match self
                .stop(
                    StopParams {
                        stems: names,
                        ..StopParams::default()
                    },
                    actor,
                )
                .await
            {
                Ok(r) => stop_error = r.failed.into_iter().next().map(|f| f.error),
                Err(e) => stop_error = Some(e),
            }
            unmet_stopped = self.unmet_stopped(stopped);
        }
        let mut unmet = self.unmet(requires);
        let mut start_error = None;
        let mut restarted_stopped = false;
        if !unmet.is_empty() && p.start_deps && unmet_stopped.is_empty() {
            let names: Vec<String> = unmet.iter().map(|(n, _)| n.clone()).collect();
            match self
                .start(
                    StartParams {
                        stems: names,
                        caller_env: p.caller_env.clone(),
                        ..StartParams::default()
                    },
                    actor,
                )
                .await
            {
                Ok(r) => start_error = r.failed.into_iter().next().map(|f| f.error),
                Err(e) => start_error = Some(e),
            }
            unmet = self.unmet(requires);
            // A hard dependency of a `requires` stem may be one of the
            // `requires_stopped` ones: starting them started it again.
            unmet_stopped = self.unmet_stopped(stopped);
            restarted_stopped = !unmet_stopped.is_empty();
        }
        if unmet.is_empty() && unmet_stopped.is_empty() {
            return Ok(());
        }
        let what = match stem {
            Some(s) => format!("script `{name}` of `{s}`"),
            None => format!("workspace script `{name}`"),
        };
        let listed = |l: &[(String, StemState)]| {
            l.iter()
                .map(|(n, s)| format!("{n} ({s})"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let names = |l: &[(String, StemState)]| {
            l.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut msgs = Vec::new();
        let mut hints = Vec::new();
        if !unmet.is_empty() {
            msgs.push(format!("{what} requires healthy stems: {}", listed(&unmet)));
            hints.push(if p.start_deps {
                "they could not be started; see `stems status` and `stems logs`".to_string()
            } else {
                format!(
                    "start them with `stems up {}`, or rerun with `--start-deps`",
                    names(&unmet)
                )
            });
        }
        if !unmet_stopped.is_empty() {
            msgs.push(format!(
                "{what} requires stopped stems: {}",
                listed(&unmet_stopped)
            ));
            hints.push(if restarted_stopped {
                format!(
                    "`--start-deps` started them again as hard dependencies of the `requires` stems: `requires` and `requires_stopped` conflict ({})",
                    names(&unmet_stopped)
                )
            } else if p.stop_deps {
                "they could not be stopped; see `stems status` (running dependants need `stems stop --cascade`)".to_string()
            } else {
                format!(
                    "stop them with `stems stop {}`, or rerun with `--stop-deps`",
                    names(&unmet_stopped)
                )
            });
        }
        let pairs = |l: &[(String, StemState)]| {
            l.iter()
                .map(|(n, s)| json!({ "stem": n, "state": s }))
                .collect::<Vec<_>>()
        };
        Err(Error::new(ErrorCode::ScriptRequiresUnmet, msgs.join("; "))
            .with_hint(hints.join("; "))
            .with_details(json!({
                "stem": stem,
                "script": name,
                "requires": requires,
                "unmet": pairs(&unmet),
                "start_error": start_error,
                "requires_stopped": stopped,
                "unmet_stopped": pairs(&unmet_stopped),
                "stop_error": stop_error,
            })))
    }

    /// Wait while `stem` is in its start sequence (bounded by `bound`).
    async fn wait_ready(&self, stem: &str, bound: Duration) -> Result<(), Error> {
        let cell = self.cell(stem);
        let mut rx = cell.watch();
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let ph = *rx.borrow_and_update();
            if !in_start_sequence(ph.state, ph.pending) {
                return Ok(());
            }
            match tokio::time::timeout_at(deadline, rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Ok(()),
                Err(_) => {
                    return Err(Error::new(
                        ErrorCode::StartTimeout,
                        format!(
                            "`{stem}` is still {} after {:.1}s; the script was not run",
                            ph.state,
                            bound.as_secs_f64()
                        ),
                    )
                    .with_hint("wait longer with `--wait <duration>` (e.g. `--wait 2m`)")
                    .with_details(json!({
                        "stem": stem,
                        "state": ph.state,
                        "waited_ms": bound.as_millis() as u64,
                    })));
                }
            }
        }
    }

    /// `run_script`: returns [`RunScriptResult`] (or [`RunScriptAccepted`]
    /// with `wait: false`) as JSON.
    pub async fn run_script(&self, p: RunScriptParams, actor: &str) -> Result<Value, Error> {
        let ws = self.workspace_for(actor, p.caller_env.as_ref(), &[])?;
        let stem_name = p.stem.as_deref();
        let name = p.name.as_str();
        let (script, stem) = match stem_name {
            Some(s) => {
                schedule::plan(&ws, &[s.to_string()], true)?;
                let st = ws
                    .workspace
                    .stem(s)
                    .ok_or_else(|| Error::internal(format!("stem `{s}` vanished")))?;
                let script = st
                    .scripts
                    .get(name)
                    .ok_or_else(|| not_found(Some(s), name, st.scripts.keys().collect()))?;
                (script.clone(), Some(st.clone()))
            }
            None => {
                let script =
                    ws.workspace.scripts.get(name).ok_or_else(|| {
                        not_found(None, name, ws.workspace.scripts.keys().collect())
                    })?;
                (script.clone(), None)
            }
        };
        let parsed = match &p.args {
            ScriptArgsInput::Argv(argv) => parse_args(&script.args, argv),
            ScriptArgsInput::Object(obj) => parse_args_json(&script.args, obj),
        }
        .map_err(|e| with_context(e, stem_name, name))?;

        self.ensure_requires(&ws, &p, &script, actor).await?;
        // The caller's environment is the script's base layer (37).
        let call = CallEnv::new(p.caller_env.clone(), BTreeMap::new());
        if let Some(s) = stem_name {
            let bound = p
                .ready_timeout_ms
                .map_or(DEFAULT_READY_TIMEOUT, Duration::from_millis);
            self.wait_ready(s, bound).await?;
        }

        let (env, cwd_override, shell) = match &stem {
            Some(st) => {
                // What stems set at the stem's start (its `--pass-env`, the
                // outputs then) over this call's base, else built now.
                let launch_env = self
                    .cell(&st.name)
                    .info()
                    .launch
                    .as_ref()
                    .map(|l| l.env.clone());
                let env = match launch_env {
                    Some(e) if call.caller.is_some() => {
                        let mut full = call.base(&self.core.base_env).clone();
                        full.extend(e.own.clone());
                        StemEnv { own: e.own, full }
                    }
                    Some(e) => e,
                    None => stem_env_with(&self.core, &ws.workspace, st, &call, false)?,
                };
                let cwd_override = (st.codebase.is_none() && !script.cwd_set)
                    .then(|| self.core.scripts.state_dir(Some(&st.name)));
                let shell = match &st.runtime {
                    StemRuntime::Process(pr) => Some(pr.shell.clone()),
                    _ => None,
                };
                (env, cwd_override, shell)
            }
            None => (workspace_env(&self.core, &ws.workspace, &call), None, None),
        };
        let slot_key = stem_name.unwrap_or(WORKSPACE_LOG_STEM);
        let slot = (!script.concurrent).then(|| self.runs.slot(slot_key));
        let run_id = crate::state::new_run_id();
        let job = Job {
            core: self.core.clone(),
            ws: ws.clone(),
            stem: p.stem.clone(),
            name: p.name.clone(),
            script,
            parsed,
            env,
            cwd_override,
            shell,
            actor: actor.to_string(),
            run_id: run_id.clone(),
            slot,
            cancel: self.runs.cancel.child_token(),
        };
        let guard = self.runs.active.clone().read_owned().await;
        let task = tokio::spawn(async move {
            let r = job.run().await;
            drop(guard);
            r
        });
        if !p.wait {
            return serde_json::to_value(RunScriptAccepted {
                run_id,
                stem: p.stem,
                script: p.name,
            })
            .map_err(|e| Error::internal(e.to_string()));
        }
        let r = task
            .await
            .map_err(|e| Error::internal(format!("script task failed: {e}")))??;
        serde_json::to_value(r).map_err(|e| Error::internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_sequence_states() {
        assert!(in_start_sequence(StemState::Setup, false));
        assert!(in_start_sequence(StemState::Starting, false));
        assert!(in_start_sequence(StemState::Seeding, false));
        assert!(in_start_sequence(StemState::Healthy, true));
        assert!(!in_start_sequence(StemState::Healthy, false));
        assert!(!in_start_sequence(StemState::Stopped, false));
        assert!(!in_start_sequence(StemState::Failed, false));
    }

    #[test]
    fn retry_backoff_doubles_up_to_ten_seconds() {
        let b = retry_backoff();
        let d = |n| stems_core::restart::backoff_delay(&b, n);
        assert_eq!(d(1), Duration::from_millis(500));
        assert_eq!(d(2), Duration::from_secs(1));
        assert_eq!(d(3), Duration::from_secs(2));
        assert_eq!(d(10), Duration::from_secs(10));
    }
}
