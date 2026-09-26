//! Log capture (deliverable 12, `docs/logs.md`, FR-LG-1/2/4/5).
//!
//! * [`LogHub`] — one per daemon. Implements the supervisor's
//!   [`OutputSink`]: every started unit's [`OutputStream`] is forwarded to
//!   its stem's [`LogSink`]. Also answers queries (`query_logs`), feeds live
//!   subscribers (`subscribe_logs`) and hands out [`ScriptWriter`]s (16/17).
//! * [`LogSink`] — one per stem, created on first use: a bounded channel into
//!   **one writer task** that owns the stem's rotating file
//!   `<daemon dir>/logs/<stem>/current.log` and is the only writer of the
//!   in-memory ring, so both see records in the same order.
//!
//! Each line is parsed with [`stems_core::logs::parse_line`] (level
//! detection, JSON fields); the timestamp is the daemon's receive time. The
//! file holds one [`LogRecord`] JSON object per line, so replay from files
//! (`--since` older than the ring, or a daemon restarted after a crash) is
//! exact. Writes are batched (everything queued is written in one go) and
//! flushed once the stem has been idle for [`FLUSH_IDLE`], or at the latest
//! every [`FLUSH_IDLE`] under continuous output. Files are written whether or
//! not any client is attached. Size rotation follows workspace `logs:`
//! (`max_size`, `keep`) through [`stems_core::logs::rotate_plan`].

pub mod export;

pub use stems_core::logs::{LogQuery, LogRecord};

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::json;
use stems_api::{EventKind, LogFilter, QUERY_LOGS_CAP, QueryLogsParams, QueryLogsResult};
use stems_core::logs::{
    Level, LevelFilter, RingBuffer, RotateStep, RotationPolicy, SinceSpec, Stream, parse_line,
    rotate_plan,
};
use stems_core::{Error, ErrorCode};
use stems_runtime::{OutputEvent, OutputStream, OutputStreamKind};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::events::EventBus;
use crate::supervisor::OutputSink;

/// Directory under the daemon dir holding one sub-directory per stem.
pub const LOGS_DIR: &str = "logs";
/// Flush a stem's file after this much idle time (and at least this often).
pub const FLUSH_IDLE: Duration = Duration::from_millis(100);
/// Queued lines per stem before the forwarder waits (the process's output
/// channel then lags and the gap is recorded as a `dropped` record).
pub const SINK_QUEUE: usize = 8192;
/// Live-channel capacity per `subscribe_logs` client before it lags.
pub const LIVE_CAPACITY: usize = 4096;
/// Most messages handled per writer batch before yielding.
const BATCH_MAX: usize = 1024;
/// Text of the record noted when a stem is adopted (live output is gone).
pub const ADOPTED_TEXT: &str = "[stems] adopted: live output unavailable, showing file log";

/// Settings from workspace `logs:`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogSettings {
    /// Records kept in memory per stem.
    pub ring: usize,
    /// File rotation.
    pub rotation: RotationPolicy,
}

impl LogSettings {
    /// From the resolved workspace `logs:` block.
    pub fn from_config(logs: &stems_config::Logs) -> Self {
        Self {
            ring: logs.ring as usize,
            rotation: RotationPolicy::from_config(logs),
        }
    }
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            ring: stems_config::defaults::LOGS_RING as usize,
            rotation: RotationPolicy {
                max_size: stems_config::defaults::LOGS_MAX_SIZE,
                keep: stems_config::defaults::LOGS_KEEP as usize,
            },
        }
    }
}

/// A record as broadcast to live subscribers, with its per-stem sequence
/// number (position in the stem's ring) for de-duplicating a replay.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveRecord {
    /// Sequence number within the stem's ring.
    pub seq: u64,
    /// The record.
    pub record: LogRecord,
}

enum Msg {
    /// A raw line to parse.
    Line {
        stream: Stream,
        tag: Option<String>,
        text: String,
        ts: DateTime<Utc>,
    },
    /// A ready record (synthetic notes).
    Record(LogRecord),
    /// New rotation settings.
    Configure(RotationPolicy),
    /// Flush the file, then reply.
    Flush(oneshot::Sender<()>),
}

/// One stem's sink: the sending side of its writer task plus the ring the
/// task fills.
pub struct LogSink {
    stem: String,
    tx: mpsc::Sender<Msg>,
    ring: Arc<Mutex<RingBuffer<LogRecord>>>,
    /// The stem's directory already held log files when the sink was
    /// created (history from a previous daemon run).
    had_history: bool,
}

impl LogSink {
    /// The stem.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, RingBuffer<LogRecord>> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A writer for a script run's output (`stream: script`, `tag: script`).
    pub fn script_writer(&self, script: &str) -> ScriptWriter {
        ScriptWriter {
            stem: self.stem.clone(),
            script: script.to_string(),
            tx: self.tx.clone(),
        }
    }
}

/// Writes a script's output lines into its stem's log (FR-SC-4 hook for
/// 16/17). Cheap to clone; lines keep their order per writer.
#[derive(Clone, Debug)]
pub struct ScriptWriter {
    stem: String,
    script: String,
    tx: mpsc::Sender<Msg>,
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Msg")
    }
}

impl ScriptWriter {
    /// The stem.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    /// The script name (the records' `tag`).
    pub fn script(&self) -> &str {
        &self.script
    }

    fn msg(&self, text: String) -> Msg {
        Msg::Line {
            stream: Stream::Script,
            tag: Some(self.script.clone()),
            text,
            ts: Utc::now(),
        }
    }

    /// Record one line (waits while the stem's queue is full). `false` once
    /// the daemon's log writer is gone.
    pub async fn line(&self, text: impl Into<String>) -> bool {
        self.tx.send(self.msg(text.into())).await.is_ok()
    }

    /// Record one line without waiting; `false` if the queue is full or gone.
    pub fn try_line(&self, text: impl Into<String>) -> bool {
        self.tx.try_send(self.msg(text.into())).is_ok()
    }

    /// Forward a script's output stream (stdout and stderr) until it ends.
    pub async fn forward(&self, mut stream: OutputStream) {
        while let Some(ev) = stream.recv_event().await {
            let msg = match ev {
                OutputEvent::Line(l) => Msg::Line {
                    stream: Stream::Script,
                    tag: Some(self.script.clone()),
                    text: l.text,
                    ts: DateTime::<Utc>::from(l.ts),
                },
                OutputEvent::Dropped(n) => Msg::Record(note(
                    &self.stem,
                    Stream::Script,
                    Some(&self.script),
                    format!("[stems] dropped {n} lines (output storm)"),
                )),
            };
            if self.tx.send(msg).await.is_err() {
                return;
            }
        }
    }
}

/// A synthetic record written by stems itself.
fn note(stem: &str, stream: Stream, tag: Option<&str>, text: String) -> LogRecord {
    LogRecord {
        ts: Utc::now(),
        stem: stem.to_string(),
        stream,
        tag: tag.map(str::to_string),
        level: Some(Level::Warn),
        text,
        fields: None,
    }
}

/// Options of [`LogHub::query`].
#[derive(Clone, Copy, Debug)]
pub struct QueryOptions {
    /// Read the files even when the ring covers the window.
    pub from_files: bool,
    /// Most records returned (the newest are kept).
    pub cap: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            from_files: false,
            cap: QUERY_LOGS_CAP,
        }
    }
}

/// Result of [`LogHub::query`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryOutcome {
    /// Matching records, oldest first, interleaved by `ts`.
    pub records: Vec<LogRecord>,
    /// More matched than `cap`.
    pub truncated: bool,
    /// Per stem: the ring sequence number the next record will get at the
    /// time of the query (live records below it were part of the result's
    /// window already).
    pub next_seq: HashMap<String, u64>,
}

/// All stems' log sinks of one daemon (see the module docs).
pub struct LogHub {
    dir: PathBuf,
    settings: Mutex<LogSettings>,
    sinks: Mutex<HashMap<String, Arc<LogSink>>>,
    live: broadcast::Sender<LiveRecord>,
    adopted_noted: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for LogHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogHub").field("dir", &self.dir).finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl LogHub {
    /// A hub writing under `dir` (`<daemon dir>/logs`). Sinks and their
    /// tasks are created on first use (inside a tokio runtime).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let (live, _) = broadcast::channel(LIVE_CAPACITY);
        Self {
            dir: dir.into(),
            settings: Mutex::new(LogSettings::default()),
            sinks: Mutex::new(HashMap::new()),
            live,
            adopted_noted: Mutex::new(HashSet::new()),
        }
    }

    /// The logs directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A stem's log directory.
    pub fn stem_dir(&self, stem: &str) -> PathBuf {
        self.dir.join(stem)
    }

    /// Apply workspace `logs:` settings: rotation applies to existing sinks
    /// at their next write; the ring size applies to sinks created later.
    pub fn configure(&self, settings: LogSettings) {
        *lock(&self.settings) = settings;
        for s in lock(&self.sinks).values() {
            let _ = s.tx.try_send(Msg::Configure(settings.rotation));
        }
    }

    /// The sink of `stem`, created (with its writer task) on first use.
    pub fn sink(&self, stem: &str) -> Arc<LogSink> {
        let mut sinks = lock(&self.sinks);
        if let Some(s) = sinks.get(stem) {
            return s.clone();
        }
        let settings = *lock(&self.settings);
        let dir = self.stem_dir(stem);
        let had_history = !list_log_files(&dir).is_empty();
        let ring = Arc::new(Mutex::new(RingBuffer::new(settings.ring)));
        let (tx, rx) = mpsc::channel(SINK_QUEUE);
        let writer = Writer {
            stem: stem.to_string(),
            file: LogFile::new(dir),
            policy: settings.rotation,
            ring: ring.clone(),
            live: self.live.clone(),
        };
        tokio::spawn(writer.run(rx));
        let sink = Arc::new(LogSink {
            stem: stem.to_string(),
            tx,
            ring,
            had_history,
        });
        sinks.insert(stem.to_string(), sink.clone());
        sink
    }

    fn existing_sink(&self, stem: &str) -> Option<Arc<LogSink>> {
        lock(&self.sinks).get(stem).cloned()
    }

    /// A writer for the output of `script` run for `stem` (16/17).
    pub fn script_writer(&self, stem: &str, script: &str) -> ScriptWriter {
        self.sink(stem).script_writer(script)
    }

    /// Note that `stem` was adopted from a previous daemon run: its live
    /// output is not available, only what its files hold. Idempotent until
    /// the stem is started again (11 calls this; the `stem.adopted` event
    /// does too, see [`LogHub::watch_events`]).
    pub fn note_adopted(&self, stem: &str) {
        if !lock(&self.adopted_noted).insert(stem.to_string()) {
            return;
        }
        let rec = note(stem, Stream::Err, None, ADOPTED_TEXT.to_string());
        let _ = self.sink(stem).tx.try_send(Msg::Record(rec));
    }

    /// Follow the event bus and [`note_adopted`](Self::note_adopted) every
    /// `stem.adopted`. Call once, inside the runtime.
    pub fn watch_events(self: &Arc<Self>, bus: &EventBus) {
        let mut rx = bus.subscribe();
        let hub: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if ev.kind == EventKind::STEM_ADOPTED
                            && let Some(stem) = &ev.stem
                        {
                            let Some(h) = hub.upgrade() else { return };
                            h.note_adopted(stem);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }

    /// A live receiver of every stem's records (filter by `record.stem`).
    pub fn subscribe(&self) -> broadcast::Receiver<LiveRecord> {
        self.live.subscribe()
    }

    /// Open `subscribe_logs` receivers.
    pub fn subscriber_count(&self) -> usize {
        self.live.receiver_count()
    }

    /// Flush every sink's file (waits for the writers).
    pub async fn flush_all(&self) {
        let sinks: Vec<_> = lock(&self.sinks).values().cloned().collect();
        for s in sinks {
            let (tx, rx) = oneshot::channel();
            if s.tx.send(Msg::Flush(tx)).await.is_ok() {
                let _ = rx.await;
            }
        }
    }

    /// Stems that have a sink or a log directory, sorted.
    pub fn known_stems(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = lock(&self.sinks).keys().cloned().collect();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if e.path().is_dir()
                    && let Some(n) = e.file_name().to_str()
                {
                    set.insert(n.to_string());
                }
            }
        }
        set.into_iter().collect()
    }

    /// Records matching `q`: from each stem's ring, plus its files for the
    /// part of the window older than the ring (or the whole window when the
    /// ring is empty, e.g. for stems adopted or not started by this daemon
    /// run, or with `from_files`). Blocking file I/O: call from
    /// `spawn_blocking` in async code.
    pub fn query(&self, q: &LogQuery, opts: QueryOptions) -> QueryOutcome {
        let stems = if q.stems.is_empty() {
            self.known_stems()
        } else {
            q.stems.clone()
        };
        let keep = q.tail.unwrap_or(usize::MAX).min(opts.cap);
        let mut per_stem = Vec::new();
        let mut matched_total = 0usize;
        let mut next_seq = HashMap::new();
        for stem in &stems {
            let mut out: VecDeque<LogRecord> = VecDeque::new();
            let mut push = |r: LogRecord, n: &mut usize| {
                *n += 1;
                if keep == 0 {
                    return;
                }
                if out.len() == keep {
                    out.pop_front();
                }
                out.push_back(r);
            };
            let mut matched = 0usize;
            // Ring snapshot (and the file boundary) under the ring lock.
            let (ring_part, boundary, need_files) = match self.existing_sink(stem) {
                Some(sink) => {
                    let ring = sink.ring();
                    next_seq.insert(stem.clone(), ring.seq());
                    let first = ring.iter().next().map(|r| r.ts);
                    let older_exists = sink.had_history || ring.dropped() > 0;
                    let window_older = match (q.since, first) {
                        (_, None) => true,
                        (None, Some(_)) => true,
                        (Some(s), Some(f)) => s < f,
                    };
                    let need =
                        first.is_none() || (window_older && (older_exists || opts.from_files));
                    let part: Vec<LogRecord> =
                        ring.iter().filter(|r| q.matches(r)).cloned().collect();
                    (part, first, need)
                }
                None => (Vec::new(), None, true),
            };
            if need_files {
                for path in list_log_files(&self.stem_dir(stem)) {
                    read_records(&path, &mut |r| {
                        if boundary.is_none_or(|b| r.ts < b) && q.matches(&r) {
                            push(r, &mut matched);
                        }
                    });
                }
            }
            for r in ring_part {
                push(r, &mut matched);
            }
            matched_total += matched.min(q.tail.unwrap_or(usize::MAX));
            per_stem.push(out.into_iter().collect::<Vec<_>>());
        }
        let mut records = stems_core::logs::merge_by_ts(per_stem);
        let limit = q.tail.unwrap_or(usize::MAX).min(opts.cap);
        let truncated = matched_total.min(q.tail.unwrap_or(usize::MAX)) > opts.cap;
        if records.len() > limit {
            records.drain(..records.len() - limit);
        }
        QueryOutcome {
            records,
            truncated,
            next_seq,
        }
    }
}

impl OutputSink for LogHub {
    fn script_writer(&self, stem: &str, script: &str) -> Option<ScriptWriter> {
        Some(LogHub::script_writer(self, stem, script))
    }

    fn attach(&self, stem: &str, stream: Option<OutputStream>) {
        // Adopted units have no stream (their pipes died with the old daemon).
        let Some(mut stream) = stream else { return };
        lock(&self.adopted_noted).remove(stem);
        let tx = self.sink(stem).tx.clone();
        let stem = stem.to_string();
        tokio::spawn(async move {
            while let Some(ev) = stream.recv_event().await {
                let msg = match ev {
                    OutputEvent::Line(l) => Msg::Line {
                        stream: match l.stream {
                            OutputStreamKind::Out => Stream::Out,
                            OutputStreamKind::Err => Stream::Err,
                        },
                        tag: None,
                        text: l.text,
                        ts: DateTime::<Utc>::from(l.ts),
                    },
                    OutputEvent::Dropped(n) => Msg::Record(note(
                        &stem,
                        Stream::Err,
                        None,
                        format!("[stems] dropped {n} lines (output storm)"),
                    )),
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
        });
    }
}

/// Every log file of a stem directory, oldest first (`current.<n>.log` with
/// the highest `n` first, `current.log` last).
pub fn list_log_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(usize, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            let idx = if name == "current.log" {
                0
            } else {
                name.strip_prefix("current.")?
                    .strip_suffix(".log")?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)?
            };
            Some((idx, e.path()))
        })
        .collect();
    files.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
    files.into_iter().map(|(_, p)| p).collect()
}

/// Feed every parsable record of a log file to `f` (bad lines are skipped).
pub fn read_records(path: &Path, f: &mut dyn FnMut(LogRecord)) {
    let Ok(file) = File::open(path) else { return };
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { return };
        if let Ok(r) = serde_json::from_str::<LogRecord>(&line) {
            f(r);
        }
    }
}

/// Turn RPC filters into a [`LogQuery`], resolving relative times against
/// `now`. Bad values are `USAGE` errors.
pub fn to_query(f: &LogFilter, now: DateTime<Utc>) -> Result<LogQuery, Error> {
    let time = |flag: &str, v: &Option<String>| -> Result<Option<DateTime<Utc>>, Error> {
        v.as_deref()
            .map(|s| {
                SinceSpec::parse(s)
                    .map(|t| t.resolve(now))
                    .map_err(|e| usage(flag, e.to_string()))
            })
            .transpose()
    };
    let grep = f
        .grep
        .as_deref()
        .map(|g| regex::Regex::new(g).map_err(|e| usage("grep", format!("invalid regex: {e}"))))
        .transpose()?;
    let level = f
        .level
        .as_deref()
        .map(|l| LevelFilter::parse(l).map_err(|e| usage("level", e.to_string())))
        .transpose()?;
    Ok(LogQuery {
        stems: f.stems.clone(),
        since: time("since", &f.since)?,
        until: time("until", &f.until)?,
        grep,
        level,
        script: f.script.clone(),
        tail: f.tail,
    })
}

/// `query_logs`: validate, then query on a blocking thread.
pub async fn rpc_query(
    hub: &Arc<LogHub>,
    ws: Option<Arc<stems_config::Resolved>>,
    p: QueryLogsParams,
) -> Result<QueryLogsResult, Error> {
    let q = to_query(&p.filter, Utc::now())?;
    check_stems(hub, ws.as_deref(), &q.stems)?;
    let hub = hub.clone();
    let opts = QueryOptions {
        from_files: p.from_files,
        cap: QUERY_LOGS_CAP,
    };
    let out = tokio::task::spawn_blocking(move || hub.query(&q, opts))
        .await
        .map_err(|e| Error::internal(format!("log query failed: {e}")))?;
    Ok(QueryLogsResult {
        records: out.records,
        truncated: out.truncated,
    })
}

fn usage(flag: &str, msg: String) -> Error {
    Error::usage(
        format!("invalid --{flag}: {msg}"),
        "see `stems logs --help` (times: 10m, 1h30m, 2026-09-26T10:00:00Z; levels: error, warn+)",
    )
    .with_details(json!({ "flag": flag }))
}

/// `UNKNOWN_STEM` for requested stems that are neither in the workspace nor
/// have logs.
pub fn check_stems(
    hub: &LogHub,
    ws: Option<&stems_config::Resolved>,
    stems: &[String],
) -> Result<(), Error> {
    let known = hub.known_stems();
    for s in stems {
        let in_ws = ws.is_some_and(|r| r.workspace.stems.contains_key(s));
        if !in_ws && !known.contains(s) {
            let names: Vec<String> = ws
                .map(|r| r.workspace.stems.keys().cloned().collect())
                .unwrap_or_default();
            return Err(
                Error::new(ErrorCode::UnknownStem, format!("no stem named `{s}`"))
                    .with_hint(format!("stems in this workspace: {}", names.join(", ")))
                    .with_details(json!({ "stem": s, "known": names })),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writer task
// ---------------------------------------------------------------------------

/// A stem's `current.log`, opened lazily in append mode.
struct LogFile {
    dir: PathBuf,
    out: Option<BufWriter<File>>,
    size: u64,
    warned: bool,
}

impl LogFile {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            out: None,
            size: 0,
            warned: false,
        }
    }

    fn open(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        if self.out.is_none() {
            std::fs::create_dir_all(&self.dir)?;
            let path = stems_core::logs::log_file_path(&self.dir, 0);
            let f = OpenOptions::new().create(true).append(true).open(&path)?;
            self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
            self.out = Some(BufWriter::with_capacity(64 * 1024, f));
        }
        Ok(self.out.as_mut().expect("just opened"))
    }

    fn rotate_if_needed(&mut self, incoming: u64, policy: &RotationPolicy) -> std::io::Result<()> {
        let Some(action) = rotate_plan(self.size, incoming, policy) else {
            return Ok(());
        };
        if let Some(mut w) = self.out.take() {
            w.flush()?;
        }
        for step in action.steps(&self.dir) {
            let r = match &step {
                RotateStep::Remove(p) => std::fs::remove_file(p),
                RotateStep::Rename { from, to } => std::fs::rename(from, to),
            };
            match r {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        self.size = 0;
        Ok(())
    }

    fn write(&mut self, bytes: &[u8], policy: &RotationPolicy) {
        let r = (|| {
            self.open()?;
            self.rotate_if_needed(bytes.len() as u64, policy)?;
            self.open()?.write_all(bytes)?;
            self.size += bytes.len() as u64;
            Ok::<_, std::io::Error>(())
        })();
        if let Err(e) = r {
            self.out = None;
            if !self.warned {
                self.warned = true;
                tracing::warn!(dir = %self.dir.display(), error = %e, "cannot write stem log file");
            }
        }
    }

    fn flush(&mut self) {
        if let Some(w) = self.out.as_mut()
            && let Err(e) = w.flush()
        {
            tracing::warn!(dir = %self.dir.display(), error = %e, "cannot flush stem log file");
            self.out = None;
        }
    }
}

struct Writer {
    stem: String,
    file: LogFile,
    policy: RotationPolicy,
    ring: Arc<Mutex<RingBuffer<LogRecord>>>,
    live: broadcast::Sender<LiveRecord>,
}

impl Writer {
    async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        let mut dirty = false;
        let mut last_flush = tokio::time::Instant::now();
        let mut batch = Vec::with_capacity(64);
        loop {
            let first = if dirty {
                let deadline = last_flush + FLUSH_IDLE;
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(m) => m,
                    Err(_) => {
                        self.file.flush();
                        dirty = false;
                        last_flush = tokio::time::Instant::now();
                        continue;
                    }
                }
            } else {
                rx.recv().await
            };
            let Some(first) = first else { break };
            if !dirty {
                // The idle clock starts with the first unflushed write.
                last_flush = tokio::time::Instant::now();
            }
            batch.push(first);
            while batch.len() < BATCH_MAX {
                match rx.try_recv() {
                    Ok(m) => batch.push(m),
                    Err(_) => break,
                }
            }
            for m in batch.drain(..) {
                match m {
                    Msg::Line {
                        stream,
                        tag,
                        text,
                        ts,
                    } => {
                        let rec = parse_line(&self.stem, stream, tag.as_deref(), &text, ts);
                        self.commit(rec);
                        dirty = true;
                    }
                    Msg::Record(rec) => {
                        self.commit(rec);
                        dirty = true;
                    }
                    Msg::Configure(p) => self.policy = p,
                    Msg::Flush(reply) => {
                        self.file.flush();
                        dirty = false;
                        last_flush = tokio::time::Instant::now();
                        let _ = reply.send(());
                    }
                }
            }
            if dirty && last_flush.elapsed() >= FLUSH_IDLE {
                self.file.flush();
                dirty = false;
                last_flush = tokio::time::Instant::now();
            }
        }
        self.file.flush();
    }

    /// File first, then ring and live subscribers: the same order everywhere.
    fn commit(&mut self, rec: LogRecord) {
        match serde_json::to_vec(&rec) {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                self.file.write(&bytes, &self.policy);
            }
            Err(e) => tracing::warn!(error = %e, "cannot encode log record"),
        }
        if self.live.receiver_count() > 0 {
            let seq = lock(&self.ring).push(rec.clone());
            let _ = self.live.send(LiveRecord { seq, record: rec });
        } else {
            lock(&self.ring).push(rec);
        }
    }
}

#[cfg(test)]
mod tests;
