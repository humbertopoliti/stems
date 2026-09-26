//! Small value types shared by the raw (file) schema and the resolved model.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------
// Durations
// ---------------------------------------------------------------------------

/// A humantime-style duration: `200ms`, `2s`, `1m`, `1m 30s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Dur(pub Duration);

impl Dur {
    /// Milliseconds constructor.
    pub const fn from_millis(ms: u64) -> Self {
        Self(Duration::from_millis(ms))
    }
    /// Seconds constructor.
    pub const fn from_secs(s: u64) -> Self {
        Self(Duration::from_secs(s))
    }
    /// The underlying [`Duration`].
    pub fn as_duration(self) -> Duration {
        self.0
    }
}

impl fmt::Display for Dur {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_zero() {
            return f.write_str("0s");
        }
        write!(f, "{}", humantime::format_duration(self.0))
    }
}

impl FromStr for Dur {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t == "0" {
            return Ok(Self(Duration::ZERO));
        }
        humantime::parse_duration(t)
            .map(Self)
            .map_err(|e| format!("invalid duration `{s}` ({e}); expected e.g. `200ms`, `2s`, `1m`"))
    }
}

impl Serialize for Dur {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Dur {
    fn schema_name() -> Cow<'static, str> {
        "Duration".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "A duration such as `200ms`, `2s`, `1m` or `1m 30s`.",
            "pattern": "^\\s*([0-9]+\\s*[a-zA-Z]+\\s*)+$|^0$"
        })
    }
}

// ---------------------------------------------------------------------------
// Byte sizes
// ---------------------------------------------------------------------------

/// A byte size: `512`, `64KB`, `10MB`, `2GB` (binary multiples: 1KB = 1024 B).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteSize(pub u64);

const KIB: u64 = 1024;
const UNITS: [(&str, u64); 4] = [
    ("TB", KIB * KIB * KIB * KIB),
    ("GB", KIB * KIB * KIB),
    ("MB", KIB * KIB),
    ("KB", KIB),
];

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (unit, mult) in UNITS {
            if self.0 >= mult && self.0.is_multiple_of(mult) {
                return write!(f, "{}{unit}", self.0 / mult);
            }
        }
        write!(f, "{}B", self.0)
    }
}

impl FromStr for ByteSize {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let bad = || format!("invalid size `{s}`; expected e.g. `512KB`, `10MB`, `2GB`");
        let n: u64 = num.parse().map_err(|_| bad())?;
        let mult = match unit.trim().to_ascii_uppercase().as_str() {
            "" | "B" => 1,
            "K" | "KB" | "KIB" => KIB,
            "M" | "MB" | "MIB" => KIB * KIB,
            "G" | "GB" | "GIB" => KIB * KIB * KIB,
            "T" | "TB" | "TIB" => KIB * KIB * KIB * KIB,
            _ => return Err(bad()),
        };
        n.checked_mul(mult).map(Self).ok_or_else(bad)
    }
}

impl Serialize for ByteSize {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Scalar::deserialize(d)? {
            Scalar::Int(n) if n >= 0 => Ok(Self(n as u64)),
            Scalar::String(s) => s.parse().map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!("invalid size `{other}`"))),
        }
    }
}

impl JsonSchema for ByteSize {
    fn schema_name() -> Cow<'static, str> {
        "ByteSize".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A size in bytes, or with a unit: `512KB`, `10MB`, `2GB` (1KB = 1024 bytes).",
            "anyOf": [
                { "type": "integer", "minimum": 0 },
                { "type": "string", "pattern": "^\\s*[0-9]+\\s*([kKmMgGtT]([iI]?[bB])?|[bB])?\\s*$" }
            ]
        })
    }
}

// ---------------------------------------------------------------------------
// Resource limits (deliverable 25)
// ---------------------------------------------------------------------------

/// Split `"<value> for <duration>"` into the value and the optional duration.
fn split_for(s: &str) -> Result<(&str, Option<Dur>), String> {
    let t = s.trim();
    match t.to_ascii_lowercase().find(" for ") {
        Some(i) => {
            let d: Dur = t[i + 5..].trim().parse()?;
            Ok((t[..i].trim(), Some(d)))
        }
        None => Ok((t, None)),
    }
}

/// `limits.memory`: a size (`2GB`, bytes as an integer), optionally
/// sustained (`"2GB for 30s"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryLimit {
    /// The threshold.
    pub size: ByteSize,
    /// How long usage must stay above it before it counts.
    pub for_: Option<Dur>,
}

impl FromStr for MemoryLimit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (v, for_) = split_for(s)?;
        Ok(Self {
            size: v.parse()?,
            for_,
        })
    }
}

impl fmt::Display for MemoryLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.for_ {
            Some(d) => write!(f, "{} for {d}", self.size),
            None => write!(f, "{}", self.size),
        }
    }
}

impl Serialize for MemoryLimit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MemoryLimit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Scalar::deserialize(d)? {
            Scalar::Int(n) if n >= 0 => Ok(Self {
                size: ByteSize(n as u64),
                for_: None,
            }),
            Scalar::String(s) => s.parse().map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "invalid memory limit `{other}`; expected e.g. `2GB` or `2GB for 30s`"
            ))),
        }
    }
}

impl JsonSchema for MemoryLimit {
    fn schema_name() -> Cow<'static, str> {
        "MemoryLimit".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A memory threshold: a size (`2GB`, or bytes as an integer), optionally sustained: `2GB for 30s`.",
            "anyOf": [
                { "type": "integer", "minimum": 0 },
                { "type": "string", "pattern": "^\\s*[0-9]+\\s*([kKmMgGtT]([iI]?[bB])?|[bB])?(\\s+[fF][oO][rR]\\s+.+)?\\s*$" }
            ]
        })
    }
}

/// `limits.cpu`: cores as a number (`1.5`), or a percentage of one core as
/// a string (`"80%"`, `"150%"`), optionally sustained (`"80% for 60s"`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuLimit {
    /// The threshold in cores (`80%` = 0.8).
    pub cores: f64,
    /// How long usage must stay above it before it counts.
    pub for_: Option<Dur>,
}

impl FromStr for CpuLimit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad =
            || format!("invalid cpu limit `{s}`; expected e.g. `80%`, `1.5` or `80% for 60s`");
        let (v, for_) = split_for(s).map_err(|_| bad())?;
        let cores = match v.strip_suffix('%') {
            Some(p) => p.trim().parse::<f64>().map_err(|_| bad())? / 100.0,
            None => v.parse::<f64>().map_err(|_| bad())?,
        };
        if !cores.is_finite() || cores <= 0.0 {
            return Err(bad());
        }
        Ok(Self { cores, for_ })
    }
}

impl fmt::Display for CpuLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.cores * 100.0)?;
        if let Some(d) = self.for_ {
            write!(f, " for {d}")?;
        }
        Ok(())
    }
}

impl Serialize for CpuLimit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.for_ {
            None => s.serialize_f64(self.cores),
            Some(_) => s.serialize_str(&self.to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for CpuLimit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let cores = match Scalar::deserialize(d)? {
            Scalar::Int(n) => n as f64,
            Scalar::Float(f) => f,
            Scalar::String(s) => return s.parse().map_err(serde::de::Error::custom),
            other => {
                return Err(serde::de::Error::custom(format!(
                    "invalid cpu limit `{other}`; expected e.g. `80%`, `1.5` or `80% for 60s`"
                )));
            }
        };
        if !cores.is_finite() || cores <= 0.0 {
            return Err(serde::de::Error::custom(format!(
                "invalid cpu limit `{cores}`: must be positive"
            )));
        }
        Ok(Self { cores, for_: None })
    }
}

impl JsonSchema for CpuLimit {
    fn schema_name() -> Cow<'static, str> {
        "CpuLimit".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A CPU threshold: cores as a number (`1.5`), or a percentage of one core (`80%`, `150%`), optionally sustained: `80% for 60s`.",
            "anyOf": [
                { "type": "number", "exclusiveMinimum": 0 },
                { "type": "string", "pattern": "^\\s*[0-9]+(\\.[0-9]+)?\\s*%?(\\s+[fF][oO][rR]\\s+.+)?\\s*$" }
            ]
        })
    }
}

// ---------------------------------------------------------------------------
// Scalars and string-or-list
// ---------------------------------------------------------------------------

/// A YAML scalar that is used as a string (env values, vars, arg defaults).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Scalar {
    /// `true` / `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    String(String),
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Bool(b) => write!(f, "{b}"),
            Scalar::Int(i) => write!(f, "{i}"),
            Scalar::Float(x) => write!(f, "{x}"),
            Scalar::String(s) => f.write_str(s),
        }
    }
}

/// A command given either as one shell string or as an argv list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StringOrList {
    /// `"npm run dev"`
    String(String),
    /// `["npm", "run", "dev"]`
    List(Vec<String>),
}

impl StringOrList {
    /// As an argv-ish list (a single string becomes a one-element list).
    pub fn to_list(&self) -> Vec<String> {
        match self {
            Self::String(s) => vec![s.clone()],
            Self::List(l) => l.clone(),
        }
    }
    /// As a single string (lists are joined with spaces).
    pub fn to_joined(&self) -> String {
        match self {
            Self::String(s) => s.clone(),
            Self::List(l) => l.join(" "),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool requirements
// ---------------------------------------------------------------------------

/// A `requires:` entry: a version range for a known tool (`node: ">=20"`), or
/// a range plus the command that prints the version and an optional regex
/// whose first capture group (else whole match) is the version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Requirement {
    /// `">=20"`, `"^1.2"`, `">=1 <2"`.
    Version(String),
    /// `{ version, command, regex }`.
    Custom(RequirementSpec),
}

/// Long form of a [`Requirement`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequirementSpec {
    /// Version range, e.g. `">=1.2 <2"`.
    pub version: String,
    /// Shell command printing the version; default `<tool> --version`.
    pub command: Option<String>,
    /// Regex extracting the version from the command output (first capture
    /// group, else the whole match); default: the first `N[.N[.N]]` number.
    pub regex: Option<String>,
}

impl Requirement {
    /// The version range.
    pub fn version(&self) -> &str {
        match self {
            Self::Version(v) => v,
            Self::Custom(s) => &s.version,
        }
    }
    /// Custom version command, if any.
    pub fn command(&self) -> Option<&str> {
        match self {
            Self::Version(_) => None,
            Self::Custom(s) => s.command.as_deref(),
        }
    }
    /// Custom extraction regex, if any.
    pub fn regex(&self) -> Option<&str> {
        match self {
            Self::Version(_) => None,
            Self::Custom(s) => s.regex.as_deref(),
        }
    }
}

// ---------------------------------------------------------------------------
// File modes
// ---------------------------------------------------------------------------

/// A Unix file mode, written in octal: `"0600"`, `644`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileMode(pub u32);

impl fmt::Display for FileMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04o}", self.0)
    }
}

impl FromStr for FileMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim().trim_start_matches("0o");
        u32::from_str_radix(t, 8)
            .ok()
            .filter(|m| *m <= 0o7777)
            .map(Self)
            .ok_or_else(|| format!("invalid file mode `{s}`; expected octal such as `0644`"))
    }
}

impl Serialize for FileMode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FileMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Integers are read as their decimal digits interpreted as octal, so
        // `mode: 644` and `mode: "0644"` mean the same thing.
        let s = Scalar::deserialize(d)?.to_string();
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for FileMode {
    fn schema_name() -> Cow<'static, str> {
        "FileMode".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Octal Unix file mode, e.g. \"0644\".",
            "anyOf": [
                { "type": "string", "pattern": "^(0o)?[0-7]{1,4}$" },
                { "type": "integer", "minimum": 0 }
            ]
        })
    }
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

/// The four stem types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum StemType {
    /// A local process (process group) started by stems.
    Process,
    /// A single Docker container.
    Docker,
    /// A service of a docker compose project.
    Compose,
    /// Monitored only; never started or stopped.
    External,
}

impl fmt::Display for StemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Process => "process",
            Self::Docker => "docker",
            Self::Compose => "compose",
            Self::External => "external",
        })
    }
}

/// When a `depends_on` edge is satisfied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    /// The dependency's process/container is running.
    Started,
    /// The dependency passed its health check.
    Healthy,
    /// The dependency is healthy and its `seed` script completed.
    Seeded,
}

/// Informational protocol carried by a `depends_on` edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// HTTP.
    Http,
    /// gRPC.
    Grpc,
    /// AMQP.
    Amqp,
    /// Raw TCP.
    Tcp,
}

/// Health check kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HealthType {
    /// TCP connect succeeds.
    Tcp,
    /// HTTP request returns the expected status (and body).
    Http,
    /// A shell command exits 0.
    Command,
    /// The container's own Docker healthcheck.
    Docker,
    /// The process (group) is alive.
    Process,
    /// gRPC health protocol.
    Grpc,
}

/// Crash restart policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Never restart.
    Never,
    /// Restart when the stem exits non-zero / crashes.
    OnFailure,
    /// Always restart when it exits.
    Always,
}

/// Declared type of a script argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ArgType {
    /// Free text.
    String,
    /// Integer.
    Int,
    /// Floating point number.
    Float,
    /// Boolean.
    Bool,
    /// One of `values`.
    Enum,
    /// A filesystem path.
    Path,
}

/// What a watch rule's `paths` are relative to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WatchRoot {
    /// The stem's codebase (default).
    Codebase,
    /// The integration repo (workspace root).
    Workspace,
}

/// What happens when a watch rule fires.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WatchAction {
    /// Restart the stem.
    Restart,
    /// Run `build`, then restart.
    Rebuild,
    /// Run the named script (`script:<name>`).
    Script(String),
    /// Send a signal to the process group (`signal:<SIG>`).
    Signal(String),
}

impl fmt::Display for WatchAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Restart => f.write_str("restart"),
            Self::Rebuild => f.write_str("rebuild"),
            Self::Script(s) => write!(f, "script:{s}"),
            Self::Signal(s) => write!(f, "signal:{s}"),
        }
    }
}

impl FromStr for WatchAction {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || {
            format!(
                "invalid watch action `{s}`; expected `restart`, `rebuild`, `script:<name>` or `signal:<SIG>`"
            )
        };
        match s.trim() {
            "restart" => Ok(Self::Restart),
            "rebuild" => Ok(Self::Rebuild),
            t => match t.split_once(':') {
                Some(("script", n)) if !n.trim().is_empty() => Ok(Self::Script(n.trim().into())),
                Some(("signal", n)) if !n.trim().is_empty() => Ok(Self::Signal(n.trim().into())),
                _ => Err(bad()),
            },
        }
    }
}

impl Serialize for WatchAction {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WatchAction {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for WatchAction {
    fn schema_name() -> Cow<'static, str> {
        "WatchAction".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "`restart`, `rebuild`, `script:<name>` or `signal:<SIG>`.",
            "pattern": "^(restart|rebuild|script:.+|signal:.+)$"
        })
    }
}

// ---------------------------------------------------------------------------
// Config reload (33)
// ---------------------------------------------------------------------------

/// `config.reload.auto_apply`: what the daemon applies on its own when the
/// config changes on disk. Written `false` (default), `true` or `"all"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AutoApply {
    /// Nothing: the change waits for `stems config apply` (`false`).
    #[default]
    Off,
    /// Only what needs no restart (`true`).
    Hot,
    /// Everything, restarts included (`"all"`).
    All,
}

impl Serialize for AutoApply {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Off => s.serialize_bool(false),
            Self::Hot => s.serialize_bool(true),
            Self::All => s.serialize_str("all"),
        }
    }
}

impl<'de> Deserialize<'de> for AutoApply {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bool(bool),
            Text(String),
        }
        match Repr::deserialize(d)? {
            Repr::Bool(false) => Ok(Self::Off),
            Repr::Bool(true) => Ok(Self::Hot),
            Repr::Text(t) if t.trim() == "all" => Ok(Self::All),
            Repr::Text(t) => Err(serde::de::Error::custom(format!(
                "invalid auto_apply `{t}`; expected `false`, `true` or `all`"
            ))),
        }
    }
}

impl JsonSchema for AutoApply {
    fn schema_name() -> Cow<'static, str> {
        "AutoApply".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "`false`: wait for `stems config apply`; `true`: apply what needs no restart; `all`: apply everything, restarts included.",
            "anyOf": [
                { "type": "boolean" },
                { "type": "string", "enum": ["all"] }
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_and_print() {
        for (s, ms) in [
            ("200ms", 200),
            ("2s", 2000),
            ("1m", 60_000),
            ("1m 30s", 90_000),
            ("0", 0),
        ] {
            let d: Dur = s.parse().unwrap();
            assert_eq!(d.0.as_millis(), ms, "{s}");
        }
        assert_eq!(Dur::from_millis(400).to_string(), "400ms");
        assert_eq!(Dur::from_secs(600).to_string(), "10m");
        assert_eq!(Dur::from_millis(0).to_string(), "0s");
        assert!("fast".parse::<Dur>().is_err());
        assert!("10".parse::<Dur>().is_err());
    }

    #[test]
    fn byte_sizes() {
        assert_eq!("10MB".parse::<ByteSize>().unwrap().0, 10 * 1024 * 1024);
        assert_eq!("2g".parse::<ByteSize>().unwrap().0, 2 * 1024 * 1024 * 1024);
        assert_eq!("512".parse::<ByteSize>().unwrap().0, 512);
        assert_eq!(ByteSize(10 * 1024 * 1024).to_string(), "10MB");
        assert_eq!(ByteSize(1500).to_string(), "1500B");
        assert!("ten".parse::<ByteSize>().is_err());
        assert!("5XB".parse::<ByteSize>().is_err());
    }

    #[test]
    fn limit_forms() {
        let m: MemoryLimit = serde_yaml_ng::from_str("2GB for 30s").unwrap();
        assert_eq!(m.size.0, 2 * 1024 * 1024 * 1024);
        assert_eq!(m.for_, Some(Dur::from_secs(30)));
        let m: MemoryLimit = serde_yaml_ng::from_str("100MB").unwrap();
        assert_eq!((m.size.0, m.for_), (100 * 1024 * 1024, None));
        let m: MemoryLimit = serde_yaml_ng::from_str("1024").unwrap();
        assert_eq!(m.size.0, 1024);
        let c: CpuLimit = serde_yaml_ng::from_str("1.5").unwrap();
        assert_eq!((c.cores, c.for_), (1.5, None));
        let c: CpuLimit = serde_yaml_ng::from_str("2").unwrap();
        assert_eq!(c.cores, 2.0);
        let c: CpuLimit = serde_yaml_ng::from_str("80% for 60s").unwrap();
        assert!((c.cores - 0.8).abs() < 1e-9);
        assert_eq!(c.for_, Some(Dur::from_secs(60)));
        let c: CpuLimit = serde_yaml_ng::from_str("'150%'").unwrap();
        assert!((c.cores - 1.5).abs() < 1e-9);
        for bad in ["-1", "0", "'abc'", "'80% for ever'", "'%'"] {
            assert!(serde_yaml_ng::from_str::<CpuLimit>(bad).is_err(), "{bad}");
        }
        assert!(serde_yaml_ng::from_str::<MemoryLimit>("'2XB'").is_err());
        assert!(serde_yaml_ng::from_str::<MemoryLimit>("'2GB for'").is_err());
        assert_eq!(serde_json::to_string(&c).unwrap(), "1.5");
    }

    #[test]
    fn watch_actions() {
        assert_eq!(
            "restart".parse::<WatchAction>().unwrap(),
            WatchAction::Restart
        );
        assert_eq!(
            "script:lint".parse::<WatchAction>().unwrap(),
            WatchAction::Script("lint".into())
        );
        assert_eq!(
            "signal:HUP".parse::<WatchAction>().unwrap(),
            WatchAction::Signal("HUP".into())
        );
        assert!("script:".parse::<WatchAction>().is_err());
        assert!("explode".parse::<WatchAction>().is_err());
    }

    #[test]
    fn file_modes() {
        assert_eq!("0600".parse::<FileMode>().unwrap().0, 0o600);
        let m: FileMode = serde_yaml_ng::from_str("644").unwrap();
        assert_eq!(m.0, 0o644);
        assert_eq!(FileMode(0o644).to_string(), "0644");
        assert!("0999".parse::<FileMode>().is_err());
    }
}
