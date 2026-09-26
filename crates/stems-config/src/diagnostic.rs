//! Diagnostics and the load error type. Deliverable 06 maps these onto
//! `stems_core::Error`; this crate must not depend on stems-core.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::path::ConfigPath;

/// Stable diagnostic codes produced by this crate.
pub mod codes {
    /// No `stems.yaml` found (explicit path, `STEMS_WORKSPACE`, or walking up).
    pub const WORKSPACE_NOT_FOUND: &str = "WORKSPACE_NOT_FOUND";
    /// YAML syntax error, unknown field, wrong type, missing `type`, field not
    /// valid for the stem type.
    pub const SCHEMA_INVALID: &str = "SCHEMA_INVALID";
    /// A `${…}` reference that cannot be resolved (incl. variable cycles).
    pub const UNRESOLVED_VARIABLE: &str = "UNRESOLVED_VARIABLE";
    /// An `include:` / `extends:` target does not exist or cannot be read.
    pub const INCLUDE_NOT_FOUND: &str = "INCLUDE_NOT_FOUND";
    /// `include:` / `extends:` files form a cycle.
    pub const INCLUDE_CYCLE: &str = "INCLUDE_CYCLE";
    /// A config file exists but cannot be read.
    pub const CONFIG_READ_FAILED: &str = "CONFIG_READ_FAILED";
}

/// A source location (1-based line and column).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    /// File.
    pub file: PathBuf,
    /// Line, 1-based.
    pub line: usize,
    /// Column, 1-based.
    pub col: usize,
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file.display(), self.line, self.col)
    }
}

/// A problem found while loading. Non-fatal ones are returned in
/// [`crate::Resolved::diagnostics`]; fatal ones in [`ConfigErrors`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable code from [`codes`].
    pub code: String,
    /// Human message.
    pub message: String,
    /// Path into the merged config tree.
    pub path: Option<ConfigPath>,
    /// Suggested fix.
    pub hint: Option<String>,
    /// Source location, when known.
    pub location: Option<Span>,
}

impl Diagnostic {
    /// New diagnostic with a code and message.
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            path: None,
            hint: None,
            location: None,
        }
    }
    /// Attach a config path.
    #[must_use]
    pub fn with_path(mut self, path: ConfigPath) -> Self {
        self.path = Some(path);
        self
    }
    /// Attach a hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    /// Attach a location.
    #[must_use]
    pub fn with_location(mut self, span: Option<Span>) -> Self {
        self.location = span;
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(p) = &self.path {
            write!(f, " (at {p})")?;
        }
        if let Some(l) = &self.location {
            write!(f, " [{l}]")?;
        }
        Ok(())
    }
}

/// Fatal load failure: one or more diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub struct ConfigErrors {
    /// The errors, in discovery order.
    pub errors: Vec<Diagnostic>,
}

impl ConfigErrors {
    /// Wrap a single diagnostic.
    pub fn one(d: Diagnostic) -> Self {
        Self { errors: vec![d] }
    }
}

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}
