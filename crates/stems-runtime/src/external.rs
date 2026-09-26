//! The runtime of `type: external` stems (FR-ST-3): stems monitors them but
//! never starts or stops them.
//!
//! * `start` spawns nothing and returns a [`Handle::External`]; the
//!   supervisor moves the stem to `unknown` (deliverable 21's health probe
//!   then moves it to `healthy`/`unhealthy`).
//! * `stop` is a no-op ([`StopOutcome::AlreadyDead`]).
//! * `describe` has nothing to describe (`Unsupported`), there is no output
//!   stream, and an external stem never "exits" (`wait` never resolves,
//!   `is_alive` is `true`), so a waiter cannot mistake it for a crash.
//! * nothing is adopted after a daemon restart and nothing is an orphan.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::output::OutputStream;
use crate::runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, Runtime, RuntimeError, RuntimeFacts, StartSpec,
    StopOutcome,
};

/// No-op runtime for monitor-only stems.
#[derive(Debug, Default)]
pub struct ExternalRuntime {
    next: AtomicU64,
}

impl ExternalRuntime {
    /// A new external runtime.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Runtime for ExternalRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        match spec {
            StartSpec::External { .. } => Ok(Handle::External {
                id: HandleId(self.next.fetch_add(1, Ordering::Relaxed) + 1),
            }),
            StartSpec::Process(p) => Err(RuntimeError::Unsupported(format!(
                "the external runtime does not run processes (`{}`)",
                p.command
            ))),
            StartSpec::Docker(c) => Err(RuntimeError::Unsupported(format!(
                "the external runtime does not run containers (`{}`)",
                c.stem
            ))),
        }
    }

    async fn stop(&self, _h: &Handle, _grace: Duration) -> Result<StopOutcome, RuntimeError> {
        Ok(StopOutcome::AlreadyDead)
    }

    async fn is_alive(&self, h: &Handle) -> bool {
        h.is_external()
    }

    async fn describe(&self, _h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        Err(RuntimeError::Unsupported(
            "an external stem has no process to describe".into(),
        ))
    }

    fn output_stream(&self, _h: &Handle) -> Option<OutputStream> {
        None
    }

    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        if !h.is_external() {
            return Err(RuntimeError::NotFound(h.id()));
        }
        std::future::pending().await
    }

    async fn adopt(&self, _record: &AdoptRecord) -> Option<Handle> {
        None
    }

    fn release(&self, _h: &Handle) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessSpec;

    #[tokio::test]
    async fn start_and_stop_are_no_ops() {
        let rt = ExternalRuntime::new();
        let h = rt
            .start(&StartSpec::External { stem: "db".into() })
            .await
            .unwrap();
        assert!(h.is_external());
        assert_eq!((h.pid(), h.pgid()), (0, 0));
        assert!(rt.is_alive(&h).await);
        assert!(rt.describe(&h).await.is_err());
        assert!(rt.output_stream(&h).is_none());
        assert_eq!(
            rt.stop(&h, Duration::from_secs(1)).await.unwrap(),
            StopOutcome::AlreadyDead
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rt.wait(&h))
                .await
                .is_err(),
            "an external stem never exits"
        );
        let other = rt
            .start(&StartSpec::External { stem: "db".into() })
            .await
            .unwrap();
        assert_ne!(other.id(), h.id());
        assert!(
            rt.start(&StartSpec::Process(ProcessSpec::shell("true", "/")))
                .await
                .is_err()
        );
    }
}
