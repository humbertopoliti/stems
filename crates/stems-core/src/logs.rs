//! Log records and the pure logic around them (plan 12): line parsing and
//! level detection, query filters, the in-memory ring buffer, the size
//! rotation plan and multi-stem interleaving.
//!
//! Nothing here does I/O; the daemon's `LogSink` owns the writer task and
//! the files and calls into this module.
//!
//! # Record schema (`docs/logs.md`)
//!
//! Every captured line becomes one [`LogRecord`], serialised as one JSON
//! object (one per line in `--json` / NDJSON output and in the query RPCs):
//!
//! ```json
//! {
//!   "ts": "2026-09-26T10:00:00.123456Z",
//!   "stem": "shop-api",
//!   "stream": "err",
//!   "tag": null,
//!   "level": "error",
//!   "text": "db connection refused",
//!   "fields": { "request_id": "r-42" }
//! }
//! ```
//!
//! | field    | type                                   | meaning |
//! |----------|----------------------------------------|---------|
//! | `ts`     | RFC 3339 UTC string                    | daemon receive time (not the time the program printed) |
//! | `stem`   | string                                 | stem that produced the line |
//! | `stream` | `"out"` \| `"err"` \| `"script"`       | stdout, stderr, or output of a lifecycle/custom script |
//! | `tag`    | string \| `null`                       | script name for `stream: script` lines, else `null` |
//! | `level`  | `"trace"` \| `"debug"` \| `"info"` \| `"warn"` \| `"error"` \| `null` | detected level, `null` when unknown |
//! | `text`   | string                                 | the message: `msg`/`message` of a JSON line, otherwise the raw line |
//! | `fields` | object \| `null`                       | remaining keys of a JSON line (level and message keys removed) |
//!
//! All keys are always present (absent values are `null`), so consumers can
//! rely on a fixed shape.

use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use stems_config::ByteSize;

/// Error returned by the parsers in this module (level, level filter, since).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

// ---------------------------------------------------------------------------
// Stream and level
// ---------------------------------------------------------------------------

/// Which output stream a line came from. Recorded before any parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    /// The stem's stdout.
    Out,
    /// The stem's stderr.
    Err,
    /// Output (stdout or stderr) of a script; the script name is in `tag`.
    Script,
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stream::Out => "out",
            Stream::Err => "err",
            Stream::Script => "script",
        })
    }
}

/// Severity of a log line, ordered from least (`Trace`) to most (`Error`)
/// severe, so `level >= Level::Warn` means "warn or worse".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Trace.
    Trace,
    /// Debug.
    Debug,
    /// Info (also `notice`).
    Info,
    /// Warning.
    Warn,
    /// Error (also `fatal`, `critical`, `panic`, `alert`, `emergency`).
    Error,
}

impl Level {
    /// All levels, least severe first.
    pub const ALL: [Level; 5] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
    ];

    /// The lowercase name used in JSON and on the command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// Map a numeric level.
    ///
    /// - `0..=7` are syslog severities (RFC 5424): 0–3 → `Error`,
    ///   4 → `Warn`, 5–6 → `Info`, 7 → `Debug`.
    /// - `>= 10` are pino/bunyan levels: 10 → `Trace`, 20 → `Debug`,
    ///   30 → `Info`, 40 → `Warn`, 50+ → `Error` (values in between round
    ///   down to the bucket below).
    /// - Anything else (negative, 8, 9) → `None`.
    pub fn from_number(n: i64) -> Option<Level> {
        match n {
            0..=3 => Some(Level::Error),
            4 => Some(Level::Warn),
            5 | 6 => Some(Level::Info),
            7 => Some(Level::Debug),
            10..=19 => Some(Level::Trace),
            20..=29 => Some(Level::Debug),
            30..=39 => Some(Level::Info),
            40..=49 => Some(Level::Warn),
            50.. => Some(Level::Error),
            _ => None,
        }
    }

    /// Map a JSON value found under a level key (string name or number).
    pub fn from_json(v: &serde_json::Value) -> Option<Level> {
        match v {
            serde_json::Value::String(s) => s.parse().ok(),
            serde_json::Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .and_then(Level::from_number),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = ParseError;

    /// Case-insensitive level names, with common aliases (`warning`,
    /// `err`, `fatal`, `critical`, `notice`, …) and numeric strings
    /// (see [`Level::from_number`]).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        let lower = t.to_ascii_lowercase();
        let lvl = match lower.as_str() {
            "trace" | "trc" | "verbose" => Level::Trace,
            "debug" | "dbg" => Level::Debug,
            "info" | "inf" | "information" | "informational" | "notice" => Level::Info,
            "warn" | "wrn" | "warning" => Level::Warn,
            "error" | "err" | "eror" | "fatal" | "critical" | "crit" | "panic" | "alert"
            | "emerg" | "emergency" | "severe" => Level::Error,
            _ => {
                return t
                    .parse::<i64>()
                    .ok()
                    .and_then(Level::from_number)
                    .ok_or_else(|| {
                        ParseError(format!(
                            "unknown log level `{s}`; expected trace, debug, info, warn or error"
                        ))
                    });
            }
        };
        Ok(lvl)
    }
}

/// A `--level` filter: `warn` matches exactly `warn`, `warn+` matches `warn`
/// and everything more severe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LevelFilter {
    /// The level named in the filter.
    pub level: Level,
    /// `true` for `level+` (this level and above), `false` for an exact match.
    pub and_above: bool,
}

impl LevelFilter {
    /// Parse `error`, `warn+`, `INFO+`, …
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let t = s.trim();
        let (name, and_above) = match t.strip_suffix('+') {
            Some(rest) => (rest, true),
            None => (t, false),
        };
        Ok(Self {
            level: name.parse()?,
            and_above,
        })
    }

    /// Whether a record level passes. Records without a level never pass.
    pub fn matches(&self, level: Option<Level>) -> bool {
        match level {
            None => false,
            Some(l) if self.and_above => l >= self.level,
            Some(l) => l == self.level,
        }
    }
}

impl FromStr for LevelFilter {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl fmt::Display for LevelFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.level, if self.and_above { "+" } else { "" })
    }
}

// ---------------------------------------------------------------------------
// Records and line parsing
// ---------------------------------------------------------------------------

/// One captured line. See the module docs for the JSON schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Daemon receive time.
    pub ts: DateTime<Utc>,
    /// Stem name.
    pub stem: String,
    /// Source stream.
    pub stream: Stream,
    /// Script name for script output, else `None`.
    pub tag: Option<String>,
    /// Detected level, `None` when unknown.
    pub level: Option<Level>,
    /// Message text (JSON `msg`/`message`, or the raw line).
    pub text: String,
    /// Remaining JSON fields, `None` for plain lines or when nothing remains.
    pub fields: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Keys (compared case-insensitively) that carry a level in JSON lines, in
/// priority order.
const LEVEL_KEYS: [&str; 3] = ["level", "lvl", "severity"];
/// Keys (compared case-insensitively) that carry the message, in priority order.
const MSG_KEYS: [&str; 2] = ["msg", "message"];

/// Leading level in a plain line, optionally after a timestamp and/or
/// bracketed: `ERROR boom`, `[warn] x`, `2026-09-26T10:00:00Z INFO x`,
/// `[2026-09-26 10:00:00,123] [error] x`, `12:00:01 DEBUG: x`, `<error> x`.
static PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    let ts = r"(?:\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?|\d{2}:\d{2}:\d{2}(?:[.,]\d+)?)";
    let re = format!(
        r"(?i)^\s*(?:\[?{ts}\]?\s*[-|]?\s*)?[\[(<]?\s*(?P<lvl>trace|debug|info|warn(?:ing)?|error|err|fatal|critical|crit)\s*[\])>]?(?:[:|\s-]|$)"
    );
    Regex::new(&re).expect("valid prefix regex")
});

/// `level=error` / `lvl="warn"` anywhere in a logfmt-style line.
static LOGFMT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|\s)(?:level|lvl|severity)="?(?P<lvl>[a-z]+)"?(?:\s|$)"#)
        .expect("valid logfmt regex")
});

/// Best-effort level of a plain (non-JSON) line: a leading level word (see
/// [`parse_line`]), else a logfmt `level=` pair, else `None`.
pub fn detect_level(line: &str) -> Option<Level> {
    if let Some(c) = PREFIX_RE.captures(line) {
        return c["lvl"].parse().ok();
    }
    LOGFMT_RE.captures(line).and_then(|c| c["lvl"].parse().ok())
}

/// Turn one raw line into a record.
///
/// - Trailing `\r`/`\n` are stripped.
/// - If the line is a JSON object: the first of `level`/`lvl`/`severity`
///   (key compared case-insensitively; value a name or a number, see
///   [`Level::from_str`] and [`Level::from_number`]) gives the level, the
///   first of `msg`/`message` gives `text` (non-string values are rendered as
///   JSON); both keys are removed and the remaining keys become `fields`
///   (`None` if empty). A JSON object with no message key keeps the raw line
///   as `text`. An unrecognised level value is left in `fields`.
/// - Otherwise `text` is the raw line and the level comes from
///   [`detect_level`]: `ERROR`, `WARN`/`WARNING`, `INFO`, `DEBUG`, `TRACE`
///   (also `ERR`, `FATAL`, `CRITICAL`), case-insensitive, optionally
///   bracketed (`[error]`, `(warn)`, `<info>`), optionally after an ISO-8601
///   or `HH:MM:SS` timestamp; or a logfmt `level=…` pair.
pub fn parse_line(
    stem: &str,
    stream: Stream,
    tag: Option<&str>,
    raw: &str,
    ts: DateTime<Utc>,
) -> LogRecord {
    let line = raw.trim_end_matches(['\n', '\r']);
    let mut rec = LogRecord {
        ts,
        stem: stem.to_string(),
        stream,
        tag: tag.map(str::to_string),
        level: None,
        text: line.to_string(),
        fields: None,
    };
    let trimmed = line.trim_start();
    if trimmed.starts_with('{')
        && let Ok(serde_json::Value::Object(mut map)) =
            serde_json::from_str::<serde_json::Value>(trimmed)
    {
        if let Some((key, lvl)) =
            find_key(&map, &LEVEL_KEYS).and_then(|k| Level::from_json(&map[&k]).map(|l| (k, l)))
        {
            rec.level = Some(lvl);
            map.remove(&key);
        }
        if let Some(key) = find_key(&map, &MSG_KEYS)
            && let Some(v) = map.remove(&key)
        {
            rec.text = match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            };
        }
        rec.fields = (!map.is_empty()).then_some(map);
        return rec;
    }
    rec.level = detect_level(line);
    rec
}

/// First key of `map` equal (ASCII case-insensitively) to one of `wanted`,
/// honouring the priority order of `wanted`.
fn find_key(map: &serde_json::Map<String, serde_json::Value>, wanted: &[&str]) -> Option<String> {
    wanted
        .iter()
        .find_map(|w| map.keys().find(|k| k.eq_ignore_ascii_case(w)).cloned())
}

// ---------------------------------------------------------------------------
// --since / --until
// ---------------------------------------------------------------------------

/// A `--since`/`--until` value: a relative duration (`10m`, `2h30m`,
/// `1.5s`, `500ms`) meaning "that long before now", or an RFC 3339 instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinceSpec {
    /// Relative to now.
    Ago(Duration),
    /// An absolute instant.
    At(DateTime<Utc>),
}

impl SinceSpec {
    /// Parse a relative duration or an RFC 3339 timestamp.
    ///
    /// Durations are one or more `<number><unit>` groups (whitespace allowed
    /// between groups); the number may be fractional; units are `ms`, `s`,
    /// `m`, `h`, `d`, `w` (plus long forms `sec`, `min`, `hour`, `day`, …).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let t = s.trim();
        if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
            return Ok(SinceSpec::At(dt.with_timezone(&Utc)));
        }
        parse_duration(t).map(SinceSpec::Ago).map_err(|_| {
            ParseError(format!(
                "invalid time `{s}`; expected a duration like `10m`, `2h30m`, `1.5s` or an RFC 3339 timestamp"
            ))
        })
    }

    /// The absolute instant this spec denotes, given the current time.
    pub fn resolve(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match *self {
            SinceSpec::At(t) => t,
            SinceSpec::Ago(d) => chrono::Duration::from_std(d)
                .ok()
                .and_then(|d| now.checked_sub_signed(d))
                .unwrap_or(DateTime::<Utc>::MIN_UTC),
        }
    }
}

impl FromStr for SinceSpec {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Parse a compound duration with optional fractional parts: `10m`,
/// `2h30m`, `1.5s`, `1m 30s`, `250ms`. A bare `0` is zero.
pub fn parse_duration(s: &str) -> Result<Duration, ParseError> {
    let bad = || ParseError(format!("invalid duration `{s}`"));
    let t = s.trim();
    if t == "0" {
        return Ok(Duration::ZERO);
    }
    if t.is_empty() {
        return Err(bad());
    }
    let mut total = 0f64;
    let mut rest = t;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or_else(bad)?;
        if num_end == 0 {
            return Err(bad());
        }
        let n: f64 = rest[..num_end].parse().map_err(|_| bad())?;
        rest = rest[num_end..].trim_start();
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let secs = match rest[..unit_end].to_ascii_lowercase().as_str() {
            "ms" | "msec" | "millis" => 0.001,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            "w" | "week" | "weeks" => 604_800.0,
            _ => return Err(bad()),
        };
        total += n * secs;
        rest = rest[unit_end..].trim_start();
    }
    Duration::try_from_secs_f64(total).map_err(|_| bad())
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// Filters of `stems logs` / the `query_logs` RPC. `Default` matches
/// everything. Relative `--since` values are resolved to instants by the
/// caller ([`SinceSpec::resolve`]) so matching is deterministic.
#[derive(Clone, Debug, Default)]
pub struct LogQuery {
    /// Stems to include; empty = all.
    pub stems: Vec<String>,
    /// Include records with `ts >= since`.
    pub since: Option<DateTime<Utc>>,
    /// Include records with `ts <= until`.
    pub until: Option<DateTime<Utc>>,
    /// Include records whose `text` matches.
    pub grep: Option<Regex>,
    /// Level filter; records without a level are excluded when set.
    pub level: Option<LevelFilter>,
    /// Include only script output with this `tag`.
    pub script: Option<String>,
    /// Keep only the last `n` matching records ([`LogQuery::apply`]).
    pub tail: Option<usize>,
}

impl LogQuery {
    /// Whether a single record passes every per-record filter (`tail` is
    /// not a per-record filter; see [`LogQuery::apply`]).
    pub fn matches(&self, rec: &LogRecord) -> bool {
        if !self.stems.is_empty() && !self.stems.contains(&rec.stem) {
            return false;
        }
        if self.since.is_some_and(|t| rec.ts < t) || self.until.is_some_and(|t| rec.ts > t) {
            return false;
        }
        if let Some(f) = &self.level
            && !f.matches(rec.level)
        {
            return false;
        }
        if let Some(s) = &self.script
            && rec.tag.as_deref() != Some(s.as_str())
        {
            return false;
        }
        if let Some(re) = &self.grep
            && !re.is_match(&rec.text)
        {
            return false;
        }
        true
    }

    /// Filter records (in the given order) and then keep the last `tail`.
    pub fn apply<I: IntoIterator<Item = LogRecord>>(&self, records: I) -> Vec<LogRecord> {
        let mut out: Vec<LogRecord> = records.into_iter().filter(|r| self.matches(r)).collect();
        if let Some(n) = self.tail
            && out.len() > n
        {
            out.drain(..out.len() - n);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Ring buffer
// ---------------------------------------------------------------------------

/// A fixed-capacity FIFO that drops the oldest item when full, and numbers
/// every pushed item with a monotonically increasing sequence number
/// (starting at 0) so followers can resume with [`RingBuffer::since_seq`].
#[derive(Clone, Debug)]
pub struct RingBuffer<T> {
    items: VecDeque<T>,
    capacity: usize,
    /// Sequence number the next pushed item will get (= total pushes).
    next_seq: u64,
}

impl<T> RingBuffer<T> {
    /// An empty buffer holding at most `capacity` items. A capacity of 0
    /// keeps nothing but still counts pushes.
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity.min(1 << 16)),
            capacity,
            next_seq: 0,
        }
    }

    /// Append an item, evicting the oldest when full. Returns its sequence number.
    pub fn push(&mut self, item: T) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.capacity == 0 {
            return seq;
        }
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(item);
        seq
    }

    /// Items currently held, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter()
    }

    /// Number of items held.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Maximum number of items held.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The sequence number the next push will get (= total items ever pushed).
    pub fn seq(&self) -> u64 {
        self.next_seq
    }

    /// Sequence number of the oldest held item (equal to [`Self::seq`] when empty).
    pub fn first_seq(&self) -> u64 {
        self.next_seq - self.items.len() as u64
    }

    /// Number of items evicted so far.
    pub fn dropped(&self) -> u64 {
        self.first_seq()
    }

    /// The most recent item.
    pub fn last(&self) -> Option<&T> {
        self.items.back()
    }

    /// The last `n` items (or fewer), oldest first.
    pub fn last_n(&self, n: usize) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.items.iter().skip(self.items.len().saturating_sub(n))
    }

    /// Items with sequence number `>= seq`, with their numbers, oldest first.
    /// Items already evicted are silently skipped.
    pub fn since_seq(&self, seq: u64) -> impl Iterator<Item = (u64, &T)> {
        let first = self.first_seq();
        let skip = seq.saturating_sub(first) as usize;
        self.items
            .iter()
            .enumerate()
            .skip(skip)
            .map(move |(i, t)| (first + i as u64, t))
    }

    /// Drop every held item (the sequence counter keeps going).
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

// ---------------------------------------------------------------------------
// Rotation
// ---------------------------------------------------------------------------

/// Size-based rotation settings (workspace `logs:`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotationPolicy {
    /// Rotate before a write would make `current.log` exceed this size.
    /// `0` disables rotation.
    pub max_size: ByteSize,
    /// Rotated files kept (`current.1.log` … `current.<keep>.log`); at most
    /// `keep + 1` files exist.
    pub keep: usize,
}

impl RotationPolicy {
    /// From the resolved workspace `logs:` settings.
    pub fn from_config(logs: &stems_config::Logs) -> Self {
        Self {
            max_size: logs.max_size,
            keep: logs.keep as usize,
        }
    }
}

/// What to do before the next write: shift the rotated files by one.
///
/// Execute [`RotateAction::steps`] in order; a missing source file for a
/// rename/remove is not an error (ignore `NotFound`). Then open a fresh
/// `current.log`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RotateAction {
    /// Index to delete first (`Some(keep)`, or `Some(0)` when `keep == 0`,
    /// meaning `current.log` is simply discarded).
    pub delete: Option<usize>,
    /// Renames `(from_index, to_index)`, in the order they must happen.
    pub renames: Vec<(usize, usize)>,
}

/// One filesystem operation of a [`RotateAction`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotateStep {
    /// Remove this file if it exists.
    Remove(PathBuf),
    /// Rename `from` to `to` if `from` exists.
    Rename {
        /// Source.
        from: PathBuf,
        /// Destination.
        to: PathBuf,
    },
}

impl RotateAction {
    /// Concrete operations under `dir`, in execution order.
    pub fn steps(&self, dir: &Path) -> Vec<RotateStep> {
        let mut out = Vec::new();
        if let Some(i) = self.delete {
            out.push(RotateStep::Remove(log_file_path(dir, i)));
        }
        for &(from, to) in &self.renames {
            out.push(RotateStep::Rename {
                from: log_file_path(dir, from),
                to: log_file_path(dir, to),
            });
        }
        out
    }
}

/// Decide whether writing `incoming_len` more bytes to a `current.log` of
/// `current_size` bytes needs a rotation first.
///
/// Rotates when the file is non-empty and the write would push it past
/// `max_size`. An empty file never rotates (a single line longer than
/// `max_size` is still written whole). `max_size == 0` disables rotation.
pub fn rotate_plan(
    current_size: u64,
    incoming_len: u64,
    policy: &RotationPolicy,
) -> Option<RotateAction> {
    let max = policy.max_size.0;
    if max == 0 || current_size == 0 || current_size.saturating_add(incoming_len) <= max {
        return None;
    }
    if policy.keep == 0 {
        return Some(RotateAction {
            delete: Some(0),
            renames: Vec::new(),
        });
    }
    let renames = (0..policy.keep).rev().map(|i| (i, i + 1)).collect();
    Some(RotateAction {
        delete: Some(policy.keep),
        renames,
    })
}

/// File name for rotation index `i`: `0` → `current.log`, `n` → `current.n.log`.
pub fn log_file_name(index: usize) -> String {
    if index == 0 {
        "current.log".to_string()
    } else {
        format!("current.{index}.log")
    }
}

/// `dir/<log_file_name(index)>`.
pub fn log_file_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(log_file_name(index))
}

/// All file names a stem's log directory may hold under `keep`, oldest
/// first (`current.<keep>.log` … `current.1.log`, `current.log`) — the
/// order to read them in for a chronological replay.
pub fn log_files_oldest_first(keep: usize) -> Vec<String> {
    (0..=keep).rev().map(log_file_name).collect()
}

// ---------------------------------------------------------------------------
// Interleave
// ---------------------------------------------------------------------------

/// Merge per-stem (or per-file) record lists into one list ordered by `ts`.
///
/// A stable k-way merge: each input keeps its own relative order (even if
/// it is not perfectly sorted), and records with equal timestamps are taken
/// from the lower-indexed input first.
pub fn merge_by_ts(inputs: Vec<Vec<LogRecord>>) -> Vec<LogRecord> {
    let total = inputs.iter().map(Vec::len).sum();
    let mut iters: Vec<std::iter::Peekable<std::vec::IntoIter<LogRecord>>> = inputs
        .into_iter()
        .map(|v| v.into_iter().peekable())
        .collect();
    let mut out = Vec::with_capacity(total);
    loop {
        let mut best: Option<(usize, DateTime<Utc>)> = None;
        for (i, it) in iters.iter_mut().enumerate() {
            if let Some(r) = it.peek()
                && best.is_none_or(|(_, t)| r.ts < t)
            {
                best = Some((i, r.ts));
            }
        }
        match best {
            Some((i, _)) => out.extend(iters[i].next()),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    fn rec(stem: &str, secs: i64, text: &str) -> LogRecord {
        parse_line(stem, Stream::Out, None, text, t(secs))
    }

    #[test]
    fn level_from_str_names_and_aliases() {
        let cases = [
            ("trace", Level::Trace),
            ("TRACE", Level::Trace),
            ("Debug", Level::Debug),
            ("info", Level::Info),
            ("notice", Level::Info),
            ("warn", Level::Warn),
            ("WARNING", Level::Warn),
            ("error", Level::Error),
            ("ERR", Level::Error),
            ("fatal", Level::Error),
            ("critical", Level::Error),
            ("3", Level::Error),
            ("4", Level::Warn),
            ("6", Level::Info),
            ("7", Level::Debug),
            ("30", Level::Info),
            ("50", Level::Error),
        ];
        for (s, want) in cases {
            assert_eq!(s.parse::<Level>().unwrap(), want, "{s}");
        }
        assert!("loud".parse::<Level>().is_err());
        assert!("8".parse::<Level>().is_err());
    }

    #[test]
    fn level_numbers() {
        assert_eq!(Level::from_number(0), Some(Level::Error));
        assert_eq!(Level::from_number(5), Some(Level::Info));
        assert_eq!(Level::from_number(10), Some(Level::Trace));
        assert_eq!(Level::from_number(20), Some(Level::Debug));
        assert_eq!(Level::from_number(40), Some(Level::Warn));
        assert_eq!(Level::from_number(60), Some(Level::Error));
        assert_eq!(Level::from_number(9), None);
        assert_eq!(Level::from_number(-1), None);
    }

    #[test]
    fn level_ordering() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
        assert!(Level::Debug > Level::Trace);
    }

    #[test]
    fn level_filter_parse_and_match() {
        let f = LevelFilter::parse("warn+").unwrap();
        assert_eq!(
            f,
            LevelFilter {
                level: Level::Warn,
                and_above: true
            }
        );
        assert!(f.matches(Some(Level::Warn)));
        assert!(f.matches(Some(Level::Error)));
        assert!(!f.matches(Some(Level::Info)));
        assert!(!f.matches(None));
        assert_eq!(f.to_string(), "warn+");

        let e = LevelFilter::parse("ERROR").unwrap();
        assert!(e.matches(Some(Level::Error)));
        assert!(!e.matches(Some(Level::Warn)));
        assert_eq!(e.to_string(), "error");

        let i: LevelFilter = " info+ ".parse().unwrap();
        assert!(i.matches(Some(Level::Info)) && !i.matches(Some(Level::Debug)));
        assert!(LevelFilter::parse("nope+").is_err());
        assert!(LevelFilter::parse("+").is_err());
    }

    #[test]
    fn plain_level_detection_table() {
        let cases: &[(&str, Option<Level>)] = &[
            ("ERROR something broke", Some(Level::Error)),
            ("error: lowercase", Some(Level::Error)),
            ("Error something", Some(Level::Error)),
            ("WARN disk low", Some(Level::Warn)),
            ("WARNING disk low", Some(Level::Warn)),
            ("warning: x", Some(Level::Warn)),
            ("INFO listening on :8080", Some(Level::Info)),
            ("DEBUG query took 3ms", Some(Level::Debug)),
            ("TRACE enter fn", Some(Level::Trace)),
            ("[error] bracketed", Some(Level::Error)),
            ("[WARN] bracketed", Some(Level::Warn)),
            ("[ info ] spaced bracket", Some(Level::Info)),
            ("(debug) parens", Some(Level::Debug)),
            ("<error> angle", Some(Level::Error)),
            ("ERROR", Some(Level::Error)),
            ("  INFO leading spaces", Some(Level::Info)),
            ("2026-09-26T10:00:00Z ERROR after ts", Some(Level::Error)),
            (
                "2026-09-26T10:00:00.123+02:00 WARN after ts",
                Some(Level::Warn),
            ),
            (
                "2026-09-26 10:00:00,123 INFO python style",
                Some(Level::Info),
            ),
            (
                "[2026-09-26 10:00:00] [error] bracket ts",
                Some(Level::Error),
            ),
            ("12:00:01 DEBUG short ts", Some(Level::Debug)),
            ("12:00:01.5 - WARN dash", Some(Level::Warn)),
            ("FATAL out of memory", Some(Level::Error)),
            ("ERR short", Some(Level::Error)),
            ("CRITICAL boom", Some(Level::Error)),
            ("time=1 level=warn msg=hi", Some(Level::Warn)),
            ("ts=x lvl=\"error\" msg=hi", Some(Level::Error)),
            // Negatives.
            ("hello world", None),
            ("INFORMATION is not a level word", None),
            ("errors: 0", None),
            ("warnings are fine", None),
            ("server started without error", None),
            ("", None),
            ("   ", None),
            ("infoline", None),
        ];
        for (line, want) in cases {
            assert_eq!(detect_level(line), *want, "line: {line:?}");
            let r = parse_line("s", Stream::Out, None, line, t(0));
            assert_eq!(r.level, *want, "parse_line: {line:?}");
            assert_eq!(r.text, *line);
            assert_eq!(r.fields, None);
        }
    }

    #[test]
    fn json_line_parsing_table() {
        // (line, level, text, remaining field keys)
        let cases: &[(&str, Option<Level>, &str, &[&str])] = &[
            (
                r#"{"level":"error","msg":"boom","request_id":"r1"}"#,
                Some(Level::Error),
                "boom",
                &["request_id"],
            ),
            (
                r#"{"lvl":"WARN","message":"careful"}"#,
                Some(Level::Warn),
                "careful",
                &[],
            ),
            (
                r#"{"severity":"INFO","message":"x","n":1}"#,
                Some(Level::Info),
                "x",
                &["n"],
            ),
            (
                r#"{"Level":"debug","Msg":"cased keys"}"#,
                Some(Level::Debug),
                "cased keys",
                &[],
            ),
            (
                r#"{"level":50,"msg":"pino","pid":1,"hostname":"h"}"#,
                Some(Level::Error),
                "pino",
                &["hostname", "pid"],
            ),
            (
                r#"{"level":30,"msg":"pino info"}"#,
                Some(Level::Info),
                "pino info",
                &[],
            ),
            (
                r#"{"severity":4,"msg":"syslog warn"}"#,
                Some(Level::Warn),
                "syslog warn",
                &[],
            ),
            (
                r#"{"level":"loud","msg":"unknown level kept"}"#,
                None,
                "unknown level kept",
                &["level"],
            ),
            (
                r#"{"msg":{"nested":true}}"#,
                None,
                r#"{"nested":true}"#,
                &[],
            ),
            (
                r#"  {"level":"info","msg":"leading ws"}"#,
                Some(Level::Info),
                "leading ws",
                &[],
            ),
            (
                r#"{"level":"warn","msg":"both","message":"second"}"#,
                Some(Level::Warn),
                "both",
                &["message"],
            ),
        ];
        for (line, lvl, text, keys) in cases {
            let r = parse_line("api", Stream::Err, None, line, t(0));
            assert_eq!(r.level, *lvl, "{line}");
            assert_eq!(r.text, *text, "{line}");
            let got: Vec<&str> = r
                .fields
                .as_ref()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            let mut want = keys.to_vec();
            want.sort();
            let mut got_sorted = got.clone();
            got_sorted.sort();
            assert_eq!(got_sorted, want, "{line}");
            if keys.is_empty() {
                assert!(r.fields.is_none(), "{line}");
            }
        }
    }

    #[test]
    fn json_without_message_keeps_raw_text() {
        let line = r#"{"level":"info","event":"started"}"#;
        let r = parse_line("api", Stream::Out, None, line, t(0));
        assert_eq!(r.level, Some(Level::Info));
        assert_eq!(r.text, line);
        assert_eq!(r.fields.unwrap()["event"], "started");
    }

    #[test]
    fn non_object_json_and_broken_json_are_plain() {
        let r = parse_line("a", Stream::Out, None, "[1,2,3]", t(0));
        assert_eq!(r.fields, None);
        let r = parse_line("a", Stream::Out, None, "{not json ERROR", t(0));
        assert_eq!(r.fields, None);
        assert_eq!(r.level, None);
        assert_eq!(r.text, "{not json ERROR");
    }

    #[test]
    fn trailing_newlines_are_stripped_and_tag_kept() {
        let r = parse_line("db", Stream::Script, Some("setup"), "INFO done\r\n", t(0));
        assert_eq!(r.text, "INFO done");
        assert_eq!(r.tag.as_deref(), Some("setup"));
        assert_eq!(r.stream, Stream::Script);
        assert_eq!(r.level, Some(Level::Info));
    }

    #[test]
    fn record_json_shape() {
        let r = parse_line(
            "shop-api",
            Stream::Err,
            None,
            r#"{"level":"error","msg":"db connection refused","request_id":"r-42"}"#,
            Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(),
        );
        insta::assert_snapshot!(
            serde_json::to_string_pretty(&r).unwrap(),
            @r#"
        {
          "ts": "2026-09-26T10:00:00Z",
          "stem": "shop-api",
          "stream": "err",
          "tag": null,
          "level": "error",
          "text": "db connection refused",
          "fields": {
            "request_id": "r-42"
          }
        }
        "#
        );
        let plain = parse_line("db", Stream::Script, Some("seed"), "hello", r.ts);
        insta::assert_snapshot!(
            serde_json::to_string(&plain).unwrap(),
            @r#"{"ts":"2026-09-26T10:00:00Z","stem":"db","stream":"script","tag":"seed","level":null,"text":"hello","fields":null}"#
        );
        let back: LogRecord = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn since_spec_parsing() {
        let d = |s: &str| match SinceSpec::parse(s).unwrap() {
            SinceSpec::Ago(d) => d,
            other => panic!("{other:?}"),
        };
        assert_eq!(d("10m"), Duration::from_secs(600));
        assert_eq!(d("2h30m"), Duration::from_secs(9000));
        assert_eq!(d("1.5s"), Duration::from_millis(1500));
        assert_eq!(d("500ms"), Duration::from_millis(500));
        assert_eq!(d("1m 30s"), Duration::from_secs(90));
        assert_eq!(d("1d"), Duration::from_secs(86_400));
        assert_eq!(d("0"), Duration::ZERO);
        assert_eq!(d(" 5 min "), Duration::from_secs(300));
        match SinceSpec::parse("2026-09-26T10:00:00Z").unwrap() {
            SinceSpec::At(t) => assert_eq!(t, Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap()),
            other => panic!("{other:?}"),
        }
        match SinceSpec::parse("2026-09-26T12:00:00+02:00").unwrap() {
            SinceSpec::At(t) => assert_eq!(t, Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap()),
            other => panic!("{other:?}"),
        }
        for bad in [
            "",
            "abc",
            "10",
            "10x",
            "m",
            "1..5s",
            "-5m",
            "2026-13-01T00:00:00Z",
        ] {
            assert!(SinceSpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn since_spec_resolve() {
        let now = t(1000);
        assert_eq!(SinceSpec::Ago(Duration::from_secs(10)).resolve(now), t(990));
        assert_eq!(SinceSpec::At(t(5)).resolve(now), t(5));
    }

    #[test]
    fn query_matches_each_filter() {
        let mut err = rec("api", 10, "ERROR boom");
        err.stream = Stream::Err;
        let info = rec("api", 20, "INFO fine");
        let script = parse_line("db", Stream::Script, Some("setup"), "WARN slow", t(30));
        let plain = rec("web", 40, "no level here");
        let all = vec![err.clone(), info.clone(), script.clone(), plain.clone()];

        let q = LogQuery::default();
        assert!(all.iter().all(|r| q.matches(r)));

        let q = LogQuery {
            stems: vec!["api".into()],
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![err.clone(), info.clone()]);

        let q = LogQuery {
            since: Some(t(20)),
            until: Some(t(30)),
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![info.clone(), script.clone()]);

        let q = LogQuery {
            grep: Some(Regex::new("bo+m|level").unwrap()),
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![err.clone(), plain.clone()]);

        let q = LogQuery {
            level: Some(LevelFilter::parse("warn+").unwrap()),
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![err.clone(), script.clone()]);

        let q = LogQuery {
            script: Some("setup".into()),
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![script.clone()]);

        let q = LogQuery {
            tail: Some(3),
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), all[1..].to_vec());

        let q = LogQuery {
            tail: Some(1),
            stems: vec!["api".into()],
            ..Default::default()
        };
        assert_eq!(q.apply(all.clone()), vec![info.clone()]);

        let q = LogQuery {
            tail: Some(0),
            ..Default::default()
        };
        assert!(q.apply(all).is_empty());
    }

    #[test]
    fn ring_buffer_basics() {
        let mut rb = RingBuffer::new(3);
        assert!(rb.is_empty());
        assert_eq!(rb.seq(), 0);
        for i in 0..5 {
            assert_eq!(rb.push(i), i as u64);
        }
        assert_eq!(rb.len(), 3);
        assert_eq!(rb.capacity(), 3);
        assert_eq!(rb.seq(), 5);
        assert_eq!(rb.first_seq(), 2);
        assert_eq!(rb.dropped(), 2);
        assert_eq!(rb.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(rb.last_n(2).copied().collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(rb.last_n(10).copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(rb.last_n(0).count(), 0);
        assert_eq!(rb.last(), Some(&4));
        assert_eq!(
            rb.since_seq(3).map(|(s, v)| (s, *v)).collect::<Vec<_>>(),
            vec![(3, 3), (4, 4)]
        );
        assert_eq!(rb.since_seq(0).count(), 3);
        assert_eq!(rb.since_seq(5).count(), 0);
        rb.clear();
        assert!(rb.is_empty());
        assert_eq!(rb.seq(), 5);
        assert_eq!(rb.push(9), 5);
    }

    #[test]
    fn ring_buffer_zero_capacity_counts() {
        let mut rb = RingBuffer::new(0);
        rb.push("a");
        rb.push("b");
        assert!(rb.is_empty());
        assert_eq!(rb.seq(), 2);
        assert_eq!(rb.since_seq(0).count(), 0);
    }

    #[test]
    fn rotation_on_size_boundary() {
        let p = RotationPolicy {
            max_size: ByteSize(100),
            keep: 2,
        };
        assert_eq!(rotate_plan(0, 500, &p), None, "empty file never rotates");
        assert_eq!(rotate_plan(90, 10, &p), None, "exactly max is fine");
        let a = rotate_plan(90, 11, &p).expect("over max rotates");
        assert_eq!(a.delete, Some(2));
        assert_eq!(a.renames, vec![(1, 2), (0, 1)]);
        let dir = Path::new("/logs/api");
        assert_eq!(
            a.steps(dir),
            vec![
                RotateStep::Remove("/logs/api/current.2.log".into()),
                RotateStep::Rename {
                    from: "/logs/api/current.1.log".into(),
                    to: "/logs/api/current.2.log".into()
                },
                RotateStep::Rename {
                    from: "/logs/api/current.log".into(),
                    to: "/logs/api/current.1.log".into()
                },
            ]
        );
    }

    #[test]
    fn rotation_edge_policies() {
        let none = RotationPolicy {
            max_size: ByteSize(0),
            keep: 3,
        };
        assert_eq!(rotate_plan(1 << 40, 1, &none), None);
        let keep0 = RotationPolicy {
            max_size: ByteSize(10),
            keep: 0,
        };
        let a = rotate_plan(10, 1, &keep0).unwrap();
        assert_eq!(a.delete, Some(0));
        assert!(a.renames.is_empty());
        let cfg = stems_config::Logs {
            max_size: ByteSize(200 * 1024),
            keep: 2,
        };
        assert_eq!(
            RotationPolicy::from_config(&cfg),
            RotationPolicy {
                max_size: ByteSize(204_800),
                keep: 2
            }
        );
    }

    #[test]
    fn rotation_simulation_keeps_at_most_keep_plus_one_files() {
        // Simulate files as a Vec indexed by rotation index.
        let p = RotationPolicy {
            max_size: ByteSize(200),
            keep: 2,
        };
        let mut files: Vec<Option<Vec<u32>>> = vec![Some(Vec::new())];
        let mut size = 0u64;
        for line in 0..100u32 {
            let len = 20;
            if let Some(a) = rotate_plan(size, len, &p) {
                if let Some(d) = a.delete
                    && d < files.len()
                {
                    files[d] = None;
                }
                for (from, to) in a.renames {
                    if files.len() <= to {
                        files.resize(to + 1, None);
                    }
                    let moved = files.get_mut(from).and_then(Option::take);
                    files[to] = moved;
                }
                files[0] = Some(Vec::new());
                size = 0;
            }
            files[0].as_mut().unwrap().push(line);
            size += len;
        }
        let existing: Vec<_> = files.iter().flatten().collect();
        assert!(existing.len() <= 3);
        assert_eq!(*files[0].as_ref().unwrap().last().unwrap(), 99);
        assert_eq!(files[0].as_ref().unwrap().len(), 10);
        assert_eq!(files[2].as_ref().unwrap()[0], 70);
    }

    #[test]
    fn file_name_scheme() {
        assert_eq!(log_file_name(0), "current.log");
        assert_eq!(log_file_name(1), "current.1.log");
        assert_eq!(log_file_name(12), "current.12.log");
        assert_eq!(
            log_file_path(Path::new("/x"), 3),
            PathBuf::from("/x/current.3.log")
        );
        assert_eq!(
            log_files_oldest_first(2),
            vec!["current.2.log", "current.1.log", "current.log"]
        );
        assert_eq!(log_files_oldest_first(0), vec!["current.log"]);
    }

    #[test]
    fn merge_interleaves_by_ts_stably() {
        let a = vec![rec("a", 1, "a1"), rec("a", 3, "a3"), rec("a", 5, "a5")];
        let b = vec![rec("b", 2, "b2"), rec("b", 3, "b3"), rec("b", 6, "b6")];
        let c = vec![rec("c", 3, "c3")];
        let merged = merge_by_ts(vec![a, b, c, Vec::new()]);
        let texts: Vec<_> = merged.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["a1", "b2", "a3", "b3", "c3", "a5", "b6"]);
    }

    #[test]
    fn merge_keeps_each_input_order() {
        // Input `a` is not sorted; its internal order must survive.
        let a = vec![rec("a", 5, "a5"), rec("a", 1, "a1")];
        let b = vec![rec("b", 3, "b3")];
        let texts: Vec<_> = merge_by_ts(vec![a, b])
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(texts, ["b3", "a5", "a1"]);
        assert!(merge_by_ts(Vec::new()).is_empty());
    }
}
