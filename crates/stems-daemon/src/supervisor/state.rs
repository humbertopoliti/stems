//! The per-stem state machine (REQUIREMENTS §6.4): which transitions the
//! supervisor may take. Every transition emits a `stem.state` event.
//!
//! ```text
//! Stopped ──▶ Setup ──▶ Starting ──▶ Healthy ──▶ Seeding ──▶ Healthy
//!    │  ▲        │          │           │  ▲                    │
//!    │  │        ▼          ▼           ▼  │ (probe)            │
//!    │  └─ Stopping ◀── Failed ◀─── Unhealthy ──(restart)──▶ Starting
//!    ▼
//! Unknown (external stems: monitored, never started)
//! ```
//!
//! Deliverable 10 uses `Stopped → Starting → Healthy`, `→ Stopping →
//! Stopped` and `→ Failed`; `Setup`/`Seeding` (16) and `Unhealthy` (21/22)
//! are already legal so later deliverables only add behaviour.

use stems_core::StemState;

/// Is `from → to` a legal transition? Self-transitions are not (the
/// supervisor never emits them).
pub fn allowed(from: StemState, to: StemState) -> bool {
    use StemState::*;
    matches!(
        (from, to),
        (Stopped, Setup | Starting | Failed | Unknown)
            | (Setup, Starting | Failed | Stopping)
            | (Starting, Healthy | Unhealthy | Failed | Stopping)
            | (Healthy, Seeding | Unhealthy | Failed | Stopping | Stopped)
            | (Seeding, Healthy | Failed | Stopping)
            | (Unhealthy, Healthy | Starting | Failed | Stopping | Stopped)
            | (Stopping, Stopped | Failed)
            | (Failed, Setup | Starting | Stopping | Stopped)
            | (Unknown, Healthy | Unhealthy | Stopped)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_core::StemState::{self, *};

    /// The full table, written out: every (from, to) pair not listed here
    /// must be refused.
    const LEGAL: &[(StemState, StemState)] = &[
        (Stopped, Setup),
        (Stopped, Starting),
        (Stopped, Failed),
        (Stopped, Unknown),
        (Setup, Starting),
        (Setup, Failed),
        (Setup, Stopping),
        (Starting, Healthy),
        (Starting, Unhealthy),
        (Starting, Failed),
        (Starting, Stopping),
        (Healthy, Seeding),
        (Healthy, Unhealthy),
        (Healthy, Failed),
        (Healthy, Stopping),
        (Healthy, Stopped),
        (Seeding, Healthy),
        (Seeding, Failed),
        (Seeding, Stopping),
        (Unhealthy, Healthy),
        (Unhealthy, Starting),
        (Unhealthy, Failed),
        (Unhealthy, Stopping),
        (Unhealthy, Stopped),
        (Stopping, Stopped),
        (Stopping, Failed),
        (Failed, Setup),
        (Failed, Starting),
        (Failed, Stopping),
        (Failed, Stopped),
        (Unknown, Healthy),
        (Unknown, Unhealthy),
        (Unknown, Stopped),
    ];

    #[test]
    fn transition_table_is_exhaustive() {
        let mut legal = 0;
        for from in StemState::ALL {
            for to in StemState::ALL {
                let want = LEGAL.contains(&(from, to));
                assert_eq!(allowed(from, to), want, "{from} -> {to}");
                legal += usize::from(want);
            }
        }
        assert_eq!(legal, LEGAL.len());
        assert_eq!(StemState::ALL.len() * StemState::ALL.len(), 81);
    }

    #[test]
    fn no_self_transitions_and_everything_can_stop() {
        for s in StemState::ALL {
            assert!(!allowed(s, s), "{s} -> {s}");
        }
        // Every running state can be stopped (down never gets stuck).
        for s in StemState::ALL.into_iter().filter(|s| s.is_running()) {
            assert!(
                allowed(s, Stopping) || s == Stopping,
                "{s} cannot be stopped"
            );
        }
        // Stopping always ends.
        assert!(allowed(Stopping, Stopped));
        // Failed can be retried and cleared.
        assert!(allowed(Failed, Starting) && allowed(Failed, Stopped));
    }
}
