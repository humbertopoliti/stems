//! Params and result of `pull`: refresh the images of docker stems from
//! their registries. See `docs/docker.md` ("stems pull").

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::lifecycle::{StemFailure, UpResult};

/// `pull` params.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullParams {
    /// Stems whose image is pulled; empty = every enabled docker stem with an
    /// `image` (except `pull: never` ones, which are pulled only when named).
    #[serde(default)]
    pub stems: Vec<String>,
    /// Restart the running stems whose image changed.
    #[serde(default)]
    pub restart: bool,
}

/// One pulled image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PulledImage {
    /// Stem.
    pub stem: String,
    /// Image reference as configured.
    pub image: String,
    /// Local image id before the pull (`null`: not present).
    pub before: Option<String>,
    /// Local image id after the pull.
    pub after: Option<String>,
    /// The local image changed (new, or a moved tag).
    pub changed: bool,
    /// Wall time of the pull (shared by stems using the same image).
    pub duration_ms: u64,
}

/// A selected stem that was not pulled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullSkip {
    /// Stem.
    pub stem: String,
    /// Why: `not a docker stem`, `built from \`build:\``, `\`pull: never\``.
    pub reason: String,
}

/// `pull` result.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PullResult {
    /// Every pull (and restart, with `restart`) succeeded.
    pub ok: bool,
    /// Images pulled, in start order.
    pub pulled: Vec<PulledImage>,
    /// Selected stems not pulled.
    pub skipped: Vec<PullSkip>,
    /// Stems whose pull failed (`IMAGE_PULL_FAILED`, `DOCKER_UNAVAILABLE`).
    pub failed: Vec<StemFailure>,
    /// With `restart`: the restart of the running stems whose image changed
    /// (`null` when none needed it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restarted: Option<UpResult>,
}
