//! The file schema: what a single `stems.yaml` / include / `stems.local.yaml`
//! may contain. Everything is optional here because partial files (includes,
//! local overlays) use the same schema; defaults are applied later in
//! [`crate::defaults`]. The JSON Schema in `schema/stems.schema.json` is
//! generated from [`RawWorkspace`].

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::types::{
    ArgType, ByteSize, Condition, Dur, FileMode, HealthType, Protocol, Requirement, RestartPolicy,
    Scalar, StemType, StringOrList, WatchAction, WatchRoot,
};

/// Deserialize "a string, or else a `T`", giving `T`'s own error message
/// instead of serde's opaque "did not match any variant of untagged enum".
fn string_or<'de, D, T, R>(
    d: D,
    from_str: impl FnOnce(String) -> Result<R, String>,
    from_t: impl FnOnce(T) -> R,
    from_other: impl FnOnce(&serde_yaml_ng::Value) -> Option<Result<R, String>>,
) -> Result<R, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    use serde::de::Error;
    let v = serde_yaml_ng::Value::deserialize(d)?;
    if let serde_yaml_ng::Value::String(s) = v {
        return from_str(s).map_err(D::Error::custom);
    }
    if let Some(r) = from_other(&v) {
        return r.map_err(D::Error::custom);
    }
    if !v.is_mapping() {
        return Err(D::Error::custom(format!(
            "expected a string or a mapping, found {}",
            describe(&v)
        )));
    }
    serde_yaml_ng::from_value::<T>(v)
        .map(from_t)
        .map_err(|e| D::Error::custom(e.to_string()))
}

fn describe(v: &serde_yaml_ng::Value) -> String {
    use serde_yaml_ng::Value;
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => format!("boolean `{b}`"),
        Value::Number(n) => format!("number `{n}`"),
        Value::String(s) => format!("string `{s}`"),
        Value::Sequence(_) => "a sequence".into(),
        Value::Mapping(_) => "a mapping".into(),
        Value::Tagged(t) => format!("tagged value `{}`", t.tag),
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// Root of a `stems.yaml` file (or an included / local file).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "stems workspace configuration")]
#[schemars(rename = "Workspace")]
pub struct RawWorkspace {
    /// Config format version. Only `1` exists.
    pub schema_version: Option<u32>,
    /// Workspace name (defaults to the integration repo's directory name).
    pub name: Option<String>,
    /// Named variables, usable anywhere as `${var.<name>}`.
    #[serde(default)]
    pub vars: IndexMap<String, Scalar>,
    /// Environment applied to every stem (stem `env` wins).
    #[serde(default)]
    pub env: IndexMap<String, Scalar>,
    /// Tool version requirements, e.g. `node: ">=20"`, or
    /// `mytool: { version: ">=1.2", command: "mytool -V", regex: "v([0-9.]+)" }`.
    #[serde(default)]
    pub requires: IndexMap<String, Requirement>,
    /// Named subsets of stems.
    #[serde(default)]
    pub profiles: IndexMap<String, RawProfile>,
    /// Profile used by `up` when none is given.
    pub default_profile: Option<String>,
    /// Refuse to start stems outside the selected profile, even as dependencies.
    pub strict_profiles: Option<bool>,
    /// Guard rails for the agent / MCP interface.
    pub agent: Option<RawAgent>,
    /// Log retention.
    pub logs: Option<RawLogs>,
    /// Metrics sampling.
    pub metrics: Option<RawMetrics>,
    /// Where git codebases are cloned (default `.stems/repos`).
    pub repos_dir: Option<String>,
    /// Workspace-level scripts (`bootstrap`, `teardown`, custom).
    #[serde(default)]
    pub scripts: IndexMap<String, RawScript>,
    /// Extra YAML files merged into this one (paths relative to this file).
    #[serde(default)]
    pub include: Vec<String>,
    /// A base workspace file merged underneath this one.
    pub extends: Option<String>,
    /// The stems, keyed by name.
    #[serde(default)]
    pub stems: IndexMap<String, RawStem>,
}

/// A profile: a list of stems, or the name of another profile.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Profile")]
pub enum RawProfile {
    /// `backend: [postgres, api]`
    Stems(Vec<String>),
    /// `default: backend` (alias of another profile).
    Alias(String),
}

/// `agent:` block.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Agent")]
pub struct RawAgent {
    /// Allow destructive MCP operations.
    pub allow_destructive: Option<bool>,
    /// If non-empty, only these MCP tools are exposed.
    pub allowed_tools: Option<Vec<String>>,
    /// MCP tools never exposed.
    pub denied_tools: Option<Vec<String>>,
}

/// `logs:` block.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Logs")]
pub struct RawLogs {
    /// Rotate a stem's log file at this size.
    pub max_size: Option<ByteSize>,
    /// Rotated files to keep.
    pub keep: Option<u32>,
}

/// `metrics:` block.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Metrics")]
pub struct RawMetrics {
    /// Sampling interval.
    pub interval: Option<Dur>,
    /// Persist samples to disk.
    pub persist: Option<bool>,
}

// ---------------------------------------------------------------------------
// Stem
// ---------------------------------------------------------------------------

/// One stem. Type-specific fields are listed flat; using a field that does
/// not apply to the stem's `type` is reported as a diagnostic.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Stem")]
pub struct RawStem {
    // --- common -----------------------------------------------------------
    /// Must equal the map key if given.
    pub name: Option<String>,
    /// `process`, `docker`, `compose` or `external`. Required once all files are merged.
    #[serde(rename = "type")]
    pub kind: Option<StemType>,
    /// Human description.
    pub description: Option<String>,
    /// Local path or git repository with the stem's code.
    pub codebase: Option<RawCodebase>,
    /// `false` keeps the stem listed but excludes it from runs.
    pub enabled: Option<bool>,
    /// Environment variables.
    #[serde(default)]
    pub env: IndexMap<String, Scalar>,
    /// Env files (relative to the workspace root), applied after `env`.
    pub env_files: Option<Vec<String>>,
    /// Ports: `5432`, `"15432:5432"` or `{name, port, container_port}`.
    pub ports: Option<Vec<RawPort>>,
    /// Dependencies: a stem name or `{stem, condition, soft, protocol, via}`.
    pub depends_on: Option<Vec<RawDependency>>,
    /// Lifecycle (`setup`, `seed`, `build`, `start`, `stop`, `reset`, `health`,
    /// `pre_start`, `post_start`, `pre_stop`, `post_stop`) and custom scripts.
    #[serde(default)]
    pub scripts: IndexMap<String, RawScript>,
    /// Health check.
    pub health: Option<RawHealth>,
    /// Watchdog rules.
    pub watch: Option<Vec<RawWatch>>,
    /// Crash restart policy.
    pub restart: Option<RawRestart>,
    /// Resource warning thresholds.
    pub limits: Option<RawLimits>,
    /// Values published for dependants (evaluated at runtime).
    #[serde(default)]
    pub outputs: IndexMap<String, String>,
    /// Files materialised into the codebase at run time.
    pub overlays: Option<Vec<RawOverlay>>,
    /// Free-form tags.
    pub tags: Option<Vec<String>>,
    /// Time between SIGTERM and SIGKILL.
    pub stop_grace: Option<Dur>,

    // --- process ----------------------------------------------------------
    /// (process) Working directory, relative to the codebase.
    pub cwd: Option<String>,
    /// (process, docker) Command to run. Process: a shell string; docker: overrides CMD.
    pub command: Option<StringOrList>,
    /// (process) Shell used to run `command`.
    pub shell: Option<String>,
    /// (process) Attach stdin.
    pub stdin: Option<bool>,

    // --- docker -----------------------------------------------------------
    /// (docker) Image reference.
    pub image: Option<String>,
    /// (docker) Build instead of pulling.
    pub build: Option<RawDockerBuild>,
    /// (docker) Volume specs, `name:/path` or `./host:/path`.
    pub volumes: Option<Vec<String>>,
    /// (docker) Entrypoint override.
    pub entrypoint: Option<StringOrList>,
    /// (docker) Network to attach to.
    pub network: Option<String>,
    /// (docker) Extra container labels.
    #[serde(default)]
    pub labels: IndexMap<String, String>,
    /// (docker) Container healthcheck override.
    pub healthcheck: Option<RawDockerHealthcheck>,

    // --- compose ----------------------------------------------------------
    /// (compose) Compose file (relative to the workspace root).
    pub file: Option<String>,
    /// (compose) Service name (defaults to the stem name).
    pub service: Option<String>,
    /// (compose) Compose project name (defaults to the workspace name).
    pub project_name: Option<String>,
    /// (compose) Adopt an already-running service instead of starting it.
    pub adopt: Option<bool>,
}

/// `codebase:` — a path string (local path or git URL) or `{git, ref, path}`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Codebase")]
pub enum RawCodebase {
    /// Local path (absolute, `~/…`, or relative to the integration repo), or a git URL.
    Path(String),
    /// A git repository.
    Git(RawGitCodebase),
}

/// `{git, ref, path}` codebase.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "GitCodebase")]
pub struct RawGitCodebase {
    /// Repository URL (ssh, https or file).
    pub git: String,
    /// Branch, tag or commit.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Checkout directory (default `<repos_dir>/<stem>`).
    pub path: Option<String>,
}

impl<'de> Deserialize<'de> for RawCodebase {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(d, |s| Ok(Self::Path(s)), Self::Git, |_| None)
    }
}

/// A port declaration.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Port")]
pub enum RawPort {
    /// `5432`
    Number(u16),
    /// `"15432:5432"` (host:container) or `"5432"`.
    Mapping(String),
    /// `{name, port, container_port}`
    Spec(RawPortSpec),
}

/// `{name, port, container_port}` port form.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "PortSpec")]
pub struct RawPortSpec {
    /// Port name, referenced as `${stem.<n>.ports.<name>}`.
    pub name: Option<String>,
    /// Host port number, or `auto` to allocate a free one at start.
    pub port: RawPortValue,
    /// Container-side port (docker/compose); defaults to `port`.
    #[serde(alias = "container")]
    pub container_port: Option<u16>,
}

/// A port number or `auto`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "PortValue")]
pub enum RawPortValue {
    /// A fixed port.
    Number(u16),
    /// Allocate at runtime.
    Auto(AutoKeyword),
}

/// The literal `auto`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AutoKeyword {
    /// `auto`
    Auto,
}

impl<'de> Deserialize<'de> for RawPortValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match serde_yaml_ng::Value::deserialize(d)? {
            serde_yaml_ng::Value::String(s) if s == "auto" => Ok(Self::Auto(AutoKeyword::Auto)),
            serde_yaml_ng::Value::Number(n) => n
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .map(Self::Number)
                .ok_or_else(|| D::Error::custom(format!("invalid port `{n}`: expected 0-65535"))),
            other => Err(D::Error::custom(format!(
                "invalid port {}: expected a number or `auto`",
                describe(&other)
            ))),
        }
    }
}

impl<'de> Deserialize<'de> for RawPort {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(
            d,
            |s| Ok(Self::Mapping(s)),
            Self::Spec,
            |v| match v {
                serde_yaml_ng::Value::Number(n) => Some(
                    n.as_u64()
                        .and_then(|n| u16::try_from(n).ok())
                        .map(Self::Number)
                        .ok_or_else(|| format!("invalid port `{n}`: expected 0-65535")),
                ),
                _ => None,
            },
        )
    }
}

/// A dependency edge.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Dependency")]
pub enum RawDependency {
    /// Just the stem name (condition `healthy`).
    Name(String),
    /// Full form.
    Spec(RawDependencySpec),
}

/// `{stem, condition, soft, protocol, via}`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "DependencySpec")]
pub struct RawDependencySpec {
    /// The stem depended upon.
    pub stem: String,
    /// When the edge is satisfied (default `healthy`).
    pub condition: Option<Condition>,
    /// Informational only: no ordering, allowed in cycles.
    pub soft: Option<bool>,
    /// Protocol metadata.
    pub protocol: Option<Protocol>,
    /// Address metadata, e.g. `${stem.api.port}`.
    pub via: Option<String>,
}

impl<'de> Deserialize<'de> for RawDependency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(d, |s| Ok(Self::Name(s)), Self::Spec, |_| None)
    }
}

/// A script: an inline shell string or a full spec.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Script")]
pub enum RawScript {
    /// Inline shell command.
    Inline(String),
    /// Full form.
    Spec(Box<RawScriptSpec>),
}

impl<'de> Deserialize<'de> for RawScript {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(
            d,
            |s| Ok(Self::Inline(s)),
            |s| Self::Spec(Box::new(s)),
            |_| None,
        )
    }
}

/// Full script form. Exactly one of `command` / `file` is expected.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "ScriptSpec")]
pub struct RawScriptSpec {
    /// Inline shell command.
    pub command: Option<String>,
    /// Script file in the integration repo (relative to the workspace root).
    pub file: Option<String>,
    /// Shown in the TUI and as the MCP tool description.
    pub description: Option<String>,
    /// Declared arguments.
    pub args: Option<Vec<RawArg>>,
    /// Stamp inputs: globs relative to the codebase.
    pub inputs: Option<Vec<String>>,
    /// Stems that must be healthy before this script runs.
    pub requires: Option<Vec<String>>,
    /// Kill the script after this long.
    pub timeout: Option<Dur>,
    /// Retries on failure.
    pub retries: Option<u32>,
    /// May run concurrently with other scripts of the same stem.
    pub concurrent: Option<bool>,
    /// Env var names whose values are part of the stamp hash.
    pub stamp_env: Option<Vec<String>>,
    /// Working directory (relative to the codebase).
    pub cwd: Option<String>,
}

/// A declared script argument.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Arg")]
pub struct RawArg {
    /// Argument name.
    pub name: String,
    /// Type (default `string`).
    #[serde(rename = "type")]
    pub kind: Option<ArgType>,
    /// Default value.
    pub default: Option<Scalar>,
    /// Must be given.
    pub required: Option<bool>,
    /// Help text.
    pub description: Option<String>,
    /// Allowed values for `type: enum`.
    pub values: Option<Vec<String>>,
}

/// Health check.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Health")]
pub struct RawHealth {
    /// Check kind.
    #[serde(rename = "type")]
    pub kind: Option<HealthType>,
    /// (tcp, grpc) Host (default `localhost`).
    pub host: Option<String>,
    /// (tcp, grpc) Port (default: the stem's first port).
    pub port: Option<RawHealthPort>,
    /// (http) URL.
    pub url: Option<String>,
    /// (http) Expected status (default: any 2xx).
    pub status: Option<u16>,
    /// (http) Response body must contain this.
    pub body_contains: Option<String>,
    /// (command) Shell command; exit 0 = healthy.
    pub command: Option<String>,
    /// Probe interval.
    pub interval: Option<Dur>,
    /// Probe timeout.
    pub timeout: Option<Dur>,
    /// Consecutive failures before unhealthy.
    pub retries: Option<u32>,
    /// Failures during this period after start are not counted.
    pub start_period: Option<Dur>,
    /// Give up on becoming healthy after this long.
    pub start_timeout: Option<Dur>,
}

/// A health port: a number or a substitution string like `${stem.web.port}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "HealthPort")]
pub enum RawHealthPort {
    /// Fixed port.
    Number(u16),
    /// Expression, resolved by substitution.
    Expr(String),
}

/// A watchdog rule.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Watch")]
pub struct RawWatch {
    /// Globs to watch.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Extra globs to ignore (added to the built-in ignores).
    pub ignore: Option<Vec<String>>,
    /// Debounce window.
    pub debounce: Option<Dur>,
    /// Action when fired (default `restart`).
    pub action: Option<WatchAction>,
    /// Quiet period after an action before the rule can fire again.
    pub settle: Option<Dur>,
    /// What `paths` are relative to (default `codebase`).
    pub root: Option<WatchRoot>,
}

/// Restart policy.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Restart")]
pub struct RawRestart {
    /// `never`, `on-failure` or `always`.
    pub policy: Option<RestartPolicy>,
    /// Max restarts within `window` before the stem is marked failed.
    pub max: Option<u32>,
    /// Sliding window for `max`.
    pub window: Option<Dur>,
    /// Exponential backoff.
    pub backoff: Option<RawBackoff>,
    /// Restart when the health check reports unhealthy.
    pub on_unhealthy: Option<bool>,
    /// How long a stem may stay unhealthy before `on_unhealthy` restarts it.
    pub unhealthy_grace: Option<Dur>,
}

/// Backoff parameters.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Backoff")]
pub struct RawBackoff {
    /// First delay.
    pub initial: Option<Dur>,
    /// Delay cap.
    pub max: Option<Dur>,
    /// Multiplier.
    pub factor: Option<f64>,
}

/// Resource warning thresholds.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Limits")]
pub struct RawLimits {
    /// Memory threshold, e.g. `2GB`.
    pub memory: Option<ByteSize>,
    /// CPU threshold in cores (1.0 = one full core).
    pub cpu: Option<f64>,
}

/// An overlay file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Overlay")]
pub struct RawOverlay {
    /// Template (with `${…}` substitution) in the integration repo.
    pub template: Option<String>,
    /// Plain file copied verbatim from the integration repo.
    pub file: Option<String>,
    /// Destination, relative to the codebase.
    pub dest: String,
    /// Keep the file on `down`.
    pub keep: Option<bool>,
    /// File mode.
    pub mode: Option<FileMode>,
}

/// Docker build settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "DockerBuild")]
pub struct RawDockerBuild {
    /// Build context (relative to the codebase, else the workspace root).
    pub context: Option<String>,
    /// Dockerfile, relative to the context.
    pub dockerfile: Option<String>,
}

/// Docker healthcheck override.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "DockerHealthcheck")]
pub struct RawDockerHealthcheck {
    /// Test command.
    pub test: Option<StringOrList>,
    /// Interval.
    pub interval: Option<Dur>,
    /// Timeout.
    pub timeout: Option<Dur>,
    /// Retries.
    pub retries: Option<u32>,
    /// Start period.
    pub start_period: Option<Dur>,
    /// Disable the image's healthcheck.
    pub disable: Option<bool>,
}
