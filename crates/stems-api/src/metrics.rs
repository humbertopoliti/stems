//! Metrics on the wire (deliverable 25, `docs/metrics.md`): the `metrics`
//! RPC and the `metrics` block of [`crate::StemStatus`].

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_core::StemState;
use stems_core::metrics::Sample;

/// Samples a sparkline shows (the CLI table and the TUI).
pub const SPARK_SAMPLES: usize = 30;

/// Sort order of the `metrics` RPC (`--sort`): highest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MetricsSort {
    /// By `latest.cpu_pct`.
    Cpu,
    /// By `latest.rss_bytes`.
    Mem,
}

/// `metrics` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MetricsParams {
    /// Only these stems (default: every enabled stem).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Include the samples of the last `history_ms` milliseconds
    /// (`--history 5m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_ms: Option<u64>,
    /// Include the last `last` samples (sparklines; ignored when
    /// `history_ms` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<usize>,
    /// Order the stems, highest first (default: declaration order).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<MetricsSort>,
    /// Also compute disk usage (FR-MT-3; slow, cached 60 s).
    #[serde(default)]
    pub disk: bool,
}

/// The latest numbers of a stem (the `metrics` block of `status`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricsSummary {
    /// When the sample was taken.
    pub ts: DateTime<Utc>,
    /// CPU usage in percent of one core (can exceed 100).
    pub cpu_pct: f64,
    /// Resident memory in bytes.
    pub rss_bytes: u64,
    /// Processes in the tree (containers: processes in the container).
    pub children: u32,
}

impl MetricsSummary {
    /// The summary of a sample.
    pub fn of(s: &Sample) -> Self {
        Self {
            ts: s.ts,
            cpu_pct: s.cpu_pct,
            rss_bytes: s.rss_bytes,
            children: s.children,
        }
    }
}

/// One configured threshold (`limits:`) and whether it is crossed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LimitStatus {
    /// `memory` or `cpu`.
    pub metric: String,
    /// Bytes, or percent of one core.
    pub limit: f64,
    /// Seconds the value must stay above the limit.
    pub for_s: u64,
    /// Currently crossed (the stem is degraded for it).
    pub crossed: bool,
}

/// Disk usage of a stem (`--disk`, FR-MT-3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiskUsage {
    /// Bytes under the codebase's build-output directories (`target`,
    /// `dist`, `build`, `node_modules`, `.venv`); 0 without a codebase.
    pub codebase_build_bytes: u64,
    /// The directories counted, with their size.
    pub dirs: Vec<DiskDir>,
    /// Bytes in the stem's named docker volumes; `null` when it has none or
    /// Docker is not reachable.
    pub volumes_bytes: Option<u64>,
    /// The walk hit its file limit: sizes are lower bounds.
    #[serde(default)]
    pub truncated: bool,
}

/// One measured directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiskDir {
    /// Absolute path.
    pub path: String,
    /// Bytes.
    pub bytes: u64,
}

/// One stem in the `metrics` result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StemMetrics {
    /// Stem name.
    pub name: String,
    /// `process`, `docker`, `compose`, `external`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Lifecycle state.
    #[schemars(with = "String")]
    pub state: StemState,
    /// The newest sample (`null` when not running or not sampled yet;
    /// always `null` for external stems).
    #[schemars(with = "Option<Value>")]
    pub latest: Option<Sample>,
    /// Samples asked for with `history_ms` / `last`, oldest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Vec<Value>>")]
    pub history: Option<Vec<Sample>>,
    /// TCP ports the stem listens on (refreshed every 10 s).
    #[serde(default)]
    pub open_ports: Vec<u16>,
    /// Configured thresholds.
    #[serde(default)]
    pub limits: Vec<LimitStatus>,
    /// Disk usage (`disk: true` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskUsage>,
}

/// Workspace totals over the listed stems' latest samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricsTotals {
    /// Sum of `cpu_pct`.
    pub cpu_pct: f64,
    /// Sum of `rss_bytes`.
    pub rss_bytes: u64,
    /// Sum of `children`.
    pub children: u32,
    /// Sum of `disk.codebase_build_bytes` (`disk: true` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
}

impl MetricsTotals {
    /// Totals of `stems`.
    pub fn of(stems: &[StemMetrics]) -> Self {
        let mut t = Self::default();
        for s in stems {
            if let Some(l) = &s.latest {
                t.cpu_pct += l.cpu_pct;
                t.rss_bytes += l.rss_bytes;
                t.children += l.children;
            }
            if let Some(d) = &s.disk {
                *t.disk_bytes.get_or_insert(0) +=
                    d.codebase_build_bytes + d.volumes_bytes.unwrap_or(0);
            }
        }
        t
    }
}

/// `metrics` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MetricsResult {
    /// Sampling interval in milliseconds (`metrics.interval`).
    pub interval_ms: u64,
    /// Per stem, in declaration order or by `sort`.
    pub stems: Vec<StemMetrics>,
    /// Totals.
    pub totals: MetricsTotals,
}

/// Order `stems` highest first by `sort` (stems without a sample last,
/// ties by name).
pub fn sort_stems(stems: &mut [StemMetrics], sort: MetricsSort) {
    let key = |s: &StemMetrics| -> f64 {
        s.latest.as_ref().map_or(-1.0, |l| match sort {
            MetricsSort::Cpu => l.cpu_pct,
            MetricsSort::Mem => l.rss_bytes as f64,
        })
    };
    stems.sort_by(|a, b| {
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn stem(name: &str, cpu: f64, rss: u64) -> StemMetrics {
        StemMetrics {
            name: name.into(),
            kind: "process".into(),
            state: StemState::Healthy,
            latest: Some(Sample {
                ts: Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(),
                cpu_pct: cpu,
                rss_bytes: rss,
                children: 1,
                uptime_s: 1,
                restarts: 0,
            }),
            history: None,
            open_ports: vec![],
            limits: vec![],
            disk: None,
        }
    }

    #[test]
    fn sort_and_totals() {
        let mut v = vec![stem("a", 1.0, 30), stem("b", 50.0, 10), stem("c", 5.0, 20)];
        let mut idle = stem("z", 0.0, 0);
        idle.latest = None;
        v.push(idle);
        sort_stems(&mut v, MetricsSort::Mem);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a", "c", "b", "z"]);
        sort_stems(&mut v, MetricsSort::Cpu);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["b", "c", "a", "z"]);
        let t = MetricsTotals::of(&v);
        assert_eq!((t.cpu_pct, t.rss_bytes, t.children), (56.0, 60, 3));
        assert_eq!(t.disk_bytes, None);
    }

    #[test]
    fn params_defaults() {
        let p: MetricsParams = serde_json::from_str("{}").unwrap();
        assert_eq!(p, MetricsParams::default());
        let p: MetricsParams =
            serde_json::from_str(r#"{"sort":"mem","history_ms":10000,"disk":true}"#).unwrap();
        assert_eq!(p.sort, Some(MetricsSort::Mem));
        assert_eq!(p.history_ms, Some(10_000));
        assert!(p.disk);
    }
}
