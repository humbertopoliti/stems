//! The layer scheduler behind `up`, `start` and `restart` (FR-GR-2).
//!
//! Every stem of the plan gets a task. A task waits for each of its hard
//! dependencies' edge condition (`started` / `healthy` / `seeded`) *within
//! this run*, then takes one of `max_parallel` permits, asks the stem's actor
//! to start, and holds the permit until the stem is ready (`healthy`, or
//! `unknown` for external stems) or failed. Tasks are spawned in
//! `start_order()` layer order so permits go to earlier layers first.
//!
//! A dependency that failed or was skipped skips its dependants. With
//! fail-fast, the first failure also skips every stem that has not asked to
//! start yet (already started stems keep running). Cancellation (a `down`
//! arriving mid-`up`) skips everything not yet started and stops waiting.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use stems_api::{StemFailure, UpResult};
use stems_config::{Condition, Resolved, StemType};
use stems_core::{Error, ErrorCode, StemState, WorkspaceGraph};
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
}

/// The plan for `requested` (all enabled stems if empty), adding hard
/// dependencies transitively unless `no_deps`.
pub fn plan(ws: &Resolved, requested: &[String], no_deps: bool) -> Result<Plan, Error> {
    let w = &ws.workspace;
    for r in requested {
        match w.stem(r) {
            None => {
                return Err(Error::new(
                    ErrorCode::UnknownStem,
                    format!("stem `{r}` does not exist in workspace `{}`", w.name),
                )
                .with_hint(format!(
                    "known stems: {}",
                    w.stems.keys().cloned().collect::<Vec<_>>().join(", ")
                ))
                .with_details(json!({ "stem": r })));
            }
            Some(s) if !s.enabled => {
                return Err(Error::new(
                    ErrorCode::UnknownStem,
                    format!("stem `{r}` is disabled (`enabled: false`)"),
                )
                .with_hint(format!(
                    "enable it in stems.local.yaml: stems.{r}.enabled: true"
                ))
                .with_details(json!({ "stem": r, "disabled": true })));
            }
            Some(_) => {}
        }
    }
    let mut set: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = if requested.is_empty() {
        w.stems().map(|s| s.name.clone()).collect()
    } else {
        requested.iter().cloned().collect()
    };
    while let Some(n) = queue.pop_front() {
        if !set.insert(n.clone()) || no_deps {
            continue;
        }
        if let Some(s) = w.stem(&n) {
            for d in s.depends_on.iter().filter(|d| !d.soft) {
                if w.stem(&d.stem).is_some_and(|t| t.enabled) {
                    queue.push_back(d.stem.clone());
                }
            }
        }
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
    /// `--pass-env` values.
    pub pass_env: BTreeMap<String, String>,
    /// Cancels the run.
    pub cancel: CancellationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Pending,
    Started,
    Ready,
    Failed,
    Skipped,
}

impl Stage {
    fn satisfies(self, c: Condition) -> bool {
        match c {
            Condition::Started => matches!(self, Stage::Started | Stage::Ready),
            Condition::Healthy | Condition::Seeded => self == Stage::Ready,
        }
    }
    fn is_bad(self) -> bool {
        matches!(self, Stage::Failed | Stage::Skipped)
    }
}

enum Outcome {
    Ready,
    Failed(Error),
    Skipped,
}

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
    let mut set = JoinSet::new();
    for (i, name) in plan.order.iter().enumerate() {
        let cell = cells[name].clone();
        let deps = plan.deps.get(name).cloned().unwrap_or_default();
        let (stages, abort, sem, opts) = (stages.clone(), abort.clone(), sem.clone(), opts.clone());
        let name = name.clone();
        set.spawn(async move {
            let out = one(&name, &cell, &deps, &stages, &abort, &sem, &opts).await;
            let stage = match &out {
                Outcome::Ready => Stage::Ready,
                Outcome::Failed(_) => Stage::Failed,
                Outcome::Skipped => Stage::Skipped,
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
            Outcome::Ready => res.ready.push(name),
            Outcome::Failed(error) => res.failed.push(StemFailure { stem: name, error }),
            Outcome::Skipped => res.skipped.push(name),
        }
    }
    res.ok = res.failed.is_empty() && res.skipped.is_empty();
    res
}

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
        let r = tokio::select! {
            r = rx.wait_for(|s| s.satisfies(*cond) || s.is_bad()) => r.map(|s| *s).unwrap_or(Stage::Skipped),
            () = opts.cancel.cancelled() => return Outcome::Skipped,
            () = until(opts.deadline) => return Outcome::Failed(timeout_error(name)),
        };
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
    if let Err(e) = cell
        .start(
            opts.ws.clone(),
            &opts.actor,
            &opts.reason,
            opts.pass_env.clone(),
        )
        .await
    {
        return fail(e);
    }
    stages[name].send_replace(Stage::Started);
    let external = opts
        .ws
        .workspace
        .stem(name)
        .is_some_and(|s| s.kind() == StemType::External);
    let mut ph = cell.watch();
    let end = tokio::select! {
        r = ph.wait_for(|p| !matches!(p.state, StemState::Starting | StemState::Setup | StemState::Seeding)) => r.map(|p| p.state).unwrap_or(StemState::Failed),
        () = opts.cancel.cancelled() => return Outcome::Skipped,
        () = until(opts.deadline) => return fail(timeout_error(name)),
    };
    match end {
        StemState::Healthy => Outcome::Ready,
        StemState::Unknown if external => Outcome::Ready,
        other => fail(cell.error().unwrap_or_else(|| {
            Error::new(
                ErrorCode::StartFailed,
                format!("`{name}` is {other} instead of ready"),
            )
            .with_details(json!({ "stem": name, "state": other }))
        })),
    }
}
