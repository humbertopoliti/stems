//! Domain model: the error catalogue, the dependency graph and `validate`
//! semantics over a loaded workspace. No daemon, no runtime I/O (validation
//! only reads the filesystem and runs `<tool> --version`).
//!
//! - [`Error`] / [`ErrorCode`] / [`Errors`]: the single error type (§7.5).
//! - [`validate::validate`]: every static check of `stems validate`.
//! - [`graph::start_order`] / [`graph::stop_order`] (or [`WorkspaceGraph`]).

mod error;
pub mod graph;
pub mod tools;
pub mod validate;
pub mod version;

pub use error::{Error, ErrorCode, Errors, Span, UnknownErrorCode, exit, sort_errors};
pub use graph::{Cycle, WorkspaceGraph, start_order, stop_order};
pub use validate::{ValidateOptions, load_and_validate, validate};

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-core");
    }
}
