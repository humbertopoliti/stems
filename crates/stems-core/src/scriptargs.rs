//! Script catalogue and argument schemas (FR-SC-2, FR-SC-7; deliverable 17).
//!
//! - [`build_catalog`]: every runnable script of a workspace, the data behind
//!   `stems scripts`, the `script_catalog` RPC, the TUI menu (30) and MCP (31).
//! - [`parse_args`]: validate `stems run … -- <argv>` against a script's
//!   declared `args` with a dynamically built `clap::Command`.
//! - [`parse_args_json`]: the same validation for a JSON object (MCP / TUI).
//! - [`json_schema_for`] / [`mcp_tool_name`]: MCP tool input schemas and names.
//!
//! Every failure is `SCRIPT_ARGS_INVALID` with `details { arg, reason }`.

use std::collections::BTreeMap;

use clap::builder::{BoolishValueParser, PossibleValuesParser};
use clap::error::{ContextKind, ContextValue};
use clap::{Arg, ArgAction, ColorChoice, Command};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use stems_config::{
    ArgType, Dur, LIFECYCLE_SCRIPTS, Scalar, Script, ScriptArg, WORKSPACE_LIFECYCLE_SCRIPTS,
    Workspace,
};

use crate::{Error, ErrorCode};

/// Whether a script is one of the fixed lifecycle names or a custom one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ScriptKind {
    /// `setup`, `seed`, `start`, … (stems) or `bootstrap`, `teardown` (workspace).
    Lifecycle,
    /// Any other named script.
    Custom,
}

/// Schema-only mirror of [`ScriptArg`] (which lives in `stems-config` and does
/// not derive `JsonSchema`). Serialized shape is identical.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct ScriptArgSchema {
    name: String,
    #[serde(rename = "type")]
    kind: ArgType,
    default: Option<Scalar>,
    required: bool,
    description: Option<String>,
    values: Vec<String>,
}

/// One runnable script, as listed by `stems scripts` / `script_catalog`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScriptCatalogEntry {
    /// Owning stem; `None` for workspace-level scripts.
    pub stem: Option<String>,
    /// Script name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Declared arguments.
    #[schemars(with = "Vec<ScriptArgSchema>")]
    pub args: Vec<ScriptArg>,
    /// Stems that must be healthy first.
    pub requires: Vec<String>,
    /// Lifecycle or custom.
    pub kind: ScriptKind,
    /// Timeout; `None` = no timeout.
    pub timeout: Option<Dur>,
    /// Retries after a failure.
    pub retries: u32,
    /// Whether concurrent runs are allowed.
    pub concurrent: bool,
}

impl ScriptCatalogEntry {
    fn new(stem: Option<&str>, name: &str, script: &Script, kind: ScriptKind) -> Self {
        Self {
            stem: stem.map(str::to_string),
            name: name.to_string(),
            description: script.description.clone(),
            args: script.args.clone(),
            requires: script.requires.clone(),
            kind,
            timeout: script.timeout,
            retries: script.retries,
            concurrent: script.concurrent,
        }
    }

    /// The MCP tool name of this script ([`mcp_tool_name`]).
    pub fn mcp_tool_name(&self) -> String {
        mcp_tool_name(self.stem.as_deref(), &self.name)
    }
}

/// Every script of the workspace: workspace-level scripts first (`stem:
/// None`, sorted by name), then each enabled stem in declaration order with
/// its scripts sorted by name. Disabled stems are omitted (they cannot run).
pub fn build_catalog(ws: &Workspace) -> Vec<ScriptCatalogEntry> {
    let mut out = Vec::new();
    let mut names: Vec<&String> = ws.scripts.keys().collect();
    names.sort();
    for name in names {
        let kind = if WORKSPACE_LIFECYCLE_SCRIPTS.contains(&name.as_str()) {
            ScriptKind::Lifecycle
        } else {
            ScriptKind::Custom
        };
        out.push(ScriptCatalogEntry::new(None, name, &ws.scripts[name], kind));
    }
    for stem in ws.stems() {
        let mut names: Vec<&String> = stem.scripts.keys().collect();
        names.sort();
        for name in names {
            let kind = if LIFECYCLE_SCRIPTS.contains(&name.as_str()) {
                ScriptKind::Lifecycle
            } else {
                ScriptKind::Custom
            };
            out.push(ScriptCatalogEntry::new(
                Some(&stem.name),
                name,
                &stem.scripts[name],
                kind,
            ));
        }
    }
    out
}

/// A validated argument value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    /// `string`, `enum` and `path` arguments.
    Str(String),
    /// `int` arguments.
    Int(i64),
    /// `bool` arguments.
    Bool(bool),
    /// `float` arguments.
    Float(f64),
}

impl std::fmt::Display for ArgValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Str(s) => f.write_str(s),
            Self::Int(i) => write!(f, "{i}"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Float(x) => write!(f, "{x}"),
        }
    }
}

/// Validated script arguments (defaults applied).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParsedArgs {
    /// Values by declared name. Optional args without a default and not given
    /// are absent.
    pub values: BTreeMap<String, ArgValue>,
    /// Raw `--` arguments, passed through untouched when the script declares
    /// no `args` (always empty otherwise).
    pub passthrough: Vec<String>,
}

impl ParsedArgs {
    /// The argv handed to the script: `--name value` per value (sorted by
    /// name; bools as `--name true|false`), followed by any passthrough args.
    pub fn to_argv(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.values.len() * 2 + self.passthrough.len());
        for (name, v) in &self.values {
            out.push(format!("--{name}"));
            out.push(v.to_string());
        }
        out.extend(self.passthrough.iter().cloned());
        out
    }

    /// `STEMS_ARG_<NAME>` env vars (name upper-cased, non-alphanumerics → `_`).
    pub fn to_env(&self) -> BTreeMap<String, String> {
        self.values
            .iter()
            .map(|(name, v)| (env_name(name), v.to_string()))
            .collect()
    }
}

/// `STEMS_ARG_<NAME>` for an argument name.
pub fn env_name(arg: &str) -> String {
    let upper: String = arg
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("STEMS_ARG_{upper}")
}

/// `<stem>__<script>` (`workspace__<script>` for workspace scripts) with every
/// character outside `[A-Za-z0-9_]` replaced by `_`.
pub fn mcp_tool_name(stem: Option<&str>, script: &str) -> String {
    let raw = match stem {
        Some(s) => format!("{s}__{script}"),
        None => format!("workspace__{script}"),
    };
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn invalid(arg: &str, reason: impl Into<String>, schema: &[ScriptArg]) -> Error {
    let reason = reason.into();
    Error::new(
        ErrorCode::ScriptArgsInvalid,
        format!("invalid script argument `--{arg}`: {reason}"),
    )
    .with_hint(usage_hint(schema))
    .with_details(json!({ "arg": arg, "reason": reason }))
}

fn type_label(a: &ScriptArg) -> String {
    match a.kind {
        ArgType::Enum => a.values.join("|"),
        ArgType::String => "string".into(),
        ArgType::Int => "int".into(),
        ArgType::Float => "float".into(),
        ArgType::Bool => "true|false".into(),
        ArgType::Path => "path".into(),
    }
}

fn usage_hint(schema: &[ScriptArg]) -> String {
    if schema.is_empty() {
        return "this script declares no `args`; everything after `--` is passed through".into();
    }
    let parts: Vec<String> = schema
        .iter()
        .map(|a| {
            let mut s = format!("--{} <{}>", a.name, type_label(a));
            if is_required(a) {
                s.push_str(" (required)");
            } else if let Some(d) = &a.default {
                s.push_str(&format!(" (default {d})"));
            }
            s
        })
        .collect();
    format!("declared args: {}", parts.join(", "))
}

fn is_required(a: &ScriptArg) -> bool {
    a.required && a.default.is_none()
}

fn check_schema(schema: &[ScriptArg]) -> Result<(), Error> {
    let mut seen = std::collections::BTreeSet::new();
    for a in schema {
        let ok = !a.name.is_empty()
            && !a.name.starts_with('-')
            && a.name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !ok {
            return Err(invalid(
                &a.name,
                "the declared argument name must match [A-Za-z0-9_-]+",
                schema,
            ));
        }
        if !seen.insert(a.name.as_str()) {
            return Err(invalid(
                &a.name,
                "the argument is declared more than once",
                schema,
            ));
        }
        if a.kind == ArgType::Enum && a.values.is_empty() {
            return Err(invalid(
                &a.name,
                "an `enum` argument must declare `values`",
                schema,
            ));
        }
    }
    Ok(())
}

fn build_command(schema: &[ScriptArg]) -> Command {
    let mut cmd = Command::new("script")
        .no_binary_name(true)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .color(ColorChoice::Never);
    for a in schema {
        let mut arg = Arg::new(a.name.clone())
            .long(a.name.clone())
            .value_name(a.name.clone())
            .action(ArgAction::Set)
            .required(is_required(a));
        arg = match a.kind {
            ArgType::String | ArgType::Path => arg.allow_hyphen_values(true),
            ArgType::Int => arg
                .value_parser(clap::value_parser!(i64))
                .allow_negative_numbers(true),
            ArgType::Float => arg
                .value_parser(clap::value_parser!(f64))
                .allow_negative_numbers(true),
            ArgType::Bool => arg
                .value_parser(BoolishValueParser::new())
                .num_args(0..=1)
                .default_missing_value("true"),
            ArgType::Enum => arg.value_parser(PossibleValuesParser::new(a.values.clone())),
        };
        if let Some(d) = &a.default {
            arg = arg.default_value(d.to_string());
        }
        cmd = cmd.arg(arg);
    }
    cmd
}

fn context_first(err: &clap::Error, kind: ContextKind) -> Option<String> {
    match err.get(kind)? {
        ContextValue::String(s) => Some(s.clone()),
        ContextValue::Strings(v) => v.first().cloned(),
        _ => None,
    }
}

fn clap_error(err: &clap::Error, schema: &[ScriptArg]) -> Error {
    let raw = context_first(err, ContextKind::InvalidArg).unwrap_or_default();
    let token = raw.split([' ', '=']).next().unwrap_or_default();
    let arg = token.trim_start_matches('-');
    let rendered = err.render().to_string();
    let reason = rendered
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("Usage:")
                && !l.starts_with("For more information")
                && !l.starts_with("tip:")
        })
        .map(|l| l.trim_start_matches("error: "))
        .collect::<Vec<_>>()
        .join(" ");
    invalid(arg, reason, schema)
}

/// Validate `argv` (the arguments after `--`) against `schema`. With an
/// empty schema every argument is passed through untouched.
pub fn parse_args(schema: &[ScriptArg], argv: &[String]) -> Result<ParsedArgs, Error> {
    if schema.is_empty() {
        return Ok(ParsedArgs {
            values: BTreeMap::new(),
            passthrough: argv.to_vec(),
        });
    }
    check_schema(schema)?;
    let matches = build_command(schema)
        .try_get_matches_from(argv)
        .map_err(|e| clap_error(&e, schema))?;
    let mut values = BTreeMap::new();
    for a in schema {
        let v = match a.kind {
            ArgType::String | ArgType::Path | ArgType::Enum => matches
                .get_one::<String>(&a.name)
                .map(|s| ArgValue::Str(s.clone())),
            ArgType::Int => matches.get_one::<i64>(&a.name).map(|i| ArgValue::Int(*i)),
            ArgType::Float => matches.get_one::<f64>(&a.name).map(|x| ArgValue::Float(*x)),
            ArgType::Bool => matches.get_one::<bool>(&a.name).map(|b| ArgValue::Bool(*b)),
        };
        if let Some(v) = v {
            values.insert(a.name.clone(), v);
        }
    }
    Ok(ParsedArgs {
        values,
        passthrough: Vec::new(),
    })
}

fn json_label(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Coerce one JSON value to the declared type; `Err(reason)` on mismatch.
fn coerce(a: &ScriptArg, v: &Value) -> Result<ArgValue, String> {
    let bad = || format!("expected {}, got {}", type_label(a), json_label(v));
    match a.kind {
        ArgType::String | ArgType::Path => match v {
            Value::String(s) => Ok(ArgValue::Str(s.clone())),
            Value::Number(n) => Ok(ArgValue::Str(n.to_string())),
            Value::Bool(b) => Ok(ArgValue::Str(b.to_string())),
            _ => Err(bad()),
        },
        ArgType::Int => match v {
            Value::Number(n) => n.as_i64().map(ArgValue::Int).ok_or_else(bad),
            Value::String(s) => s
                .trim()
                .parse::<i64>()
                .map(ArgValue::Int)
                .map_err(|e| format!("invalid value '{s}': {e}")),
            _ => Err(bad()),
        },
        ArgType::Float => match v {
            Value::Number(n) => n.as_f64().map(ArgValue::Float).ok_or_else(bad),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .map(ArgValue::Float)
                .map_err(|e| format!("invalid value '{s}': {e}")),
            _ => Err(bad()),
        },
        ArgType::Bool => match v {
            Value::Bool(b) => Ok(ArgValue::Bool(*b)),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "on" | "1" => Ok(ArgValue::Bool(true)),
                "false" | "no" | "n" | "off" | "0" => Ok(ArgValue::Bool(false)),
                _ => Err(format!("invalid value '{s}': expected true or false")),
            },
            _ => Err(bad()),
        },
        ArgType::Enum => {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => return Err(bad()),
            };
            if a.values.contains(&s) {
                Ok(ArgValue::Str(s))
            } else {
                Err(format!(
                    "invalid value '{s}' [possible values: {}]",
                    a.values.join(", ")
                ))
            }
        }
    }
}

fn scalar_json(s: &Scalar) -> Value {
    match s {
        Scalar::Bool(b) => Value::Bool(*b),
        Scalar::Int(i) => Value::from(*i),
        Scalar::Float(x) => Value::from(*x),
        Scalar::String(s) => Value::String(s.clone()),
    }
}

/// Validate a JSON object of arguments (MCP / TUI path) against `schema`:
/// unknown keys are rejected, values are coerced to the declared type
/// (`"42"` → int, `42` → string, …), enum membership is checked, defaults
/// applied and required args enforced. `null` counts as absent.
pub fn parse_args_json(
    schema: &[ScriptArg],
    input: &Map<String, Value>,
) -> Result<ParsedArgs, Error> {
    check_schema(schema)?;
    let mut unknown: Vec<&String> = input
        .keys()
        .filter(|k| !schema.iter().any(|a| &a.name == *k))
        .collect();
    unknown.sort();
    if let Some(k) = unknown.first() {
        return Err(invalid(k, format!("unexpected argument '{k}'"), schema));
    }
    let mut values = BTreeMap::new();
    for a in schema {
        let given = input.get(&a.name).filter(|v| !v.is_null());
        let v = match (given, &a.default) {
            (Some(v), _) => coerce(a, v).map_err(|r| invalid(&a.name, r, schema))?,
            (None, Some(d)) => coerce(a, &scalar_json(d))
                .map_err(|r| invalid(&a.name, format!("declared default: {r}"), schema))?,
            (None, None) if a.required => {
                return Err(invalid(
                    &a.name,
                    "the required argument was not provided",
                    schema,
                ));
            }
            (None, None) => continue,
        };
        values.insert(a.name.clone(), v);
    }
    Ok(ParsedArgs {
        values,
        passthrough: Vec::new(),
    })
}

/// A JSON Schema (draft 2020-12 subset) object describing `schema`: what the
/// MCP server (31) publishes as a tool's `inputSchema`.
pub fn json_schema_for(schema: &[ScriptArg]) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for a in schema {
        let mut p = Map::new();
        let ty = match a.kind {
            ArgType::String | ArgType::Path | ArgType::Enum => "string",
            ArgType::Int => "integer",
            ArgType::Float => "number",
            ArgType::Bool => "boolean",
        };
        p.insert("type".into(), ty.into());
        if let Some(d) = &a.description {
            p.insert("description".into(), d.clone().into());
        }
        if a.kind == ArgType::Enum {
            p.insert("enum".into(), a.values.clone().into());
        }
        if let Some(d) = &a.default {
            let v = coerce(a, &scalar_json(d)).map_or_else(
                |_| scalar_json(d),
                |v| match v {
                    ArgValue::Str(s) => Value::String(s),
                    ArgValue::Int(i) => Value::from(i),
                    ArgValue::Bool(b) => Value::Bool(b),
                    ArgValue::Float(x) => Value::from(x),
                },
            );
            p.insert("default".into(), v);
        }
        if is_required(a) {
            required.push(Value::String(a.name.clone()));
        }
        props.insert(a.name.clone(), Value::Object(p));
    }
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arg(name: &str, kind: ArgType) -> ScriptArg {
        ScriptArg {
            name: name.into(),
            kind,
            default: None,
            required: false,
            description: None,
            values: Vec::new(),
        }
    }

    /// The `create-test-user` schema plus one arg of every other type.
    fn schema() -> Vec<ScriptArg> {
        vec![
            ScriptArg {
                required: true,
                ..arg("email", ArgType::String)
            },
            ScriptArg {
                values: vec!["admin".into(), "user".into()],
                default: Some(Scalar::String("admin".into())),
                ..arg("role", ArgType::Enum)
            },
            ScriptArg {
                default: Some(Scalar::Int(1000)),
                ..arg("rows", ArgType::Int)
            },
            arg("verbose", ArgType::Bool),
            arg("ratio", ArgType::Float),
            arg("out-dir", ArgType::Path),
        ]
    }

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn ok(s: &str) -> String {
        let p = parse_args(&schema(), &argv(s)).unwrap_or_else(|e| panic!("{s}: {e}"));
        p.to_argv().join(" ")
    }

    fn err(s: &str) -> (String, String) {
        let e = parse_args(&schema(), &argv(s)).expect_err(s);
        assert_eq!(e.code, ErrorCode::ScriptArgsInvalid);
        assert!(e.message.contains(e.details["arg"].as_str().unwrap()));
        (
            e.details["arg"].as_str().unwrap().to_string(),
            e.details["reason"].as_str().unwrap().to_string(),
        )
    }

    #[test]
    fn valid_argv_table() {
        let cases = [
            ("--email a@b.c", "--email a@b.c --role admin --rows 1000"),
            (
                "--email a@b.c --role user --rows -5",
                "--email a@b.c --role user --rows -5",
            ),
            (
                "--email=a@b.c --verbose",
                "--email a@b.c --role admin --rows 1000 --verbose true",
            ),
            (
                "--verbose false --email -dash",
                "--email -dash --role admin --rows 1000 --verbose false",
            ),
            (
                "--email x --ratio 0.5 --out-dir ./o",
                "--email x --out-dir ./o --ratio 0.5 --role admin --rows 1000",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(ok(input), want, "{input}");
        }
    }

    #[test]
    fn invalid_argv_table() {
        let cases = [
            ("", "email", "required"),
            ("--role admin", "email", "required"),
            ("--email a --rows many", "rows", "invalid value 'many'"),
            ("--email a --role superuser", "role", "superuser"),
            ("--email a --nope 1", "nope", "unexpected argument"),
            ("--email a stray", "stray", "unexpected argument"),
            ("--email a --verbose maybe", "verbose", "maybe"),
            (
                "--email a --email b",
                "email",
                "cannot be used multiple times",
            ),
        ];
        for (input, arg, reason) in cases {
            let (a, r) = err(input);
            assert_eq!(a, arg, "{input}: {r}");
            assert!(r.contains(reason), "{input}: {r}");
        }
    }

    #[test]
    fn env_and_values() {
        let p = parse_args(&schema(), &argv("--email a@b.c --out-dir /x --verbose")).unwrap();
        assert_eq!(p.values["rows"], ArgValue::Int(1000));
        assert_eq!(p.values["verbose"], ArgValue::Bool(true));
        let env = p.to_env();
        assert_eq!(env["STEMS_ARG_EMAIL"], "a@b.c");
        assert_eq!(env["STEMS_ARG_OUT_DIR"], "/x");
        assert_eq!(env["STEMS_ARG_ROLE"], "admin");
        assert_eq!(env["STEMS_ARG_VERBOSE"], "true");
        assert!(!env.contains_key("STEMS_ARG_RATIO"));
    }

    #[test]
    fn passthrough_without_schema() {
        let raw = argv("--anything goes -x 1 positional");
        let p = parse_args(&[], &raw).unwrap();
        assert!(p.values.is_empty());
        assert_eq!(p.passthrough, raw);
        assert_eq!(p.to_argv(), raw);
        assert!(p.to_env().is_empty());
    }

    #[test]
    fn bad_schema_is_reported_not_panicking() {
        let s = vec![arg("bad name", ArgType::String)];
        assert!(parse_args(&s, &[]).is_err());
        let s = vec![arg("a", ArgType::Int), arg("a", ArgType::Int)];
        assert!(parse_args(&s, &[]).is_err());
        let s = vec![arg("e", ArgType::Enum)];
        assert!(parse_args(&s, &[]).is_err());
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn json_path_matches_argv_path() {
        let pairs = [
            (json!({"email": "a@b.c"}), "--email a@b.c"),
            (
                json!({"email": "a@b.c", "role": "user", "rows": "-5"}),
                "--email a@b.c --role user --rows -5",
            ),
            (
                json!({"email": 42, "verbose": "yes", "ratio": 1, "rows": 7}),
                "--email 42 --verbose true --ratio 1 --rows 7",
            ),
            (json!({"email": "x", "verbose": null}), "--email x"),
        ];
        for (input, args) in pairs {
            let j = parse_args_json(&schema(), &obj(input.clone())).unwrap();
            let a = parse_args(&schema(), &argv(args)).unwrap();
            assert_eq!(j, a, "{input}");
        }
    }

    #[test]
    fn json_path_invalid() {
        let cases = [
            (json!({}), "email", "required"),
            (json!({"email": "a", "rows": 1.5}), "rows", "expected int"),
            (
                json!({"email": "a", "rows": "x"}),
                "rows",
                "invalid value 'x'",
            ),
            (
                json!({"email": "a", "role": "superuser"}),
                "role",
                "possible values: admin, user",
            ),
            (
                json!({"email": "a", "zzz": 1, "aaa": 2}),
                "aaa",
                "unexpected argument",
            ),
            (json!({"email": ["a"]}), "email", "got an array"),
            (
                json!({"email": "a", "verbose": 3}),
                "verbose",
                "got a number",
            ),
        ];
        for (input, arg, reason) in cases {
            let e = parse_args_json(&schema(), &obj(input.clone())).expect_err("invalid");
            assert_eq!(e.code, ErrorCode::ScriptArgsInvalid);
            assert_eq!(e.details["arg"], arg, "{input}");
            let r = e.details["reason"].as_str().unwrap();
            assert!(r.contains(reason), "{input}: {r}");
        }
        let e = parse_args_json(&[], &obj(json!({"x": 1}))).unwrap_err();
        assert_eq!(e.details["arg"], "x");
    }

    #[test]
    fn tool_names() {
        assert_eq!(
            mcp_tool_name(Some("shop-api"), "create-test-user"),
            "shop_api__create_test_user"
        );
        assert_eq!(
            mcp_tool_name(None, "nuke-databases"),
            "workspace__nuke_databases"
        );
        assert_eq!(mcp_tool_name(Some("a.b"), "x y"), "a_b__x_y");
    }
}
