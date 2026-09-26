//! The one place that decides when a started stem satisfies a `depends_on`
//! condition. Deliverable 21 replaces [`AliveWaiter`] with real probes.
//!
//! [`AliveWaiter`] today:
//!
//! * `started` — the process is alive;
//! * `healthy` — the process stayed alive for `health.start_period` and, for
//!   `tcp`/`http` checks, its port accepts TCP connections; bounded by
//!   `health.start_timeout` (`HEALTH_TIMEOUT`);
//! * `seeded` — same as `healthy` until 16 runs seed scripts.
//!
//! A process that exits while waited on fails with `START_FAILED` (exit code
//! and signal in details).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use stems_config::{Condition, Health, HealthType};
use stems_core::{Error, ErrorCode};
use stems_runtime::{ExitStatus, Handle, Runtime};
use tokio::time::Instant;

/// Poll interval for liveness and port checks.
const POLL: Duration = Duration::from_millis(50);

/// What is waited on.
#[derive(Clone)]
pub struct WaitTarget {
    /// Stem name.
    pub stem: String,
    /// The runtime that started it.
    pub runtime: Arc<dyn Runtime>,
    /// Its handle.
    pub handle: Handle,
    /// Its health config (deferred port references already rendered).
    pub health: Option<Health>,
    /// Port to check for `tcp`/`http` health, already resolved.
    pub probe_port: Option<u16>,
}

/// Decides when a started stem satisfies a condition.
#[async_trait::async_trait]
pub trait Waiter: Send + Sync {
    /// Resolve once `target` satisfies `condition`, or fail (`START_FAILED`,
    /// `HEALTH_TIMEOUT`, ...).
    async fn wait_condition(&self, target: &WaitTarget, condition: Condition) -> Result<(), Error>;
}

/// "Alive for `start_period`" (plus a TCP connect for tcp/http checks).
#[derive(Clone, Copy, Debug, Default)]
pub struct AliveWaiter;

/// `START_FAILED` for a process that exited while starting.
pub fn exited_error(stem: &str, status: ExitStatus) -> Error {
    let how = match (status.code, status.signal) {
        (Some(c), _) => format!("exited with code {c}"),
        (None, Some(s)) => format!("was killed by signal {s}"),
        (None, None) => "exited".to_string(),
    };
    Error::new(
        ErrorCode::StartFailed,
        format!("`{stem}` {how} before it became ready"),
    )
    .with_hint(format!(
        "check its output (`stems logs {stem}` from deliverable 12), or run its start command by hand in the codebase"
    ))
    .with_details(json!({ "stem": stem, "exit_code": status.code, "signal": status.signal }))
}

async fn dead(target: &WaitTarget) -> Option<Error> {
    if target.runtime.is_alive(&target.handle).await {
        return None;
    }
    let status = tokio::time::timeout(
        Duration::from_millis(500),
        target.runtime.wait(&target.handle),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or(ExitStatus::UNKNOWN);
    Some(exited_error(&target.stem, status))
}

async fn port_open(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::TcpStream::connect(("127.0.0.1", port))
        )
        .await,
        Ok(Ok(_))
    )
}

#[async_trait::async_trait]
impl Waiter for AliveWaiter {
    async fn wait_condition(&self, target: &WaitTarget, condition: Condition) -> Result<(), Error> {
        if let Some(e) = dead(target).await {
            return Err(e);
        }
        if condition == Condition::Started {
            return Ok(());
        }
        let (start_period, start_timeout) = target
            .health
            .as_ref()
            .map_or((Duration::ZERO, Duration::from_secs(60)), |h| {
                (h.start_period.as_duration(), h.start_timeout.as_duration())
            });
        let now = Instant::now();
        let alive_until = now + start_period;
        let deadline = now + start_timeout.max(start_period);
        let check_port = target.health.as_ref().is_some_and(|h| {
            matches!(
                h.kind,
                HealthType::Tcp | HealthType::Http | HealthType::Grpc
            )
        });
        loop {
            if let Some(e) = dead(target).await {
                return Err(e);
            }
            let now = Instant::now();
            if now >= alive_until {
                match (check_port, target.probe_port) {
                    (true, Some(p)) => {
                        if port_open(p).await {
                            return Ok(());
                        }
                    }
                    _ => return Ok(()),
                }
            }
            if now >= deadline {
                let what = target
                    .probe_port
                    .filter(|_| check_port)
                    .map(|p| format!(" (port {p} not accepting connections)"))
                    .unwrap_or_default();
                return Err(Error::new(
                    ErrorCode::HealthTimeout,
                    format!(
                        "`{}` did not become healthy within {}s{what}",
                        target.stem,
                        start_timeout.as_secs_f64()
                    ),
                )
                .with_hint(format!(
                    "check `stems logs {}`; raise `health.start_timeout` if it is just slow to boot",
                    target.stem
                ))
                .with_details(json!({ "stem": target.stem, "port": target.probe_port })));
            }
            let next = if now < alive_until {
                (alive_until - now).min(POLL)
            } else {
                POLL
            };
            tokio::time::sleep(next).await;
        }
    }
}
