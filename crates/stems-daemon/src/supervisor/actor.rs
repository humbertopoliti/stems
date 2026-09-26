//! One actor task per stem. It owns the stem's runtime [`Handle`] and is the
//! only writer of its state; the supervisor talks to it through [`Cmd`]s.
//! Readers (status, the scheduler) look at [`StemCell`] (a mutex-guarded
//! snapshot plus a `watch` of the [`Phase`]).
//!
//! Per start the actor spawns two helper tasks tagged with the start's
//! *generation*: an exit watcher (`runtime.wait`) and the readiness wait
//! ([`Waiter`]). Their results come back as [`Cmd::Exited`] /
//! [`Cmd::Ready`]; a result from an older generation is ignored, so a stop
//! or restart never races a stale exit. Deliverable 22's restart loop and
//! 24's watchdog triggers belong here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use stems_api::{EventKind, PortStatus, StemStatus};
use stems_config::{
    Condition, Health, HealthPort, Resolved, ScriptSource, Stem, StemRuntime, StemType,
};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::{ExitStatus, Handle, ProcessSpec, Runtime, StartSpec};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use super::Core;
use super::env::{EnvInputs, build_env, render_refs};
use super::ports::check_free;
use super::state;
use super::waiter::{WaitTarget, exited_error};
use crate::events::EventDraft;

/// What the watch channel carries: enough for waiters to decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phase {
    /// Current state.
    pub state: StemState,
    /// Start generation (bumped by every start and stop).
    pub generation: u64,
}

/// Messages to a stem's actor.
pub(crate) enum Cmd {
    /// Start (no-op if running). Replies once spawned (state `starting`) or failed.
    Start {
        ws: Arc<Resolved>,
        actor: String,
        reason: String,
        pass_env: BTreeMap<String, String>,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    /// Stop (SIGTERM → grace → SIGKILL). Replies when stopped.
    Stop {
        grace: Duration,
        actor: String,
        reason: String,
        reply: oneshot::Sender<StopReply>,
    },
    /// The process of `generation` exited.
    Exited { generation: u64, status: ExitStatus },
    /// The readiness wait of `generation` finished.
    Ready {
        generation: u64,
        result: Result<(), Error>,
    },
}

/// Outcome of a stop.
#[derive(Debug)]
pub enum StopReply {
    /// Was running, now stopped.
    Stopped,
    /// Nothing was running (or external).
    NotRunning,
    /// The stop failed.
    Failed(Error),
}

/// Mutable facts about a stem (behind the cell's mutex).
pub(crate) struct StemInfo {
    pub state: StemState,
    pub reason: Option<String>,
    pub handle: Option<Handle>,
    pub runtime: Option<Arc<dyn Runtime>>,
    pub ports: Vec<(String, u16)>,
    pub started_at: Option<(Instant, DateTime<Utc>)>,
    pub restarts: u32,
    pub env: BTreeMap<String, String>,
    pub error: Option<Error>,
    pub generation: u64,
    pub grace: Duration,
    ready_task: Option<AbortHandle>,
}

/// A stem as seen by the supervisor: shared snapshot + actor mailbox.
pub struct StemCell {
    /// Stem name.
    pub name: String,
    info: Mutex<StemInfo>,
    phase: watch::Sender<Phase>,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl StemCell {
    /// A new cell (state `stopped`) and its actor task.
    pub(crate) fn spawn(core: Arc<Core>, name: &str) -> Arc<StemCell> {
        let (tx, rx) = mpsc::unbounded_channel();
        let cell = Arc::new(StemCell {
            name: name.to_string(),
            info: Mutex::new(StemInfo {
                state: StemState::Stopped,
                reason: None,
                handle: None,
                runtime: None,
                ports: Vec::new(),
                started_at: None,
                restarts: 0,
                env: BTreeMap::new(),
                error: None,
                generation: 0,
                grace: Duration::from_secs(10),
                ready_task: None,
            }),
            phase: watch::Sender::new(Phase {
                state: StemState::Stopped,
                generation: 0,
            }),
            tx,
        });
        tokio::spawn(run(core, cell.clone(), rx));
        cell
    }

    pub(crate) fn info(&self) -> MutexGuard<'_, StemInfo> {
        self.info.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current state.
    pub fn state(&self) -> StemState {
        self.info().state
    }

    /// Last error.
    pub fn error(&self) -> Option<Error> {
        self.info().error.clone()
    }

    /// Subscribe to phase changes.
    pub fn watch(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    /// Ask the actor to start the stem.
    pub(crate) async fn start(
        &self,
        ws: Arc<Resolved>,
        actor: &str,
        reason: &str,
        pass_env: BTreeMap<String, String>,
    ) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Start {
            ws,
            actor: actor.into(),
            reason: reason.into(),
            pass_env,
            reply,
        });
        rx.await
            .unwrap_or_else(|_| Err(Error::internal("stem actor went away")))
    }

    /// Ask the actor to stop the stem.
    pub(crate) async fn stop(&self, grace: Duration, actor: &str, reason: &str) -> StopReply {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Stop {
            grace,
            actor: actor.into(),
            reason: reason.into(),
            reply,
        });
        rx.await
            .unwrap_or_else(|_| StopReply::Failed(Error::internal("stem actor went away")))
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Move to `to`, emitting `stem.state`. Illegal transitions (a bug) are
    /// logged and refused.
    fn transition(
        &self,
        core: &Core,
        to: StemState,
        reason: impl Into<String>,
        actor: &str,
        data: Value,
    ) -> bool {
        let reason = reason.into();
        let (from, generation, pid) = {
            let mut info = self.info();
            let from = info.state;
            if from == to {
                info.reason = Some(reason);
                return false;
            }
            if !state::allowed(from, to) {
                tracing::error!(stem = %self.name, %from, %to, "illegal state transition refused");
                return false;
            }
            info.state = to;
            info.reason = Some(reason.clone());
            (from, info.generation, info.handle.as_ref().map(Handle::pid))
        };
        let mut data = match data {
            Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        data.entry("pid").or_insert(json!(pid));
        core.events.emit(
            EventDraft::new(EventKind::STEM_STATE, actor)
                .stem(&self.name)
                .transition(from.as_str(), to.as_str())
                .reason(reason)
                .data(Value::Object(data)),
        );
        self.phase.send_replace(Phase {
            state: to,
            generation,
        });
        true
    }

    fn bump_generation(&self) -> u64 {
        let mut info = self.info();
        info.generation += 1;
        if let Some(t) = info.ready_task.take() {
            t.abort();
        }
        info.generation
    }

    fn fail(&self, core: &Core, e: Error, actor: &str) {
        self.info().error = Some(e.clone());
        let data = json!({ "error": e });
        self.transition(core, StemState::Failed, e.message.clone(), actor, data);
    }

    fn clear_process(&self) -> Option<(Handle, Arc<dyn Runtime>)> {
        let mut info = self.info();
        info.started_at = None;
        match (info.handle.take(), info.runtime.take()) {
            (Some(h), Some(r)) => Some((h, r)),
            _ => None,
        }
    }

    /// The `status` view of this stem.
    pub fn status(&self, stem: &Stem, core: &Core, verbose: bool) -> StemStatus {
        let info = self.info();
        let running = info.handle.is_some();
        let ports = stem
            .ports
            .iter()
            .map(|p| {
                let auto = matches!(p.port, stems_config::PortRef::Auto);
                let port = info
                    .ports
                    .iter()
                    .find(|(n, _)| *n == p.name)
                    .map(|(_, v)| *v)
                    .or_else(|| match p.port {
                        stems_config::PortRef::Fixed(n) => Some(n),
                        stems_config::PortRef::Auto => core.ports.allocated(&stem.name, &p.name),
                    });
                PortStatus {
                    name: p.name.clone(),
                    port,
                    auto,
                }
            })
            .collect();
        StemStatus {
            name: stem.name.clone(),
            kind: stem.kind().to_string(),
            state: info.state,
            glyph: info.state.glyph(false),
            reason: info.reason.clone(),
            pid: info.handle.as_ref().map(Handle::pid),
            pgid: info.handle.as_ref().map(Handle::pgid),
            ports,
            uptime_s: info.started_at.map(|(i, _)| i.elapsed().as_secs()),
            started_at: info.started_at.map(|(_, t)| t),
            restarts: info.restarts,
            health: None,
            error: info.error.clone(),
            env: (verbose && running).then(|| info.env.clone()),
        }
    }
}

/// The actor loop.
async fn run(core: Arc<Core>, cell: Arc<StemCell>, mut rx: mpsc::UnboundedReceiver<Cmd>) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Start {
                ws,
                actor,
                reason,
                pass_env,
                reply,
            } => {
                let r = start(&core, &cell, &ws, &actor, &reason, &pass_env).await;
                let _ = reply.send(r);
            }
            Cmd::Stop {
                grace,
                actor,
                reason,
                reply,
            } => {
                let r = stop(&core, &cell, grace, &actor, &reason).await;
                let _ = reply.send(r);
            }
            Cmd::Exited { generation, status } => on_exit(&core, &cell, generation, status).await,
            Cmd::Ready { generation, result } => on_ready(&core, &cell, generation, result).await,
        }
    }
}

async fn start(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    ws: &Arc<Resolved>,
    actor: &str,
    reason: &str,
    pass_env: &BTreeMap<String, String>,
) -> Result<(), Error> {
    let Some(stem) = ws.workspace.stem(&cell.name) else {
        return Err(Error::new(
            ErrorCode::UnknownStem,
            format!("stem `{}` does not exist", cell.name),
        ));
    };
    let current = cell.state();
    if stem.kind() == StemType::External {
        if current == StemState::Stopped {
            cell.transition(
                core,
                StemState::Unknown,
                "external: not managed by stems (no health probe yet)",
                actor,
                json!({}),
            );
        }
        return Ok(());
    }
    if current.is_running() {
        return Ok(());
    }
    cell.info().grace = stem.stop_grace.as_duration();
    cell.transition(core, StemState::Starting, reason, actor, json!({}));
    match spawn(core, cell, ws, stem, pass_env).await {
        Ok(()) => Ok(()),
        Err(e) => {
            cell.fail(core, e.clone(), actor);
            Err(e)
        }
    }
}

/// The start command of a process stem: `command`, else `scripts.start`.
fn start_script(stem: &Stem) -> Result<(String, String, std::path::PathBuf), Error> {
    let StemRuntime::Process(p) = &stem.runtime else {
        return Err(Error::internal(format!(
            "`{}` is not a process stem",
            stem.name
        )));
    };
    let script = match (&p.command, stem.scripts.get("start").map(|s| &s.source)) {
        (Some(c), _) => c.clone(),
        (None, Some(ScriptSource::Command(c))) => c.clone(),
        (None, Some(ScriptSource::File(f))) => {
            format!("exec '{}'", f.display().to_string().replace('\'', "'\\''"))
        }
        (None, None) => {
            return Err(Error::new(
                ErrorCode::StartFailed,
                format!("`{}` has no `command` and no `scripts.start`", stem.name),
            )
            .with_hint(format!(
                "add `command:` or `scripts.start` to `stems.{}`",
                stem.name
            )));
        }
    };
    Ok((p.shell.clone(), script, p.cwd.clone()))
}

fn port_of_url(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let (_, port) = authority.rsplit_once(':')?;
    port.parse().ok()
}

async fn spawn(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    ws: &Arc<Resolved>,
    stem: &Stem,
    pass_env: &BTreeMap<String, String>,
) -> Result<(), Error> {
    let runtime = core.runtimes.get(stem.kind())?;
    let workspace = &ws.workspace;
    let mut allocated = Vec::new();

    // Ports: fixed ones must be free; auto ones are allocated (sticky).
    let mut ports = Vec::new();
    for p in &stem.ports {
        let hp = core.ports.host_port(&stem.name, p)?;
        if hp.newly_allocated {
            allocated.push((stem.name.clone(), p.name.clone(), hp.port));
        }
        ports.push((p.name.clone(), hp.port));
    }
    let checks = ports.clone();
    let name = stem.name.clone();
    tokio::task::spawn_blocking(move || {
        checks
            .iter()
            .try_for_each(|(n, port)| check_free(&name, n, *port))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))??;

    let env = build_env(
        workspace,
        stem,
        &EnvInputs {
            base: &core.base_env,
            pass_env,
            run_id: &core.run_id,
            ports: &core.ports,
        },
        &mut allocated,
    )?;
    let mut lookup =
        |n: &str, p: Option<&str>| core.ports.resolve_ref(workspace, n, p, &mut allocated);
    let health: Option<Health> = stem.health.clone().map(|mut h| {
        h.url = h.url.map(|u| render_refs(&u, &stem.name, &mut lookup));
        if let Some(HealthPort::Deferred(s)) = &h.port {
            let r = render_refs(s, &stem.name, &mut lookup);
            h.port = Some(match r.trim().parse() {
                Ok(n) => HealthPort::Number(n),
                Err(_) => HealthPort::Deferred(r),
            });
        }
        h
    });
    for (s, n, p) in &allocated {
        core.events.emit(
            EventDraft::new(EventKind::STEM_PORT_ALLOCATED, stems_api::DAEMON_ACTOR)
                .stem(s)
                .data(json!({ "port": p, "name": n })),
        );
    }
    let probe_port = health
        .as_ref()
        .and_then(|h| match &h.port {
            Some(HealthPort::Number(n)) => Some(*n),
            _ => h.url.as_deref().and_then(port_of_url),
        })
        .or_else(|| ports.first().map(|(_, p)| *p));

    let (shell, script, cwd) = start_script(stem)?;
    let spec = StartSpec::Process(ProcessSpec {
        command: shell,
        args: vec!["-c".into(), script],
        shell: false,
        cwd,
        env: env.full,
        clear_env: true,
    });
    let handle = runtime.start(&spec).await.map_err(|e| {
        Error::new(
            ErrorCode::StartFailed,
            format!("cannot start `{}`: {e}", stem.name),
        )
        .with_hint("check the stem's `command`/`scripts.start`, `shell` and `cwd`")
        .with_details(json!({ "stem": stem.name }))
    })?;

    let generation = {
        let mut info = cell.info();
        info.generation += 1;
        info.handle = Some(handle.clone());
        info.runtime = Some(runtime.clone());
        info.ports = ports;
        info.started_at = Some((Instant::now(), Utc::now()));
        info.env = env.own;
        info.error = None;
        info.generation
    };
    cell.phase.send_replace(Phase {
        state: StemState::Starting,
        generation,
    });
    core.sink.attach(&stem.name, runtime.output_stream(&handle));

    // Exit watcher.
    let (tx, rt, h) = (cell.tx.clone(), runtime.clone(), handle.clone());
    tokio::spawn(async move {
        let status = rt.wait(&h).await.unwrap_or(ExitStatus::UNKNOWN);
        let _ = tx.send(Cmd::Exited { generation, status });
    });
    // Readiness.
    let target = WaitTarget {
        stem: stem.name.clone(),
        runtime,
        handle,
        health,
        probe_port,
    };
    let (tx, waiter) = (cell.tx.clone(), core.waiter.clone());
    let task = tokio::spawn(async move {
        let result = waiter.wait_condition(&target, Condition::Healthy).await;
        let _ = tx.send(Cmd::Ready { generation, result });
    });
    cell.info().ready_task = Some(task.abort_handle());
    Ok(())
}

async fn on_ready(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64, r: Result<(), Error>) {
    {
        let info = cell.info();
        if info.generation != generation || info.state != StemState::Starting {
            return;
        }
    }
    cell.info().ready_task = None;
    match r {
        Ok(()) => {
            cell.transition(
                core,
                StemState::Healthy,
                "ready (process alive for start_period; probes land in 21)",
                stems_api::DAEMON_ACTOR,
                json!({}),
            );
        }
        Err(e) => {
            cell.bump_generation();
            if let Some((h, rt)) = cell.clear_process() {
                let grace = cell.info().grace;
                if let Err(err) = rt.stop(&h, grace).await {
                    tracing::warn!(stem = %cell.name, error = %err, "stopping a stem that failed to become ready");
                }
                rt.release(&h);
            }
            cell.fail(core, e, stems_api::DAEMON_ACTOR);
        }
    }
}

async fn on_exit(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64, status: ExitStatus) {
    let (state, pid) = {
        let info = cell.info();
        if info.generation != generation {
            return;
        }
        (info.state, info.handle.as_ref().map(Handle::pid))
    };
    if !state.is_running() || state == StemState::Stopping {
        return;
    }
    core.events.emit(
        EventDraft::new(EventKind::PROCESS_EXITED, stems_api::DAEMON_ACTOR)
            .stem(&cell.name)
            .data(json!({ "pid": pid, "code": status.code, "signal": status.signal })),
    );
    cell.bump_generation();
    if let Some((h, rt)) = cell.clear_process() {
        rt.release(&h);
    }
    let actor = stems_api::DAEMON_ACTOR;
    if state == StemState::Starting {
        cell.fail(core, exited_error(&cell.name, status), actor);
        return;
    }
    // Deliverable 22 decides about restarts here.
    let how = match (status.code, status.signal) {
        (Some(c), _) => format!("exited with code {c}"),
        (None, Some(s)) => format!("killed by signal {s}"),
        _ => "exited".into(),
    };
    let data = json!({ "exit_code": status.code, "signal": status.signal });
    if status.success() {
        cell.transition(core, StemState::Stopped, how, actor, data);
    } else {
        cell.transition(core, StemState::Failed, how, actor, data);
    }
}

async fn stop(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    grace: Duration,
    actor: &str,
    reason: &str,
) -> StopReply {
    let state = cell.state();
    let running = cell.info().handle.is_some();
    if !running {
        if state == StemState::Failed {
            cell.transition(core, StemState::Stopped, reason, actor, json!({}));
        }
        return StopReply::NotRunning;
    }
    cell.bump_generation();
    cell.transition(core, StemState::Stopping, reason, actor, json!({}));
    let Some((h, rt)) = cell.clear_process() else {
        cell.transition(core, StemState::Stopped, reason, actor, json!({}));
        return StopReply::Stopped;
    };
    let pid = h.pid();
    let outcome = rt.stop(&h, grace).await;
    rt.release(&h);
    match outcome {
        Ok(o) => {
            cell.transition(
                core,
                StemState::Stopped,
                reason,
                actor,
                json!({ "pid": pid, "outcome": o }),
            );
            StopReply::Stopped
        }
        Err(e) => {
            let err = Error::internal(format!("stopping `{}` failed: {e}", cell.name));
            cell.fail(core, err.clone(), actor);
            StopReply::Failed(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_ports() {
        assert_eq!(port_of_url("http://localhost:8080/healthz"), Some(8080));
        assert_eq!(port_of_url("http://[::1]:81/x"), Some(81));
        assert_eq!(port_of_url("https://httpbin.org/get"), None);
        assert_eq!(port_of_url("localhost:9"), Some(9));
    }
}
