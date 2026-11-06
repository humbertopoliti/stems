//! Lifecycle scripts around a stem's process (deliverable 16,
//! `docs/scripts.md`), and the supervisor operations built on them
//! (`build`, `reset`, `stamps`, `up --fresh`, workspace `bootstrap` /
//! `teardown`).
//!
//! The start sequence of a stem, run by its actor:
//!
//! ```text
//! [setup]      state `setup`, only when its stamp is missing or changed
//! [build]      `when: changed` (stamped like setup) or `always` (45)
//! [pre_start]
//! start        state `starting` → wait → `healthy`
//! [post_start]                       (still `healthy`, phase `pending`)
//! [seed]       state `seeding`, only when its stamp is missing or changed
//! done         `healthy`, `seeded: true` — `condition: seeded` holds
//! ```
//!
//! and the stop sequence: `[pre_stop]`, the custom `stop` script (else
//! SIGTERM) bounded by `stop_grace`, SIGKILL for what is left, `[post_stop]`.
//! A failed `setup` is `SETUP_FAILED`, any other failed script
//! `SCRIPT_FAILED`; the stem is then `failed` (a started process is stopped
//! first). Failed `pre_stop`/`post_stop` hooks are logged, never block a stop.
//!
//! A `soft_fail` script (41) that fails (after its retries) does not fail
//! anything: nothing is stamped, the sequence goes on, and the stem keeps a
//! warning (`status.warnings`, degraded) and gets `stem.degraded` until it
//! is stopped or started again. A soft-failed `seed` leaves `seeded: false`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use stems_api::{
    BuildParams, BuildResult, DegradedScript, EventKind, ResetParams, ResetResult,
    ScriptRunSummary, StampEntry, StampsParams, StampsResult, StemFailure,
};
use stems_config::{Resolved, Script, Stem, StemRuntime, Workspace};
use stems_core::stamps::{Stamp, compute_stamp};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::{Handle, Runtime, RuntimeError, StopOutcome};
use tokio_util::sync::CancellationToken;

use super::actor::StemCell;
use super::env::{CallEnv, EnvInputs, StemEnv, build_env};
use super::{Core, Supervisor, schedule};
use crate::events::EventDraft;
use crate::scripts::{RunContext, ScriptRef, ScriptResult};

/// Stem scripts that take part in the lifecycle (so a start keeps a [`Launch`]).
const SEQUENCE_SCRIPTS: [&str; 7] = [
    "setup",
    "pre_start",
    "post_start",
    "seed",
    "pre_stop",
    "stop",
    "post_stop",
];

/// What a running start sequence needs to run the stem's scripts later
/// (seed after `healthy`, stop hooks).
#[derive(Clone)]
pub(crate) struct Launch {
    /// The workspace the stem was started from.
    pub ws: Arc<Resolved>,
    /// The stem's environment (full, and what stems sets: `inherit_env:
    /// false` scripts get only the latter over a minimal base, 37).
    pub env: StemEnv,
    /// Who started it.
    pub actor: String,
}

/// The stem's full environment (FR-ST-4) for its scripts, over the
/// daemon's own environment; auto ports allocated on the way are announced
/// (`stem.port_allocated`).
pub(crate) fn stem_env(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    pass_env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, Error> {
    let call = CallEnv {
        caller: None,
        pass: pass_env.clone(),
    };
    stem_env_with(core, ws, stem, &call, false).map(|e| e.full)
}

/// [`stem_env`] over `call`'s base (37), full and own layers;
/// `strict_outputs`: an output reference without a value is
/// `UNRESOLVED_VARIABLE` (the start sequence, 26) instead of kept literally.
pub(crate) fn stem_env_with(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    call: &CallEnv,
    strict_outputs: bool,
) -> Result<StemEnv, Error> {
    let mut allocated = Vec::new();
    let env = build_env(
        ws,
        stem,
        &EnvInputs {
            base: call.base(&core.base_env),
            pass_env: &call.pass,
            run_id: &core.run_id,
            ports: &core.ports,
            outputs: Some(&core.outputs),
            strict_outputs,
        },
        &mut allocated,
    )?;
    for (s, n, p) in &allocated {
        core.events.emit(
            EventDraft::new(EventKind::STEM_PORT_ALLOCATED, stems_api::DAEMON_ACTOR)
                .stem(s)
                .data(json!({ "port": p, "name": n })),
        );
    }
    Ok(env)
}

/// The environment of a workspace script: the caller's (else the
/// daemon's, minus `STEMS_*`), workspace `env`, `--pass-env`,
/// `STEMS_WORKSPACE`, `STEMS_RUN_ID`.
pub(crate) fn workspace_env(core: &Core, ws: &Workspace, call: &CallEnv) -> StemEnv {
    let mut own: BTreeMap<String, String> =
        ws.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    own.extend(call.pass.clone());
    own.insert("STEMS_WORKSPACE".into(), ws.root.display().to_string());
    own.insert("STEMS_RUN_ID".into(), core.run_id.clone());
    let mut full: BTreeMap<String, String> = call
        .base(&core.base_env)
        .iter()
        .filter(|(k, _)| !stems_core::callerenv::is_stems_injected(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    full.extend(own.clone());
    StemEnv { own, full }
}

/// Run `name` of `stem` (with its `retries`). `Ok(None)`: no such script.
async fn run_stem_script(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    name: &str,
    env: &StemEnv,
    actor: &str,
    cancel: Option<&CancellationToken>,
) -> Result<Option<ScriptResult>, Error> {
    let Some(script) = stem.scripts.get(name) else {
        return Ok(None);
    };
    // `${stem.<n>.outputs.X}` / auto ports in an inline script (26).
    let script = &super::outputs::render_script(core, ws, &stem.name, script);
    // A stem without a codebase has no natural cwd: never run inside the
    // integration repo unless `cwd: workspace` (or any cwd) says so.
    let cwd_override = (stem.codebase.is_none() && !script.cwd_set)
        .then(|| core.scripts.state_dir(Some(&stem.name)));
    let shell = match &stem.runtime {
        StemRuntime::Process(p) => Some(p.shell.clone()),
        _ => None,
    };
    let mut attempt = 0;
    loop {
        let r = core
            .scripts
            .run(
                &ws.root,
                Some(&stem.name),
                ScriptRef { name, script },
                RunContext {
                    env: env.full.clone(),
                    own_env: Some(env.own.clone()),
                    cwd_override: cwd_override.clone(),
                    shell: shell.clone(),
                    actor: actor.to_string(),
                    cancel: cancel.cloned(),
                    ..RunContext::default()
                },
            )
            .await?;
        if r.success() || r.cancelled || attempt >= script.retries {
            return Ok(Some(r));
        }
        attempt += 1;
        // FR-SC-8: back off before the retry (a stop cancels the wait).
        let delay = stems_core::restart::backoff_delay(&super::run::retry_backoff(), attempt);
        match cancel {
            Some(c) => {
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = c.cancelled() => return Ok(Some(ScriptResult { cancelled: true, ..r })),
                }
            }
            None => tokio::time::sleep(delay).await,
        }
    }
}

/// The stamp of `script` of `stem` now: its text, its `inputs` (relative to
/// the codebase) and its `stamp_env` values.
pub(crate) fn stamp_of(
    ws: &Workspace,
    stem: &Stem,
    name: &str,
    script: &Script,
    env: &BTreeMap<String, String>,
) -> Result<Stamp, Error> {
    let text = crate::scripts::script_text(&ws.root, script)?;
    let senv: BTreeMap<String, String> = script
        .stamp_env
        .iter()
        .filter_map(|k| env.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    compute_stamp(&text, &script.inputs, stem.default_cwd(&ws.root), &senv).map_err(|e| {
        Error::new(
            ErrorCode::ScriptFailed,
            format!(
                "cannot compute the stamp of script `{name}` of `{}`: {e}",
                stem.name
            ),
        )
        .with_hint(format!(
            "check `stems.{}.scripts.{name}.inputs` (globs relative to the codebase)",
            stem.name
        ))
        .with_details(json!({ "stem": stem.name, "script": name, "reason": "stamp" }))
    })
}

fn needs_run(core: &Core, stem: &str, name: &str, stamp: &Stamp) -> bool {
    core.state
        .as_ref()
        .is_none_or(|s| s.stamps().needs_run(stem, name, stamp))
}

fn record_stamp(core: &Core, stem: &str, name: &str, stamp: Stamp) {
    if let Some(s) = &core.state {
        s.update_stamps(|st| st.record(stem, name, stamp));
    }
}

fn clear_stamps(core: &Core, stem: &str) -> bool {
    core.state.as_ref().is_some_and(|s| {
        s.update_stamps(|st| {
            let had = st.for_stem(stem).is_some();
            st.clear_stem(stem);
            had
        })
    })
}

/// The stem was stopped while a start-sequence script ran.
fn stopped_during(core: &Core, cell: &StemCell, actor: &str, what: &str) -> Error {
    if cell.state() == StemState::Setup {
        cell.transition(core, StemState::Stopping, "stopped", actor, json!({}));
        cell.transition(core, StemState::Stopped, "stopped", actor, json!({}));
    }
    Error::new(
        ErrorCode::StartFailed,
        format!("`{}` was stopped during {what}", cell.name),
    )
    .with_details(json!({ "stem": cell.name, "script": what, "reason": "cancelled" }))
}

fn fail(core: &Core, cell: &StemCell, e: Error, actor: &str) -> Error {
    cell.fail(core, e.clone(), actor);
    e
}

/// A `soft_fail` script failed (41): the stem keeps a warning (one per
/// script) and `stem.degraded` is announced. Nothing is stamped.
fn soft_fail(core: &Core, cell: &StemCell, r: &ScriptResult, actor: &str) {
    let w = r.warning();
    {
        let mut info = cell.info();
        info.warnings.retain(|x| x.script != w.script);
        info.warnings.push(w);
    }
    core.events.emit(degraded_event(&cell.name, r, actor));
}

/// `stem.degraded {script, exit, signal, timed_out}` for a soft failure (41).
fn degraded_event(stem: &str, r: &ScriptResult, actor: &str) -> EventDraft {
    EventDraft::new(EventKind::STEM_DEGRADED, actor)
        .stem(stem)
        .data(json!({
            "script": r.script,
            "exit": r.exit.code,
            "signal": r.exit.signal,
            "timed_out": r.timed_out,
        }))
}

/// Ready, but not seeded: its `soft_fail` seed failed in this run (41).
pub(crate) fn seed_soft_failed(cell: &StemCell) -> bool {
    let info = cell.info();
    !info.seeded && info.warnings.iter().any(|w| w.script == "seed")
}

/// The soft failures (41) of the `ready` stems, for `UpResult.degraded`.
pub(crate) fn degraded_of(sup: &Supervisor, ready: &[String]) -> Vec<DegradedScript> {
    ready
        .iter()
        .flat_map(|n| {
            sup.cell(n)
                .info()
                .warnings
                .iter()
                .map(|w| DegradedScript {
                    stem: n.clone(),
                    script: w.script.clone(),
                    exit: w.exit,
                    timed_out: w.timed_out,
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Before `starting`: `setup` (when its stamp is missing or changed; state
/// `setup`, `SETUP_FAILED`) and `pre_start` (`SCRIPT_FAILED`). Keeps a
/// [`Launch`] for the scripts that follow.
pub(crate) async fn before_start(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    ws: &Arc<Resolved>,
    stem: &Stem,
    actor: &str,
    call: &CallEnv,
) -> Result<(), Error> {
    // A `build.before_setup` marker (44) belongs to one start sequence.
    super::containers::clear_prebuilt(core, &stem.name);
    let built_now;
    {
        let mut info = cell.info();
        info.seeded = false;
        info.pending = false;
        info.launch = None;
        // A new attempt re-runs the scripts and re-decides (41); a policy
        // restart does not re-run `setup`/`seed`. A `build` warning stays
        // only when that build ran in this request (`restart --build`) or
        // this is a respawn of that run; a later start does not re-run it.
        let respawn = info.restart.respawning;
        let keep_build = std::mem::take(&mut info.keep_build_warning) || respawn;
        built_now = std::mem::take(&mut info.built_in_request);
        info.warnings.retain(|w| match w.script.as_str() {
            "build" => keep_build,
            "setup" | "seed" => respawn,
            _ => false,
        });
    }
    let build_in_sequence = stem
        .scripts
        .get("build")
        .is_some_and(|b| !b.when.is_on_demand());
    if !build_in_sequence
        && !SEQUENCE_SCRIPTS
            .iter()
            .any(|n| stem.scripts.contains_key(*n))
    {
        // No scripts: only the overlays (18).
        let mut env = call.base(&core.base_env).clone();
        env.extend(call.pass.clone());
        return super::overlays::materialise(core, &ws.workspace, stem, &env, actor)
            .map_err(|e| fail(core, cell, e, actor));
    }
    let env = stem_env_with(core, &ws.workspace, stem, call, true)
        .map_err(|e| fail(core, cell, e, actor))?;
    let token = CancellationToken::new();
    {
        let mut info = cell.info();
        info.launch = Some(Launch {
            ws: ws.clone(),
            env: env.clone(),
            actor: actor.to_string(),
        });
        info.script_cancel = Some(token.clone());
    }
    let r = setup_and_pre_start(
        core,
        cell,
        &ws.workspace,
        stem,
        actor,
        &env,
        &token,
        built_now,
    )
    .await;
    cell.info().script_cancel = None;
    if r.is_err() {
        // Failed or cancelled after a `before_setup` build: no start follows.
        super::containers::clear_prebuilt(core, &stem.name);
    }
    r
}

/// `build.before_setup` of a docker or compose stem (44, 51): build its
/// image(s) before `setup` (`docker.build` events) and give the scripts that
/// follow `STEMS_IMAGE` (compose: also `STEMS_IMAGES`). `None`: nothing to do (the env is as is).
async fn build_images_before_setup(
    core: &Core,
    cell: &StemCell,
    ws: &Workspace,
    stem: &Stem,
    actor: &str,
    env: &StemEnv,
) -> Result<Option<StemEnv>, Error> {
    let vars = super::containers::build_before_setup(core, ws, stem, &env.own, &env.full)
        .await
        .map_err(|e| fail(core, cell, e, actor))?;
    Ok(vars.map(|vars| {
        let mut env = env.clone();
        env.own.extend(vars.clone());
        env.full.extend(vars);
        env
    }))
}

#[allow(clippy::too_many_arguments)]
async fn setup_and_pre_start(
    core: &Core,
    cell: &StemCell,
    ws: &Workspace,
    stem: &Stem,
    actor: &str,
    env: &StemEnv,
    token: &CancellationToken,
    built_now: bool,
) -> Result<(), Error> {
    // `build.before_setup` (44): images first; `setup` sees STEMS_IMAGE(S).
    let built = build_images_before_setup(core, cell, ws, stem, actor, env).await?;
    let env = built.as_ref().unwrap_or(env);
    // A policy restart (22) never reruns `setup`: its stamp is intact.
    let respawn = cell.info().restart.respawning;
    if let Some(script) = stem.scripts.get("setup").filter(|_| !respawn) {
        let stamp = stamp_of(ws, stem, "setup", script, &env.full)
            .map_err(|e| fail(core, cell, e, actor))?;
        if needs_run(core, &stem.name, "setup", &stamp) {
            cell.transition(
                core,
                StemState::Setup,
                "running setup (stamp missing or changed)",
                actor,
                json!({ "script": "setup" }),
            );
            let r = run_stem_script(core, ws, stem, "setup", env, actor, Some(token))
                .await
                .map_err(|e| fail(core, cell, e, actor))?;
            match r {
                Some(r) if r.cancelled => return Err(stopped_during(core, cell, actor, "setup")),
                // Not done, not stamped: runs again next time (41).
                Some(r) if r.soft_failed() => soft_fail(core, cell, &r, actor),
                Some(r) if !r.success() => return Err(fail(core, cell, r.setup_error(), actor)),
                _ => record_stamp(core, &stem.name, "setup", stamp),
            }
        }
    }
    if token.is_cancelled() {
        return Err(stopped_during(core, cell, actor, "setup"));
    }
    // `restart --build` ran `build` already in this request.
    if !built_now {
        build_step(core, cell, ws, stem, actor, env, token).await?;
    }
    // Overlays after setup, before pre_start (18).
    super::overlays::materialise(core, ws, stem, &env.full, actor)
        .map_err(|e| fail(core, cell, e, actor))?;
    let r = run_stem_script(core, ws, stem, "pre_start", env, actor, Some(token))
        .await
        .map_err(|e| fail(core, cell, e, actor))?;
    match r {
        Some(r) if r.cancelled => Err(stopped_during(core, cell, actor, "pre_start")),
        Some(r) if r.soft_failed() => {
            soft_fail(core, cell, &r, actor);
            Ok(())
        }
        Some(r) if !r.success() => Err(fail(core, cell, r.error(), actor)),
        _ => Ok(()),
    }
}

/// `build` in the start sequence (45), after `setup`: `when: always` runs on
/// every start, `when: changed` when its stamp is missing or changed; never
/// on a policy respawn. A failure is `SCRIPT_FAILED` unless `soft_fail`.
async fn build_step(
    core: &Core,
    cell: &StemCell,
    ws: &Workspace,
    stem: &Stem,
    actor: &str,
    env: &StemEnv,
    token: &CancellationToken,
) -> Result<(), Error> {
    use stems_config::BuildWhen;
    let Some(script) = stem.scripts.get("build") else {
        return Ok(());
    };
    if script.when == BuildWhen::OnDemand || cell.info().restart.respawning {
        return Ok(());
    }
    let stamp = if script.when == BuildWhen::Changed {
        let stamp = stamp_of(ws, stem, "build", script, &env.full)
            .map_err(|e| fail(core, cell, e, actor))?;
        if !needs_run(core, &stem.name, "build", &stamp) {
            return Ok(());
        }
        Some(stamp)
    } else {
        None
    };
    let r = run_stem_script(core, ws, stem, "build", env, actor, Some(token))
        .await
        .map_err(|e| fail(core, cell, e, actor))?;
    match r {
        Some(r) if r.cancelled => Err(stopped_during(core, cell, actor, "build")),
        Some(r) if r.soft_failed() => {
            soft_fail(core, cell, &r, actor);
            Ok(())
        }
        Some(r) if !r.success() => Err(fail(core, cell, r.error(), actor)),
        _ => {
            if let Some(stamp) = stamp {
                record_stamp(core, &stem.name, "build", stamp);
            }
            Ok(())
        }
    }
}

/// Does the stem run `post_start` or `seed` once healthy?
pub(crate) fn has_after_ready(cell: &StemCell) -> bool {
    let info = cell.info();
    info.launch.as_ref().is_some_and(|l| {
        l.ws.workspace
            .stem(&cell.name)
            .is_some_and(|s| s.scripts.contains_key("post_start") || s.scripts.contains_key("seed"))
    })
}

/// After `healthy`: `post_start`, then `seed` (state `seeding`) when its
/// stamp is missing or changed; then `seeded` and the sequence is done. A
/// failure stops the process and fails the stem (`SCRIPT_FAILED`).
pub(crate) async fn after_ready(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64) {
    let Some(launch) = cell.info().launch.clone() else {
        cell.info().pending = false;
        cell.notify();
        return;
    };
    let Some(stem) = launch.ws.workspace.stem(&cell.name).cloned() else {
        cell.info().pending = false;
        cell.notify();
        return;
    };
    let token = CancellationToken::new();
    cell.info().script_cancel = Some(token.clone());
    let r = post_start_and_seed(core, cell, &launch, &stem, &token).await;
    let current = {
        let mut info = cell.info();
        info.script_cancel = None;
        info.pending = false;
        info.generation == generation
    };
    match r {
        _ if !current => {}
        Ok(()) => {
            if cell.state() == StemState::Seeding {
                // A soft-failed seed (41) leaves the stem unseeded.
                let seeded = cell.info().seeded;
                cell.transition(
                    core,
                    StemState::Healthy,
                    if seeded {
                        "seeded"
                    } else {
                        "seed failed (soft_fail)"
                    },
                    &launch.actor,
                    json!({ "seeded": seeded }),
                );
            } else {
                cell.notify();
            }
        }
        // Cancelled by a stop, which is queued behind us and takes over.
        Err(None) => cell.notify(),
        Err(Some(e)) => {
            cell.bump_generation();
            if let Some((h, rt)) = cell.clear_process() {
                let grace = cell.info().grace;
                if let Err(err) = rt.stop(&h, grace).await {
                    tracing::warn!(stem = %cell.name, error = %err, "stopping a stem whose script failed");
                }
                rt.release(&h);
            }
            cell.fail(core, e, &launch.actor);
        }
    }
}

async fn post_start_and_seed(
    core: &Core,
    cell: &StemCell,
    launch: &Launch,
    stem: &Stem,
    token: &CancellationToken,
) -> Result<(), Option<Error>> {
    let (ws, env, actor) = (&launch.ws.workspace, &launch.env, launch.actor.as_str());
    match run_stem_script(core, ws, stem, "post_start", env, actor, Some(token)).await {
        Err(e) => return Err(Some(e)),
        Ok(Some(r)) if r.cancelled => return Err(None),
        Ok(Some(r)) if r.soft_failed() => soft_fail(core, cell, &r, actor),
        Ok(Some(r)) if !r.success() => return Err(Some(r.error())),
        Ok(_) => {}
    }
    let Some(script) = stem.scripts.get("seed") else {
        return Ok(());
    };
    // A policy restart (22) never reruns `seed`: the data is still there.
    {
        let mut info = cell.info();
        if info.restart.respawning {
            info.seeded = info.restart.was_seeded;
            return Ok(());
        }
    }
    if token.is_cancelled() {
        return Err(None);
    }
    let stamp = stamp_of(ws, stem, "seed", script, &env.full).map_err(Some)?;
    if needs_run(core, &stem.name, "seed", &stamp) {
        cell.transition(
            core,
            StemState::Seeding,
            "running seed (stamp missing or changed)",
            actor,
            json!({ "script": "seed" }),
        );
        match run_stem_script(core, ws, stem, "seed", env, actor, Some(token)).await {
            Err(e) => return Err(Some(e)),
            Ok(Some(r)) if r.cancelled => return Err(None),
            // Not seeded, not stamped: `condition: seeded` dependants are
            // skipped, the seed runs again next time (41).
            Ok(Some(r)) if r.soft_failed() => {
                soft_fail(core, cell, &r, actor);
                return Ok(());
            }
            Ok(Some(r)) if !r.success() => return Err(Some(r.error())),
            Ok(_) => record_stamp(core, &stem.name, "seed", stamp),
        }
    }
    cell.info().seeded = true;
    Ok(())
}

fn launch_of(cell: &StemCell) -> Option<(Launch, Stem)> {
    let launch = cell.info().launch.clone()?;
    let stem = launch.ws.workspace.stem(&cell.name).cloned()?;
    Some((launch, stem))
}

/// Run a stop hook; failures are logged (the stop goes on).
async fn stop_hook(core: &Core, cell: &StemCell, name: &str, actor: &str) {
    let Some((launch, stem)) = launch_of(cell) else {
        return;
    };
    match run_stem_script(
        core,
        &launch.ws.workspace,
        &stem,
        name,
        &launch.env,
        actor,
        None,
    )
    .await
    {
        Ok(Some(r)) if !r.success() => {
            tracing::warn!(stem = %cell.name, script = name, error = %r.error(), "stop hook failed")
        }
        Err(e) => tracing::warn!(stem = %cell.name, script = name, error = %e, "stop hook failed"),
        _ => {}
    }
}

/// `pre_stop`, before the process is signalled.
pub(crate) async fn pre_stop(core: &Core, cell: &StemCell, actor: &str) {
    stop_hook(core, cell, "pre_stop", actor).await;
}

/// `post_stop`, once the process group is gone. Also ends `seeded`.
pub(crate) async fn post_stop(core: &Core, cell: &StemCell, actor: &str) {
    stop_hook(core, cell, "post_stop", actor).await;
    let mut info = cell.info();
    info.seeded = false;
    info.pending = false;
}

/// Stop the unit: the stem's `stop` script when it has one (bounded by
/// `grace`), then SIGTERM/SIGKILL for whatever is left; plain
/// SIGTERM → grace → SIGKILL otherwise.
pub(crate) async fn stop_unit(
    core: &Core,
    cell: &StemCell,
    h: &Handle,
    rt: &Arc<dyn Runtime>,
    grace: Duration,
    actor: &str,
) -> Result<StopOutcome, RuntimeError> {
    let Some((launch, stem)) = launch_of(cell).filter(|(_, s)| s.scripts.contains_key("stop"))
    else {
        return rt.stop(h, grace).await;
    };
    let deadline = tokio::time::Instant::now() + grace;
    let token = CancellationToken::new();
    let timer = {
        let token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            token.cancel();
        })
    };
    let r = run_stem_script(
        core,
        &launch.ws.workspace,
        &stem,
        "stop",
        &launch.env,
        actor,
        Some(&token),
    )
    .await;
    timer.abort();
    match &r {
        Ok(Some(r)) if !r.success() => {
            tracing::warn!(stem = %cell.name, error = %r.error(), "custom stop script failed")
        }
        Err(e) => tracing::warn!(stem = %cell.name, error = %e, "custom stop script failed"),
        _ => {}
    }
    let exited = tokio::time::timeout_at(deadline, rt.wait(h)).await.is_ok();
    // Clean up the group (and SIGKILL a leader that outlived the grace).
    let outcome = rt.stop(h, Duration::ZERO).await?;
    Ok(if exited {
        StopOutcome::Graceful
    } else {
        outcome
    })
}

fn summary(r: &ScriptResult) -> ScriptRunSummary {
    ScriptRunSummary {
        stem: r.stem.clone(),
        script: r.script.clone(),
        exit: r.exit.code,
        duration_ms: r.duration_ms,
    }
}

impl Supervisor {
    /// Run workspace script `name` (`bootstrap`, `teardown`). `Ok(None)`: none.
    async fn workspace_script(
        &self,
        ws: &Workspace,
        name: &str,
        actor: &str,
        call: &CallEnv,
    ) -> Result<Option<ScriptResult>, Error> {
        let Some(script) = ws.scripts.get(name) else {
            return Ok(None);
        };
        let env = workspace_env(&self.core, ws, call);
        let r = self
            .core
            .scripts
            .run(
                &ws.root,
                None,
                ScriptRef { name, script },
                RunContext {
                    env: env.full,
                    own_env: Some(env.own),
                    actor: actor.to_string(),
                    ..RunContext::default()
                },
            )
            .await?;
        Ok(Some(r))
    }

    /// Workspace `bootstrap` (once per `up`, before any stem): `SETUP_FAILED`;
    /// with `soft_fail` (41) `up` goes on and the failure is returned.
    pub(crate) async fn bootstrap(
        &self,
        ws: &Workspace,
        actor: &str,
        call: &CallEnv,
    ) -> Result<Option<DegradedScript>, Error> {
        match self.workspace_script(ws, "bootstrap", actor, call).await? {
            Some(r) if r.soft_failed() => {
                let stem = crate::scripts::WORKSPACE_LOG_STEM;
                self.core.events.emit(degraded_event(stem, &r, actor));
                Ok(Some(DegradedScript {
                    stem: stem.to_string(),
                    script: r.script.clone(),
                    exit: r.exit.code,
                    timed_out: r.timed_out,
                }))
            }
            Some(r) if !r.success() => Err(r.setup_error()),
            _ => Ok(None),
        }
    }

    /// Workspace `teardown` (end of `down --all`): `SCRIPT_FAILED`.
    pub(crate) async fn teardown(&self, ws: &Workspace, actor: &str) -> Result<(), Error> {
        match self
            .workspace_script(ws, "teardown", actor, &CallEnv::default())
            .await?
        {
            Some(r) if !r.success() => Err(r.error()),
            _ => Ok(()),
        }
    }

    /// A successful explicit `build` of a `when: changed` script is stamped.
    fn stamp_built(&self, ws: &Workspace, stem: &Stem, env: &StemEnv) {
        let Some(script) = stem
            .scripts
            .get("build")
            .filter(|s| s.when == stems_config::BuildWhen::Changed)
        else {
            return;
        };
        if let Ok(stamp) = stamp_of(ws, stem, "build", script, &env.full) {
            record_stamp(&self.core, &stem.name, "build", stamp);
        }
    }

    /// `build` scripts of `names` in start order (the ops lock is held by the caller).
    pub(crate) async fn build_stems(
        &self,
        ws: &Resolved,
        names: &[String],
        call: &CallEnv,
        actor: &str,
    ) -> BuildResult {
        let mut res = BuildResult::default();
        for name in names {
            let Some(stem) = ws.workspace.stem(name) else {
                continue;
            };
            if !stem.scripts.contains_key("build") {
                res.skipped.push(name.clone());
                continue;
            }
            let mut env_of = None;
            let r = match stem_env_with(&self.core, &ws.workspace, stem, call, false) {
                Ok(env) => {
                    let r = run_stem_script(
                        &self.core,
                        &ws.workspace,
                        stem,
                        "build",
                        &env,
                        actor,
                        None,
                    )
                    .await;
                    env_of = Some(env);
                    r
                }
                Err(e) => Err(e),
            };
            match r {
                Ok(Some(r)) if r.success() => {
                    // A `when: changed` build that succeeded is stamped: the
                    // start that follows does not run it again (45).
                    if let Some(env) = &env_of {
                        self.stamp_built(&ws.workspace, stem, env);
                    }
                    // A build that succeeds ends an earlier soft failure (41).
                    self.cell(name)
                        .info()
                        .warnings
                        .retain(|w| w.script != "build");
                    res.built.push(summary(&r));
                }
                // An explicit build that fails, fails; the stem is also
                // degraded, and `restart --build` starts it anyway (41).
                Ok(Some(r)) if r.soft_failed() => {
                    soft_fail(&self.core, &self.cell(name), &r, actor);
                    res.failed.push(StemFailure {
                        stem: name.clone(),
                        error: r.error(),
                    });
                }
                Ok(Some(r)) => res.failed.push(StemFailure {
                    stem: name.clone(),
                    error: r.error(),
                }),
                Ok(None) => res.skipped.push(name.clone()),
                Err(error) => res.failed.push(StemFailure {
                    stem: name.clone(),
                    error,
                }),
            }
        }
        res.ok = res.failed.is_empty();
        res
    }

    /// Stop `names` (those running), run their `reset` scripts and clear
    /// their stamps (the ops lock is held by the caller).
    pub(crate) async fn reset_stems(
        &self,
        ws: &Resolved,
        names: &[String],
        actor: &str,
    ) -> ResetResult {
        let running: Vec<String> = names
            .iter()
            .filter(|n| {
                ws.workspace
                    .stem(n)
                    .is_some_and(|s| s.kind() != stems_config::StemType::External)
            })
            .filter(|n| self.cell(n).state().is_running())
            .cloned()
            .collect();
        let stop = self.stop_set(ws, &running, None, actor, "reset").await;
        let mut res = ResetResult {
            stopped: stop.stopped,
            failed: stop.failed,
            ..ResetResult::default()
        };
        // Reset dependants before what they depend on.
        for name in names.iter().rev() {
            let Some(stem) = ws.workspace.stem(name) else {
                continue;
            };
            if res.failed.iter().any(|f| &f.stem == name) {
                continue;
            }
            if stem.scripts.contains_key("reset") {
                let r = match stem_env_with(
                    &self.core,
                    &ws.workspace,
                    stem,
                    &CallEnv::default(),
                    false,
                ) {
                    Ok(env) => {
                        run_stem_script(&self.core, &ws.workspace, stem, "reset", &env, actor, None)
                            .await
                    }
                    Err(e) => Err(e),
                };
                match r {
                    Ok(Some(r)) if r.success() => res.reset.push(summary(&r)),
                    Ok(Some(r)) => {
                        res.failed.push(StemFailure {
                            stem: name.clone(),
                            error: r.error(),
                        });
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        res.failed.push(StemFailure {
                            stem: name.clone(),
                            error,
                        });
                        continue;
                    }
                }
            }
            if clear_stamps(&self.core, name) {
                res.cleared.push(name.clone());
            }
        }
        res.reset.reverse();
        res.cleared.reverse();
        res.ok = res.failed.is_empty();
        res
    }

    /// `build`: run the `build` scripts of the stems (all enabled stems when
    /// none are named). Running stems are not restarted (`restart --build` does).
    pub async fn build(&self, p: BuildParams, actor: &str) -> Result<BuildResult, Error> {
        let ws = self.workspace_for(actor, p.caller_env.as_ref(), &[])?;
        let plan = schedule::plan(&ws, &p.stems, true)?;
        let _ops = self.ops.lock().await;
        let call = CallEnv::new(p.caller_env, BTreeMap::new());
        Ok(self.build_stems(&ws, &plan.order, &call, actor).await)
    }

    /// `reset`: stop the stems, run their `reset` scripts, clear their stamps
    /// (every enabled stem when none are named).
    pub async fn reset(&self, p: ResetParams, actor: &str) -> Result<ResetResult, Error> {
        let ws = self.workspace(true, actor)?;
        let plan = schedule::plan(&ws, &p.stems, true)?;
        let _ops = self.ops.lock().await;
        Ok(self.reset_stems(&ws, &plan.order, actor).await)
    }

    /// `stamps`: list (or clear) the recorded stamps.
    pub fn stamps(&self, p: StampsParams, actor: &str) -> Result<StampsResult, Error> {
        if let Some(s) = &p.stem {
            let ws = self.workspace(false, actor)?;
            schedule::plan(&ws, std::slice::from_ref(s), true)?;
        }
        let Some(store) = &self.core.state else {
            return Ok(StampsResult::default());
        };
        let all = store.stamps();
        let stamps = entries(&all, p.stem.as_deref());
        if p.clear {
            store.update_stamps(|st| match &p.stem {
                Some(s) => st.clear_stem(s),
                None => st.clear(),
            });
        }
        Ok(StampsResult {
            stamps,
            cleared: p.clear,
        })
    }
}

/// Stamps of `store` as a sorted list (only `stem`'s when given).
pub fn entries(store: &stems_core::stamps::StampStore, stem: Option<&str>) -> Vec<StampEntry> {
    store
        .0
        .iter()
        .filter(|(s, _)| stem.is_none_or(|x| x == s.as_str()))
        .flat_map(|(s, m)| {
            m.iter().map(move |(script, st)| StampEntry {
                stem: s.clone(),
                script: script.clone(),
                hash: st.hash.clone(),
                computed_at: st.computed_at,
                inputs: st.inputs.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every script name the start/stop sequence runs is a documented hook
    /// (`stems_config::lifecycle::HOOKS`), and every stem hook the table
    /// lists is a lifecycle script name (36).
    #[test]
    fn sequence_scripts_are_documented_hooks() {
        for name in SEQUENCE_SCRIPTS {
            assert!(
                stems_config::lifecycle::hook_named(name).is_some(),
                "{name} is not in stems_config::lifecycle::HOOKS"
            );
            assert!(stems_config::is_lifecycle_script(name), "{name}");
        }
        for h in stems_config::HOOKS {
            match h.scope {
                stems_config::HookScope::Stem => {
                    assert!(stems_config::is_lifecycle_script(h.name), "{}", h.name);
                }
                stems_config::HookScope::Workspace => {
                    assert!(
                        stems_config::WORKSPACE_LIFECYCLE_SCRIPTS.contains(&h.name),
                        "{}",
                        h.name
                    );
                }
            }
        }
    }
}
