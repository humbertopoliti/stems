//! Custom scripts (deliverable 17, `docs/scripts.md`): the `run_script` and
//! `script_catalog` methods.
//!
//! `run_script` runs any named script of a stem or of the workspace:
//!
//! 1. finds it (`UNKNOWN_STEM`, `SCRIPT_NOT_FOUND`);
//! 2. validates the arguments against its `args` schema
//!    ([`stems_core::scriptargs`], `SCRIPT_ARGS_INVALID`, exit 2);
//! 3. checks `requires:` — every listed stem must be `healthy`; with
//!    `start_deps` they are started first (like `stems start`), else
//!    `SCRIPT_REQUIRES_UNMET`;
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

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use stems_api::{
    EventKind, RunScriptAccepted, RunScriptParams, RunScriptResult, ScriptArgsInput,
    ScriptCatalogParams, ScriptCatalogResult, StartParams,
};
use stems_config::{Backoff, Resolved, Script, StemRuntime};
use stems_core::scriptargs::{ParsedArgs, build_catalog, parse_args, parse_args_json};
use stems_core::{Error, ErrorCode, StemState};
use tokio_util::sync::CancellationToken;

use super::hooks::{stem_env, workspace_env};
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
    env: BTreeMap<String, String>,
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
                        env: self.env.clone(),
                        extra_env: self.parsed.to_env(),
                        cwd_override: self.cwd_override.clone(),
                        shell: self.shell.clone(),
                        actor: self.actor.clone(),
                        cancel: Some(self.cancel.clone()),
                        event_extra: extra,
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

    async fn ensure_requires(
        &self,
        ws: &Resolved,
        stem: Option<&str>,
        name: &str,
        requires: &[String],
        start_deps: bool,
        actor: &str,
    ) -> Result<(), Error> {
        if !requires.is_empty() {
            schedule::plan(ws, requires, true)?;
        }
        let mut unmet = self.unmet(requires);
        let mut start_error = None;
        if !unmet.is_empty() && start_deps {
            let names: Vec<String> = unmet.iter().map(|(n, _)| n.clone()).collect();
            match self
                .start(
                    StartParams {
                        stems: names,
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
        }
        if unmet.is_empty() {
            return Ok(());
        }
        let what = match stem {
            Some(s) => format!("script `{name}` of `{s}`"),
            None => format!("workspace script `{name}`"),
        };
        let list: Vec<String> = unmet.iter().map(|(n, s)| format!("{n} ({s})")).collect();
        let names: Vec<&str> = unmet.iter().map(|(n, _)| n.as_str()).collect();
        let hint = if start_deps {
            "they could not be started; see `stems status` and `stems logs`".to_string()
        } else {
            format!(
                "start them with `stems up {}`, or rerun with `--start-deps`",
                names.join(" ")
            )
        };
        Err(Error::new(
            ErrorCode::ScriptRequiresUnmet,
            format!("{what} requires healthy stems: {}", list.join(", ")),
        )
        .with_hint(hint)
        .with_details(json!({
            "stem": stem,
            "script": name,
            "requires": requires,
            "unmet": unmet
                .iter()
                .map(|(n, s)| json!({ "stem": n, "state": s }))
                .collect::<Vec<_>>(),
            "start_error": start_error,
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
        let ws = self.workspace(true, actor)?;
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

        self.ensure_requires(&ws, stem_name, name, &script.requires, p.start_deps, actor)
            .await?;
        if let Some(s) = stem_name {
            let bound = p
                .ready_timeout_ms
                .map_or(DEFAULT_READY_TIMEOUT, Duration::from_millis);
            self.wait_ready(s, bound).await?;
        }

        let (env, cwd_override, shell) = match &stem {
            Some(st) => {
                let launch_env = self
                    .cell(&st.name)
                    .info()
                    .launch
                    .as_ref()
                    .map(|l| l.env.clone());
                let env = match launch_env {
                    Some(e) => e,
                    None => stem_env(&self.core, &ws.workspace, st, &BTreeMap::new())?,
                };
                let cwd_override = (st.codebase.is_none() && !script.cwd_set)
                    .then(|| self.core.scripts.state_dir(Some(&st.name)));
                let shell = match &st.runtime {
                    StemRuntime::Process(pr) => Some(pr.shell.clone()),
                    _ => None,
                };
                (env, cwd_override, shell)
            }
            None => (
                workspace_env(&self.core, &ws.workspace, &BTreeMap::new()),
                None,
                None,
            ),
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
