//! When a started stem satisfies a `depends_on` condition.
//!
//! Since deliverable 21 the production [`ProbeWaiter`] hands readiness to
//! the health probes ([`super::probes`]): the per-stem probe task moves the
//! stem `starting → healthy` on the first passing probe (or `failed` with
//! `START_TIMEOUT`), and the scheduler waits on the stem's real state:
//!
//! * `started` — the process is alive (spawned and not exited);
//! * `healthy` — the stem's health check passed (state `healthy`);
//! * `seeded` — `healthy` and the start sequence (`post_start`, `seed`, 16)
//!   finished (the phase's `pending` flag is clear).
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

use super::probes::{DockerProber, HttpProber, Prober, ProcessProber, TcpProber};

/// Shortest poll interval of the stand-alone wait.
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
    /// `START_TIMEOUT`, ...). Only used when [`Waiter::probes`] is `false`.
    async fn wait_condition(&self, target: &WaitTarget, condition: Condition) -> Result<(), Error>;

    /// `true`: the supervisor runs the health probes of deliverable 21
    /// ([`super::probes`]) for readiness *and* afterwards (`healthy ⇄
    /// unhealthy`), and never calls [`Waiter::wait_condition`]. Test fakes
    /// keep the default (`false`): a one-shot readiness wait, no probes.
    fn probes(&self) -> bool {
        false
    }

    /// A prober to use instead of the one built from the stem's health
    /// config (tests script probe outcomes with it). `None`: build it.
    fn prober(&self, _target: &WaitTarget) -> Option<Arc<dyn Prober>> {
        None
    }
}

/// The production waiter: readiness and health come from the probes.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeWaiter;

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

#[async_trait::async_trait]
impl Waiter for ProbeWaiter {
    /// Stand-alone use (no supervisor probe task): `started` = alive;
    /// `healthy`/`seeded` = the `tcp`/`http`/`process`/`docker` probe passes
    /// once (other types: alive), bounded by `health.start_timeout`.
    async fn wait_condition(&self, target: &WaitTarget, condition: Condition) -> Result<(), Error> {
        if let Some(e) = dead(target).await {
            return Err(e);
        }
        if condition == Condition::Started {
            return Ok(());
        }
        let Some(h) = &target.health else {
            return Ok(());
        };
        let prober: Arc<dyn Prober> = match (h.kind, target.probe_port) {
            (HealthType::Tcp, Some(port)) => Arc::new(TcpProber::new(
                h.host.clone(),
                port,
                h.timeout.as_duration(),
            )),
            (HealthType::Http, _) if h.url.is_some() => {
                Arc::new(HttpProber::new(h.url.as_deref().unwrap_or_default(), h)?)
            }
            (HealthType::Docker, _) => Arc::new(DockerProber {
                runtime: target.runtime.clone(),
                handle: target.handle.clone(),
            }),
            _ => Arc::new(ProcessProber {
                runtime: target.runtime.clone(),
                handle: target.handle.clone(),
            }),
        };
        let deadline = Instant::now() + h.start_timeout.as_duration();
        let every = h.interval.as_duration().max(POLL);
        loop {
            if let Some(e) = dead(target).await {
                return Err(e);
            }
            let r = prober.probe().await;
            if r.ok {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    ErrorCode::StartTimeout,
                    format!(
                        "`{}` did not become healthy within {}s (last probe: {})",
                        target.stem,
                        h.start_timeout.as_duration().as_secs_f64(),
                        r.detail
                    ),
                )
                .with_details(json!({ "stem": target.stem, "port": target.probe_port })));
            }
            tokio::time::sleep(every).await;
        }
    }

    fn probes(&self) -> bool {
        true
    }
}
