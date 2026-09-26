//! Restart policy decisions and backoff (plan 22), as a pure state machine
//! driven by explicit `Instant`s so it can be tested without sleeping.
//!
//! The stem actor owns one [`RestartTracker`] per stem and:
//!
//! - calls [`RestartTracker::on_exit`] whenever the process/container exits
//!   and acts on the [`RestartDecision`];
//! - calls [`RestartTracker::on_healthy`] when the stem becomes healthy and
//!   [`RestartTracker::on_unhealthy`] when it stops being healthy;
//! - calls [`RestartTracker::reset`] on a user-initiated `stems restart`;
//! - reports [`RestartTracker::restarts`] / [`RestartTracker::restarts_in_window`]
//!   in `StemStatus` and uses [`RestartTracker::is_restart_degraded`] for the
//!   `restarts` degraded reason (FR-HS-4).
//!
//! # Decision table
//!
//! | intent     | policy       | exit                | decision |
//! |------------|--------------|---------------------|----------|
//! | `UserStop` | any          | any                 | `Stop` |
//! | `Crash`    | `never`      | success (code 0)    | `Stop` |
//! | `Crash`    | `never`      | failure             | `Fail` |
//! | `Crash`    | `on-failure` | success             | `Stop` |
//! | `Crash`    | `on-failure` | failure             | `Restart` or `GiveUp` |
//! | `Crash`    | `always`     | any                 | `Restart` or `GiveUp` |
//!
//! "Success" means exit code 0 and no signal; anything else (non-zero code,
//! killed by a signal, unknown) is a failure.
//!
//! `Restart` vs `GiveUp`: restarts older than `window` are forgotten; if
//! `max` restarts remain in the window, the decision is `GiveUp` (the stem
//! goes `Failed` with `MAX_RESTARTS`, event `stem.gave_up`). Otherwise the
//! restart is recorded and its delay is
//! `initial × factor^(attempt − 1)`, capped at `backoff.max`.
//!
//! # Backoff attempt counter
//!
//! `attempt` (1-based, reported in `stem.restarting {attempt}`) counts
//! consecutive restarts and resets to 0 when either:
//! - the stem had been healthy for at least `window` when it exited
//!   ([`RestartTracker::on_healthy`]),
//! - every earlier restart has aged out of the window, or
//! - [`RestartTracker::reset`] is called.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use stems_config::{Backoff, Restart, RestartPolicy};

/// Restarts in the window at or above which the stem is degraded (FR-HS-4).
pub const DEGRADED_RESTARTS: usize = 3;

/// How a process ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExitInfo {
    /// Exit code, if it exited normally.
    pub code: Option<i32>,
    /// Terminating signal, if killed by one.
    pub signal: Option<i32>,
}

impl ExitInfo {
    /// Exited normally with `code`.
    pub fn code(code: i32) -> Self {
        Self {
            code: Some(code),
            signal: None,
        }
    }

    /// Killed by `signal`.
    pub fn signal(signal: i32) -> Self {
        Self {
            code: None,
            signal: Some(signal),
        }
    }

    /// Exit code 0 and no signal.
    pub fn success(&self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }
}

/// Why the process exited, as far as the actor knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Nobody asked it to stop (crash, clean exit on its own, OOM kill, …).
    Crash,
    /// A user/agent `stop`, `down` or `restart` asked it to stop.
    UserStop,
}

/// What the actor should do after an exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartDecision {
    /// Start again after `delay`; `attempt` is 1-based.
    Restart {
        /// Backoff delay before starting.
        delay: Duration,
        /// Consecutive restart attempt number (1-based).
        attempt: u32,
    },
    /// Too many restarts in the window: `Failed` with `MAX_RESTARTS`.
    GiveUp,
    /// Go to `Stopped`.
    Stop,
    /// Go to `Failed` (policy `never`, non-zero exit).
    Fail,
}

/// `initial × factor^(attempt − 1)` capped at `max` (`attempt` is 1-based;
/// `0` is treated as `1`). Non-finite or negative results fall back to `max`
/// / zero respectively.
pub fn backoff_delay(backoff: &Backoff, attempt: u32) -> Duration {
    let cap = backoff.max.as_duration();
    let exp = attempt.saturating_sub(1).min(i32::MAX as u32) as i32;
    let secs = backoff.initial.as_duration().as_secs_f64() * backoff.factor.powi(exp);
    if secs.is_nan() || secs < 0.0 {
        return Duration::ZERO.min(cap);
    }
    Duration::try_from_secs_f64(secs).map_or(cap, |d| d.min(cap))
}

/// Per-stem restart bookkeeping. See the module docs.
#[derive(Clone, Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    max: u32,
    window: Duration,
    backoff: Backoff,
    /// Instants of restarts still inside the window (oldest first).
    recent: VecDeque<Instant>,
    /// Consecutive attempts for backoff.
    consecutive: u32,
    /// Total restarts since the tracker was created.
    total: u32,
    /// Healthy since (cleared on exit / unhealthy).
    healthy_since: Option<Instant>,
}

impl RestartTracker {
    /// A tracker for a stem's resolved `restart:` settings.
    pub fn new(config: &Restart) -> Self {
        Self {
            policy: config.policy,
            max: config.max,
            window: config.window.as_duration(),
            backoff: config.backoff.clone(),
            recent: VecDeque::new(),
            consecutive: 0,
            total: 0,
            healthy_since: None,
        }
    }

    /// The policy in force.
    pub fn policy(&self) -> RestartPolicy {
        self.policy
    }

    /// Decide what to do about an exit at `now`, recording the restart if
    /// the decision is `Restart`. See the module-level decision table.
    pub fn on_exit(&mut self, exit: ExitInfo, intent: Intent, now: Instant) -> RestartDecision {
        let healthy_long_enough = self
            .healthy_since
            .is_some_and(|h| now.saturating_duration_since(h) >= self.window);
        self.healthy_since = None;

        if intent == Intent::UserStop {
            return RestartDecision::Stop;
        }
        let wants_restart = match self.policy {
            RestartPolicy::Never => {
                return if exit.success() {
                    RestartDecision::Stop
                } else {
                    RestartDecision::Fail
                };
            }
            RestartPolicy::OnFailure => !exit.success(),
            RestartPolicy::Always => true,
        };
        if !wants_restart {
            return RestartDecision::Stop;
        }

        self.prune(now);
        if healthy_long_enough || self.recent.is_empty() {
            self.consecutive = 0;
        }
        if self.recent.len() >= self.max as usize {
            return RestartDecision::GiveUp;
        }
        self.recent.push_back(now);
        self.total = self.total.saturating_add(1);
        self.consecutive = self.consecutive.saturating_add(1);
        RestartDecision::Restart {
            delay: backoff_delay(&self.backoff, self.consecutive),
            attempt: self.consecutive,
        }
    }

    /// The stem became healthy at `now` (idempotent: repeated calls while it
    /// stays healthy keep the first instant).
    pub fn on_healthy(&mut self, now: Instant) {
        self.healthy_since.get_or_insert(now);
    }

    /// The stem stopped being healthy (unhealthy/degraded/starting again).
    pub fn on_unhealthy(&mut self) {
        self.healthy_since = None;
    }

    /// User-initiated `restart`: forget the window and the backoff. The
    /// lifetime [`Self::restarts`] total is kept.
    pub fn reset(&mut self) {
        self.recent.clear();
        self.consecutive = 0;
        self.healthy_since = None;
    }

    /// Total policy restarts since the tracker was created.
    pub fn restarts(&self) -> u32 {
        self.total
    }

    /// Restarts younger than `window` at `now`.
    pub fn restarts_in_window(&self, now: Instant) -> u32 {
        self.recent
            .iter()
            .filter(|t| now.saturating_duration_since(**t) < self.window)
            .count() as u32
    }

    /// `true` when at least [`DEGRADED_RESTARTS`] restarts are in the window.
    pub fn is_restart_degraded(&self, now: Instant) -> bool {
        self.restarts_in_window(now) as usize >= DEGRADED_RESTARTS
    }

    /// The current consecutive attempt count (0 = no restart since the last reset).
    pub fn attempt(&self) -> u32 {
        self.consecutive
    }

    fn prune(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= self.window)
        {
            self.recent.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_config::Dur;

    fn cfg(policy: RestartPolicy, max: u32, window: Duration) -> Restart {
        Restart {
            policy,
            max,
            window: Dur(window),
            backoff: Backoff {
                initial: Dur::from_millis(500),
                max: Dur::from_secs(30),
                factor: 2.0,
            },
            on_unhealthy: false,
            unhealthy_grace: Dur::from_secs(1),
        }
    }

    /// Plan 22 defaults: on-failure, max 5, window 10m, 500ms ×2 up to 30s.
    fn defaults() -> Restart {
        cfg(RestartPolicy::OnFailure, 5, Duration::from_secs(600))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn restart(delay_ms: u64, attempt: u32) -> RestartDecision {
        RestartDecision::Restart {
            delay: ms(delay_ms),
            attempt,
        }
    }

    #[test]
    fn backoff_sequence_with_defaults() {
        let mut t = RestartTracker::new(&defaults());
        let t0 = Instant::now();
        let crash = ExitInfo::code(1);
        assert_eq!(t.on_exit(crash, Intent::Crash, t0), restart(500, 1));
        assert_eq!(
            t.on_exit(crash, Intent::Crash, t0 + ms(600)),
            restart(1000, 2)
        );
        assert_eq!(
            t.on_exit(crash, Intent::Crash, t0 + ms(1700)),
            restart(2000, 3)
        );
        assert_eq!(t.restarts(), 3);
        assert_eq!(t.restarts_in_window(t0 + ms(1700)), 3);
        assert!(t.is_restart_degraded(t0 + ms(1700)));
    }

    #[test]
    fn backoff_delay_is_capped() {
        let b = defaults().backoff;
        let seq: Vec<u64> = (1..=9)
            .map(|a| backoff_delay(&b, a).as_millis() as u64)
            .collect();
        assert_eq!(
            seq,
            [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]
        );
        assert_eq!(backoff_delay(&b, 0), ms(500));
        assert_eq!(backoff_delay(&b, u32::MAX), Duration::from_secs(30));
        let weird = Backoff {
            initial: Dur::from_secs(60),
            max: Dur::from_secs(5),
            factor: 1.0,
        };
        assert_eq!(backoff_delay(&weird, 1), Duration::from_secs(5));
        let nan = Backoff {
            initial: Dur::from_secs(1),
            max: Dur::from_secs(5),
            factor: f64::NAN,
        };
        assert_eq!(backoff_delay(&nan, 3), Duration::ZERO);
    }

    #[test]
    fn max_in_window_gives_up() {
        let mut t =
            RestartTracker::new(&cfg(RestartPolicy::OnFailure, 2, Duration::from_secs(600)));
        let t0 = Instant::now();
        let c = ExitInfo::code(3);
        assert!(matches!(
            t.on_exit(c, Intent::Crash, t0),
            RestartDecision::Restart { attempt: 1, .. }
        ));
        assert!(matches!(
            t.on_exit(c, Intent::Crash, t0 + ms(10)),
            RestartDecision::Restart { attempt: 2, .. }
        ));
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(20)),
            RestartDecision::GiveUp
        );
        assert_eq!(t.restarts(), 2, "giving up is not a restart");
    }

    #[test]
    fn max_zero_gives_up_immediately() {
        let mut t = RestartTracker::new(&cfg(RestartPolicy::Always, 0, Duration::from_secs(1)));
        assert_eq!(
            t.on_exit(ExitInfo::code(1), Intent::Crash, Instant::now()),
            RestartDecision::GiveUp
        );
    }

    #[test]
    fn window_expiry_forgets_attempts_and_resets_backoff() {
        let w = Duration::from_secs(3);
        let mut t = RestartTracker::new(&cfg(RestartPolicy::OnFailure, 2, w));
        let t0 = Instant::now();
        let c = ExitInfo::code(1);
        assert_eq!(t.on_exit(c, Intent::Crash, t0), restart(500, 1));
        assert_eq!(t.on_exit(c, Intent::Crash, t0 + ms(1000)), restart(1000, 2));
        assert_eq!(t.restarts_in_window(t0 + ms(2999)), 2);
        assert_eq!(t.restarts_in_window(t0 + ms(3000)), 1);
        assert_eq!(t.restarts_in_window(t0 + ms(4000)), 0);
        // The first restart aged out: one slot free, backoff continues.
        assert_eq!(t.on_exit(c, Intent::Crash, t0 + ms(3500)), restart(2000, 3));
        // Both remaining in the window: give up.
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(3600)),
            RestartDecision::GiveUp
        );
        // Much later, everything aged out: fresh backoff.
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(10_000)),
            restart(500, 1)
        );
        assert_eq!(t.restarts(), 4);
    }

    #[test]
    fn degraded_threshold_follows_window() {
        let w = Duration::from_secs(3);
        let mut t = RestartTracker::new(&cfg(RestartPolicy::Always, 10, w));
        let t0 = Instant::now();
        for i in 0..3 {
            t.on_exit(ExitInfo::code(1), Intent::Crash, t0 + ms(i * 100));
        }
        assert!(t.is_restart_degraded(t0 + ms(300)));
        assert!(!t.is_restart_degraded(t0 + ms(3000)));
        assert!(!t.is_restart_degraded(t0 + ms(10_000)));
    }

    #[test]
    fn healthy_for_window_resets_backoff() {
        let w = Duration::from_secs(10);
        let mut t = RestartTracker::new(&cfg(RestartPolicy::OnFailure, 100, w));
        let t0 = Instant::now();
        let c = ExitInfo::code(1);
        assert_eq!(t.on_exit(c, Intent::Crash, t0), restart(500, 1));
        assert_eq!(t.on_exit(c, Intent::Crash, t0 + ms(100)), restart(1000, 2));
        // Healthy, but not for a full window: backoff continues.
        t.on_healthy(t0 + ms(1000));
        assert_eq!(t.on_exit(c, Intent::Crash, t0 + ms(5000)), restart(2000, 3));
        // Healthy for a full window (repeated on_healthy keeps the first instant).
        t.on_healthy(t0 + ms(6000));
        t.on_healthy(t0 + ms(12_000));
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(16_000)),
            restart(500, 1)
        );
        // on_unhealthy clears the healthy streak.
        t.on_healthy(t0 + ms(17_000));
        t.on_unhealthy();
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(20_000)),
            restart(1000, 2)
        );
    }

    #[test]
    fn reset_clears_window_and_backoff_but_not_total() {
        let mut t = RestartTracker::new(&cfg(RestartPolicy::Always, 2, Duration::from_secs(600)));
        let t0 = Instant::now();
        let c = ExitInfo::code(1);
        t.on_exit(c, Intent::Crash, t0);
        t.on_exit(c, Intent::Crash, t0 + ms(1));
        assert_eq!(
            t.on_exit(c, Intent::Crash, t0 + ms(2)),
            RestartDecision::GiveUp
        );
        t.reset();
        assert_eq!(t.attempt(), 0);
        assert_eq!(t.restarts_in_window(t0 + ms(3)), 0);
        assert_eq!(t.on_exit(c, Intent::Crash, t0 + ms(3)), restart(500, 1));
        assert_eq!(t.restarts(), 3);
    }

    #[test]
    fn decision_table() {
        use RestartDecision as D;
        use RestartPolicy::*;
        let ok = ExitInfo::code(0);
        let fail = ExitInfo::code(3);
        let sig = ExitInfo::signal(9);
        let unknown = ExitInfo::default();
        let r1 = restart(500, 1);
        // (policy, exit, intent, expected)
        let table = [
            (Never, ok, Intent::Crash, D::Stop),
            (Never, fail, Intent::Crash, D::Fail),
            (Never, sig, Intent::Crash, D::Fail),
            (Never, unknown, Intent::Crash, D::Fail),
            (Never, ok, Intent::UserStop, D::Stop),
            (Never, fail, Intent::UserStop, D::Stop),
            (OnFailure, ok, Intent::Crash, D::Stop),
            (OnFailure, fail, Intent::Crash, r1),
            (OnFailure, sig, Intent::Crash, r1),
            (OnFailure, unknown, Intent::Crash, r1),
            (OnFailure, ok, Intent::UserStop, D::Stop),
            (OnFailure, fail, Intent::UserStop, D::Stop),
            (OnFailure, sig, Intent::UserStop, D::Stop),
            (Always, ok, Intent::Crash, r1),
            (Always, fail, Intent::Crash, r1),
            (Always, sig, Intent::Crash, r1),
            (Always, ok, Intent::UserStop, D::Stop),
            (Always, fail, Intent::UserStop, D::Stop),
            (Always, sig, Intent::UserStop, D::Stop),
        ];
        for (policy, exit, intent, want) in table {
            let mut t = RestartTracker::new(&cfg(policy, 5, Duration::from_secs(600)));
            assert_eq!(t.policy(), policy);
            let got = t.on_exit(exit, intent, Instant::now());
            assert_eq!(got, want, "{policy:?} {exit:?} {intent:?}");
            let recorded = u32::from(matches!(want, D::Restart { .. }));
            assert_eq!(t.restarts(), recorded, "{policy:?} {exit:?} {intent:?}");
        }
    }

    #[test]
    fn user_stop_never_counts() {
        let mut t = RestartTracker::new(&cfg(RestartPolicy::Always, 1, Duration::from_secs(600)));
        let t0 = Instant::now();
        for i in 0..5 {
            assert_eq!(
                t.on_exit(ExitInfo::signal(15), Intent::UserStop, t0 + ms(i)),
                RestartDecision::Stop
            );
        }
        assert_eq!(t.restarts(), 0);
        assert_eq!(
            t.on_exit(ExitInfo::code(1), Intent::Crash, t0),
            restart(500, 1)
        );
    }

    #[test]
    fn exit_info_success() {
        assert!(ExitInfo::code(0).success());
        assert!(!ExitInfo::code(1).success());
        assert!(!ExitInfo::signal(9).success());
        assert!(
            !ExitInfo {
                code: Some(0),
                signal: Some(9)
            }
            .success()
        );
        assert!(!ExitInfo::default().success());
    }
}
