//! End-to-end test harness for `stems` (deliverable 04).
//!
//! The test binary (`tests/e2e.rs`, `harness = false`) calls
//! [`runner::main`], which runs every `tests/features/**/*.feature` scenario
//! against the real `stems` binary with cucumber-rs. Each scenario gets an
//! isolated [`world::E2eWorld`]: a temp copy of an example workspace, its own
//! `STEMS_HOME` and port block; the global After hook ([`hooks::after`])
//! fails the scenario with `LEAK:` if anything is left running.
//!
//! See `STEPS.md` for the step vocabulary and the [`runner`] module docs
//! for the environment variables and the PENDING.txt mechanism.

pub mod golden;
pub mod hooks;
pub mod http;
pub mod pending;
pub mod procs;
pub mod remap;
pub mod runner;
pub mod steps;
pub mod util;
pub mod world;

pub use world::E2eWorld;
