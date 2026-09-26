//! Restart policies and backoff (deliverable 22, `docs/restart.md`): what a
//! stem's actor does when its process/container exits on its own, when it
//! stays `unhealthy` too long (`restart.on_unhealthy`), and when a watchdog
//! (24) asks for a restart that bypasses the policy.
//!
//! The decision itself is [`stems_core::restart::RestartTracker`] (pure,
//! unit-tested there). This module applies it:
//!
//! ```text
//! exit (not asked for) ──▶ tracker.on_exit(exit, Crash)
//!    Restart{delay, attempt} ──▶ `starting` "restarting in <delay> (attempt n)"
//!                                 + `stem.restarting {attempt, delay_ms, exit_code}`
//!                                 ── delay (cancellable) ──▶ respawn:
//!                                 pre_start, start, wait, post_start
//!                                 (no setup, no seed; same ports, same overlays)
//!    GiveUp                  ──▶ `failed` MAX_RESTARTS + `stem.gave_up`
//!    Stop / Fail             ──▶ `stopped` / `failed` (policy `never`, clean exits)
//! ```
//!
//! A user `stop`/`down`/`restart` bumps the start generation *before* it
//! signals the process, so that exit never reaches [`on_exit`] (intent
//! `UserStop` is implicit), and it cancels a pending backoff timer. A user
//! start (`up`, `start`, `restart`) builds a fresh tracker: the window and
//! the backoff start over; the lifetime `restarts` count is kept.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use stems_api::EventKind;
use stems_config::{Resolved, Restart, RestartPolicy, Stem};
use stems_core::restart::{ExitInfo, Intent, RestartDecision, RestartTracker};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::{ExitStatus, Handle, Runtime};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::Core;
use super::actor::{Cmd, StemCell};
use super::waiter::exited_error;
use crate::events::EventDraft;

/// Per-stem restart bookkeeping, kept in the actor's `StemInfo`.
#[derive(Default)]
pub(crate) struct RestartState {
    /// The stem's `restart:` settings from the last user start.
    config: Option<Restart>,
    /// Window, backoff and totals since the last user start.
    tracker: Option<RestartTracker>,
    /// The workspace and `--pass-env` of the last user start (a respawn
    /// uses the same config, environment and ports).
    ws: Option<Arc<Resolved>>,
    pass_env: BTreeMap<String, String>,
    /// Cancels the pending backoff timer (a stop/down/shutdown).
    backoff: Option<CancellationToken>,
    /// Cancels the `unhealthy_grace` timer (the stem left `unhealthy`).
    unhealthy: Option<CancellationToken>,
    /// Attempt of the respawn in progress (for its reason text).
    attempt: u32,
    /// The current start is a respawn: hooks skip `setup` and `seed`.
    pub respawning: bool,
    /// `seeded` before the crash, restored after a respawn (the seed does
    /// not run again; its data is still there).
    pub was_seeded: bool,
}

impl RestartState {
    /// A user start: fresh tracker for `stem`'s settings (this is the
    /// counter reset of `stems restart`).
    pub(crate) fn begin(
        &mut self,
        ws: &Arc<Resolved>,
        stem: &Stem,
        pass_env: &BTreeMap<String, String>,
    ) {
        self.cancel_timers();
        self.config = Some(stem.restart.clone());
        self.tracker = Some(RestartTracker::new(&stem.restart));
        self.ws = Some(ws.clone());
        self.pass_env = pass_env.clone();
        self.respawning = false;
        self.attempt = 0;
    }

    /// A config reload was applied (33): policy restarts use the latest
    /// applied config from now on (its workspace and `restart:` settings).
    /// The window and backoff are kept unless the `restart:` settings
    /// changed (then they start over under the new limits).
    pub(crate) fn rebase(&mut self, ws: &Arc<Resolved>, stem: &Stem) {
        if self.ws.is_none() {
            return;
        }
        self.ws = Some(ws.clone());
        if self.config.as_ref() != Some(&stem.restart) {
            self.config = Some(stem.restart.clone());
            // New limits: a fresh window and backoff under them.
            if self.tracker.is_some() {
                self.tracker = Some(RestartTracker::new(&stem.restart));
            }
        }
    }

    /// The workspace policy restarts start from (33: the config a running
    /// stem runs with).
    pub(crate) fn workspace(&self) -> Option<&Arc<Resolved>> {
        self.ws.as_ref()
    }

    /// An adopted unit (crash recovery, `adopt_orphans`): the policy applies
    /// to it from now on (with no `--pass-env`).
    pub(crate) fn adopted(&mut self, ws: &Arc<Resolved>, stem: &Stem) {
        self.begin(ws, stem, &BTreeMap::new());
    }

    /// Restarts in the window now (`StemStatus.restarts_in_window`).
    pub(crate) fn in_window(&self, now: Instant) -> u32 {
        self.tracker
            .as_ref()
            .map_or(0, |t| t.restarts_in_window(now))
    }

    /// A backoff timer is pending: take and cancel it.
    pub(crate) fn cancel_backoff(&mut self) -> bool {
        match self.backoff.take() {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    /// No `on_unhealthy` timer is pending (tests).
    #[cfg(test)]
    pub(crate) fn unhealthy_timer_cancelled(&self) -> bool {
        self.unhealthy.is_none()
    }

    fn cancel_timers(&mut self) {
        self.cancel_backoff();
        if let Some(t) = self.unhealthy.take() {
            t.cancel();
        }
    }

    /// Called on every state transition: feeds the tracker's healthy
    /// uptime (the backoff resets after `window` of it) and arms/disarms the
    /// `on_unhealthy` timer.
    pub(crate) fn observe(
        &mut self,
        to: StemState,
        generation: u64,
        tx: &mpsc::UnboundedSender<Cmd>,
    ) {
        let now = Instant::now();
        if let Some(t) = self.tracker.as_mut() {
            if matches!(to, StemState::Healthy | StemState::Seeding) {
                t.on_healthy(now);
            } else {
                t.on_unhealthy();
            }
        }
        if to != StemState::Unhealthy {
            if let Some(t) = self.unhealthy.take() {
                t.cancel();
            }
            return;
        }
        let Some(cfg) = self
            .config
            .as_ref()
            .filter(|c| c.on_unhealthy && c.policy != RestartPolicy::Never)
        else {
            return;
        };
        if self.ws.is_none() || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let grace = cfg.unhealthy_grace.as_duration();
        let token = CancellationToken::new();
        if let Some(old) = self.unhealthy.replace(token.clone()) {
            old.cancel();
        }
        let tx = tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(grace) => {
                    let _ = tx.send(Cmd::UnhealthyGrace { generation });
                }
                () = token.cancelled() => {}
            }
        });
    }
}

/// Why a restart happens (`stem.restarting` `data.reason`).
#[derive(Clone, Debug)]
pub(crate) enum Cause {
    /// The process/container exited on its own.
    Exit(ExitStatus),
    /// `unhealthy` for longer than `restart.unhealthy_grace`.
    Unhealthy,
    /// [`StemCell::restart_bypassing_policy`] (watchdogs, 24).
    Bypass(String),
}

impl Cause {
    fn data(&self) -> Value {
        match self {
            Cause::Exit(s) => json!({ "reason": "exit", "exit_code": s.code, "signal": s.signal }),
            Cause::Unhealthy => json!({ "reason": "unhealthy" }),
            Cause::Bypass(why) => json!({ "reason": why }),
        }
    }
}

/// `500ms`, `1s`, `1.5s`, `30s`.
pub(crate) fn fmt_delay(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn how(status: ExitStatus) -> String {
    match (status.code, status.signal) {
        (Some(c), _) => format!("exited with code {c}"),
        (None, Some(s)) => format!("killed by signal {s}"),
        _ => "exited".into(),
    }
}

/// The process of a running stem exited without being asked to (the
/// actor already emitted `process.exited` and bumped the generation).
/// `state` is the state it was in; `unit` its handle, to release.
pub(crate) async fn on_exit(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    state: StemState,
    status: ExitStatus,
    unit: Option<(Handle, Arc<dyn Runtime>)>,
) {
    let actor = stems_api::DAEMON_ACTOR;
    let exit = ExitInfo {
        code: status.code,
        signal: status.signal,
    };
    let decision = {
        let mut info = cell.info();
        let first_start = state == StemState::Starting && !info.restart.respawning;
        let now = Instant::now();
        let d = match (
            first_start,
            info.restart.ws.is_some(),
            info.restart.tracker.as_mut(),
        ) {
            // A user start that dies before it is ready fails the start
            // (`up` reports START_FAILED); the policy is for running stems.
            (true, _, _) => None,
            (false, true, Some(t)) => Some(t.on_exit(exit, Intent::Crash, now)),
            // No workspace to restart from: as policy `never`.
            _ => Some(if exit.success() {
                RestartDecision::Stop
            } else {
                RestartDecision::Fail
            }),
        };
        if matches!(d, Some(RestartDecision::Restart { .. })) {
            info.restarts = info.restarts.saturating_add(1);
            info.restart.was_seeded = info.seeded;
        }
        d
    };
    let restarting = matches!(decision, Some(RestartDecision::Restart { .. }));
    if let Some((h, rt)) = unit {
        if restarting {
            // Docker keeps the exited container for `docker restart` (14).
            super::containers::after_stop(core, &cell.name, &h, "restart").await;
        }
        rt.release(&h);
    }
    let data = json!({ "exit_code": status.code, "signal": status.signal });
    match decision {
        Some(RestartDecision::Restart { delay, attempt }) => {
            schedule(core, cell, delay, attempt, true, Cause::Exit(status), actor);
        }
        Some(RestartDecision::GiveUp) => give_up(core, cell, &Cause::Exit(status)),
        None => cell.fail(core, exited_error(&cell.name, status), actor),
        // A respawn that dies before it is ready and is not restarted.
        Some(RestartDecision::Stop | RestartDecision::Fail) if state == StemState::Starting => {
            cell.fail(core, exited_error(&cell.name, status), actor);
        }
        Some(RestartDecision::Stop) => {
            cell.transition(core, StemState::Stopped, how(status), actor, data);
        }
        Some(RestartDecision::Fail) => {
            cell.transition(core, StemState::Failed, how(status), actor, data);
        }
    }
}

/// Go `starting` ("restarting in …"), emit `stem.restarting` and arm the
/// cancellable backoff timer that sends [`Cmd::Respawn`].
fn schedule(
    core: &Core,
    cell: &Arc<StemCell>,
    delay: Duration,
    attempt: u32,
    counted: bool,
    cause: Cause,
    actor: &str,
) {
    let delay_ms = delay.as_millis() as u64;
    let reason = match &cause {
        Cause::Bypass(why) => format!("restarting ({why})"),
        _ => format!("restarting in {} (attempt {attempt})", fmt_delay(delay)),
    };
    let token = CancellationToken::new();
    let generation = {
        let mut info = cell.info();
        info.restart.cancel_backoff();
        info.restart.backoff = Some(token.clone());
        info.restart.attempt = attempt;
        info.generation
    };
    let mut data = cause.data();
    data["attempt"] = json!(attempt);
    data["delay_ms"] = json!(delay_ms);
    data["counted"] = json!(counted);
    // `starting` first, so whoever sees the event sees the new state too.
    cell.transition(
        core,
        StemState::Starting,
        reason.clone(),
        actor,
        json!({ "attempt": attempt, "delay_ms": delay_ms }),
    );
    core.events.emit(
        EventDraft::new(EventKind::STEM_RESTARTING, actor)
            .stem(&cell.name)
            .reason(reason)
            .data(data),
    );
    let c = cell.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = tokio::time::sleep(delay) => c.send(Cmd::Respawn { generation }),
            () = token.cancelled() => {}
        }
    });
}

/// `failed` with `MAX_RESTARTS`, event `stem.gave_up`.
fn give_up(core: &Core, cell: &StemCell, cause: &Cause) {
    let actor = stems_api::DAEMON_ACTOR;
    let (attempts, max, window) = {
        let info = cell.info();
        let (max, window) = info
            .restart
            .config
            .as_ref()
            .map_or((0, Duration::ZERO), |c| (c.max, c.window.as_duration()));
        (info.restart.in_window(Instant::now()), max, window)
    };
    let window_ms = window.as_millis() as u64;
    let name = &cell.name;
    let mut details = cause.data();
    details["stem"] = json!(name);
    details["attempts"] = json!(attempts);
    details["max"] = json!(max);
    details["window_ms"] = json!(window_ms);
    let e = Error::new(
        ErrorCode::MaxRestarts,
        format!(
            "`{name}` was restarted {attempts} times within {} and failed again: giving up (restart.max = {max})",
            fmt_delay(window)
        ),
    )
    .with_hint(format!(
        "check `stems logs {name}`, fix the crash and run `stems restart {name}`; or raise `restart.max`/`restart.window`"
    ))
    .with_details(details.clone());
    core.events.emit(
        EventDraft::new(EventKind::STEM_GAVE_UP, actor)
            .stem(name)
            .reason(e.message.clone())
            .data(details),
    );
    cell.fail(core, e, actor);
}

/// The backoff elapsed: start the stem again with the config, environment
/// and ports of its last user start. `pre_start`/`post_start` run;
/// `setup`/`seed` do not (their stamps are intact).
pub(crate) async fn respawn(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64) {
    let actor = stems_api::DAEMON_ACTOR;
    let (ws, pass_env, attempt) = {
        let mut info = cell.info();
        if info.generation != generation || info.state != StemState::Starting {
            return;
        }
        match info.restart.backoff.take() {
            Some(t) if !t.is_cancelled() => {}
            _ => return,
        }
        let Some(ws) = info.restart.ws.clone() else {
            return;
        };
        info.restart.respawning = true;
        (ws, info.restart.pass_env.clone(), info.restart.attempt)
    };
    let Some(stem) = ws.workspace.stem(&cell.name).cloned() else {
        let e = Error::internal(format!("`{}` vanished from the workspace", cell.name));
        cell.fail(core, e, actor);
        return;
    };
    cell.info().reason = Some(if attempt == 0 {
        "restarting".into()
    } else {
        format!("restarting (attempt {attempt})")
    });
    if super::hooks::before_start(core, cell, &ws, &stem, actor, &pass_env)
        .await
        .is_err()
    {
        // A failed pre_start already failed the stem; a stop during it
        // leaves it here, still `starting`.
        if cell.state() == StemState::Starting {
            cell.transition(core, StemState::Stopping, "stopped", actor, json!({}));
            cell.transition(core, StemState::Stopped, "stopped", actor, json!({}));
        }
        return;
    }
    if let Err(e) = super::actor::spawn(core, cell, &ws, &stem, &pass_env).await {
        cell.fail(core, e, actor);
    }
}

/// Stop the running unit for a restart: stop hooks and the stop script
/// run, overlays and (docker) the container are kept, ports stay allocated.
async fn stop_for_restart(core: &Arc<Core>, cell: &Arc<StemCell>, actor: &str) {
    cell.bump_generation();
    let Some((h, rt)) = cell.clear_process() else {
        return;
    };
    let grace = cell.info().grace;
    super::hooks::pre_stop(core, cell, actor).await;
    if let Err(e) = super::hooks::stop_unit(core, cell, &h, &rt, grace, actor).await {
        tracing::warn!(stem = %cell.name, error = %e, "stopping for a restart failed");
    }
    super::containers::after_stop(core, &cell.name, &h, "restart").await;
    rt.release(&h);
    super::hooks::post_stop(core, cell, actor).await;
}

/// `restart.on_unhealthy`: the stem stayed `unhealthy` for
/// `unhealthy_grace`. Counts against the policy like a crash.
pub(crate) async fn on_unhealthy_grace(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64) {
    let decision = {
        let mut info = cell.info();
        if info.generation != generation || info.state != StemState::Unhealthy {
            return;
        }
        match info.restart.unhealthy.take() {
            Some(t) if !t.is_cancelled() => {}
            _ => return,
        }
        let now = Instant::now();
        // Unhealthy counts as a failure (no exit code).
        let Some(d) = info
            .restart
            .tracker
            .as_mut()
            .map(|t| t.on_exit(ExitInfo::default(), Intent::Crash, now))
        else {
            return;
        };
        if matches!(d, RestartDecision::Restart { .. }) {
            info.restarts = info.restarts.saturating_add(1);
        }
        info.restart.was_seeded = info.seeded;
        d
    };
    let actor = stems_api::DAEMON_ACTOR;
    match decision {
        RestartDecision::Restart { delay, attempt } => {
            stop_for_restart(core, cell, actor).await;
            schedule(core, cell, delay, attempt, true, Cause::Unhealthy, actor);
        }
        RestartDecision::GiveUp => {
            stop_for_restart(core, cell, actor).await;
            give_up(core, cell, &Cause::Unhealthy);
        }
        // Policy `never` never arms the timer.
        RestartDecision::Stop | RestartDecision::Fail => {}
    }
}

/// A restart that bypasses the policy (watchdogs, 24): not counted, no
/// backoff. Refused unless the stem runs (or waits for a restart).
pub(crate) async fn bypass(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    actor: &str,
    why: &str,
) -> Result<(), Error> {
    let (state, has_ws) = {
        let info = cell.info();
        (info.state, info.restart.ws.is_some())
    };
    if !matches!(
        state,
        StemState::Starting | StemState::Healthy | StemState::Unhealthy
    ) || !has_ws
    {
        return Err(Error::usage(
            format!(
                "`{}` is not running ({state}): nothing to restart",
                cell.name
            ),
            format!("start it with `stems start {}`", cell.name),
        )
        .with_details(json!({ "stem": cell.name, "state": state })));
    }
    {
        let mut info = cell.info();
        info.restart.was_seeded = info.seeded;
    }
    stop_for_restart(core, cell, actor).await;
    schedule(
        core,
        cell,
        Duration::ZERO,
        0,
        false,
        Cause::Bypass(why.to_string()),
        actor,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_read_well() {
        assert_eq!(fmt_delay(Duration::from_millis(500)), "500ms");
        assert_eq!(fmt_delay(Duration::from_secs(1)), "1s");
        assert_eq!(fmt_delay(Duration::from_millis(1500)), "1.5s");
        assert_eq!(fmt_delay(Duration::from_secs(600)), "600s");
        assert_eq!(fmt_delay(Duration::ZERO), "0ms");
    }
}
