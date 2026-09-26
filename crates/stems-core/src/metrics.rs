//! Metrics math and bookkeeping (plan 25): CPU % formulas, the per-stem
//! sample history, sparklines and threshold tracking with hysteresis.
//!
//! The sampler task in the daemon gathers raw numbers (process tree CPU
//! time + RSS, or Docker stats) and turns them into [`Sample`]s with the
//! functions here; the actor feeds samples to [`ThresholdTracker`]s.
//!
//! # Sample schema (`docs/metrics.md`)
//!
//! ```json
//! { "ts": "2026-09-26T10:00:00Z", "cpu_pct": 12.5, "rss_bytes": 104857600,
//!   "children": 3, "uptime_s": 42, "restarts": 0 }
//! ```
//!
//! # Formulas
//!
//! - Process CPU %: `(cpu_ms₂ − cpu_ms₁) / (wall_ms₂ − wall_ms₁) × 100`,
//!   summed over the process tree; one fully busy core = 100, so
//!   multi-threaded stems can exceed 100 ([`cpu_percent`]).
//! - Docker CPU %: `(cpu_delta / system_delta) × online_cpus × 100`, the
//!   formula of `docker stats` ([`docker_cpu_percent`]).

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use stems_config::{ByteSize, Dur, Limits};

use crate::logs::RingBuffer;

/// One metrics sample of a stem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Sample time.
    pub ts: DateTime<Utc>,
    /// CPU usage in percent of one core (can exceed 100).
    pub cpu_pct: f64,
    /// Resident memory in bytes (Docker: usage minus cache).
    pub rss_bytes: u64,
    /// Processes in the tree, including the root.
    pub children: u32,
    /// Seconds since the stem's current process started.
    pub uptime_s: u64,
    /// Policy restarts so far.
    pub restarts: u32,
}

/// CPU % between two readings of `(cumulative_cpu_time_ms, wall_clock_ms)`.
///
/// Returns 0 when no wall time elapsed or the CPU counter went backwards
/// (process replaced).
pub fn cpu_percent(prev: (u64, u64), cur: (u64, u64)) -> f64 {
    let wall = cur.1.saturating_sub(prev.1);
    if wall == 0 || cur.0 < prev.0 {
        return 0.0;
    }
    (cur.0 - prev.0) as f64 / wall as f64 * 100.0
}

/// Docker's CPU % formula from two consecutive stats readings:
/// `cpu_delta = cpu_stats.cpu_usage.total_usage − precpu_stats.cpu_usage.total_usage`,
/// `system_delta = cpu_stats.system_cpu_usage − precpu_stats.system_cpu_usage`,
/// `online_cpus = cpu_stats.online_cpus` (or the length of `percpu_usage`).
///
/// Returns 0 when either delta is 0; `online_cpus == 0` is treated as 1.
pub fn docker_cpu_percent(cpu_delta: u64, system_delta: u64, online_cpus: u32) -> f64 {
    if cpu_delta == 0 || system_delta == 0 {
        return 0.0;
    }
    (cpu_delta as f64 / system_delta as f64) * f64::from(online_cpus.max(1)) * 100.0
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// In-memory sample history of one stem (a ring; default 30 min at 2 s =
/// 900 samples).
#[derive(Clone, Debug)]
pub struct MetricsHistory {
    ring: RingBuffer<Sample>,
}

impl MetricsHistory {
    /// Default history window (30 minutes).
    pub const DEFAULT_WINDOW: Duration = Duration::from_secs(30 * 60);

    /// A history large enough for `window` at one sample per `interval`
    /// (`ceil(window / interval)`, at least 1).
    pub fn new(window: Duration, interval: Duration) -> Self {
        Self::with_capacity(Self::capacity_for(window, interval))
    }

    /// `ceil(window / interval)`, at least 1 (a zero interval gives 1).
    pub fn capacity_for(window: Duration, interval: Duration) -> usize {
        if interval.is_zero() {
            return 1;
        }
        let n = window.as_nanos().div_ceil(interval.as_nanos());
        usize::try_from(n).unwrap_or(usize::MAX).max(1)
    }

    /// A history holding at most `capacity` samples.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            ring: RingBuffer::new(capacity),
        }
    }

    /// Append a sample (evicting the oldest when full).
    pub fn push(&mut self, sample: Sample) {
        self.ring.push(sample);
    }

    /// The newest sample.
    pub fn latest(&self) -> Option<&Sample> {
        self.ring.last()
    }

    /// Samples with `ts >= since`, oldest first (`--history 5m`).
    pub fn slice(&self, since: DateTime<Utc>) -> Vec<&Sample> {
        self.ring.iter().filter(|s| s.ts >= since).collect()
    }

    /// The last `n` samples, oldest first (sparklines use 30).
    pub fn last_n(&self, n: usize) -> Vec<&Sample> {
        self.ring.last_n(n).collect()
    }

    /// All samples, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Sample> + ExactSizeIterator {
        self.ring.iter()
    }

    /// Samples held.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Maximum samples held.
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }
}

// ---------------------------------------------------------------------------
// Sparklines
// ---------------------------------------------------------------------------

/// Unicode sparkline ramp, lowest first.
pub const SPARK_UNICODE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// ASCII fallback ramp (non-UTF-8 terminals), lowest first.
pub const SPARK_ASCII: [char; 8] = ['_', '.', ',', '-', '~', '=', '+', '#'];

/// Glyph set for [`sparkline_with`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparkStyle {
    /// `▁▂▃▄▅▆▇█`.
    Unicode,
    /// `_.,-~=+#`.
    Ascii,
}

/// Unicode sparkline of the last `width` values, auto-scaled (see
/// [`sparkline_with`]).
pub fn sparkline(values: &[f64], width: usize) -> String {
    sparkline_with(values, width, SparkStyle::Unicode, None)
}

/// ASCII sparkline of the last `width` values, auto-scaled.
pub fn sparkline_ascii(values: &[f64], width: usize) -> String {
    sparkline_with(values, width, SparkStyle::Ascii, None)
}

/// Render the last `width` values as exactly `width` characters.
///
/// - Fewer values than `width`: left-padded with spaces (newest on the right).
/// - Scale: `range = Some((lo, hi))` fixes it (e.g. `(0.0, 100.0)` for CPU);
///   `None` uses `min(0, smallest)` … `largest`, so bars show magnitude
///   relative to zero (all zeros → lowest bar; constant non-zero → highest).
/// - Values are clamped to the range; non-finite values render as the
///   lowest bar.
pub fn sparkline_with(
    values: &[f64],
    width: usize,
    style: SparkStyle,
    range: Option<(f64, f64)>,
) -> String {
    let ramp = match style {
        SparkStyle::Unicode => &SPARK_UNICODE,
        SparkStyle::Ascii => &SPARK_ASCII,
    };
    let shown = &values[values.len().saturating_sub(width)..];
    let (lo, hi) = range.unwrap_or_else(|| {
        let finite = shown.iter().copied().filter(|v| v.is_finite());
        let (mn, mx) = finite.fold((0f64, f64::NEG_INFINITY), |(a, b), v| (a.min(v), b.max(v)));
        (mn, if mx.is_finite() { mx } else { mn })
    });
    let span = hi - lo;
    let top = (ramp.len() - 1) as f64;
    let mut out = String::with_capacity(width * 3);
    out.extend(std::iter::repeat_n(' ', width - shown.len()));
    for &v in shown {
        let idx = if !v.is_finite() || span <= 0.0 || !span.is_finite() {
            if v.is_finite() && span == 0.0 && v > 0.0 && v >= hi {
                ramp.len() - 1
            } else {
                0
            }
        } else {
            (((v - lo) / span).clamp(0.0, 1.0) * top).round() as usize
        };
        out.push(ramp[idx]);
    }
    out
}

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// Hysteresis for clearing a crossed threshold: the value must fall below
/// `limit × (1 − HYSTERESIS)`.
pub const HYSTERESIS: f64 = 0.10;

/// Metric a threshold watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    /// `cpu_pct` (percent of one core).
    Cpu,
    /// `rss_bytes`.
    Memory,
}

impl Metric {
    /// `"cpu"` / `"memory"` (also the degraded reason and event `metric`).
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Cpu => "cpu",
            Metric::Memory => "memory",
        }
    }

    /// This metric's value in a sample.
    pub fn value(self, s: &Sample) -> f64 {
        match self {
            Metric::Cpu => s.cpu_pct,
            Metric::Memory => s.rss_bytes as f64,
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resource limit from a stem's `limits:`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Threshold {
    /// Watched metric.
    pub metric: Metric,
    /// Limit: CPU percent (100 = one core) or bytes.
    pub limit: f64,
    /// How long the value must stay above the limit before it counts
    /// (0 = the first sample above crosses).
    pub for_secs: u64,
}

impl Threshold {
    /// Thresholds from resolved `limits:` (memory in bytes; `cpu` in cores
    /// → `cores × 100` percent), both with `for_secs = 0`.
    pub fn from_limits(limits: &Limits) -> Vec<Threshold> {
        let mut out = Vec::new();
        if let Some(m) = limits.memory {
            out.push(Threshold {
                metric: Metric::Memory,
                limit: m.0 as f64,
                for_secs: 0,
            });
        }
        if let Some(c) = limits.cpu {
            out.push(Threshold {
                metric: Metric::Cpu,
                limit: c * 100.0,
                for_secs: 0,
            });
        }
        out
    }
}

/// Error from [`parse_limit`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid limit `{input}`: {reason}")]
pub struct LimitParseError {
    /// The rejected text.
    pub input: String,
    /// Why.
    pub reason: String,
}

/// Parse a limit string: `<value> [for <duration>]`.
///
/// - `80%` → CPU at 80 % of one core (`150%` = 1.5 cores).
/// - A bare number (`1.5`) → CPU in cores (→ 150 %), like `limits.cpu`.
/// - A size with a unit (`2GB`, `512MB`, `100KB`, `64B`) → memory; see
///   `ByteSize` (1KB = 1024 B).
/// - Optional ` for <duration>` (`for 60s`, `for 1m 30s`), rounded down to
///   whole seconds.
pub fn parse_limit(s: &str) -> Result<Threshold, LimitParseError> {
    let err = |reason: &str| LimitParseError {
        input: s.to_string(),
        reason: reason.to_string(),
    };
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let (value, for_secs) = match lower.find(" for ") {
        Some(i) => {
            let d: Dur = t[i + 5..].trim().parse().map_err(|e: String| err(&e))?;
            (t[..i].trim(), d.as_duration().as_secs())
        }
        None => (t, 0),
    };
    if value.is_empty() {
        return Err(err("missing value"));
    }
    let (metric, limit) = if let Some(p) = value.strip_suffix('%') {
        let pct: f64 = p
            .trim()
            .parse()
            .map_err(|_| err("expected a percentage like `80%`"))?;
        (Metric::Cpu, pct)
    } else if let Ok(cores) = value.parse::<f64>() {
        (Metric::Cpu, cores * 100.0)
    } else {
        let b = ByteSize::from_str(value).map_err(|e| err(&e))?;
        (Metric::Memory, b.0 as f64)
    };
    if !limit.is_finite() || limit <= 0.0 {
        return Err(err("limit must be positive"));
    }
    Ok(Threshold {
        metric,
        limit,
        for_secs,
    })
}

/// A threshold state change, emitted as `stem.threshold {metric, value, limit}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ThresholdEvent {
    /// The value stayed above `limit` for `for_secs`: degraded.
    Crossed {
        /// Metric.
        metric: Metric,
        /// Value at crossing.
        value: f64,
        /// The limit.
        limit: f64,
    },
    /// The value fell below `limit × 0.9`: no longer degraded.
    Cleared {
        /// Metric.
        metric: Metric,
        /// Value at clearing.
        value: f64,
        /// The limit.
        limit: f64,
    },
}

/// Tracks one [`Threshold`] over successive samples.
///
/// - Not crossed: a value strictly above `limit` starts (or continues) an
///   "above" streak; once the streak has lasted `for_secs` the threshold is
///   crossed (`Crossed` emitted once). A value at or below `limit` ends the
///   streak.
/// - Crossed: the value must drop strictly below `limit × 0.9`
///   ([`HYSTERESIS`]) to clear (`Cleared` emitted once); values between the
///   two keep it crossed.
#[derive(Clone, Debug)]
pub struct ThresholdTracker {
    threshold: Threshold,
    above_since: Option<Instant>,
    crossed: bool,
}

impl ThresholdTracker {
    /// A tracker in the "not crossed" state.
    pub fn new(threshold: Threshold) -> Self {
        Self {
            threshold,
            above_since: None,
            crossed: false,
        }
    }

    /// The tracked threshold.
    pub fn threshold(&self) -> &Threshold {
        &self.threshold
    }

    /// Whether the threshold is currently crossed (degraded reason active).
    pub fn is_crossed(&self) -> bool {
        self.crossed
    }

    /// Feed a sample observed at `now`; returns a state change, if any.
    pub fn observe(&mut self, sample: &Sample, now: Instant) -> Option<ThresholdEvent> {
        let Threshold {
            metric,
            limit,
            for_secs,
        } = self.threshold;
        let value = metric.value(sample);
        if self.crossed {
            if value < limit * (1.0 - HYSTERESIS) {
                self.crossed = false;
                self.above_since = None;
                return Some(ThresholdEvent::Cleared {
                    metric,
                    value,
                    limit,
                });
            }
            return None;
        }
        if value > limit {
            let since = *self.above_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= Duration::from_secs(for_secs) {
                self.crossed = true;
                return Some(ThresholdEvent::Crossed {
                    metric,
                    value,
                    limit,
                });
            }
        } else {
            self.above_since = None;
        }
        None
    }

    /// Back to "not crossed" (e.g. the stem restarted).
    pub fn reset(&mut self) {
        self.crossed = false;
        self.above_since = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_790_000_000 + secs, 0).unwrap()
    }

    fn sample(secs: i64, cpu: f64, rss: u64) -> Sample {
        Sample {
            ts: ts(secs),
            cpu_pct: cpu,
            rss_bytes: rss,
            children: 1,
            uptime_s: secs as u64,
            restarts: 0,
        }
    }

    #[test]
    fn cpu_percent_from_two_samples() {
        assert_eq!(cpu_percent((0, 0), (500, 1000)), 50.0);
        assert_eq!(cpu_percent((1000, 10_000), (3000, 11_000)), 200.0);
        assert_eq!(cpu_percent((100, 100), (100, 2100)), 0.0);
        assert_eq!(cpu_percent((100, 100), (200, 100)), 0.0, "no wall time");
        assert_eq!(cpu_percent((900, 0), (100, 1000)), 0.0, "counter reset");
        assert_eq!(cpu_percent((0, 1000), (10, 500)), 0.0, "clock backwards");
    }

    #[test]
    fn docker_formula() {
        // 2 cpus, container used 25 % of total system time → 50 %.
        assert_eq!(docker_cpu_percent(250, 1000, 2), 50.0);
        assert_eq!(docker_cpu_percent(1000, 1000, 4), 400.0);
        assert_eq!(docker_cpu_percent(0, 1000, 4), 0.0);
        assert_eq!(docker_cpu_percent(10, 0, 4), 0.0);
        assert_eq!(docker_cpu_percent(100, 1000, 0), 10.0);
    }

    #[test]
    fn history_capacity_and_slice() {
        assert_eq!(
            MetricsHistory::capacity_for(MetricsHistory::DEFAULT_WINDOW, Duration::from_secs(2)),
            900
        );
        assert_eq!(
            MetricsHistory::capacity_for(Duration::from_secs(5), Duration::from_secs(2)),
            3
        );
        assert_eq!(
            MetricsHistory::capacity_for(Duration::ZERO, Duration::from_secs(2)),
            1
        );
        assert_eq!(
            MetricsHistory::capacity_for(Duration::from_secs(5), Duration::ZERO),
            1
        );

        let mut h = MetricsHistory::new(Duration::from_secs(10), Duration::from_secs(2));
        assert_eq!(h.capacity(), 5);
        assert!(h.is_empty() && h.latest().is_none());
        for i in 0..8 {
            h.push(sample(i * 2, i as f64, 0));
        }
        assert_eq!(h.len(), 5);
        assert_eq!(h.latest().unwrap().ts, ts(14));
        let s: Vec<i64> = h
            .slice(ts(10))
            .iter()
            .map(|s| s.ts.timestamp() - 1_790_000_000)
            .collect();
        assert_eq!(s, [10, 12, 14]);
        assert_eq!(h.slice(ts(100)).len(), 0);
        assert_eq!(h.slice(ts(0)).len(), 5);
        assert_eq!(h.last_n(2).len(), 2);
        assert_eq!(h.iter().next().unwrap().ts, ts(6));
    }

    #[test]
    fn sample_json_shape() {
        let s = Sample {
            ts: Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(),
            cpu_pct: 12.5,
            rss_bytes: 104_857_600,
            children: 3,
            uptime_s: 42,
            restarts: 0,
        };
        insta::assert_snapshot!(serde_json::to_string(&s).unwrap(), @r#"{"ts":"2026-09-26T10:00:00Z","cpu_pct":12.5,"rss_bytes":104857600,"children":3,"uptime_s":42,"restarts":0}"#);
    }

    #[test]
    fn sparkline_golden() {
        let ramp: Vec<f64> = (0..8).map(f64::from).collect();
        let wave: Vec<f64> = (0..30)
            .map(|i| 50.0 + 50.0 * (f64::from(i) / 3.0).sin())
            .collect();
        let cases = [
            ("ramp", sparkline(&ramp, 8)),
            ("ramp padded", sparkline(&ramp, 12)),
            ("ramp truncated", sparkline(&ramp, 4)),
            ("wave", sparkline(&wave, 30)),
            ("wave ascii", sparkline_ascii(&wave, 30)),
            ("zeros", sparkline(&[0.0; 5], 5)),
            ("constant", sparkline(&[7.0; 5], 5)),
            ("empty", sparkline(&[], 5)),
            (
                "cpu fixed 0..100",
                sparkline_with(
                    &[0.0, 10.0, 50.0, 100.0, 250.0],
                    5,
                    SparkStyle::Unicode,
                    Some((0.0, 100.0)),
                ),
            ),
            ("nan", sparkline(&[1.0, f64::NAN, 4.0], 3)),
            ("negative", sparkline(&[-4.0, 0.0, 4.0], 3)),
        ];
        let out: String = cases
            .iter()
            .map(|(name, s)| format!("{name:>18} |{s}|\n"))
            .collect();
        insta::assert_snapshot!(out);
    }

    #[test]
    fn sparkline_width_is_exact() {
        for w in [0, 1, 5, 30] {
            for n in [0, 3, 30, 100] {
                let v: Vec<f64> = (0..n).map(f64::from).collect();
                assert_eq!(sparkline(&v, w).chars().count(), w);
                assert_eq!(sparkline_ascii(&v, w).len(), w);
            }
        }
    }

    #[test]
    fn parse_limit_forms() {
        let p = |s| parse_limit(s).unwrap();
        assert_eq!(
            p("2GB"),
            Threshold {
                metric: Metric::Memory,
                limit: 2.0 * 1024.0 * 1024.0 * 1024.0,
                for_secs: 0
            }
        );
        assert_eq!(
            p("80% for 60s"),
            Threshold {
                metric: Metric::Cpu,
                limit: 80.0,
                for_secs: 60
            }
        );
        assert_eq!(p("80%").for_secs, 0);
        assert_eq!(p(" 150 % ").limit, 150.0);
        assert_eq!(p("1.5").limit, 150.0);
        assert_eq!(p("1.5").metric, Metric::Cpu);
        assert_eq!(p("100MB FOR 1m 30s").for_secs, 90);
        assert_eq!(p("100MB").limit, 104_857_600.0);
        assert_eq!(p("512KB").metric, Metric::Memory);
        for bad in [
            "",
            "abc",
            "80% for",
            "80% for ever",
            "-5%",
            "0%",
            "0",
            "2XB",
            "%",
        ] {
            assert!(parse_limit(bad).is_err(), "{bad:?}");
        }
        let e = parse_limit("abc").unwrap_err();
        assert!(e.to_string().starts_with("invalid limit `abc`"), "{e}");
    }

    #[test]
    fn thresholds_from_config_limits() {
        let l = Limits {
            memory: Some(ByteSize(1024)),
            cpu: Some(0.8),
        };
        let t = Threshold::from_limits(&l);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].metric, Metric::Memory);
        assert_eq!(t[0].limit, 1024.0);
        assert_eq!(t[1].metric, Metric::Cpu);
        assert!((t[1].limit - 80.0).abs() < 1e-9);
        assert!(Threshold::from_limits(&Limits::default()).is_empty());
    }

    #[test]
    fn threshold_immediate_cross_and_hysteresis() {
        let mb = 1024 * 1024;
        let mut t = ThresholdTracker::new(parse_limit("100MB").unwrap());
        let now = Instant::now();
        assert_eq!(t.observe(&sample(0, 0.0, 50 * mb), now), None);
        assert_eq!(
            t.observe(&sample(0, 0.0, 100 * mb), now),
            None,
            "equal is not above"
        );
        match t.observe(&sample(0, 0.0, 150 * mb), now) {
            Some(ThresholdEvent::Crossed {
                metric,
                value,
                limit,
            }) => {
                assert_eq!(metric, Metric::Memory);
                assert_eq!(value, (150 * mb) as f64);
                assert_eq!(limit, (100 * mb) as f64);
            }
            other => panic!("{other:?}"),
        }
        assert!(t.is_crossed());
        assert_eq!(
            t.observe(&sample(0, 0.0, 200 * mb), now),
            None,
            "emitted once"
        );
        // Inside the hysteresis band: still crossed.
        assert_eq!(t.observe(&sample(0, 0.0, 95 * mb), now), None);
        assert_eq!(
            t.observe(&sample(0, 0.0, 90 * mb), now),
            None,
            "exactly 90% stays"
        );
        assert!(t.is_crossed());
        assert!(matches!(
            t.observe(&sample(0, 0.0, 89 * mb), now),
            Some(ThresholdEvent::Cleared { .. })
        ));
        assert!(!t.is_crossed());
        assert_eq!(t.observe(&sample(0, 0.0, 10 * mb), now), None);
    }

    #[test]
    fn threshold_for_duration_required() {
        let mut t = ThresholdTracker::new(parse_limit("80% for 60s").unwrap());
        let t0 = Instant::now();
        let s = |cpu| sample(0, cpu, 0);
        let at = |secs| t0 + Duration::from_secs(secs);
        assert_eq!(t.observe(&s(90.0), at(0)), None);
        assert_eq!(t.observe(&s(95.0), at(30)), None);
        // Dips to the limit: streak broken.
        assert_eq!(t.observe(&s(80.0), at(40)), None);
        assert_eq!(t.observe(&s(90.0), at(50)), None);
        assert_eq!(t.observe(&s(90.0), at(109)), None);
        assert!(matches!(
            t.observe(&s(90.0), at(110)),
            Some(ThresholdEvent::Crossed { .. })
        ));
        assert_eq!(t.observe(&s(75.0), at(120)), None);
        assert!(matches!(
            t.observe(&s(50.0), at(130)),
            Some(ThresholdEvent::Cleared { .. })
        ));
        // A new crossing needs a new full streak.
        assert_eq!(t.observe(&s(90.0), at(140)), None);
        t.reset();
        assert!(!t.is_crossed());
        assert_eq!(t.observe(&s(90.0), at(150)), None);
        assert!(t.observe(&s(90.0), at(210)).is_some());
        assert_eq!(t.threshold().for_secs, 60);
    }

    #[test]
    fn threshold_event_json() {
        let e = ThresholdEvent::Crossed {
            metric: Metric::Cpu,
            value: 91.5,
            limit: 80.0,
        };
        insta::assert_snapshot!(serde_json::to_string(&e).unwrap(), @r#"{"kind":"crossed","metric":"cpu","value":91.5,"limit":80.0}"#);
    }
}
