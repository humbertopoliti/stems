//! Stem lifecycle states and the status glyph table (§4.7, FR-HS-2).
//!
//! | glyph | unicode | ascii  | colour  | states                                   |
//! |-------|---------|--------|---------|------------------------------------------|
//! | healthy       | `✓` | `OK`   | green   | `healthy`                        |
//! | failed        | `✗` | `FAIL` | red     | `unhealthy`, `failed`            |
//! | degraded      | `!` | `WARN` | yellow  | `healthy` with a warning (FR-GR-6) |
//! | stopped       | `·` | `-`    | gray    | `stopped`                        |
//! | unknown       | `?` | `?`    | magenta | `unknown`                        |
//! | transitioning | `↻` | `..`   | cyan    | `setup`, `starting`, `seeding`, `stopping` |

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// What a stem looks like at a glance: one glyph per stem in `status`,
/// `graph` and the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glyph {
    /// `✓` green.
    Healthy,
    /// `✗` red.
    Failed,
    /// `!` yellow: healthy but with a warning (dependency unhealthy,
    /// flapping, thresholds).
    Degraded,
    /// `·` gray.
    Stopped,
    /// `?` magenta (external unreachable, daemon unaware).
    Unknown,
    /// `↻` cyan: setup, starting, seeding, stopping.
    Transitioning,
}

impl Glyph {
    /// Every glyph, in legend order.
    pub const ALL: [Glyph; 6] = [
        Glyph::Healthy,
        Glyph::Degraded,
        Glyph::Failed,
        Glyph::Stopped,
        Glyph::Unknown,
        Glyph::Transitioning,
    ];

    /// The Unicode glyph (one terminal column).
    pub fn unicode(self) -> &'static str {
        match self {
            Glyph::Healthy => "✓",
            Glyph::Failed => "✗",
            Glyph::Degraded => "!",
            Glyph::Stopped => "·",
            Glyph::Unknown => "?",
            Glyph::Transitioning => "↻",
        }
    }

    /// The ASCII fallback (`--no-unicode`, dumb terminals).
    pub fn ascii(self) -> &'static str {
        match self {
            Glyph::Healthy => "OK",
            Glyph::Failed => "FAIL",
            Glyph::Degraded => "WARN",
            Glyph::Stopped => "-",
            Glyph::Unknown => "?",
            Glyph::Transitioning => "..",
        }
    }

    /// [`Glyph::unicode`] or [`Glyph::ascii`].
    pub fn symbol(self, unicode: bool) -> &'static str {
        if unicode {
            self.unicode()
        } else {
            self.ascii()
        }
    }

    /// Colour name: `green`, `red`, `yellow`, `gray`, `magenta`, `cyan`.
    pub fn color(self) -> &'static str {
        match self {
            Glyph::Healthy => "green",
            Glyph::Failed => "red",
            Glyph::Degraded => "yellow",
            Glyph::Stopped => "gray",
            Glyph::Unknown => "magenta",
            Glyph::Transitioning => "cyan",
        }
    }

    /// SGR parameter of [`Glyph::color`] for ANSI terminals.
    pub fn ansi(self) -> &'static str {
        match self {
            Glyph::Healthy => "32",
            Glyph::Failed => "31",
            Glyph::Degraded => "33",
            Glyph::Stopped => "90",
            Glyph::Unknown => "35",
            Glyph::Transitioning => "36",
        }
    }

    /// Lower-case name (`healthy`, `failed`, ...), as serialized.
    pub fn name(self) -> &'static str {
        match self {
            Glyph::Healthy => "healthy",
            Glyph::Failed => "failed",
            Glyph::Degraded => "degraded",
            Glyph::Stopped => "stopped",
            Glyph::Unknown => "unknown",
            Glyph::Transitioning => "transitioning",
        }
    }

    /// The legend line, e.g. `✓ healthy  ! degraded  ✗ failed  · stopped  ? unknown  ↻ transitioning`.
    pub fn legend(unicode: bool) -> String {
        Glyph::ALL
            .iter()
            .map(|g| format!("{} {}", g.symbol(unicode), g.name()))
            .collect::<Vec<_>>()
            .join("  ")
    }
}

impl fmt::Display for Glyph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.unicode())
    }
}

/// Lifecycle state of a stem as tracked by the supervisor (FR-HS-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StemState {
    /// Not running.
    Stopped,
    /// Running `bootstrap`/`setup`/`build` scripts.
    Setup,
    /// Started, waiting for the first passing health check.
    Starting,
    /// Health check passing.
    Healthy,
    /// Running but the health check fails.
    Unhealthy,
    /// Running the `seed` script.
    Seeding,
    /// Shutting down.
    Stopping,
    /// Exited or could not be started.
    Failed,
    /// State cannot be determined (external stem unreachable).
    Unknown,
}

impl StemState {
    /// Every state, in lifecycle order.
    pub const ALL: [StemState; 9] = [
        StemState::Stopped,
        StemState::Setup,
        StemState::Starting,
        StemState::Healthy,
        StemState::Unhealthy,
        StemState::Seeding,
        StemState::Stopping,
        StemState::Failed,
        StemState::Unknown,
    ];

    /// Lower-case name, as serialized.
    pub fn as_str(self) -> &'static str {
        match self {
            StemState::Stopped => "stopped",
            StemState::Setup => "setup",
            StemState::Starting => "starting",
            StemState::Healthy => "healthy",
            StemState::Unhealthy => "unhealthy",
            StemState::Seeding => "seeding",
            StemState::Stopping => "stopping",
            StemState::Failed => "failed",
            StemState::Unknown => "unknown",
        }
    }

    /// The glyph for this state; `degraded` (a warning such as an unhealthy
    /// hard dependency, FR-GR-6) turns `healthy` into `!`.
    pub fn glyph(&self, degraded: bool) -> Glyph {
        match self {
            StemState::Healthy if degraded => Glyph::Degraded,
            StemState::Healthy => Glyph::Healthy,
            StemState::Unhealthy | StemState::Failed => Glyph::Failed,
            StemState::Stopped => Glyph::Stopped,
            StemState::Unknown => Glyph::Unknown,
            StemState::Setup | StemState::Starting | StemState::Seeding | StemState::Stopping => {
                Glyph::Transitioning
            }
        }
    }

    /// `true` while a process/container is (or should be) up.
    pub fn is_running(self) -> bool {
        !matches!(
            self,
            StemState::Stopped | StemState::Failed | StemState::Unknown
        )
    }
}

impl fmt::Display for StemState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error parsing a [`StemState`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown stem state `{0}`")]
pub struct UnknownStemState(pub String);

impl FromStr for StemState {
    type Err = UnknownStemState;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        StemState::ALL
            .into_iter()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| UnknownStemState(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_table() {
        let table: Vec<String> = Glyph::ALL
            .iter()
            .map(|g| format!("{} {} {} {}", g.name(), g.unicode(), g.ascii(), g.color()))
            .collect();
        assert_eq!(
            table,
            [
                "healthy ✓ OK green",
                "degraded ! WARN yellow",
                "failed ✗ FAIL red",
                "stopped · - gray",
                "unknown ? ? magenta",
                "transitioning ↻ .. cyan",
            ]
        );
        assert_eq!(
            Glyph::legend(true),
            "✓ healthy  ! degraded  ✗ failed  · stopped  ? unknown  ↻ transitioning"
        );
    }

    #[test]
    fn state_glyphs() {
        assert_eq!(StemState::Healthy.glyph(false), Glyph::Healthy);
        assert_eq!(StemState::Healthy.glyph(true), Glyph::Degraded);
        assert_eq!(StemState::Unhealthy.glyph(false), Glyph::Failed);
        assert_eq!(StemState::Failed.glyph(true), Glyph::Failed);
        assert_eq!(StemState::Stopped.glyph(false), Glyph::Stopped);
        assert_eq!(StemState::Unknown.glyph(false), Glyph::Unknown);
        for s in [
            StemState::Setup,
            StemState::Starting,
            StemState::Seeding,
            StemState::Stopping,
        ] {
            assert_eq!(s.glyph(false), Glyph::Transitioning);
        }
    }

    #[test]
    fn state_serde_display_and_parse_agree() {
        for s in StemState::ALL {
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{s}\""));
            assert_eq!(s.to_string().parse::<StemState>().unwrap(), s);
        }
        assert!("zombie".parse::<StemState>().is_err());
        assert_eq!(
            serde_json::to_string(&Glyph::Transitioning).unwrap(),
            "\"transitioning\""
        );
    }
}
