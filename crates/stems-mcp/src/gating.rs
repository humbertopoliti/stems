//! Destructive-action gating (FR-AI-3) and tool filtering.
//!
//! A call is **destructive** when it can lose data or tear the environment
//! down: `down` with `all` or `volumes`, `up` with `fresh`, `reset`, and
//! `doctor` with `fix`. It runs only when the caller passed `confirm: true`
//! **and** the workspace sets `agent.allow_destructive: true`:
//!
//! | destructive | confirm | allow_destructive | result |
//! |---|---|---|---|
//! | no  | any   | any   | runs |
//! | yes | false | any   | `DESTRUCTIVE_NOT_CONFIRMED` |
//! | yes | true  | false | `DESTRUCTIVE_NOT_ALLOWED` |
//! | yes | true  | true  | runs |
//!
//! `agent.allowed_tools` (when non-empty, only matching tools exist) and
//! `agent.denied_tools` (matching tools never exist) are glob lists over
//! tool names, auto script tools included; a filtered tool is absent from
//! `tools/list` and a call to it is a `USAGE` error.

use globset::{Glob, GlobMatcher};
use serde_json::{Value, json};
use stems_config::Agent;
use stems_core::{Error, ErrorCode};

/// The config key that allows destructive MCP calls.
pub const SETTING: &str = "agent.allow_destructive";

/// Whether a call of `tool` with `args` is destructive.
pub fn is_destructive(tool: &str, args: &Value) -> bool {
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    match tool {
        "down" => flag("all") || flag("volumes"),
        "up" => flag("fresh"),
        "reset" => true,
        "doctor" => flag("fix"),
        _ => false,
    }
}

/// What makes this call destructive, for messages (`down with all`).
fn what(tool: &str, args: &Value) -> String {
    let set: Vec<&str> = ["all", "volumes", "fresh", "fix"]
        .into_iter()
        .filter(|k| args.get(*k).and_then(Value::as_bool).unwrap_or(false))
        .collect();
    if set.is_empty() {
        format!("`{tool}`")
    } else {
        format!("`{tool}` with `{}`", set.join("`, `"))
    }
}

/// The gate: `Ok` when the call may run.
pub fn check(tool: &str, args: &Value, allow_destructive: bool) -> Result<(), Error> {
    if !is_destructive(tool, args) {
        return Ok(());
    }
    let confirmed = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let action = what(tool, args);
    if !confirmed {
        return Err(Error::new(
            ErrorCode::DestructiveNotConfirmed,
            format!("{action} is destructive and needs `confirm: true`"),
        )
        .with_hint(format!(
            "ask the user, then call it again with `confirm: true`; the workspace must also allow it (`{SETTING}: true` in stems.yaml or stems.local.yaml, or `stems config set {SETTING} true`)"
        ))
        .with_details(json!({ "tool": tool, "setting": SETTING })));
    }
    if !allow_destructive {
        return Err(Error::new(
            ErrorCode::DestructiveNotAllowed,
            format!("{action} is destructive and this workspace does not allow destructive agent actions"),
        )
        .with_hint(format!(
            "a human can allow it: set `{SETTING}: true` in stems.yaml (or stems.local.yaml), or run `stems config set {SETTING} true`"
        ))
        .with_details(json!({ "tool": tool, "setting": SETTING, "value": false })));
    }
    Ok(())
}

/// `agent.allowed_tools` / `agent.denied_tools` compiled.
#[derive(Clone, Debug, Default)]
pub struct ToolFilter {
    allowed: Vec<GlobMatcher>,
    denied: Vec<GlobMatcher>,
}

fn compile(globs: &[String]) -> Vec<GlobMatcher> {
    globs
        .iter()
        .filter_map(|g| Glob::new(g).ok().map(|g| g.compile_matcher()))
        .collect()
}

impl ToolFilter {
    /// The filter of the workspace's `agent:` block.
    pub fn new(agent: &Agent) -> Self {
        Self {
            allowed: compile(&agent.allowed_tools),
            denied: compile(&agent.denied_tools),
        }
    }

    /// Whether `tool` is exposed.
    pub fn allows(&self, tool: &str) -> bool {
        (self.allowed.is_empty() || self.allowed.iter().any(|g| g.is_match(tool)))
            && !self.denied.iter().any(|g| g.is_match(tool))
    }

    /// The error for a call to a filtered tool.
    pub fn denied_error(tool: &str) -> Error {
        Error::usage(
            format!("the tool `{tool}` is disabled for agents in this workspace"),
            "it is excluded by `agent.allowed_tools` / `agent.denied_tools` in stems.yaml; a human can change that setting",
        )
        .with_details(json!({ "tool": tool }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_table() {
        let cases: &[(&str, Value, bool, Option<ErrorCode>)] = &[
            ("down", json!({}), false, None),
            ("down", json!({"stems": ["a"]}), false, None),
            (
                "down",
                json!({"all": true}),
                true,
                Some(ErrorCode::DestructiveNotConfirmed),
            ),
            (
                "down",
                json!({"all": true, "confirm": true}),
                false,
                Some(ErrorCode::DestructiveNotAllowed),
            ),
            ("down", json!({"all": true, "confirm": true}), true, None),
            (
                "down",
                json!({"volumes": true, "confirm": false}),
                true,
                Some(ErrorCode::DestructiveNotConfirmed),
            ),
            ("up", json!({"profile": "x"}), false, None),
            (
                "up",
                json!({"fresh": true}),
                true,
                Some(ErrorCode::DestructiveNotConfirmed),
            ),
            (
                "reset",
                json!({"stems": ["a"]}),
                true,
                Some(ErrorCode::DestructiveNotConfirmed),
            ),
            (
                "reset",
                json!({"stems": ["a"], "confirm": true}),
                false,
                Some(ErrorCode::DestructiveNotAllowed),
            ),
            (
                "reset",
                json!({"stems": ["a"], "confirm": true}),
                true,
                None,
            ),
            ("doctor", json!({}), false, None),
            (
                "doctor",
                json!({"fix": true, "confirm": true}),
                false,
                Some(ErrorCode::DestructiveNotAllowed),
            ),
            (
                "stop",
                json!({"stems": ["a"], "cascade": true}),
                false,
                None,
            ),
        ];
        for (tool, args, allow, want) in cases {
            let got = check(tool, args, *allow).err().map(|e| e.code);
            assert_eq!(got, *want, "{tool} {args} allow={allow}");
        }
    }

    #[test]
    fn hints_name_the_setting() {
        let e = check("down", &json!({"all": true}), false).unwrap_err();
        assert!(e.hint.unwrap().contains(SETTING));
        let e = check("down", &json!({"all": true, "confirm": true}), false).unwrap_err();
        let hint = e.hint.unwrap();
        assert!(
            hint.contains("stems config set agent.allow_destructive true"),
            "{hint}"
        );
        assert!(e.message.contains("`down` with `all`"), "{}", e.message);
    }

    #[test]
    fn filter_globs() {
        let f = ToolFilter::new(&Agent {
            allow_destructive: false,
            allowed_tools: vec![],
            denied_tools: vec!["reset".into(), "*__nuke*".into()],
        });
        assert!(f.allows("get_status"));
        assert!(!f.allows("reset"));
        assert!(!f.allows("workspace__nuke_databases"));
        let f = ToolFilter::new(&Agent {
            allow_destructive: false,
            allowed_tools: vec!["get_*".into(), "list_stems".into()],
            denied_tools: vec!["get_config".into()],
        });
        assert!(f.allows("get_logs"));
        assert!(f.allows("list_stems"));
        assert!(!f.allows("get_config"));
        assert!(!f.allows("stop"));
        assert_eq!(ToolFilter::denied_error("reset").code, ErrorCode::Usage);
    }
}
