//! The layer scheduler behind `up`, `start` and `restart` (FR-GR-2).
//!
//! Every stem of the plan gets a task. A task waits for each of its hard
//! dependencies' edge condition (`started` / `healthy` / `seeded`) *within
//! this run*, then takes one of `max_parallel` permits, asks the stem's actor
//! to start, and holds the permit until the stem is ready (`healthy`, or
//! `unknown` for external stems) or failed. Tasks are spawned in
//! `start_order()` layer order so permits go to earlier layers first.
//!
//! A dependency that failed or was skipped skips its dependants; one that is
//! ready but whose `soft_fail` seed failed (41) skips its `condition:
//! seeded` dependants (reason `seed_degraded`). With
//! fail-fast, the first failure also skips every stem that has not asked to
//! start yet (already started stems keep running). Cancellation (a `down`
//! arriving mid-`up`) skips everything not yet started and stops waiting.
//!
//! A hard edge to a disabled stem is dropped: the dependant starts without
//! it (`if_disabled: start`, the default since 51). With `if_disabled:
//! skip` the dependant is left out of the plan and reported as skipped with
//! reason `dependency_disabled:<dep>`, and so are its own hard dependants
//! (46). Those skips are the configuration's intent: they do not make `ok`
//! false.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use stems_api::{StemFailure, UpResult};
use stems_config::{Condition, IfDisabled, Resolved, StemType};
use stems_core::{Error, ErrorCode, SelectOptions, Selection, StemState, WorkspaceGraph};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::actor::StemCell;

/// What to start, in layer order, with the edges to wait on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Stems in start order (layer by layer, sorted within a layer).
    pub order: Vec<String>,
    /// `start_order()` layers restricted to the plan.
    pub layers: Vec<Vec<String>>,
    /// Hard dependencies inside the plan, with their condition.
    pub deps: HashMap<String, Vec<(String, Condition)>>,
    /// Stems left out because a hard dependency, direct or not, is disabled
    /// (46): `(stem, disabled dependency)`, in start order.
    pub disabled_skips: Vec<(String, String)>,
    /// Disabled stems the selection covers (46, `UpResult::disabled`).
    pub disabled: Vec<String>,
}

/// The plan for `requested` (all enabled stems if empty), adding hard
/// dependencies transitively unless `no_deps`. The closure is
/// [`Selection::resolve`] (26); no profile applies here, see [`plan_up`].
pub fn plan(ws: &Resolved, requested: &[String], no_deps: bool) -> Result<Plan, Error> {
    let w = &ws.workspace;
    let names: Vec<String> = if requested.is_empty() {
        w.stems().map(|s| s.name.clone()).collect()
    } else {
        requested.to_vec()
    };
    let opts = SelectOptions {
        include_deps: !no_deps,
        strict_profiles: None,
    };
    let sel = Selection::resolve(w, None, &names, opts)?;
    let mut plan = plan_closure(ws, &sel.closure)?;
    plan.disabled = disabled_of(ws, &sel.closure, requested.is_empty(), &[]);
    Ok(plan)
}

/// The plan of `up`: `requested` stems, else the profile (`profile`, which
/// the client resolved from `--profile` / `STEMS_PROFILE`, else the
/// workspace default), else everything; hard dependencies added (26).
pub fn plan_up(
    ws: &Resolved,
    profile: Option<&str>,
    requested: &[String],
) -> Result<(Plan, Selection), Error> {
    let sel = Selection::resolve(&ws.workspace, profile, requested, SelectOptions::default())?;
    let mut plan = plan_closure(ws, &sel.closure)?;
    let everything = requested.is_empty() && sel.profile.is_none();
    plan.disabled = disabled_of(ws, &sel.closure, everything, &sel.disabled);
    Ok((plan, sel))
}

/// The disabled stems a selection covers (46), in declaration order: every
/// disabled stem when `everything` was selected, else `members` (a
/// profile's disabled members) and the disabled targets of the edges of
/// `closure`.
fn disabled_of(
    ws: &Resolved,
    closure: &[String],
    everything: bool,
    members: &[String],
) -> Vec<String> {
    let w = &ws.workspace;
    let mut set: HashSet<&str> = members.iter().map(String::as_str).collect();
    for n in closure {
        if let Some(s) = w.stem(n) {
            set.extend(s.depends_on.iter().map(|d| d.stem.as_str()));
        }
    }
    w.disabled_stems()
        .filter(|s| everything || set.contains(s.name.as_str()))
        .map(|s| s.name.clone())
        .collect()
}

/// Layers and edges for an already closed set of stems.
fn plan_closure(ws: &Resolved, closure: &[String]) -> Result<Plan, Error> {
    let w = &ws.workspace;
    let mut set: HashSet<String> = closure.iter().cloned().collect();
    // Hard edges to disabled stems skip the dependant, transitively over
    // hard edges (46). Layers are in dependency order, so a dependency's
    // verdict is known before its dependants'.
    let mut skips: Vec<(String, String)> = Vec::new();
    for n in w.start_order()?.into_iter().flatten() {
        if !set.contains(&n) {
            continue;
        }
        let Some(s) = w.stem(&n) else { continue };
        let cause = s.depends_on.iter().filter(|d| !d.soft).find_map(|d| {
            if w.stem(&d.stem).is_some_and(|t| !t.enabled) {
                (d.if_disabled == IfDisabled::Skip).then(|| d.stem.clone())
            } else {
                skips
                    .iter()
                    .find(|(k, _)| *k == d.stem)
                    .map(|(_, why)| why.clone())
            }
        });
        if let Some(dep) = cause {
            skips.push((n, dep));
        }
    }
    for (n, _) in &skips {
        set.remove(n);
    }
    let layers: Vec<Vec<String>> = w
        .start_order()?
        .into_iter()
        .map(|l| {
            l.into_iter()
                .filter(|n| set.contains(n))
                .collect::<Vec<_>>()
        })
        .filter(|l: &Vec<String>| !l.is_empty())
        .collect();
    let order: Vec<String> = layers.iter().flatten().cloned().collect();
    let deps = order
        .iter()
        .map(|n| {
            let edges = w
                .stem(n)
                .map(|s| {
                    s.depends_on
                        .iter()
                        .filter(|d| !d.soft && set.contains(&d.stem))
                        .map(|d| (d.stem.clone(), d.condition))
                        .collect()
                })
                .unwrap_or_default();
            (n.clone(), edges)
        })
        .collect();
    Ok(Plan {
        order,
        layers,
        deps,
        disabled_skips: skips,
        disabled: Vec::new(),
    })
}

/// Options of one scheduler run.
#[derive(Clone)]
pub struct RunOptions {
    /// Workspace to start from.
    pub ws: Arc<Resolved>,
    /// Who asked.
    pub actor: String,
    /// `stem.state` reason for the starts.
    pub reason: String,
    /// Skip everything not started after the first failure.
    pub fail_fast: bool,
    /// Concurrent starts.
    pub max_parallel: usize,
    /// Overall deadline.
    pub deadline: Option<Instant>,
    /// The caller's environment and `--pass-env` values (37); `None`: a
    /// start stems makes on its own (config apply), so each stem reuses
    /// the environment of its last user start.
    pub call_env: Option<super::env::CallEnv>,
    /// `restart` without `--pass-env` (51): each stem keeps the `--pass-env`
    /// set of its last user start under `call_env`'s caller environment.
    pub keep_pass: bool,
    /// Cancels the run.
    pub cancel: CancellationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Pending,
    Started,
    /// `healthy`, `post_start`/`seed` still running (16).
    Healthy,
    Ready,
    /// Ready, but its `soft_fail` seed failed: not seeded (41).
    Unseeded,
    Failed,
    Skipped,
    /// Skipped because of a soft-failed seed upstream (`seed_degraded`).
    SkippedSeed,
}

impl Stage {
    fn satisfies(self, c: Condition) -> bool {
        match c {
            Condition::Started => matches!(
                self,
                Stage::Started | Stage::Healthy | Stage::Ready | Stage::Unseeded
            ),
            Condition::Healthy => matches!(self, Stage::Healthy | Stage::Ready | Stage::Unseeded),
            // Ready = the whole start sequence, seed included (16).
            Condition::Seeded => self == Stage::Ready,
        }
    }
    fn is_bad(self) -> bool {
        matches!(self, Stage::Failed | Stage::Skipped | Stage::SkippedSeed)
    }
}

enum Outcome {
    Ready,
    /// Ready, not seeded (its `soft_fail` seed failed, 41).
    Unseeded,
    Failed(Error),
    Skipped,
    /// Skipped for a reason other than a failed dependency (41).
    SkippedBecause(&'static str),
}

/// `skipped_reasons` value: a `condition: seeded` dependency (or one of its
/// skipped dependants) is ready but its `soft_fail` seed failed (41).
const SEED_DEGRADED: &str = "seed_degraded";

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

fn timeout_error(stem: &str) -> Error {
    Error::new(
        ErrorCode::StartTimeout,
        format!("`{stem}` was not ready before the `--timeout` deadline"),
    )
    .with_hint("raise `--timeout`, or check `stems status` for what it is waiting on")
    .with_details(json!({ "stem": stem }))
}

/// Run `plan`, using `cell(name)` to reach each stem's actor.
pub async fn run(
    plan: &Plan,
    cells: &HashMap<String, Arc<StemCell>>,
    opts: &RunOptions,
) -> UpResult {
    let stages: HashMap<String, watch::Sender<Stage>> = plan
        .order
        .iter()
        .map(|n| (n.clone(), watch::Sender::new(Stage::Pending)))
        .collect();
    let stages = Arc::new(stages);
    let abort = Arc::new(AtomicBool::new(false));
    let sem = Arc::new(Semaphore::new(opts.max_parallel.max(1)));
    // Stems some planned dependant waits on with `healthy`/`seeded`: an
    // external one is then only ready once its probe passes (21).
    let needed: Arc<HashSet<String>> = Arc::new(
        plan.deps
            .values()
            .flatten()
            .filter(|(_, c)| *c != Condition::Started)
            .map(|(d, _)| d.clone())
            .collect(),
    );
    let mut set = JoinSet::new();
    for (i, name) in plan.order.iter().enumerate() {
        let cell = cells[name].clone();
        let deps = plan.deps.get(name).cloned().unwrap_or_default();
        let (stages, abort, sem, opts) = (stages.clone(), abort.clone(), sem.clone(), opts.clone());
        let name = name.clone();
        let wait_health = needed.contains(&name);
        set.spawn(async move {
            let out = one(&name, &cell, &deps, &stages, &abort, &sem, &opts).await;
            let out = match out {
                Outcome::Ready if wait_health => external_healthy(&name, &cell, &opts).await,
                o => o,
            };
            let stage = match &out {
                Outcome::Ready => Stage::Ready,
                Outcome::Unseeded => Stage::Unseeded,
                Outcome::Failed(_) => Stage::Failed,
                Outcome::SkippedBecause(SEED_DEGRADED) => Stage::SkippedSeed,
                Outcome::Skipped | Outcome::SkippedBecause(_) => Stage::Skipped,
            };
            stages[&name].send_replace(stage);
            (i, name, out)
        });
    }
    let mut outs: Vec<(usize, String, Outcome)> = Vec::new();
    while let Some(r) = set.join_next().await {
        match r {
            Ok(o) => outs.push(o),
            Err(e) => tracing::error!(error = %e, "scheduler task panicked"),
        }
    }
    outs.sort_by_key(|(i, _, _)| *i);
    let mut res = UpResult::default();
    for (_, name, out) in outs {
        match out {
            Outcome::Ready | Outcome::Unseeded => res.ready.push(name),
            Outcome::Failed(error) => res.failed.push(StemFailure { stem: name, error }),
            Outcome::Skipped => res.skipped.push(name),
            Outcome::SkippedBecause(why) => {
                res.skipped_reasons.insert(name.clone(), why.to_string());
                res.skipped.push(name);
            }
        }
    }
    res.ok = res.failed.is_empty() && res.skipped.is_empty();
    // Skips for a disabled dependency are the config's intent (46).
    for (name, dep) in &plan.disabled_skips {
        res.skipped_reasons
            .insert(name.clone(), format!("{DEPENDENCY_DISABLED}{dep}"));
        res.skipped.push(name.clone());
    }
    res.disabled = plan.disabled.clone();
    res
}

/// Prefix of the `skipped_reasons` value of a stem skipped because a hard
/// dependency is disabled (46): `dependency_disabled:<dep>`.
pub const DEPENDENCY_DISABLED: &str = "dependency_disabled:";

async fn one(
    name: &str,
    cell: &Arc<StemCell>,
    deps: &[(String, Condition)],
    stages: &HashMap<String, watch::Sender<Stage>>,
    abort: &AtomicBool,
    sem: &Arc<Semaphore>,
    opts: &RunOptions,
) -> Outcome {
    for (dep, cond) in deps {
        let mut rx = stages[dep].subscribe();
        let unseeded = |s: Stage| s == Stage::Unseeded && *cond == Condition::Seeded;
        let r = tokio::select! {
            r = rx.wait_for(|s| s.satisfies(*cond) || s.is_bad() || unseeded(*s)) => r.map(|s| *s).unwrap_or(Stage::Skipped),
            () = opts.cancel.cancelled() => return Outcome::Skipped,
            () = until(opts.deadline) => return Outcome::Failed(timeout_error(name)),
        };
        // Skipped (transitively) because a `condition: seeded` dependency's
        // `soft_fail` seed failed: the reason says so all the way down.
        if unseeded(r) || r == Stage::SkippedSeed {
            return Outcome::SkippedBecause(SEED_DEGRADED);
        }
        if r.is_bad() {
            return Outcome::Skipped;
        }
    }
    let skip = || abort.load(Ordering::SeqCst) || opts.cancel.is_cancelled();
    if skip() {
        return Outcome::Skipped;
    }
    let _permit = tokio::select! {
        p = sem.clone().acquire_owned() => match p { Ok(p) => p, Err(_) => return Outcome::Skipped },
        () = opts.cancel.cancelled() => return Outcome::Skipped,
    };
    if skip() {
        return Outcome::Skipped;
    }
    let fail = |e: Error| {
        if opts.fail_fast {
            abort.store(true, Ordering::SeqCst);
        }
        Outcome::Failed(e)
    };
    // The start includes `setup`/`pre_start` (16), which may take long: a
    // cancellation (`down`) must not wait for it (the stop kills the script).
    let started = tokio::select! {
        r = cell.start_with(
            opts.ws.clone(),
            &opts.actor,
            &opts.reason,
            match opts.call_env.clone() {
                Some(mut call) => {
                    if opts.keep_pass {
                        call.pass = cell.info().restart.call_env().pass;
                    }
                    call
                }
                None => cell.info().restart.call_env(),
            },
        ) => r,
        () = opts.cancel.cancelled() => return Outcome::Skipped,
    };
    if let Err(e) = started {
        return fail(e);
    }
    stages[name].send_replace(Stage::Started);
    let external = opts
        .ws
        .workspace
        .stem(name)
        .is_some_and(|s| s.kind() == StemType::External);
    let mut ph = cell.watch();
    // `healthy` with `pending` (post_start/seed to run, 16) already satisfies
    // `condition: healthy` edges; the stem is ready once nothing is pending.
    let mut healthy_seen = false;
    let end = loop {
        let seen = healthy_seen;
        let p = tokio::select! {
            r = ph.wait_for(|p| match p.state {
                StemState::Starting | StemState::Setup | StemState::Seeding => false,
                StemState::Healthy if p.pending => !seen,
                _ => true,
            }) => r.map(|p| *p).ok(),
            () = opts.cancel.cancelled() => return Outcome::Skipped,
            () = until(opts.deadline) => return fail(timeout_error(name)),
        };
        let Some(p) = p else { break StemState::Failed };
        if p.state == StemState::Healthy && p.pending {
            healthy_seen = true;
            stages[name].send_replace(Stage::Healthy);
            continue;
        }
        break p.state;
    };
    match end {
        StemState::Healthy if super::hooks::seed_soft_failed(cell) => Outcome::Unseeded,
        StemState::Healthy => Outcome::Ready,
        StemState::Unknown | StemState::Unhealthy if external => Outcome::Ready,
        other => fail(cell.error().unwrap_or_else(|| {
            Error::new(
                ErrorCode::StartFailed,
                format!("`{name}` is {other} instead of ready"),
            )
            .with_details(json!({ "stem": name, "state": other }))
        })),
    }
}

/// An external stem with a health check that a dependant needs `healthy`:
/// wait for its probe to pass, bounded by its `health.start_timeout` and
/// the run's deadline (`HEALTH_TIMEOUT`). Anything else is ready as is.
async fn external_healthy(name: &str, cell: &Arc<StemCell>, opts: &RunOptions) -> Outcome {
    let Some(limit) = super::probes::external_with_health(&opts.ws, name) else {
        return Outcome::Ready;
    };
    let mut ph = cell.watch();
    let own = Instant::now() + limit;
    let deadline = opts.deadline.map_or(own, |d| d.min(own));
    tokio::select! {
        r = ph.wait_for(|p| p.state == StemState::Healthy) => match r {
            Ok(_) => Outcome::Ready,
            Err(_) => Outcome::Skipped,
        },
        () = opts.cancel.cancelled() => Outcome::Skipped,
        () = tokio::time::sleep_until(deadline) => {
            let last = cell
                .info()
                .health
                .last(1)
                .pop()
                .map_or_else(|| "no probe finished".to_string(), |r| r.detail);
            Outcome::Failed(
                Error::new(
                    ErrorCode::HealthTimeout,
                    format!(
                        "external `{name}` did not become healthy within {}s (last probe: {last})",
                        limit.as_secs_f64()
                    ),
                )
                .with_hint(format!(
                    "check that `{name}` is up and reachable (`stems health {name}`), or raise its `health.start_timeout`"
                ))
                .with_details(json!({ "stem": name, "state": cell.state(), "last_probe": last })),
            )
        }
    }
}
