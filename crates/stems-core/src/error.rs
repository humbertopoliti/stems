//! The single error type used by every stems component, and its stable codes
//! (REQUIREMENTS.md §7.5). Errors are part of the API: `code` strings, the
//! JSON shape and the exit-code mapping never change without a major version.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::de::Deserializer;
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_config::{ConfigErrors, ConfigPath, Diagnostic, SpanIndex};

pub use stems_config::Span;

/// Process exit codes (DECISIONS.md "Exit codes").
pub mod exit {
    /// Success.
    pub const OK: i32 = 0;
    /// Runtime failure.
    pub const RUNTIME: i32 = 1;
    /// Config, validation or usage error.
    pub const CONFIG: i32 = 2;
    /// Partial success (some stems failed on `up`) or orphans found.
    pub const PARTIAL: i32 = 3;
    /// The daemon is unavailable.
    pub const DAEMON: i32 = 4;
}

macro_rules! error_codes {
    ($( $(#[$doc:meta])* $variant:ident = $name:literal, $exit:ident, $meaning:literal; )*) => {
        /// Stable error code. Serialized as its `SCREAMING_SNAKE_CASE` name.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum ErrorCode {
            $( $(#[$doc])* $variant, )*
        }

        impl ErrorCode {
            /// Every code, in catalogue order.
            pub const ALL: &'static [ErrorCode] = &[ $( ErrorCode::$variant, )* ];

            /// Every code, in catalogue order.
            pub fn all() -> &'static [ErrorCode] {
                Self::ALL
            }

            /// The stable string form, e.g. `"CYCLE"`.
            pub fn as_str(self) -> &'static str {
                match self { $( ErrorCode::$variant => $name, )* }
            }

            /// Process exit code for a command failing with this error.
            pub fn exit_code(self) -> i32 {
                match self { $( ErrorCode::$variant => exit::$exit, )* }
            }

            /// One-line meaning (the §7.5 catalogue entry).
            pub fn meaning(self) -> &'static str {
                match self { $( ErrorCode::$variant => $meaning, )* }
            }
        }

        impl FromStr for ErrorCode {
            type Err = UnknownErrorCode;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $name => Ok(ErrorCode::$variant), )*
                    other => Err(UnknownErrorCode(other.to_string())),
                }
            }
        }
    };
}

error_codes! {
    // --- config / validation (exit 2) -------------------------------------
    /// Config does not match the schema.
    SchemaInvalid = "SCHEMA_INVALID", CONFIG,
        "the config does not match the schema (YAML syntax, unknown field, wrong type, invalid value)";
    /// `schema_version` newer than this binary.
    SchemaVersionUnsupported = "SCHEMA_VERSION_UNSUPPORTED", CONFIG,
        "`schema_version` is not supported by this stems binary";
    /// No `stems.yaml` found.
    WorkspaceNotFound = "WORKSPACE_NOT_FOUND", CONFIG,
        "no stems.yaml found (--workspace, STEMS_WORKSPACE, or walking up from the cwd)";
    /// `include`/`extends` target missing.
    IncludeNotFound = "INCLUDE_NOT_FOUND", CONFIG,
        "an `include:` or `extends:` target does not exist or cannot be read";
    /// `include`/`extends` loop, or an `extends` chain deeper than 5.
    IncludeCycle = "INCLUDE_CYCLE", CONFIG,
        "`include:` / `extends:` files form a cycle, or an `extends` chain is deeper than 5 files";
    /// Config file unreadable.
    ConfigReadFailed = "CONFIG_READ_FAILED", CONFIG,
        "a config file exists but cannot be read";
    /// Same stem defined in two files.
    DuplicateStem = "DUPLICATE_STEM", CONFIG,
        "the same stem is defined in two included files";
    /// Unresolvable `${…}` reference.
    UnresolvedVariable = "UNRESOLVED_VARIABLE", CONFIG,
        "a `${var.x}` / `${stem.x…}` / `${env.X}` reference cannot be resolved";
    /// A command or reference names a stem that does not exist.
    UnknownStem = "UNKNOWN_STEM", CONFIG,
        "a command argument, profile or script `requires` names a stem that does not exist";
    /// `depends_on` names an undefined stem.
    UnknownDependency = "UNKNOWN_DEPENDENCY", CONFIG,
        "a `depends_on` edge names a stem that is not defined";
    /// Hard dependency on a disabled stem.
    DependencyDisabled = "DEPENDENCY_DISABLED", CONFIG,
        "an enabled stem has a hard `depends_on` edge to a stem with `enabled: false`";
    /// Hard-edge cycle.
    Cycle = "CYCLE", CONFIG,
        "hard `depends_on` edges form a cycle (mark one edge `soft: true` to break it)";
    /// `condition: seeded` on a stem without a seed script.
    SeededWithoutSeed = "SEEDED_WITHOUT_SEED", CONFIG,
        "a `depends_on` edge uses `condition: seeded` on a stem with no `seed` script";
    /// Two stems declare the same host port.
    PortConflict = "PORT_CONFLICT", CONFIG,
        "two enabled stems statically declare the same host port";
    /// Script file missing.
    ScriptNotFound = "SCRIPT_NOT_FOUND", CONFIG,
        "a script `file:` does not exist in the integration repo, or a named script does not exist";
    /// Script file outside the integration repo.
    ScriptOutsideWorkspace = "SCRIPT_OUTSIDE_WORKSPACE", CONFIG,
        "a script `file:` points outside the integration repo (`..`, absolute path or symlink)";
    /// Script arguments do not match the declared `args`.
    ScriptArgsInvalid = "SCRIPT_ARGS_INVALID", CONFIG,
        "arguments passed to a script do not match its declared `args`";
    /// Local codebase directory missing.
    CodebaseNotFound = "CODEBASE_NOT_FOUND", CONFIG,
        "a local `codebase:` path does not exist or is not a directory";
    /// Overlay destination exists and is not stems-owned.
    OverlayConflict = "OVERLAY_CONFLICT", CONFIG,
        "an overlay `dest` already exists in the codebase and was not written by stems";
    /// Overlay destination tracked in git (a `validate` warning, never an error).
    OverlayTrackedFile = "OVERLAY_TRACKED_FILE", CONFIG,
        "an overlay `dest` is tracked in the codebase's git index, so materialising it would dirty the repo (a warning)";
    /// `requires` tool missing or too old.
    ToolVersion = "TOOL_VERSION", CONFIG,
        "a tool listed under `requires:` is missing or its version does not satisfy the range";
    /// Strict profile misses a dependency.
    ProfileMissingDependency = "PROFILE_MISSING_DEPENDENCY", CONFIG,
        "with `strict_profiles: true`, a profile omits a hard dependency of one of its stems";
    /// A profile name that is not defined.
    UnknownProfile = "UNKNOWN_PROFILE", CONFIG,
        "`--profile`, `STEMS_PROFILE`, `profile:`, `default_profile` or a profile alias names a profile that is not defined (or aliases form a loop)";
    /// A variant name the stem does not declare (FR-ST-8).
    UnknownVariant = "UNKNOWN_VARIANT", CONFIG,
        "`stems.<stem>.variant` or `stems switch <stem> <variant>` names a variant the stem does not declare";
    /// Destructive op without confirmation.
    DestructiveNotConfirmed = "DESTRUCTIVE_NOT_CONFIRMED", CONFIG,
        "a destructive operation was requested without `--yes` / `confirm: true`";
    /// Destructive op disabled for agents.
    DestructiveNotAllowed = "DESTRUCTIVE_NOT_ALLOWED", CONFIG,
        "a destructive MCP operation was refused because `agent.allow_destructive` is false";
    /// Bad command line.
    Usage = "USAGE", CONFIG,
        "invalid command line (unknown subcommand, bad flag or argument)";
    // --- runtime (exit 1) --------------------------------------------------
    /// `stems init` in an existing workspace.
    AlreadyInitialised = "ALREADY_INITIALISED", RUNTIME,
        "`stems init` found an existing stems.yaml";
    /// A port is held by a live process.
    PortInUse = "PORT_IN_USE", RUNTIME,
        "a declared port is already held by another process (with its pid and command)";
    /// A script exited non-zero.
    ScriptFailed = "SCRIPT_FAILED", RUNTIME,
        "a script exited non-zero (exit code and last 20 lines in details)";
    /// A script's `requires` stems are not healthy.
    ScriptRequiresUnmet = "SCRIPT_REQUIRES_UNMET", RUNTIME,
        "a script's `requires:` stems are not healthy (pass `--start-deps` to start them)";
    /// `setup` failed.
    SetupFailed = "SETUP_FAILED", RUNTIME,
        "a stem's `setup` script failed, so the stem was not started";
    /// Health check never passed.
    HealthTimeout = "HEALTH_TIMEOUT", RUNTIME,
        "a stem did not become healthy within its `start_timeout`";
    /// Start did not complete.
    StartTimeout = "START_TIMEOUT", RUNTIME,
        "a stem did not start within its timeout";
    /// The process could not be spawned or exited before it was ready.
    StartFailed = "START_FAILED", RUNTIME,
        "a stem's process could not be spawned, or exited before it became ready (exit code in details)";
    /// Stop needed SIGKILL.
    StopTimeoutKilled = "STOP_TIMEOUT_KILLED", RUNTIME,
        "a stem ignored SIGTERM for `stop_grace` and was killed";
    /// Restart budget exhausted.
    MaxRestarts = "MAX_RESTARTS", RUNTIME,
        "a stem crashed more than `restart.max` times within `restart.window`";
    /// Operation on a stem stems does not manage.
    NotManaged = "NOT_MANAGED", RUNTIME,
        "the operation does not apply to an external stem (stems never starts or stops it)";
    /// Stop refused because running stems depend on it.
    HasDependants = "HAS_DEPENDANTS", RUNTIME,
        "the stem has running dependants (use `--cascade`)";
    /// Docker daemon unreachable.
    DockerUnavailable = "DOCKER_UNAVAILABLE", RUNTIME,
        "Docker is not installed or its daemon is not reachable";
    /// Image pull failed.
    ImagePullFailed = "IMAGE_PULL_FAILED", RUNTIME,
        "pulling a docker image failed";
    /// `docker compose` failed.
    ComposeFailed = "COMPOSE_FAILED", RUNTIME,
        "a `docker compose` command failed";
    /// Compose project name taken by a foreign project.
    ComposeProjectInUse = "COMPOSE_PROJECT_IN_USE", RUNTIME,
        "a compose project with this name is already running outside stems (set `adopt: true`)";
    /// `git` binary missing.
    GitNotInstalled = "GIT_NOT_INSTALLED", RUNTIME,
        "a git codebase needs `git`, which is not on PATH";
    /// `git clone`/`fetch` failed.
    GitCloneFailed = "GIT_CLONE_FAILED", RUNTIME,
        "cloning or fetching a git codebase failed";
    /// Another daemon holds the workspace lock.
    LockHeld = "LOCK_HELD", RUNTIME,
        "another stems daemon holds this workspace's lock";
    /// `stems upgrade` could not run the package manager, or it failed.
    UpgradeFailed = "UPGRADE_FAILED", RUNTIME,
        "`stems upgrade` could not run the upgrade command, or it failed";
    /// Command exists but its deliverable has not landed.
    NotImplemented = "NOT_IMPLEMENTED", RUNTIME,
        "the command is not implemented yet in this build";
    /// Bug.
    Internal = "INTERNAL", RUNTIME,
        "an unexpected internal error (a bug: please report it)";
    // --- partial (exit 3) --------------------------------------------------
    /// Orphaned processes/containers found.
    OrphansFound = "ORPHANS_FOUND", PARTIAL,
        "processes or containers from a previous run are still alive but not tracked";
    // --- daemon (exit 4) ---------------------------------------------------
    /// No daemon.
    DaemonNotRunning = "DAEMON_NOT_RUNNING", DAEMON,
        "the stems daemon for this workspace is not running";
    /// Client/daemon version skew.
    DaemonVersionMismatch = "DAEMON_VERSION_MISMATCH", DAEMON,
        "the running daemon is a different stems version than this client";
}

/// A string that is not a known [`ErrorCode`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown error code `{0}`")]
pub struct UnknownErrorCode(pub String);

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// A stems error: stable code, human message, optional config path, source
/// location, actionable hint and structured details.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    /// Stable code.
    pub code: ErrorCode,
    /// Human message (one line, no trailing period).
    pub message: String,
    /// Path into the merged config, when the error is about config.
    pub path: Option<ConfigPath>,
    /// Source location (file, line, col).
    pub span: Option<Span>,
    /// What to do about it.
    pub hint: Option<String>,
    /// Structured data for agents (`{}` when there is none).
    pub details: Value,
}

impl Error {
    /// New error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            span: None,
            hint: None,
            details: Value::Object(Default::default()),
        }
    }

    /// Attach a config path.
    #[must_use]
    pub fn with_path(mut self, path: ConfigPath) -> Self {
        self.path = Some(path);
        self
    }

    /// Attach a source location.
    #[must_use]
    pub fn with_span(mut self, span: Option<Span>) -> Self {
        self.span = span;
        self
    }

    /// Attach a hint.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach structured details.
    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// Fill `span` from `spans` using `path` (or its closest ancestor), unless
    /// already set.
    #[must_use]
    pub fn located(mut self, spans: &SpanIndex) -> Self {
        if self.span.is_none()
            && let Some(p) = &self.path
        {
            self.span = spans.locate(p).cloned();
        }
        self
    }

    /// `INTERNAL` error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
            .with_hint("this is a bug in stems; please report it with the command you ran")
    }

    /// `USAGE` error.
    pub fn usage(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::Usage, message).with_hint(hint)
    }

    /// `NOT_IMPLEMENTED` error for a command that lands in deliverable `nn`.
    pub fn not_implemented(what: &str, deliverable: &str) -> Self {
        Self::new(
            ErrorCode::NotImplemented,
            format!("`{what}` is not implemented until deliverable {deliverable}"),
        )
        .with_hint("upgrade stems, or use a command that is available (`stems --help`)")
    }

    /// Exit code of this error.
    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// Ordering key: (file, line, col); errors without a location sort last.
    fn sort_key(&self) -> (bool, Option<&Span>) {
        (self.span.is_none(), self.span.as_ref())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        match (&self.span, &self.path) {
            (Some(s), Some(p)) if !p.is_root() => write!(f, "\n  at {s} ({p})")?,
            (Some(s), _) => write!(f, "\n  at {s}")?,
            (None, Some(p)) if !p.is_root() => write!(f, "\n  at {p}")?,
            _ => {}
        }
        if let Some(h) = &self.hint {
            write!(f, "\n  hint: {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl Serialize for Error {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Error", 6)?;
        st.serialize_field("code", &self.code)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("path", &self.path)?;
        st.serialize_field("location", &self.span)?;
        st.serialize_field("hint", &self.hint)?;
        st.serialize_field("details", &self.details)?;
        st.end()
    }
}

#[derive(Deserialize)]
struct ErrorWire {
    code: ErrorCode,
    message: String,
    #[serde(default)]
    path: Option<ConfigPath>,
    #[serde(default)]
    location: Option<Span>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    details: Value,
}

impl<'de> Deserialize<'de> for Error {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = ErrorWire::deserialize(d)?;
        Ok(Self {
            code: w.code,
            message: w.message,
            path: w.path,
            span: w.location,
            hint: w.hint,
            details: if w.details.is_null() {
                Value::Object(Default::default())
            } else {
                w.details
            },
        })
    }
}

/// Hint for a loader diagnostic that carries none, so every config error
/// says what to do.
fn default_hint(code: ErrorCode, path: Option<&ConfigPath>) -> Option<String> {
    let at = path.filter(|p| !p.is_root()).map_or_else(
        || "the value at the reported location".to_string(),
        |p| format!("`{p}`"),
    );
    match code {
        ErrorCode::SchemaInvalid => Some(format!(
            "fix {at}; every field and allowed value is in schema/stems.schema.json (point your editor at it for completion)"
        )),
        ErrorCode::IncludeNotFound => Some(
            "fix the path (it is relative to the file that names it) or create the file".into(),
        ),
        ErrorCode::ConfigReadFailed => Some(
            "check that the file is readable by your user (permissions, not a directory)".into(),
        ),
        _ => None,
    }
}

impl From<Diagnostic> for Error {
    fn from(d: Diagnostic) -> Self {
        let code = d.code.parse().unwrap_or(ErrorCode::Internal);
        let hint = d.hint.or_else(|| default_hint(code, d.path.as_ref()));
        Self {
            code,
            message: d.message,
            path: d.path,
            span: d.location,
            hint,
            details: d
                .details
                .unwrap_or_else(|| Value::Object(Default::default())),
        }
    }
}

impl From<ConfigErrors> for Error {
    /// The first error of the set (use [`Errors::from`] to keep them all).
    fn from(e: ConfigErrors) -> Self {
        e.errors
            .into_iter()
            .next()
            .map(Error::from)
            .unwrap_or_else(|| Error::internal("config loading failed without an error"))
    }
}

/// A set of errors (validation collects all of them).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Errors(pub Vec<Error>);

impl Errors {
    /// Empty set.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Add an error.
    pub fn push(&mut self, e: Error) {
        self.0.push(e);
    }

    /// True if there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of errors.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate.
    pub fn iter(&self) -> std::slice::Iter<'_, Error> {
        self.0.iter()
    }

    /// The highest exit code among the errors (`0` when empty).
    pub fn exit_code(&self) -> i32 {
        self.0
            .iter()
            .map(Error::exit_code)
            .max()
            .unwrap_or(exit::OK)
    }

    /// Sort by (file, line, col), located errors first; stable, so errors at
    /// the same place keep the order they were found in.
    pub fn sort(&mut self) {
        sort_errors(&mut self.0);
    }

    /// Consume into the inner vector.
    pub fn into_vec(self) -> Vec<Error> {
        self.0
    }
}

/// Sort errors by (file, line, col), located errors first (stable).
pub fn sort_errors(errors: &mut [Error]) {
    errors.sort_by(|a, b| {
        let (ka, kb) = (a.sort_key(), b.sort_key());
        match ka.0.cmp(&kb.0) {
            Ordering::Equal => match (ka.1, kb.1) {
                (Some(x), Some(y)) => (&x.file, x.line, x.col).cmp(&(&y.file, y.line, y.col)),
                _ => Ordering::Equal,
            },
            o => o,
        }
    });
}

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

impl From<Error> for Errors {
    fn from(e: Error) -> Self {
        Self(vec![e])
    }
}

impl From<Vec<Error>> for Errors {
    fn from(v: Vec<Error>) -> Self {
        Self(v)
    }
}

impl From<ConfigErrors> for Errors {
    fn from(e: ConfigErrors) -> Self {
        Self(e.errors.into_iter().map(Error::from).collect())
    }
}

impl IntoIterator for Errors {
    type Item = Error;
    type IntoIter = std::vec::IntoIter<Error>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Errors {
    type Item = &'a Error;
    type IntoIter = std::slice::Iter<'a, Error>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl FromIterator<Error> for Errors {
    fn from_iter<I: IntoIterator<Item = Error>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn span(file: &str, line: usize, col: usize) -> Option<Span> {
        Some(Span {
            file: PathBuf::from(file),
            line,
            col,
        })
    }

    #[test]
    fn codes_roundtrip_as_strings() {
        for c in ErrorCode::all() {
            assert_eq!(c.as_str().parse::<ErrorCode>().unwrap(), *c);
            let j = serde_json::to_string(c).unwrap();
            assert_eq!(j, format!("\"{}\"", c.as_str()));
            assert_eq!(serde_json::from_str::<ErrorCode>(&j).unwrap(), *c);
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch == '_')
            );
            assert!(!c.meaning().is_empty());
        }
        assert!("NOPE".parse::<ErrorCode>().is_err());
    }

    #[test]
    fn exit_codes_follow_the_catalogue() {
        use ErrorCode::*;
        for (c, e) in [
            (SchemaInvalid, 2),
            (Cycle, 2),
            (PortConflict, 2),
            (ToolVersion, 2),
            (Usage, 2),
            (PortInUse, 1),
            (ScriptFailed, 1),
            (NotImplemented, 1),
            (NotManaged, 1),
            (AlreadyInitialised, 1),
            (OrphansFound, 3),
            (DaemonNotRunning, 4),
            (DaemonVersionMismatch, 4),
        ] {
            assert_eq!(c.exit_code(), e, "{c}");
        }
        for c in ErrorCode::all() {
            assert!((1..=4).contains(&c.exit_code()), "{c}");
        }
    }

    #[test]
    fn serializes_with_location() {
        let e = Error::new(ErrorCode::Cycle, "a -> b -> a")
            .with_path("stems.a.depends_on".parse().unwrap())
            .with_span(span("/ws/stems.yaml", 3, 5))
            .with_hint("mark it soft");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "code": "CYCLE",
                "message": "a -> b -> a",
                "path": "stems.a.depends_on",
                "location": { "file": "/ws/stems.yaml", "line": 3, "col": 5 },
                "hint": "mark it soft",
                "details": {}
            })
        );
        let back: Error = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
        let bare = serde_json::to_value(Error::new(ErrorCode::Internal, "x")).unwrap();
        assert_eq!(bare["path"], Value::Null);
        assert_eq!(bare["location"], Value::Null);
    }

    #[test]
    fn display_is_code_message_location_hint() {
        let e = Error::new(ErrorCode::UnknownDependency, "no such stem `db`")
            .with_path("stems.api.depends_on".parse().unwrap())
            .with_span(span("stems.yaml", 7, 9))
            .with_hint("define `db`");
        assert_eq!(
            e.to_string(),
            "UNKNOWN_DEPENDENCY: no such stem `db`\n  at stems.yaml:7:9 (stems.api.depends_on)\n  hint: define `db`"
        );
        let e = Error::new(ErrorCode::Usage, "bad flag");
        assert_eq!(e.to_string(), "USAGE: bad flag");
    }

    #[test]
    fn errors_exit_code_is_the_max_and_sort_is_by_location() {
        let mut es = Errors::new();
        assert_eq!(es.exit_code(), 0);
        es.push(Error::new(ErrorCode::ScriptFailed, "late").with_span(span("b.yaml", 1, 1)));
        es.push(Error::new(ErrorCode::Internal, "unlocated"));
        es.push(Error::new(ErrorCode::Cycle, "second").with_span(span("a.yaml", 9, 1)));
        es.push(Error::new(ErrorCode::Cycle, "first").with_span(span("a.yaml", 2, 7)));
        es.push(Error::new(ErrorCode::Cycle, "first-b").with_span(span("a.yaml", 2, 7)));
        es.push(Error::new(ErrorCode::DaemonNotRunning, "unlocated 2"));
        assert_eq!(es.exit_code(), 4);
        es.sort();
        let order: Vec<_> = es.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            order,
            [
                "first",
                "first-b",
                "second",
                "late",
                "unlocated",
                "unlocated 2"
            ]
        );
    }

    #[test]
    fn converts_config_diagnostics() {
        let d = Diagnostic::new("UNRESOLVED_VARIABLE", "unknown variable `${var.nope}`")
            .with_path("stems.a.env.X".parse().unwrap())
            .with_hint("declare it")
            .with_location(span("s.yaml", 4, 3));
        let e = Error::from(d.clone());
        assert_eq!(e.code, ErrorCode::UnresolvedVariable);
        assert_eq!(e.span, span("s.yaml", 4, 3));
        assert_eq!(e.hint.as_deref(), Some("declare it"));
        let ce = ConfigErrors {
            errors: vec![d, Diagnostic::new("WHAT", "odd")],
        };
        let es = Errors::from(ce.clone());
        assert_eq!(es.len(), 2);
        assert_eq!(es.0[1].code, ErrorCode::Internal);
        assert_eq!(Error::from(ce).code, ErrorCode::UnresolvedVariable);
    }
}
