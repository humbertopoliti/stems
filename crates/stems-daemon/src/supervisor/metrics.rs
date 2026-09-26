//! Metrics (deliverable 25, `docs/metrics.md`): one sampler task per
//! supervisor, the per-stem history, thresholds, persistence and the
//! `metrics` RPC.
//!
//! * [`spawn_sampler`] — every `metrics.interval` (default 2 s) the sampler
//!   looks at every running stem: process stems through
//!   `os::process_tree(pgid)` (read in one `spawn_blocking` call for all
//!   stems), docker/compose stems through one `stats(stream=false)` call each
//!   (in their own tasks, never overlapping per stem). It only reads the
//!   cells' snapshots, so it never blocks an actor.
//! * [`MetricsStore`] — per stem: a [`MetricsHistory`] (30 min),
//!   the previous per-pid CPU times (CPU % is the tree's CPU-time delta over
//!   wall time, [`tree_cpu_percent`]), the [`ThresholdTracker`]s built from
//!   `limits:` and the open ports (from `describe`, refreshed every 10 s).
//! * Thresholds: a crossing emits `stem.threshold {metric, value, limit,
//!   for_s, state}` and makes the stem `degraded` (reason `memory > 100MB`)
//!   through [`MetricsStore::crossed`] → `DegradedInputs::metrics`.
//! * `metrics.persist: true` appends every sample as one NDJSON line to
//!   `<data dir>/metrics/<stem>.ndjson`; at the first write of a new (UTC)
//!   day the file is renamed `<stem>-YYYY-MM-DD.ndjson` (7 kept).
//! * `--disk` ([`disk_usage`]): bounded walk of the codebase's build-output
//!   dirs plus the stem's named volumes (`docker system df`), cached 60 s.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use stems_api::metrics::{
    DiskDir, DiskUsage, LimitStatus, MetricsParams, MetricsResult, MetricsSummary, MetricsTotals,
    StemMetrics, sort_stems,
};
use stems_api::{EventKind, StemStatus};
use stems_config::{Limits, Resolved, Stem};
use stems_core::metrics::{
    MetricsHistory, Sample, Threshold, ThresholdEvent, ThresholdTracker, cpu_percent,
};
use stems_core::{Error, StemState};
use stems_runtime::docker::ContainerStats;
use stems_runtime::os::ProcInfo;
use stems_runtime::{Handle, Runtime};

use super::{Core, Supervisor};
use crate::events::EventDraft;

/// Default `metrics.interval`.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
/// Shortest interval the sampler honours.
const MIN_INTERVAL: Duration = Duration::from_millis(100);
/// How long open ports (from `describe`) are trusted.
const PORTS_TTL: Duration = Duration::from_secs(10);
/// How long a `--disk` result is reused.
const DISK_TTL: Duration = Duration::from_secs(60);
/// Longest a docker stats call may take.
const STATS_TIMEOUT: Duration = Duration::from_secs(5);
/// Rotated NDJSON files kept per stem.
const KEEP_ROTATED: usize = 7;
/// Build-output directories `--disk` measures in a codebase.
pub const BUILD_DIRS: [&str; 5] = ["target", "dist", "build", "node_modules", ".venv"];
/// Entries one `--disk` walk visits at most (per stem).
const DISK_WALK_LIMIT: usize = 200_000;

/// The raw reading of one stem.
#[derive(Clone, Debug)]
pub enum Reading {
    /// A process tree (process stems).
    Tree(Vec<ProcInfo>),
    /// One docker stats reading (docker/compose stems).
    Container(ContainerStats),
}

/// CPU % of a process tree between two readings `wall_ms` apart: the sum
/// over the current processes of their CPU-time growth (a process new since
/// the last reading counts with all its CPU time), over wall time, × 100.
/// Processes that exited in between are not counted (their last slice is
/// lost), so the value never goes negative.
pub fn tree_cpu_percent(prev: &HashMap<i32, u64>, cur: &[ProcInfo], wall_ms: u64) -> f64 {
    let delta: u64 = cur
        .iter()
        .map(|p| {
            p.cpu_time_ms
                .saturating_sub(prev.get(&p.pid).copied().unwrap_or(0))
        })
        .sum();
    cpu_percent((0, 0), (delta, wall_ms))
}

/// Metrics state of one stem.
struct StemEntry {
    history: MetricsHistory,
    /// Generation the entry's CPU baseline and trackers belong to.
    generation: u64,
    /// The stem is running (else `latest` is not shown).
    running: bool,
    /// Per-pid CPU time at the previous reading.
    prev: Option<(Instant, HashMap<i32, u64>)>,
    limits: Limits,
    trackers: Vec<ThresholdTracker>,
    ports: Vec<u16>,
    ports_at: Option<Instant>,
    /// A docker stats call is running.
    in_flight: bool,
}

impl StemEntry {
    fn new(capacity: usize) -> Self {
        Self {
            history: MetricsHistory::with_capacity(capacity),
            generation: u64::MAX,
            running: false,
            prev: None,
            limits: Limits::default(),
            trackers: Vec::new(),
            ports: Vec::new(),
            ports_at: None,
            in_flight: false,
        }
    }

    fn set_limits(&mut self, limits: &Limits) {
        if self.limits != *limits {
            self.limits = limits.clone();
            self.trackers = Threshold::from_limits(limits)
                .into_iter()
                .map(ThresholdTracker::new)
                .collect();
        }
    }
}

/// What one sample of a stem needs besides the reading.
#[derive(Clone, Debug)]
pub struct SampleCtx {
    /// Start generation of the reading.
    pub generation: u64,
    /// Seconds since the current process started.
    pub uptime_s: u64,
    /// Restarts so far.
    pub restarts: u32,
    /// The stem's `limits:`.
    pub limits: Limits,
    /// History capacity for a new entry.
    pub capacity: usize,
}

/// Per-stem metrics, shared by the sampler, `status` and the RPC.
#[derive(Default)]
pub struct MetricsStore {
    stems: Mutex<HashMap<String, StemEntry>>,
    persist: Mutex<Persister>,
    disk: Mutex<HashMap<String, (Instant, DiskUsage)>>,
}

impl MetricsStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, StemEntry>> {
        self.stems.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a reading taken at `now`; returns the sample and any threshold
    /// changes. A reading of an older generation than the entry's is
    /// dropped; a newer one resets the CPU baseline and the trackers.
    pub fn record(
        &self,
        stem: &str,
        ctx: &SampleCtx,
        reading: Reading,
        now: Instant,
        ts: DateTime<Utc>,
    ) -> Option<(Sample, Vec<(Threshold, ThresholdEvent)>)> {
        let mut map = self.lock();
        let e = map
            .entry(stem.to_string())
            .or_insert_with(|| StemEntry::new(ctx.capacity));
        if e.generation != ctx.generation {
            if e.generation != u64::MAX && ctx.generation < e.generation {
                return None;
            }
            e.generation = ctx.generation;
            e.prev = None;
            e.trackers.iter_mut().for_each(ThresholdTracker::reset);
        }
        e.running = true;
        e.set_limits(&ctx.limits);
        let (cpu_pct, rss_bytes, children) = match reading {
            Reading::Tree(tree) => {
                if tree.is_empty() {
                    return None;
                }
                let cpu = e.prev.as_ref().map_or(0.0, |(at, prev)| {
                    let wall = now.saturating_duration_since(*at).as_millis() as u64;
                    tree_cpu_percent(prev, &tree, wall)
                });
                e.prev = Some((now, tree.iter().map(|p| (p.pid, p.cpu_time_ms)).collect()));
                (
                    cpu,
                    tree.iter().map(|p| p.rss_bytes).sum(),
                    u32::try_from(tree.len()).unwrap_or(u32::MAX),
                )
            }
            Reading::Container(s) => (s.cpu_pct, s.mem_bytes, s.pids),
        };
        let sample = Sample {
            ts,
            cpu_pct: (cpu_pct * 10.0).round() / 10.0,
            rss_bytes,
            children,
            uptime_s: ctx.uptime_s,
            restarts: ctx.restarts,
        };
        e.history.push(sample.clone());
        let changes = e
            .trackers
            .iter_mut()
            .filter_map(|t| t.observe(&sample, now).map(|ev| (*t.threshold(), ev)))
            .collect();
        Some((sample, changes))
    }

    /// The stem is not running: hide `latest`, forget the CPU baseline and
    /// the crossed thresholds.
    pub fn idle(&self, stem: &str) {
        if let Some(e) = self.lock().get_mut(stem) {
            e.running = false;
            e.prev = None;
            e.ports.clear();
            e.ports_at = None;
            e.trackers.iter_mut().for_each(ThresholdTracker::reset);
        }
    }

    /// Descriptions of the crossed thresholds of `stem` (`memory > 100MB`),
    /// for `DegradedInputs::metrics`.
    pub fn crossed(&self, stem: &str) -> Vec<String> {
        self.lock().get(stem).map_or_else(Vec::new, |e| {
            e.trackers
                .iter()
                .filter(|t| t.is_crossed())
                .map(|t| t.threshold().describe())
                .collect()
        })
    }

    /// The newest sample of a running stem.
    pub fn latest(&self, stem: &str) -> Option<Sample> {
        let map = self.lock();
        let e = map.get(stem)?;
        e.running.then(|| e.history.latest().cloned()).flatten()
    }

    /// Fill `metrics` of every running `status` entry.
    pub fn decorate(&self, stems: &mut [StemStatus]) {
        for s in stems {
            s.metrics = if is_running(s.state) {
                self.latest(&s.name).as_ref().map(MetricsSummary::of)
            } else {
                None
            };
        }
    }

    /// Whether open ports should be refreshed (and mark them as being so).
    fn ports_due(&self, stem: &str, now: Instant) -> bool {
        let mut map = self.lock();
        let Some(e) = map.get_mut(stem) else {
            return false;
        };
        if e.ports_at
            .is_some_and(|t| now.saturating_duration_since(t) < PORTS_TTL)
        {
            return false;
        }
        e.ports_at = Some(now);
        true
    }

    fn set_ports(&self, stem: &str, ports: Vec<u16>) {
        if let Some(e) = self.lock().get_mut(stem) {
            e.ports = ports;
        }
    }

    /// Claim the docker stats slot of `stem` (`false`: a call is running).
    fn begin_stats(&self, stem: &str, capacity: usize) -> bool {
        let mut map = self.lock();
        let e = map
            .entry(stem.to_string())
            .or_insert_with(|| StemEntry::new(capacity));
        !std::mem::replace(&mut e.in_flight, true)
    }

    fn end_stats(&self, stem: &str) {
        if let Some(e) = self.lock().get_mut(stem) {
            e.in_flight = false;
        }
    }

    /// The RPC view of one stem.
    fn view(
        &self,
        stem: &Stem,
        state: StemState,
        since: Option<DateTime<Utc>>,
        last: Option<usize>,
    ) -> StemMetrics {
        let map = self.lock();
        let e = map.get(&stem.name);
        let limits = Threshold::from_limits(&stem.limits)
            .into_iter()
            .map(|t| LimitStatus {
                metric: t.metric.as_str().to_string(),
                limit: t.limit,
                for_s: t.for_secs,
                crossed: e.is_some_and(|e| {
                    e.trackers
                        .iter()
                        .any(|k| k.is_crossed() && k.threshold() == &t)
                }),
            })
            .collect();
        let history = match (since, last) {
            (Some(since), _) => Some(e.map_or_else(Vec::new, |e| {
                e.history.slice(since).into_iter().cloned().collect()
            })),
            (None, Some(n)) => Some(e.map_or_else(Vec::new, |e| {
                e.history.last_n(n).into_iter().cloned().collect()
            })),
            (None, None) => None,
        };
        StemMetrics {
            name: stem.name.clone(),
            kind: stem.kind().to_string(),
            state,
            latest: e
                .filter(|e| e.running && is_running(state))
                .and_then(|e| e.history.latest().cloned()),
            history,
            open_ports: e.map(|e| e.ports.clone()).unwrap_or_default(),
            limits,
            disk: None,
        }
    }

    /// Append `sample` to the stem's NDJSON file under `dir`.
    fn persist(&self, dir: &Path, stem: &str, sample: &Sample) {
        let mut p = self.persist.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = p.append(dir, stem, sample) {
            tracing::warn!(stem, error = %e, "cannot persist metrics sample");
        }
    }
}

// ------------------------------------------------------------------ persist

/// Appends samples to `<dir>/<stem>.ndjson`, rotating daily.
#[derive(Default)]
struct Persister {
    files: HashMap<String, (NaiveDate, std::fs::File)>,
}

impl Persister {
    fn append(&mut self, dir: &Path, stem: &str, sample: &Sample) -> std::io::Result<()> {
        let today = sample.ts.date_naive();
        let current = dir.join(format!("{stem}.ndjson"));
        let open_date = match self.files.get(stem) {
            Some((d, _)) => Some(*d),
            None => std::fs::metadata(&current)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| DateTime::<Utc>::from(t).date_naive()),
        };
        if let Some(d) = open_date
            && d != today
        {
            self.files.remove(stem);
            if current.exists() {
                std::fs::rename(&current, dir.join(format!("{stem}-{d}.ndjson")))?;
                prune_rotated(dir, stem);
            }
        }
        if !self.files.contains_key(stem) {
            std::fs::create_dir_all(dir)?;
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&current)?;
            self.files.insert(stem.to_string(), (today, f));
        }
        let (_, f) = self.files.get_mut(stem).expect("opened above");
        let mut line = serde_json::to_vec(sample).map_err(std::io::Error::other)?;
        line.push(b'\n');
        f.write_all(&line)
    }
}

/// Rotated files of `stem` (`<stem>-YYYY-MM-DD.ndjson`), oldest first.
pub fn rotated_files(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(NaiveDate, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let date = name
                .strip_prefix(stem)?
                .strip_prefix('-')?
                .strip_suffix(".ndjson")?;
            let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            Some((d, e.path()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

fn prune_rotated(dir: &Path, stem: &str) {
    let files = rotated_files(dir, stem);
    for p in files.iter().take(files.len().saturating_sub(KEEP_ROTATED)) {
        let _ = std::fs::remove_file(p);
    }
}

// ------------------------------------------------------------------ sampler

/// A state in which the stem has a process (or container) to sample.
fn is_running(state: StemState) -> bool {
    !matches!(state, StemState::Stopped | StemState::Failed)
}

/// `metrics.interval` of `ws` (at least 100 ms).
pub fn interval_of(ws: &Resolved) -> Duration {
    ws.workspace
        .metrics
        .interval
        .as_duration()
        .max(MIN_INTERVAL)
}

/// One running stem to sample.
struct Target {
    name: String,
    handle: Handle,
    runtime: Option<Arc<dyn Runtime>>,
    ctx: SampleCtx,
}

/// Start the sampler of `sup` (production daemons only; it ends with the
/// supervisor).
pub(crate) fn spawn_sampler(sup: &Arc<Supervisor>) {
    let weak = Arc::downgrade(sup);
    tokio::spawn(sampler(weak));
}

async fn sampler(weak: Weak<Supervisor>) {
    loop {
        let every = {
            let Some(sup) = weak.upgrade() else {
                return;
            };
            match sup.host.resolved() {
                Some(ws) => {
                    sup.sample_once(&ws).await;
                    interval_of(&ws)
                }
                None => DEFAULT_INTERVAL,
            }
        };
        tokio::time::sleep(every).await;
    }
}

impl Supervisor {
    /// One sampler pass over the running stems.
    async fn sample_once(&self, ws: &Arc<Resolved>) {
        let store = &self.core.metrics;
        let capacity =
            MetricsHistory::capacity_for(MetricsHistory::DEFAULT_WINDOW, interval_of(ws));
        let mut targets = Vec::new();
        for cell in self.existing_cells() {
            let Some(stem) = ws.workspace.stem(&cell.name).filter(|s| s.enabled) else {
                store.idle(&cell.name);
                continue;
            };
            let info = cell.info();
            let running = is_running(info.state);
            match (&info.handle, running) {
                (Some(h), true) if !h.is_external() => targets.push(Target {
                    name: cell.name.clone(),
                    handle: h.clone(),
                    runtime: info.runtime.clone(),
                    ctx: SampleCtx {
                        generation: info.generation,
                        uptime_s: info.started_at.map_or(0, |(i, _)| i.elapsed().as_secs()),
                        restarts: info.restarts,
                        limits: stem.limits.clone(),
                        capacity,
                    },
                }),
                _ => {
                    drop(info);
                    store.idle(&cell.name);
                }
            }
        }
        if targets.is_empty() {
            return;
        }
        let persist_dir = ws
            .workspace
            .metrics
            .persist
            .then(|| self.host.data_dir())
            .flatten()
            .map(|d| d.join("metrics"));
        let now = Instant::now();
        // Open ports, refreshed in the background every PORTS_TTL.
        for t in &targets {
            if let Some(rt) = t.runtime.clone()
                && store.ports_due(&t.name, now)
            {
                let (core, name, h) = (self.core.clone(), t.name.clone(), t.handle.clone());
                tokio::spawn(async move {
                    if let Ok(Ok(f)) = tokio::time::timeout(STATS_TIMEOUT, rt.describe(&h)).await {
                        core.metrics.set_ports(&name, f.ports);
                    }
                });
            }
        }
        let (procs, containers): (Vec<Target>, Vec<Target>) = targets
            .into_iter()
            .partition(|t| t.handle.container_id().is_none());
        // Containers: one stats call each, in the background.
        let docker = self
            .core
            .runtimes
            .containers()
            .and_then(|c| c.docker_if_connected());
        for t in containers {
            let (Some(docker), Some(id)) = (docker.clone(), t.handle.container_id()) else {
                continue;
            };
            if !store.begin_stats(&t.name, capacity) {
                continue;
            }
            let (core, id, dir) = (self.core.clone(), id.to_string(), persist_dir.clone());
            tokio::spawn(async move {
                let r = tokio::time::timeout(STATS_TIMEOUT, docker.stats(&id)).await;
                core.metrics.end_stats(&t.name);
                match r {
                    Ok(Ok(Some(s))) => {
                        record(
                            &core,
                            &t.name,
                            &t.ctx,
                            Reading::Container(s),
                            dir.as_deref(),
                        );
                    }
                    Ok(Err(e)) => tracing::debug!(stem = %t.name, error = %e, "docker stats"),
                    _ => {}
                }
            });
        }
        // Processes: every tree in one blocking call.
        if procs.is_empty() {
            return;
        }
        let pgids: Vec<i32> = procs.iter().map(|t| t.handle.pgid()).collect();
        let Ok(trees) = tokio::task::spawn_blocking(move || {
            pgids
                .into_iter()
                .map(stems_runtime::os::process_tree)
                .collect::<Vec<_>>()
        })
        .await
        else {
            return;
        };
        for (t, tree) in procs.iter().zip(trees) {
            record(
                &self.core,
                &t.name,
                &t.ctx,
                Reading::Tree(tree),
                persist_dir.as_deref(),
            );
        }
    }

    /// `metrics {stems?, history_ms?, last?, sort?, disk?}`.
    pub async fn metrics(&self, p: MetricsParams, actor: &str) -> Result<MetricsResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            super::schedule::plan(&ws, &p.stems, true)?;
        }
        let since = p
            .history_ms
            .map(|ms| Utc::now() - chrono::Duration::milliseconds(ms as i64));
        let mut stems: Vec<StemMetrics> = ws
            .workspace
            .stems()
            .filter(|s| p.stems.is_empty() || p.stems.contains(&s.name))
            .map(|s| {
                let state = self.cell(&s.name).state();
                self.core.metrics.view(s, state, since, p.last)
            })
            .collect();
        if p.disk {
            for m in &mut stems {
                if let Some(stem) = ws.workspace.stem(&m.name) {
                    m.disk = Some(self.disk_usage(&ws, stem).await);
                }
            }
        }
        if let Some(sort) = p.sort {
            sort_stems(&mut stems, sort);
        }
        let totals = MetricsTotals::of(&stems);
        Ok(MetricsResult {
            interval_ms: interval_of(&ws).as_millis() as u64,
            stems,
            totals,
        })
    }

    /// Disk usage of `stem` (cached [`DISK_TTL`]).
    async fn disk_usage(&self, ws: &Resolved, stem: &Stem) -> DiskUsage {
        let now = Instant::now();
        {
            let cache = self
                .core
                .metrics
                .disk
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some((at, d)) = cache.get(&stem.name)
                && now.saturating_duration_since(*at) < DISK_TTL
            {
                return d.clone();
            }
        }
        let root = stem.codebase.as_ref().map(|c| c.path().to_path_buf());
        let mut usage = match root {
            Some(root) => tokio::task::spawn_blocking(move || build_dirs_usage(&root))
                .await
                .unwrap_or_default(),
            None => DiskUsage::default(),
        };
        let volumes = super::containers::named_volumes(&ws.workspace, stem);
        if !volumes.is_empty()
            && let Some(c) = self.core.runtimes.containers()
        {
            let docker = match c.docker_if_connected() {
                Some(d) => Some(d),
                None => tokio::time::timeout(Duration::from_secs(2), c.docker())
                    .await
                    .ok()
                    .and_then(Result::ok),
            };
            if let Some(d) = docker
                && let Ok(Ok(sizes)) =
                    tokio::time::timeout(Duration::from_secs(10), d.volume_sizes(&volumes)).await
            {
                usage.volumes_bytes = Some(sizes.values().sum());
            }
        }
        self.core
            .metrics
            .disk
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(stem.name.clone(), (now, usage.clone()));
        usage
    }
}

/// Record a reading: history, persistence, threshold events.
fn record(core: &Core, stem: &str, ctx: &SampleCtx, reading: Reading, persist: Option<&Path>) {
    let Some((sample, changes)) =
        core.metrics
            .record(stem, ctx, reading, Instant::now(), Utc::now())
    else {
        return;
    };
    if let Some(dir) = persist {
        core.metrics.persist(dir, stem, &sample);
    }
    for (t, ev) in changes {
        let (state, value) = match ev {
            ThresholdEvent::Crossed { value, .. } => ("crossed", value),
            ThresholdEvent::Cleared { value, .. } => ("cleared", value),
        };
        core.events.emit(
            EventDraft::new(EventKind::STEM_THRESHOLD, stems_api::DAEMON_ACTOR)
                .stem(stem)
                .reason(format!("{} {state}", t.describe()))
                .data(json!({
                    "metric": t.metric.as_str(),
                    "value": value,
                    "limit": t.limit,
                    "for_s": t.for_secs,
                    "state": state,
                })),
        );
    }
}

/// Sizes of the [`BUILD_DIRS`] directly under `root` (a bounded walk that
/// does not follow symlinks; apparent file sizes).
pub fn build_dirs_usage(root: &Path) -> DiskUsage {
    let mut usage = DiskUsage::default();
    let mut budget = DISK_WALK_LIMIT;
    for name in BUILD_DIRS {
        let dir = root.join(name);
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let bytes = dir_size(&dir, &mut budget);
        usage.codebase_build_bytes += bytes;
        usage.dirs.push(DiskDir {
            path: dir.display().to_string(),
            bytes,
        });
    }
    usage.truncated = budget == 0;
    usage
}

fn dir_size(dir: &Path, budget: &mut usize) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = HashSet::new();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            if *budget == 0 {
                return total;
            }
            *budget -= 1;
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else if m.is_file() {
                use std::os::unix::fs::MetadataExt;
                // Hard links count once.
                if m.nlink() <= 1 || seen.insert((m.dev(), m.ino())) {
                    total += m.len();
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, cpu_ms: u64, rss: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: 1,
            pgid: 100,
            cpu_time_ms: cpu_ms,
            rss_bytes: rss,
            command: "python3".into(),
        }
    }

    fn ctx(generation: u64, limits: Limits) -> SampleCtx {
        SampleCtx {
            generation,
            uptime_s: 5,
            restarts: 1,
            limits,
            capacity: 10,
        }
    }

    #[test]
    fn tree_cpu_counts_growth_new_and_not_exited() {
        let prev: HashMap<i32, u64> = [(1, 1000), (2, 500), (3, 700)].into();
        // pid 1 +400 ms, pid 2 +100 ms, pid 3 exited, pid 4 new with 250 ms.
        let cur = [proc(1, 1400, 0), proc(2, 600, 0), proc(4, 250, 0)];
        assert_eq!(tree_cpu_percent(&prev, &cur, 1000), 75.0);
        // Two busy cores.
        let cur = [proc(1, 3000, 0), proc(2, 2500, 0)];
        assert_eq!(tree_cpu_percent(&prev, &cur, 2000), 200.0);
        assert_eq!(tree_cpu_percent(&prev, &cur, 0), 0.0);
        // A counter going backwards (pid reuse) counts as 0, not negative.
        assert_eq!(tree_cpu_percent(&prev, &[proc(1, 10, 0)], 1000), 0.0);
    }

    #[test]
    fn sampler_samples_a_fake_tree() {
        let store = MetricsStore::default();
        let t0 = Instant::now();
        let ts = Utc::now();
        let c = ctx(1, Limits::default());
        let (s, ev) = store
            .record(
                "api",
                &c,
                Reading::Tree(vec![proc(10, 1000, 30 << 20)]),
                t0,
                ts,
            )
            .unwrap();
        assert_eq!((s.cpu_pct, s.rss_bytes, s.children), (0.0, 30 << 20, 1));
        assert_eq!((s.uptime_s, s.restarts), (5, 1));
        assert!(ev.is_empty());
        let tree = vec![proc(10, 1500, 30 << 20), proc(11, 500, 20 << 20)];
        let (s, _) = store
            .record(
                "api",
                &c,
                Reading::Tree(tree),
                t0 + Duration::from_secs(2),
                ts,
            )
            .unwrap();
        assert_eq!(s.cpu_pct, 50.0);
        assert_eq!((s.rss_bytes, s.children), (50 << 20, 2));
        assert_eq!(store.latest("api").unwrap(), s);
        // Empty tree: no sample.
        assert!(
            store
                .record("api", &c, Reading::Tree(vec![]), t0, ts)
                .is_none()
        );
        // An older generation is dropped; a newer resets the CPU baseline.
        assert!(
            store
                .record(
                    "api",
                    &ctx(0, Limits::default()),
                    Reading::Tree(vec![proc(1, 1, 1)]),
                    t0,
                    ts
                )
                .is_none()
        );
        let (s, _) = store
            .record(
                "api",
                &ctx(2, Limits::default()),
                Reading::Tree(vec![proc(12, 9000, 1)]),
                t0 + Duration::from_secs(3),
                ts,
            )
            .unwrap();
        assert_eq!(s.cpu_pct, 0.0);
        store.idle("api");
        assert!(store.latest("api").is_none());
        // Containers take the stats as they are.
        let (s, _) = store
            .record(
                "db",
                &c,
                Reading::Container(ContainerStats {
                    cpu_pct: 12.34,
                    mem_bytes: 7,
                    pids: 3,
                }),
                t0,
                ts,
            )
            .unwrap();
        assert_eq!((s.cpu_pct, s.rss_bytes, s.children), (12.3, 7, 3));
    }

    #[test]
    fn thresholds_cross_and_clear() {
        let store = MetricsStore::default();
        let limits = Limits {
            memory: Some(stems_config::ByteSize(100 << 20)),
            ..Limits::default()
        };
        let c = ctx(1, limits);
        let t0 = Instant::now();
        let ts = Utc::now();
        let rec = |mb: u64| {
            store
                .record("api", &c, Reading::Tree(vec![proc(1, 0, mb << 20)]), t0, ts)
                .unwrap()
                .1
        };
        assert!(rec(50).is_empty());
        let ev = rec(150);
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0].1, ThresholdEvent::Crossed { .. }));
        assert_eq!(store.crossed("api"), ["memory > 100MB"]);
        assert!(rec(95).is_empty(), "hysteresis keeps it crossed");
        assert!(matches!(rec(10)[0].1, ThresholdEvent::Cleared { .. }));
        assert!(store.crossed("api").is_empty());
        rec(150);
        store.idle("api");
        assert!(
            store.crossed("api").is_empty(),
            "a stopped stem is not degraded"
        );
    }

    #[test]
    fn persist_appends_and_rotates_daily() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Persister::default();
        let day = |d: u32| Sample {
            ts: chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 9, d, 10, 0, 0).unwrap(),
            cpu_pct: 1.0,
            rss_bytes: 2,
            children: 1,
            uptime_s: 3,
            restarts: 0,
        };
        p.append(dir.path(), "api", &day(1)).unwrap();
        p.append(dir.path(), "api", &day(1)).unwrap();
        let current = dir.path().join("api.ndjson");
        let text = std::fs::read_to_string(&current).unwrap();
        assert_eq!(text.lines().count(), 2);
        let back: Sample = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(back, day(1));
        p.append(dir.path(), "api", &day(2)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&current).unwrap().lines().count(),
            1
        );
        assert!(dir.path().join("api-2026-09-01.ndjson").exists());
        for d in 3..=12 {
            p.append(dir.path(), "api", &day(d)).unwrap();
        }
        let rotated = rotated_files(dir.path(), "api");
        assert_eq!(rotated.len(), KEEP_ROTATED);
        assert!(rotated[0].ends_with("api-2026-09-05.ndjson"), "{rotated:?}");
        assert!(rotated_files(dir.path(), "ap").is_empty());
    }

    #[test]
    fn build_dirs_are_measured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dist/js")).unwrap();
        std::fs::write(dir.path().join("dist/js/app.js"), vec![0u8; 1500]).unwrap();
        std::fs::write(dir.path().join("dist/index.html"), vec![0u8; 500]).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), vec![0u8; 9999]).unwrap();
        let u = build_dirs_usage(dir.path());
        assert_eq!(u.codebase_build_bytes, 2000);
        assert_eq!(u.dirs.len(), 1);
        assert!(!u.truncated);
        assert_eq!(
            build_dirs_usage(&dir.path().join("missing")),
            DiskUsage::default()
        );
    }
}
