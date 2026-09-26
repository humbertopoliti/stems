//! Domain model: the error catalogue, the dependency graph and `validate`
//! semantics over a loaded workspace. No daemon, no runtime I/O (validation
//! only reads the filesystem and runs `<tool> --version`).
//!
//! - [`Error`] / [`ErrorCode`] / [`Errors`]: the single error type (§7.5).
//! - [`validate::validate`]: every static check of `stems validate`.
//! - [`graph::start_order`] / [`graph::stop_order`] (or [`WorkspaceGraph`]).
//! - [`layout::layout`] and [`render`]: `stems graph` text/Mermaid/DOT/JSON.
//! - [`status::StemState`] / [`status::Glyph`]: lifecycle states and glyphs.
//! - [`health`]: degraded derivation (dependency, flapping, restarts, metrics).
//! - [`logs`]: log records, line parsing, query filters, ring buffer, rotation plan.
//! - [`stamps`]: script stamp hashing and the stamp store.
//! - [`restart`]: restart policy decisions and backoff.
//! - [`metrics`]: CPU formulas, sample history, sparklines, thresholds.
//! - [`scriptargs`]: script catalogue, argument parsing/validation, MCP schemas.
//! - [`overlays`]: overlay rendering, ownership decisions, atomic writes.
//! - [`selection`]: profiles and the stem selection closure of `up`/`start`.

mod error;
pub mod graph;
pub mod health;
pub mod layout;
pub mod logs;
pub mod metrics;
pub mod overlays;
pub mod render;
pub mod restart;
pub mod scriptargs;
pub mod selection;
pub mod stamps;
pub mod status;
pub mod tools;
pub mod validate;
pub mod version;

pub use error::{Error, ErrorCode, Errors, Span, UnknownErrorCode, exit, sort_errors};
pub use graph::{Cycle, WorkspaceGraph, start_order, stop_order};
pub use selection::{SelectOptions, Selection};
pub use status::{Glyph, StemState};
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
