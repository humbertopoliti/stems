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

/// `overlays_render` params (47).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverlaysRenderParams {
    /// The stem whose overlays are rendered.
    pub stem: String,
    /// Render only: write and record nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// One rendered overlay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenderedOverlay {
    /// Absolute destination.
    pub dest: PathBuf,
    /// File mode, octal (`0644`).
    pub mode: String,
    /// `keep: true` (survives `down`).
    pub keep: bool,
    /// The rendered content (UTF-8; invalid bytes of a `file:` overlay
    /// are shown as U+FFFD).
    pub content: String,
    /// The file was written now (`false` for a dry run, or when it already
    /// held exactly this content).
    pub written: bool,
}

/// `overlays_render` result (47).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OverlaysRenderResult {
    /// The stem.
    pub stem: String,
    /// Nothing was written.
    pub dry_run: bool,
    /// Every overlay of the stem, in declaration order.
    pub files: Vec<RenderedOverlay>,
}
