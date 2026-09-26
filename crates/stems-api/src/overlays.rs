//! Params and results of the `overlays` method (deliverable 18). See
//! `docs/overlays.md`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `overlays` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverlaysParams {
    /// Only this stem's overlays.
    #[serde(default)]
    pub stem: Option<String>,
}

/// What is on disk at a recorded overlay's `dest`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OverlayFileStatus {
    /// The bytes stems wrote.
    Present,
    /// Different content (someone edited it; `down` leaves it in place).
    Modified,
    /// Nothing there.
    Missing,
}

impl From<stems_core::overlays::OverlayStatus> for OverlayFileStatus {
    fn from(s: stems_core::overlays::OverlayStatus) -> Self {
        use stems_core::overlays::OverlayStatus as S;
        match s {
            S::Present => Self::Present,
            S::Modified => Self::Modified,
            S::Missing => Self::Missing,
        }
    }
}

/// One recorded overlay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverlayEntry {
    /// Stem that declares it.
    pub stem: String,
    /// Absolute destination.
    pub dest: PathBuf,
    /// `present | modified | missing`.
    pub status: OverlayFileStatus,
    /// `keep: true` (survives `down`).
    pub keep: bool,
    /// sha256 of what stems wrote.
    pub sha256: String,
    /// Run that wrote it.
    pub run_id: String,
}

/// `overlays` result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverlaysResult {
    /// Every recorded overlay (of `stem` when given), by stem then dest.
    pub overlays: Vec<OverlayEntry>,
}
