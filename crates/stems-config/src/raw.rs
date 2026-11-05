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
    ArgType, AutoApply, BuildWhen, ByteSize, CheckSeverity, CheckWhen, Condition, CpuLimit, Dur,
    FileMode, HealthType, MemoryLimit, Protocol, PullPolicy, Requirement, RestartPolicy, Scalar,
    StemType, StringOrList, WatchAction, WatchRoot,
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
    /// User-defined checks run by `stems doctor` and before `stems up`
    /// (workspace root as the working directory).
    #[serde(default)]
    pub checks: IndexMap<String, RawCheck>,
    /// Named subsets of stems.
    #[serde(default)]
    pub profiles: IndexMap<String, RawProfile>,
    /// Profile used by `up` when none is given.
    pub default_profile: Option<String>,
    /// Refuse to start stems outside the selected profile, even as dependencies.
    pub strict_profiles: Option<bool>,
    /// Profile `up` uses when none is given (`--profile` / `STEMS_PROFILE`);
    /// meant for `stems.local.yaml`, wins over `default_profile`.
    pub profile: Option<String>,
    /// Guard rails for the agent / MCP interface.
    pub agent: Option<RawAgent>,
    /// Log retention.
    pub logs: Option<RawLogs>,
    /// Metrics sampling.
    pub metrics: Option<RawMetrics>,
    /// How the daemon handles config changes (`config.reload`, 33).
    pub config: Option<RawConfigSettings>,
    /// Desktop notifications when stems fail for good (34).
    pub notifications: Option<RawNotifications>,
    /// Env names whose values are secret (47): redacted in `stems show`,
    /// `stems env`, `status -v`, the MCP server and `logs --export`, and
    /// scrubbed from log lines. Adds to the built-in name patterns; an entry
    /// prefixed `!` (`"!PUBLIC_KEY"`) is an exception that is never redacted.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Adjust the built-in secret name patterns (47).
    pub redaction: Option<RawRedaction>,
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
    /// Named configurations: overlays with the shape of `stems.local.yaml`,
    /// inline or as a path (relative to this file) to a YAML file. The
    /// selected one is merged between this file and `stems.local.yaml`.
    #[serde(default)]
    pub configurations: IndexMap<String, RawConfiguration>,
    /// Configuration used when `configuration:` does not select one.
    pub default_configuration: Option<String>,
    /// The selected configuration (`none` for no configuration); meant for
    /// `stems.local.yaml`, wins over `default_configuration`.
    pub configuration: Option<String>,
    /// The stems, keyed by name.
    #[serde(default)]
    pub stems: IndexMap<String, RawStem>,
}

/// One entry of `configurations:`: an inline overlay (the shape of
/// `stems.local.yaml`) or the path of a YAML file holding one.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Configuration")]
pub enum RawConfiguration {
    /// Path of a YAML overlay file, relative to the file naming it.
    File(String),
    /// An inline overlay.
    Inline(Box<RawWorkspace>),
}

impl<'de> Deserialize<'de> for RawConfiguration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(d, |s| Ok(Self::File(s)), Self::Inline, |_| None)
    }
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
    /// Lines kept per stem in the daemon's in-memory ring buffer.
    pub ring: Option<u32>,
}

/// `redaction:` block (47): which env names count as secret.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Redaction")]
pub struct RawRedaction {
    /// Extra name globs (case-insensitive, `*` and `?`), e.g. `"*_DSN"`.
    pub patterns: Option<Vec<String>>,
    /// Keep the built-in patterns (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`,
    /// `*PASSWD*`, `*CREDENTIAL*`, `*_KEY`, `*_KEY_*`, `*APIKEY*`);
    /// default `true`.
    pub builtin: Option<bool>,
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

/// `config:` block (33).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "ConfigSettings")]
pub struct RawConfigSettings {
    /// What the daemon does when `stems.yaml`, `stems.local.yaml` or an
    /// included file changes while it runs.
    pub reload: Option<RawReload>,
}

/// `config.reload:` block (33).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Reload")]
pub struct RawReload {
    /// Apply changes on their own: `false` (wait for `stems config apply`),
    /// `true` (what needs no restart) or `all` (everything).
    pub auto_apply: Option<AutoApply>,
}

/// `notifications:` block (34): one desktop notification when stems end up
/// `failed` and nothing will retry them.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Notifications")]
pub struct RawNotifications {
    /// Post desktop notifications (default `true`). The environment
    /// `STEMS_NOTIFICATIONS=0|off|false` on the daemon wins over the file.
    pub enabled: Option<bool>,
    /// Quiet time after a failure before the notification is posted, so
    /// failures that come together are one notification (default `2s`).
    pub debounce: Option<Dur>,
    /// After a notification, failures inside this window are held and
    /// posted together when it ends (default `30s`).
    pub cooldown: Option<Dur>,
    /// A command to run instead of the OS notifier (`/bin/sh -c`), with
    /// `STEMS_NOTIFY_TITLE`, `STEMS_NOTIFY_BODY`, `STEMS_NOTIFY_STEMS`,
    /// `STEMS_NOTIFY_WORKSPACE` in its environment and the JSON payload on
    /// stdin. `null` (default): `terminal-notifier` / `osascript` (macOS) or
    /// `notify-send` (Linux), whichever is on `PATH`.
    pub command: Option<String>,
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
    /// `false` keeps the stem listed but excludes it from runs. A bool, or a
    /// string rendered by substitution (46): `true|1|yes|on` /
    /// `false|0|no|off|""` (case-insensitive), e.g. `"${env.WITH_SEARCH}"`
    /// (an unset variable counts as false here).
    pub enabled: Option<RawEnabled>,
    /// Environment variables.
    #[serde(default)]
    pub env: IndexMap<String, Scalar>,
    /// Env files (relative to the workspace root), applied after `env`
    /// (their values win): a path, or `{path, required: false}` for a file
    /// that may be absent.
    pub env_files: Option<Vec<RawEnvFile>>,
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
    /// Values published for dependants (FR-ST-6), evaluated when the stem
    /// becomes healthy: a template string, or `{command, secret}`.
    #[serde(default)]
    pub outputs: IndexMap<String, RawOutput>,
    /// Files materialised into the codebase at run time.
    pub overlays: Option<Vec<RawOverlay>>,
    /// Free-form tags.
    pub tags: Option<Vec<String>>,
    /// Env names of this stem whose values are secret (47), on top of the
    /// workspace's `secrets` and the built-in patterns; `!NAME` is an
    /// exception (never redacted). See the workspace `secrets`.
    pub secrets: Option<Vec<String>>,
    /// Time between SIGTERM and SIGKILL.
    pub stop_grace: Option<Dur>,
    /// Tool version requirements checked for this stem (when enabled), with
    /// its codebase as the directory for `from:` lookups.
    #[serde(default)]
    pub requires: IndexMap<String, Requirement>,
    /// User-defined checks of this stem (while enabled), run in its codebase.
    #[serde(default)]
    pub checks: IndexMap<String, RawCheck>,
    /// Active variant (FR-ST-8): the name of one of `variants`, or `local`
    /// (the base definition, same as leaving it unset). Usually set in
    /// `stems.local.yaml`, e.g. by `stems switch <stem> <variant>`.
    pub variant: Option<String>,
    /// Named alternative definitions of this stem (FR-ST-8): partial stem
    /// definitions deep-merged over the base when active (`variant:`). A
    /// variant that changes `type` drops the base's type-specific fields.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub variants: IndexMap<String, RawStem>,
    /// (variants only) `false`: the variant does not inherit the base
    /// definition; it is merged over an empty stem (only `enabled` and the
    /// selection are kept), so it must be complete, `type` included.
    pub inherit: Option<bool>,

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
    /// (docker) When to pull `image`: `missing` (default), `always` or `never`.
    pub pull: Option<PullPolicy>,
    /// (docker) Build instead of pulling: `{ context, dockerfile, secrets,
    /// allow_read }`. (compose) `true` (default) / `false`: run `compose up
    /// --build`; or `{ services, before_setup, secrets, allow_read }` (44).
    pub build: Option<RawBuild>,
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
    /// (compose) Also put the stem's env into the service's container
    /// (an `env_file:` override file), not only use it for interpolation.
    pub inject_env: Option<bool>,
}

/// One `env_files:` entry: a path, or `{path, required}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "EnvFile")]
pub enum RawEnvFile {
    /// Path relative to the workspace root; the file must exist.
    Path(String),
    /// `{path, required: false}`: skipped when absent.
    Spec(RawEnvFileSpec),
}

/// `{path, required}` form of an `env_files:` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "EnvFileSpec")]
pub struct RawEnvFileSpec {
    /// Path relative to the workspace root.
    pub path: String,
    /// `false`: an absent file is skipped (default `true`: absent is
    /// `ENV_FILE_NOT_FOUND`).
    pub required: Option<bool>,
}

impl RawEnvFile {
    /// The path as written.
    pub fn path(&self) -> &str {
        match self {
            Self::Path(p) => p,
            Self::Spec(s) => &s.path,
        }
    }
    /// Whether the file must exist (default `true`).
    pub fn required(&self) -> bool {
        match self {
            Self::Path(_) => true,
            Self::Spec(s) => s.required.unwrap_or(true),
        }
    }
}

impl<'de> Deserialize<'de> for RawEnvFile {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(d, |s| Ok(Self::Path(s)), Self::Spec, |_| None)
    }
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

/// `stems.<n>.enabled` (46): a bool, or a string (usually a `${…}`
/// template) read as `true|1|yes|on` / `false|0|no|off|""` once
/// substituted. Anything else is `SCHEMA_INVALID` at resolve time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Enabled")]
pub enum RawEnabled {
    /// `true` / `false`.
    Bool(bool),
    /// A string such as `"${env.WITH_SEARCH}"`.
    Text(String),
}

impl RawEnabled {
    /// The value: a bool, or a recognised string; `None` when the string
    /// is none of the accepted spellings.
    pub fn value(&self) -> Option<bool> {
        match self {
            RawEnabled::Bool(b) => Some(*b),
            RawEnabled::Text(s) => parse_enabled(s),
        }
    }
}

/// `true|1|yes|on` → true, `false|0|no|off|""` → false (case-insensitive,
/// surrounding whitespace ignored), anything else → `None`.
pub fn parse_enabled(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

impl<'de> Deserialize<'de> for RawEnabled {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        use serde_yaml_ng::Value;
        match Value::deserialize(d)? {
            Value::Bool(b) => Ok(Self::Bool(b)),
            Value::String(s) => Ok(Self::Text(s)),
            Value::Number(n) => Ok(Self::Text(n.to_string())),
            other => Err(D::Error::custom(format!(
                "expected a boolean or a string, found {}",
                describe(&other)
            ))),
        }
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
    /// When the target is disabled (hard edges only, 46): `skip` (default)
    /// skips this stem on `up`/`start`; `start` starts it anyway.
    pub if_disabled: Option<crate::types::IfDisabled>,
}

impl<'de> Deserialize<'de> for RawDependency {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        string_or(d, |s| Ok(Self::Name(s)), Self::Spec, |_| None)
    }
}

/// An output (FR-ST-6): a template string (`${stem.self.port}` etc.) or a
/// command whose trimmed stdout is the value.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Output")]
pub enum RawOutput {
    /// A static template, e.g. `"http://localhost:${stem.self.port}"`.
    Value(String),
    /// `{command, secret}`.
    Command(RawOutputCommand),
}

/// `{command, secret}` output form.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "OutputCommand")]
pub struct RawOutputCommand {
    /// Shell command run in the stem's environment once it is healthy; its
    /// trimmed stdout is the value.
    pub command: String,
    /// Redact the value everywhere stems shows it (`<redacted>`).
    pub secret: Option<bool>,
}

impl<'de> Deserialize<'de> for RawOutput {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde_yaml_ng::Value;
        string_or(
            d,
            |s| Ok(Self::Value(s)),
            Self::Command,
            |v| match v {
                Value::Number(n) => Some(Ok(Self::Value(n.to_string()))),
                Value::Bool(b) => Some(Ok(Self::Value(b.to_string()))),
                _ => None,
            },
        )
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
    /// Stems that must be stopped (or failed) before this script runs
    /// (`stems run --stop-deps` stops them first).
    pub requires_stopped: Option<Vec<String>>,
    /// Kill the script after this long.
    pub timeout: Option<Dur>,
    /// Retries on failure.
    pub retries: Option<u32>,
    /// A failure (after retries) does not fail the stem or `up`: the stem
    /// is degraded, nothing is stamped, the script runs again next time.
    /// Only for `setup`, `seed`, `pre_start`, `post_start`, `build` and
    /// workspace `bootstrap`.
    pub soft_fail: Option<bool>,
    /// `build` only: `on_demand` (default: `stems build`, `restart --build`,
    /// watch `rebuild`), `changed` (also in the start sequence after
    /// `setup`, when its stamp is missing or changed) or `always` (on every
    /// start).
    pub when: Option<BuildWhen>,
    /// May run concurrently with other scripts of the same stem.
    pub concurrent: Option<bool>,
    /// Env var names whose values are part of the stamp hash.
    pub stamp_env: Option<Vec<String>>,
    /// Working directory (relative to the codebase).
    pub cwd: Option<String>,
    /// Inherit the environment of the `stems` command (default true).
    /// `false`: only `PATH HOME USER LOGNAME SHELL TMPDIR TERM LANG LC_*`
    /// of it, plus everything stems sets (`PORT`, `env`, `env_files`,
    /// `STEMS_*`): a reproducible `setup`/`build`.
    pub inherit_env: Option<bool>,
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
    /// (http) Extra request headers.
    pub headers: Option<IndexMap<String, String>>,
    /// (http) Accept invalid TLS certificates (self-signed dev certs).
    pub insecure: Option<bool>,
    /// (command) Shell command; exit 0 = healthy.
    pub command: Option<String>,
    /// (command) Working directory, relative to the stem's codebase.
    pub cwd: Option<String>,
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
    /// Cascade a `restart`/`rebuild` to the stem's hard dependants
    /// (default: the stem's `restart.cascade`).
    pub cascade: Option<bool>,
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
    /// Cascading restarts: when this stem is restarted (by you, a watchdog
    /// or this policy), also restart its running hard dependants,
    /// transitively, in dependency order (default `false`).
    pub cascade: Option<bool>,
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
    /// Memory threshold, e.g. `2GB` or `"2GB for 30s"`.
    pub memory: Option<MemoryLimit>,
    /// CPU threshold: cores (`1.5`) or a percentage of one core, optionally
    /// sustained (`"80% for 60s"`).
    pub cpu: Option<CpuLimit>,
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

/// `stems.<n>.build`: a bool on a compose stem (rebuild the service's
/// image on every start), an object on a docker stem (what to build) or on
/// a compose stem (which services, when, with which secrets: 44). Which
/// form and which keys a stem type accepts is checked at resolve time
/// (`SCHEMA_INVALID` at `stems.<n>.build`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(rename = "Build")]
pub enum RawBuild {
    /// (compose) Rebuild on every start (`true`, the default) or never
    /// (`false`).
    Flag(bool),
    /// Build settings (docker: `context`, `dockerfile`; compose:
    /// `services`, `before_setup`; both: `secrets`, `allow_read`).
    Spec(RawDockerBuild),
}

/// Build settings of a docker stem (`context`, `dockerfile`) or a compose
/// stem (`services`, `before_setup`); `secrets` and `allow_read` apply to
/// both (44).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "BuildSpec")]
pub struct RawDockerBuild {
    /// (docker) Build context (relative to the codebase, else the workspace
    /// root).
    pub context: Option<String>,
    /// (docker) Dockerfile, relative to the context; may live outside it.
    pub dockerfile: Option<String>,
    /// (compose) Services whose images are built (default: the stem's
    /// `service`); list extra one-shot services (e.g. `migrate`) here.
    pub services: Option<Vec<String>>,
    /// (compose) Build the images before `setup` (default `false`: at
    /// start); `setup` then gets `STEMS_IMAGE` and `STEMS_IMAGES`.
    pub before_setup: Option<bool>,
    /// BuildKit secrets (`RUN --mount=type=secret,id=…`): `{id, file}` or
    /// `{id, env}`. Never a build arg, never in a layer.
    pub secrets: Option<Vec<RawBuildSecret>>,
    /// Exact absolute paths outside the build context the build may read
    /// (Bake's `fs.read` entitlement). No globs, no symlinks. The context,
    /// the Dockerfile's directory and secret files are granted already.
    pub allow_read: Option<Vec<String>>,
}

/// One BuildKit secret: `id` plus exactly one of `file` and `env`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "BuildSecret")]
pub struct RawBuildSecret {
    /// The id the Dockerfile mounts (`--mount=type=secret,id=<id>`).
    pub id: String,
    /// A file holding the secret (e.g. a credentials JSON).
    pub file: Option<String>,
    /// An environment variable holding the secret.
    pub env: Option<String>,
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

/// A user-defined check (`checks.<name>`): a shell command, exit 0 = ok.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RawCheck {
    /// Shell command; exit 0 means the check passes.
    pub command: String,
    /// Shown when the check fails (default: `check failed`).
    pub message: Option<String>,
    /// What to do about a failure.
    pub hint: Option<String>,
    /// `fail` (default) or `warn`.
    pub severity: Option<CheckSeverity>,
    /// Kill the command after this long (default `2s`, max `30s`).
    pub timeout: Option<Dur>,
    /// Shell command `stems doctor --fix` runs to repair a failure.
    pub fix: Option<String>,
    /// Where it runs: any of `doctor`, `up`, `validate` (default
    /// `[doctor, up]`).
    pub when: Option<Vec<CheckWhen>>,
}
