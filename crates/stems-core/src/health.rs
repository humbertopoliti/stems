//! Degraded derivation (FR-GR-6, FR-HS-2, FR-HS-4): why a stem whose own
//! health check passes is still shown as `!` degraded. Degraded is never a
//! stored state; the supervisor derives it for every `status` snapshot from
//! [`DegradedInputs`].
//!
//! | reason | when | deliverable |
//! |---|---|---|
//! | `dependency <name> unhealthy` / `... failed` | a hard (`soft: false`) dependency is `unhealthy` or `failed` | 21 |
//! | `flapping` | at least [`FLAP_TRANSITIONS`] health transitions in [`FLAP_WINDOW`] | 21 |
//! | `restarts` | the restart tracker says so (`is_restart_degraded`) | 22 |
//! | `memory` / `cpu` / ... | a metric threshold is crossed | 25 |
//! | `setup exited 3 (soft_fail)` | a `soft_fail` script failed in this run of the stem | 41 |
//!
//! Only a `healthy` stem can be degraded. A dependency in state `unknown`
//! (an external stem whose probe cannot run) does **not** degrade its
//! dependants: stems cannot tell that it is down. Stopped, starting,
//! seeding dependencies do not either (they are transient or deliberate).

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::StemState;

/// Health transitions within [`FLAP_WINDOW`] that make a stem `flapping`.
pub const FLAP_TRANSITIONS: usize = 3;

/// The window [`FLAP_TRANSITIONS`] are counted in.
pub const FLAP_WINDOW: Duration = Duration::from_secs(60);

/// One reason a healthy stem is degraded. Open for later deliverables.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DegradedReason {
    /// A hard dependency is `unhealthy` or `failed`.
    Dependency {
        /// The dependency.
        stem: String,
        /// Its state.
        state: StemState,
    },
    /// Health flapping: `transitions` health transitions in the last 60 s.
    Flapping {
        /// Transitions in the window.
        transitions: usize,
    },
    /// Too many restarts in the restart window (22).
    Restarts {
        /// Restarts in the window.
        count: u32,
    },
    /// A metric threshold is crossed (25), e.g. `memory > 2GB`.
    Metric {
        /// Human description of the threshold.
        what: String,
    },
    /// A `soft_fail` script failed in the stem's current run (41).
    Script(SoftFailure),
}

/// A `soft_fail` script that failed (41): how it ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftFailure {
    /// Script name (`setup`, `seed`, ...).
    pub script: String,
    /// Exit code (`None` when killed or timed out).
    pub exit: Option<i32>,
    /// Signal that killed it.
    pub signal: Option<i32>,
    /// It ran into its `timeout`.
    pub timed_out: bool,
}

impl fmt::Display for SoftFailure {
    /// `setup exited 3 (soft_fail)`, `seed timed out (soft_fail)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = &self.script;
        match (self.timed_out, self.exit, self.signal) {
            (true, _, _) => write!(f, "{s} timed out"),
            (false, Some(c), _) => write!(f, "{s} exited {c}"),
            (false, None, Some(sig)) => write!(f, "{s} was killed by signal {sig}"),
            (false, None, None) => write!(f, "{s} failed"),
        }?;
        f.write_str(" (soft_fail)")
    }
}

impl fmt::Display for DegradedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DegradedReason::Dependency { stem, state } => {
                write!(f, "dependency {stem} {state}")
            }
            DegradedReason::Flapping { .. } => f.write_str("flapping"),
            DegradedReason::Restarts { count } => write!(f, "restarts ({count} recently)"),
            DegradedReason::Metric { what } => f.write_str(what),
            DegradedReason::Script(s) => s.fmt(f),
        }
    }
}

/// What the derivation looks at for one stem.
#[derive(Clone, Debug, Default)]
pub struct DegradedInputs {
    /// The stem's own state (only `healthy` can be degraded).
    pub state: Option<StemState>,
    /// Its hard dependencies with their current state.
    pub hard_deps: Vec<(String, StemState)>,
    /// Health transitions in the last [`FLAP_WINDOW`].
    pub transitions_in_window: usize,
    /// Restart tracker verdict (22): `Some(count)` when degraded.
    pub restarts_degraded: Option<u32>,
    /// Crossed metric thresholds (25), already described.
    pub metrics: Vec<String>,
    /// `soft_fail` scripts that failed in the stem's current run (41).
    pub soft_failures: Vec<SoftFailure>,
}

/// The degraded reasons of a stem, in table order; empty = not degraded.
pub fn derive_degraded(i: &DegradedInputs) -> Vec<DegradedReason> {
    if i.state != Some(StemState::Healthy) {
        return Vec::new();
    }
    let mut out: Vec<DegradedReason> = i
        .hard_deps
        .iter()
        .filter(|(_, s)| matches!(s, StemState::Unhealthy | StemState::Failed))
        .map(|(n, s)| DegradedReason::Dependency {
            stem: n.clone(),
            state: *s,
        })
        .collect();
    if i.transitions_in_window >= FLAP_TRANSITIONS {
        out.push(DegradedReason::Flapping {
            transitions: i.transitions_in_window,
        });
    }
    if let Some(count) = i.restarts_degraded {
        out.push(DegradedReason::Restarts { count });
    }
    out.extend(
        i.metrics
            .iter()
            .map(|w| DegradedReason::Metric { what: w.clone() }),
    );
    out.extend(i.soft_failures.iter().cloned().map(DegradedReason::Script));
    out
}

/// The `reason` text of a degraded stem: the reasons joined with `; `.
pub fn degraded_text(reasons: &[DegradedReason]) -> Option<String> {
    (!reasons.is_empty()).then(|| {
        reasons
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use StemState::*;

    fn deps(d: &[(&str, StemState)]) -> Vec<(String, StemState)> {
        d.iter().map(|(n, s)| ((*n).to_string(), *s)).collect()
    }

    /// The derivation table: (own state, deps, transitions, expected text).
    #[test]
    fn derivation_table() {
        type Case = (
            StemState,
            Vec<(String, StemState)>,
            usize,
            Option<&'static str>,
        );
        let cases: Vec<Case> = vec![
            (Healthy, deps(&[]), 0, None),
            (Healthy, deps(&[("db", Healthy)]), 0, None),
            (
                Healthy,
                deps(&[("db", Unhealthy)]),
                0,
                Some("dependency db unhealthy"),
            ),
            (
                Healthy,
                deps(&[("db", Failed)]),
                0,
                Some("dependency db failed"),
            ),
            // Unknown externals, transient and stopped deps do not degrade.
            (Healthy, deps(&[("hosted", Unknown)]), 0, None),
            (Healthy, deps(&[("db", Starting)]), 0, None),
            (Healthy, deps(&[("db", Seeding)]), 0, None),
            (Healthy, deps(&[("db", Stopped)]), 0, None),
            (Healthy, deps(&[]), 2, None),
            (Healthy, deps(&[]), 3, Some("flapping")),
            (
                Healthy,
                deps(&[("a", Unhealthy), ("b", Failed)]),
                4,
                Some("dependency a unhealthy; dependency b failed; flapping"),
            ),
            // Only healthy stems are degraded.
            (Unhealthy, deps(&[("db", Unhealthy)]), 5, None),
            (Starting, deps(&[("db", Failed)]), 0, None),
            (Unknown, deps(&[]), 9, None),
        ];
        for (state, hard_deps, transitions, want) in cases {
            let i = DegradedInputs {
                state: Some(state),
                hard_deps: hard_deps.clone(),
                transitions_in_window: transitions,
                ..Default::default()
            };
            let got = degraded_text(&derive_degraded(&i));
            assert_eq!(
                got.as_deref(),
                want,
                "{state} deps={hard_deps:?} transitions={transitions}"
            );
        }
    }

    #[test]
    fn hooks_for_restarts_and_metrics() {
        let i = DegradedInputs {
            state: Some(Healthy),
            restarts_degraded: Some(4),
            metrics: vec!["memory > 2GB".into()],
            ..Default::default()
        };
        let r = derive_degraded(&i);
        assert_eq!(
            degraded_text(&r).as_deref(),
            Some("restarts (4 recently); memory > 2GB")
        );
        let v = serde_json::to_value(&r[0]).unwrap();
        assert_eq!(v, serde_json::json!({"kind": "restarts", "count": 4}));
    }

    #[test]
    fn soft_failed_scripts_degrade_healthy_stems_only() {
        let fail = |script: &str, exit, signal, timed_out| SoftFailure {
            script: script.into(),
            exit,
            signal,
            timed_out,
        };
        let mut i = DegradedInputs {
            state: Some(Healthy),
            soft_failures: vec![
                fail("setup", Some(3), None, false),
                fail("seed", None, None, true),
                fail("post_start", None, Some(9), false),
            ],
            ..Default::default()
        };
        let r = derive_degraded(&i);
        assert_eq!(
            degraded_text(&r).as_deref(),
            Some(
                "setup exited 3 (soft_fail); seed timed out (soft_fail); post_start was killed by signal 9 (soft_fail)"
            )
        );
        assert_eq!(
            serde_json::to_value(&r[0]).unwrap(),
            serde_json::json!({"kind": "script", "script": "setup", "exit": 3, "signal": null, "timed_out": false})
        );
        i.state = Some(Failed);
        assert!(derive_degraded(&i).is_empty());
    }
}
