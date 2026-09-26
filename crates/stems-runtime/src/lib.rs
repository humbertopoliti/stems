//! Runtimes that start, stop and observe stems.
//!
//! * [`Runtime`] — the async trait every runtime implements.
//! * [`ProcessRuntime`] — native processes in their own session/process group
//!   (see `docs/process-model.md`).
//! * [`ExternalRuntime`] — monitor-only (`type: external`) stems: nothing
//!   is started or stopped.
//! * [`DockerRuntime`] — containers through the Docker Engine API
//!   (see `docs/docker.md`).
//! * [`ComposeRuntime`] — one service of a compose file per stem, through
//!   `docker compose` (see `docs/compose.md`).
//! * [`os`] — per-OS process facts (start time, process tree, listeners).

pub mod compose;
pub mod docker;
mod external;
pub mod os;
pub mod output;
mod process;
mod runtime;

pub use compose::{ComposeOptions, ComposeRuntime, ComposeSpec};
pub use docker::{ContainerSpec, DockerOptions, DockerRuntime};
pub use external::ExternalRuntime;
pub use os::{Listener, ProcInfo, StartTime};
pub use output::{
    LineSplitter, MAX_LINE_BYTES, OUTPUT_CHANNEL_CAPACITY, OutputEvent, OutputLine, OutputStream,
    OutputStreamKind,
};
pub use process::ProcessRuntime;
pub use runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, Orphan, OrphanKind, OrphanScope, ProcessSpec,
    Runtime, RuntimeError, RuntimeFacts, StartSpec, StopOutcome,
};

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-runtime");
    }
}
