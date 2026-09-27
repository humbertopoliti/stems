//! The resolved workspace model: all files merged, defaults applied, variables
//! substituted, paths absolute. `Option` only remains where "absent" is a
//! meaningful state with no default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::types::{
    ArgType, AutoApply, ByteSize, Condition, Dur, FileMode, HealthType, Protocol, Requirement,
    RestartPolicy, Scalar, StemType, WatchAction, WatchRoot,
};

/// Fixed lifecycle script names for stems (FR-SC-1).
pub const LIFECYCLE_SCRIPTS: [&str; 11] = [
    "setup",
    "seed",
    "build",
    "start",
    "stop",
    "reset",
    "health",
    "pre_start",
    "post_start",
    "pre_stop",
    "post_stop",
];

/// Fixed workspace-level script names (FR-SC-6).
pub const WORKSPACE_LIFECYCLE_SCRIPTS: [&str; 2] = ["bootstrap", "teardown"];

/// True if `name` is a stem lifecycle script (otherwise it is a custom script).
pub fn is_lifecycle_script(name: &str) -> bool {
    LIFECYCLE_SCRIPTS.contains(&name)
}

/// A fully resolved workspace.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    /// Config format version.
    pub schema_version: u32,
    /// Workspace name.
    pub name: String,
    /// Integration repo root (directory containing `stems.yaml`).
    pub root: PathBuf,
    /// The main `stems.yaml`.
    pub config_file: PathBuf,
    /// Resolved variables.
    pub vars: IndexMap<String, String>,
    /// Workspace-level env (already folded into each stem's `env`).
    pub env: IndexMap<String, String>,
    /// Tool requirements (checked by `validate` / `doctor`).
    pub requires: IndexMap<String, Requirement>,
    /// Profiles.
    pub profiles: IndexMap<String, Profile>,
    /// Default profile, if any.
    pub default_profile: Option<String>,
    /// Strict profiles.
    pub strict_profiles: bool,
    /// Local profile override (`profile:`, usually in `stems.local.yaml`):
    /// the profile `up` uses when none is given; wins over `default_profile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Agent guard rails.
    pub agent: Agent,
    /// Log retention.
    pub logs: Logs,
    /// Metrics sampling.
    pub metrics: Metrics,
    /// Config reload settings (33). Omitted from the JSON while default.
    #[serde(default, skip_serializing_if = "ConfigSettings::is_default")]
    pub config: ConfigSettings,
    /// Managed clone directory for git codebases.
    pub repos_dir: PathBuf,
    /// Workspace-level scripts.
    pub scripts: IndexMap<String, Script>,
    /// All stems, enabled and disabled, in declaration order.
    pub stems: IndexMap<String, Stem>,
}

impl Workspace {
    /// Look up a stem by name (enabled or disabled).
    pub fn stem(&self, name: &str) -> Option<&Stem> {
        self.stems.get(name)
    }

    /// Enabled stems, in declaration order.
    pub fn stems(&self) -> impl Iterator<Item = &Stem> {
        self.stems.values().filter(|s| s.enabled)
    }

    /// Disabled stems (`enabled: false`), in declaration order.
    pub fn disabled_stems(&self) -> impl Iterator<Item = &Stem> {
        self.stems.values().filter(|s| !s.enabled)
    }

    /// Enabled stems in declaration order. The dependency order (layers
    /// for parallel start) is `stems_core::graph::start_order`, which this
    /// crate cannot call.
    pub fn stems_in_order(&self) -> Vec<&Stem> {
        self.stems().collect()
    }
}

/// A profile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Profile {
    /// Explicit stem list.
    Stems(Vec<String>),
    /// Alias of another profile (resolution is deliverable 26).
    Alias(String),
}

/// `agent:` settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agent {
    /// Allow destructive MCP operations.
    pub allow_destructive: bool,
    /// If non-empty, only these tools are exposed.
    pub allowed_tools: Vec<String>,
    /// Tools never exposed.
    pub denied_tools: Vec<String>,
}

/// `logs:` settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Logs {
    /// Rotation size.
    pub max_size: ByteSize,
    /// Rotated files kept.
    pub keep: u32,
    /// Lines kept per stem in the in-memory ring buffer.
    pub ring: u32,
}

/// `metrics:` settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Metrics {
    /// Sampling interval.
    pub interval: Dur,
    /// Persist samples.
    pub persist: bool,
}

/// `config:` settings (33).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigSettings {
    /// `config.reload`.
    #[serde(default)]
    pub reload: ReloadSettings,
}

impl ConfigSettings {
    /// Every value is the default.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `config.reload:` settings (33).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReloadSettings {
    /// What the daemon applies on its own when the config changes.
    #[serde(default)]
    pub auto_apply: AutoApply,
}

/// A resolved stem.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stem {
    /// Name (the map key).
    pub name: String,
    /// Stem type (serialized as `type`) and its type-specific settings,
    /// flattened into the stem.
    #[serde(flatten)]
    pub runtime: StemRuntime,
    /// Description.
    pub description: Option<String>,
    /// Codebase; absent for most docker/external stems.
    pub codebase: Option<Codebase>,
    /// `false` = listed but excluded from runs.
    pub enabled: bool,
    /// Effective env: workspace `env` overlaid with the stem's `env`.
    pub env: IndexMap<String, String>,
    /// The subset of `env` whose keys were set by `stems.local.yaml`
    /// (workspace-level or this stem's `env:` there), with their effective
    /// values. The runtime re-applies these after `env_files` so the
    /// FR-ST-4 order holds: workspace env → stem env → env_files → local → shell.
    #[serde(default)]
    pub local_env: BTreeMap<String, String>,
    /// Env files (absolute), applied by the runtime after `env`.
    pub env_files: Vec<PathBuf>,
    /// Ports.
    pub ports: Vec<Port>,
    /// Dependencies.
    pub depends_on: Vec<Dependency>,
    /// Scripts (lifecycle + custom), in declaration order.
    pub scripts: IndexMap<String, Script>,
    /// Health check; `None` only for external stems that declare none.
    pub health: Option<Health>,
    /// Watchdog rules.
    pub watch: Vec<Watch>,
    /// Restart policy.
    pub restart: Restart,
    /// Resource warning thresholds.
    pub limits: Limits,
    /// Outputs (FR-ST-6), evaluated when the stem becomes healthy.
    pub outputs: IndexMap<String, Output>,
    /// Overlays.
    pub overlays: Vec<Overlay>,
    /// Tags.
    pub tags: Vec<String>,
    /// SIGTERM → SIGKILL grace.
    pub stop_grace: Dur,
    /// Active variant (FR-ST-8): its name, or `local` when the base
    /// definition is active. Absent for a stem without `variants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Names of the declared variants (FR-ST-8), in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}

impl Stem {
    /// The stem type.
    pub fn kind(&self) -> StemType {
        self.runtime.kind()
    }

    /// The first declared port, which `${stem.<n>.port}` refers to.
    pub fn primary_port(&self) -> Option<&Port> {
        self.ports.first()
    }
    /// The working directory scripts default to: the codebase, else the workspace root.
    pub fn default_cwd<'a>(&'a self, ws_root: &'a Path) -> &'a Path {
        self.codebase.as_ref().map_or(ws_root, Codebase::path)
    }
}

/// A resolved codebase.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Codebase {
    /// A local directory (absolute, lexically normalised, `~` expanded).
    Local {
        /// Directory.
        path: PathBuf,
    },
    /// A git repository cloned into `path` (not cloned by the config loader).
    Git {
        /// Repository URL.
        url: String,
        /// Branch / tag / commit; `None` = the remote's default branch.
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        /// Checkout directory (`<repos_dir>/<stem>` unless overridden).
        path: PathBuf,
    },
}

impl Codebase {
    /// The directory holding the code.
    pub fn path(&self) -> &Path {
        match self {
            Self::Local { path } | Self::Git { path, .. } => path,
        }
    }
}

/// A host port: fixed, or allocated at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortRef {
    /// Fixed number.
    Fixed(u16),
    /// `port: auto`; references to it stay as `${stem.<n>.port}` until the
    /// daemon allocates a port (deliverable 10).
    Auto,
}

impl Serialize for PortRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Fixed(n) => s.serialize_u16(*n),
            Self::Auto => s.serialize_str("auto"),
        }
    }
}

impl<'de> Deserialize<'de> for PortRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match crate::raw::RawPortValue::deserialize(d)? {
            crate::raw::RawPortValue::Number(n) => Ok(Self::Fixed(n)),
            crate::raw::RawPortValue::Auto(_) => Ok(Self::Auto),
        }
    }
}

/// A resolved port.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Port {
    /// Name (`port<i>` when not given).
    pub name: String,
    /// Host-side port.
    pub port: PortRef,
    /// Container-side port (docker/compose: defaults to the fixed host port).
    pub container_port: Option<u16>,
}

/// A resolved dependency edge.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    /// Target stem.
    pub stem: String,
    /// Condition.
    pub condition: Condition,
    /// Soft edge.
    pub soft: bool,
    /// Protocol metadata.
    pub protocol: Option<Protocol>,
    /// Address metadata.
    pub via: Option<String>,
}

/// Where a script's body comes from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScriptSource {
    /// Inline shell command.
    Command(String),
    /// Script file (absolute path in the integration repo).
    File(PathBuf),
}

/// A declared output (FR-ST-6). Serialized as in `stems.yaml`: a string, or
/// `{command, secret}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Output {
    /// A template rendered with the stem's own context (references to
    /// `auto` ports are filled in when it runs).
    Value(String),
    /// A shell command run once the stem is healthy; trimmed stdout.
    Command {
        /// The command.
        command: String,
        /// Redacted wherever stems shows it.
        #[serde(default)]
        secret: bool,
    },
}

impl Output {
    /// Whether the value must be redacted.
    pub fn is_secret(&self) -> bool {
        matches!(self, Self::Command { secret: true, .. })
    }
}

/// Is `name` a valid output name (`[A-Za-z_][A-Za-z0-9_]*`)? Output names
/// become env var suffixes (`STEMS_<STEM>_OUTPUT_<NAME>`).
pub fn is_output_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A resolved script.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Script {
    /// Command or file.
    #[serde(flatten)]
    pub source: ScriptSource,
    /// Description.
    pub description: Option<String>,
    /// Declared args.
    pub args: Vec<ScriptArg>,
    /// Stamp inputs (globs relative to the codebase).
    pub inputs: Vec<String>,
    /// Stems that must be healthy first.
    pub requires: Vec<String>,
    /// Timeout; `None` = no timeout.
    pub timeout: Option<Dur>,
    /// Retries.
    pub retries: u32,
    /// Concurrent execution allowed.
    pub concurrent: bool,
    /// Env var names hashed into stamps.
    pub stamp_env: Vec<String>,
    /// Working directory (absolute). `cwd: workspace` resolves to the
    /// integration repo root.
    pub cwd: PathBuf,
    /// `cwd:` was given explicitly (`cwd: workspace` included). Scripts of a
    /// stem without a codebase run in its state directory unless this is set.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cwd_set: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A resolved script argument.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScriptArg {
    /// Name.
    pub name: String,
    /// Type.
    #[serde(rename = "type")]
    pub kind: ArgType,
    /// Default.
    pub default: Option<Scalar>,
    /// Required.
    pub required: bool,
    /// Help.
    pub description: Option<String>,
    /// Enum values.
    pub values: Vec<String>,
}

/// A health-check port: a number, or a deferred `${stem.<n>.port}` reference.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum HealthPort {
    /// Fixed port.
    Number(u16),
    /// Deferred reference (auto port), substituted by the daemon.
    Deferred(String),
}

/// A resolved health check.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Health {
    /// Check kind.
    #[serde(rename = "type")]
    pub kind: HealthType,
    /// Host (tcp/grpc/http default host).
    pub host: String,
    /// Port (tcp/grpc).
    pub port: Option<HealthPort>,
    /// URL (http).
    pub url: Option<String>,
    /// Expected status; `None` = any 2xx.
    pub status: Option<u16>,
    /// Body match (http).
    pub body_contains: Option<String>,
    /// Extra request headers (http).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Accept invalid TLS certificates (http).
    #[serde(default, skip_serializing_if = "is_false")]
    pub insecure: bool,
    /// Command (command).
    pub command: Option<String>,
    /// Working directory of the command, relative to the stem's codebase
    /// (command; `None`: the codebase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Interval.
    pub interval: Dur,
    /// Timeout.
    pub timeout: Dur,
    /// Retries.
    pub retries: u32,
    /// Start period.
    pub start_period: Dur,
    /// Start timeout.
    pub start_timeout: Dur,
}

/// A resolved watch rule.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Watch {
    /// Globs.
    pub paths: Vec<String>,
    /// Ignore globs (built-in defaults + user additions).
    pub ignore: Vec<String>,
    /// Debounce.
    pub debounce: Dur,
    /// Action.
    pub action: WatchAction,
    /// Settle period.
    pub settle: Dur,
    /// Root.
    pub root: WatchRoot,
    /// Cascade a restart to the hard dependants; `None` = the stem's
    /// `restart.cascade`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascade: Option<bool>,
}

/// A resolved restart policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Restart {
    /// Policy.
    pub policy: RestartPolicy,
    /// Max restarts within `window`.
    pub max: u32,
    /// Window.
    pub window: Dur,
    /// Backoff.
    pub backoff: Backoff,
    /// Restart on unhealthy.
    pub on_unhealthy: bool,
    /// Grace before an unhealthy restart.
    pub unhealthy_grace: Dur,
    /// Restarting this stem also restarts its hard dependants (cascade).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cascade: bool,
}

/// Backoff parameters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Backoff {
    /// First delay.
    pub initial: Dur,
    /// Cap.
    pub max: Dur,
    /// Multiplier.
    pub factor: f64,
}

/// Resource thresholds (no threshold = no warning).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Limits {
    /// Memory.
    pub memory: Option<ByteSize>,
    /// CPU cores.
    pub cpu: Option<f64>,
    /// How long memory must stay above `memory` (`"2GB for 30s"`; 25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_for: Option<Dur>,
    /// How long CPU must stay above `cpu` (`"80% for 60s"`; 25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_for: Option<Dur>,
}

/// Overlay source.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OverlaySource {
    /// Template with substitution (absolute path).
    Template(PathBuf),
    /// Verbatim file (absolute path).
    File(PathBuf),
}

/// A resolved overlay.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Overlay {
    /// Source.
    #[serde(flatten)]
    pub source: OverlaySource,
    /// Destination relative to the codebase (as written).
    pub dest: PathBuf,
    /// Keep on `down`.
    pub keep: bool,
    /// File mode.
    pub mode: FileMode,
}

/// Type-specific settings, tagged by `type`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StemRuntime {
    /// `type: process`.
    Process(ProcessSpec),
    /// `type: docker`.
    Docker(Box<DockerSpec>),
    /// `type: compose`.
    Compose(ComposeSpec),
    /// `type: external`.
    External,
}

impl StemRuntime {
    /// The stem type.
    pub fn kind(&self) -> StemType {
        match self {
            Self::Process(_) => StemType::Process,
            Self::Docker(_) => StemType::Docker,
            Self::Compose(_) => StemType::Compose,
            Self::External => StemType::External,
        }
    }
}

/// Process settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessSpec {
    /// Working directory (absolute).
    pub cwd: PathBuf,
    /// Command; `None` means `scripts.start` is used.
    pub command: Option<String>,
    /// Shell for `command` and inline scripts.
    pub shell: String,
    /// Attach stdin.
    pub stdin: bool,
}

/// Docker settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerSpec {
    /// Image (absent when `build` is used).
    pub image: Option<String>,
    /// Build settings.
    pub build: Option<DockerBuild>,
    /// Volumes.
    pub volumes: Vec<String>,
    /// CMD override.
    pub command: Option<Vec<String>>,
    /// Entrypoint override.
    pub entrypoint: Option<Vec<String>>,
    /// Network.
    pub network: Option<String>,
    /// Extra labels (stems' own labels are added by the runtime).
    pub labels: IndexMap<String, String>,
    /// Healthcheck override.
    pub healthcheck: Option<DockerHealthcheck>,
}

/// Docker build settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerBuild {
    /// Context directory (absolute).
    pub context: PathBuf,
    /// Dockerfile (absolute).
    pub dockerfile: PathBuf,
}

/// Docker healthcheck override (fields left unset keep the image's values).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DockerHealthcheck {
    /// Test.
    pub test: Option<Vec<String>>,
    /// Interval.
    pub interval: Option<Dur>,
    /// Timeout.
    pub timeout: Option<Dur>,
    /// Retries.
    pub retries: Option<u32>,
    /// Start period.
    pub start_period: Option<Dur>,
    /// Disable.
    pub disable: bool,
}

/// Compose settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeSpec {
    /// Compose file (absolute); `None` is a validation error (06).
    pub file: Option<PathBuf>,
    /// Service.
    pub service: String,
    /// Project name.
    pub project_name: String,
    /// Adopt a running service.
    pub adopt: bool,
}
