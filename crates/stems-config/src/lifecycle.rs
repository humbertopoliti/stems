//! The lifecycle hooks: the fixed script names stems runs at fixed points
//! (FR-SC-1, FR-SC-6; `docs/scripts.md` "The lifecycle order"), in one
//! table ([`HOOKS`]) that `stems --help`, `stems scripts --lifecycle`, the
//! MCP server and `stems init` all render from, so the names and the
//! "when" text cannot drift between them. Any other script name is a
//! custom script (`stems run`).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Who a hook belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    /// A stem's `scripts:` entry.
    Stem,
    /// A workspace-level `scripts:` entry.
    Workspace,
}

impl fmt::Display for HookScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HookScope::Stem => "stem",
            HookScope::Workspace => "workspace",
        })
    }
}

/// One lifecycle hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hook {
    /// The script name (`setup`, `pre_start`, …).
    pub name: &'static str,
    /// Stem or workspace script.
    pub scope: HookScope,
    /// When stems runs it: one sentence.
    pub when: &'static str,
    /// Position in the lifecycle (the index in [`HOOKS`]).
    pub order: u8,
}

const fn hook(order: u8, name: &'static str, scope: HookScope, when: &'static str) -> Hook {
    Hook {
        name,
        scope,
        when,
        order,
    }
}

/// Every lifecycle hook, in lifecycle order: the `up` sequence (workspace
/// `bootstrap`, then per stem `setup`, `pre_start`, `start`, `health`,
/// `post_start`, `seed`), the stop sequence (`pre_stop`, `stop`,
/// `post_stop`), the on-demand ones (`build`, `reset`) and workspace
/// `teardown`.
pub const HOOKS: &[Hook] = &[
    hook(
        0,
        "bootstrap",
        HookScope::Workspace,
        "once per `up`, before any stem starts",
    ),
    hook(
        1,
        "setup",
        HookScope::Stem,
        "before the first start, again when its stamp (inputs, stamp_env) changes",
    ),
    hook(
        2,
        "pre_start",
        HookScope::Stem,
        "before every start of the process, policy restarts included",
    ),
    hook(
        3,
        "start",
        HookScope::Stem,
        "the process itself (instead of `command`); `starting` until the health check passes",
    ),
    hook(
        4,
        "health",
        HookScope::Stem,
        "reserved for the `health: {type: command}` probe's output; never a custom script",
    ),
    hook(
        5,
        "post_start",
        HookScope::Stem,
        "after every start, once the stem is healthy",
    ),
    hook(
        6,
        "seed",
        HookScope::Stem,
        "after post_start, once; again when its stamp changes (state `seeding`)",
    ),
    hook(
        7,
        "pre_stop",
        HookScope::Stem,
        "before every stop, before the process is signalled",
    ),
    hook(
        8,
        "stop",
        HookScope::Stem,
        "how to stop the process (instead of SIGTERM), bounded by `stop_grace`",
    ),
    hook(
        9,
        "post_stop",
        HookScope::Stem,
        "after every stop, once the process is gone",
    ),
    hook(
        10,
        "build",
        HookScope::Stem,
        "on `stems build` and `restart --build` (between stop and start); never automatic",
    ),
    hook(
        11,
        "reset",
        HookScope::Stem,
        "on `stems reset` and `up --fresh`, after the stop; its stamps are cleared",
    ),
    hook(
        12,
        "teardown",
        HookScope::Workspace,
        "on `down --all`, after every stem stopped, before the daemon exits",
    ),
];

const fn count(scope: HookScope) -> usize {
    let mut i = 0;
    let mut n = 0;
    while i < HOOKS.len() {
        if matches!(HOOKS[i].scope, HookScope::Stem) == matches!(scope, HookScope::Stem) {
            n += 1;
        }
        i += 1;
    }
    n
}

const fn names<const N: usize>(scope: HookScope) -> [&'static str; N] {
    let mut out = [""; N];
    let mut i = 0;
    let mut n = 0;
    while i < HOOKS.len() {
        if matches!(HOOKS[i].scope, HookScope::Stem) == matches!(scope, HookScope::Stem) {
            out[n] = HOOKS[i].name;
            n += 1;
        }
        i += 1;
    }
    assert!(n == N, "HOOKS changed: update the derived array length");
    out
}

/// Fixed lifecycle script names for stems (FR-SC-1), in lifecycle order
/// (derived from [`HOOKS`]).
pub const LIFECYCLE_SCRIPTS: [&str; count(HookScope::Stem)] = names(HookScope::Stem);

/// Fixed workspace-level script names (FR-SC-6), derived from [`HOOKS`].
pub const WORKSPACE_LIFECYCLE_SCRIPTS: [&str; count(HookScope::Workspace)] =
    names(HookScope::Workspace);

/// True if `name` is a stem lifecycle script (otherwise it is a custom script).
pub fn is_lifecycle_script(name: &str) -> bool {
    LIFECYCLE_SCRIPTS.contains(&name)
}

/// The hook called `name` (stem or workspace), if any.
pub fn hook_named(name: &str) -> Option<&'static Hook> {
    HOOKS.iter().find(|h| h.name == name)
}

/// Every hook name in lifecycle order (what `lifecycle_hooks` lists).
pub fn hook_names() -> Vec<&'static str> {
    HOOKS.iter().map(|h| h.name).collect()
}

/// `setup, seed, …` for stems and `bootstrap, teardown` for the workspace:
/// the one-line summary `--help` and `stems init` print.
pub fn summary() -> String {
    let stems = LIFECYCLE_SCRIPTS.join(", ");
    let ws = WORKSPACE_LIFECYCLE_SCRIPTS.join(", ");
    format!("{stems}; workspace: {ws}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_in_lifecycle_order_and_unique() {
        for (i, h) in HOOKS.iter().enumerate() {
            assert_eq!(usize::from(h.order), i, "{}", h.name);
            assert!(!h.when.is_empty(), "{} has no `when`", h.name);
        }
        let mut names = hook_names();
        names.dedup();
        assert_eq!(names.len(), HOOKS.len());
        assert_eq!(names.len(), 13);
    }

    #[test]
    fn derived_arrays_follow_the_table() {
        assert_eq!(
            LIFECYCLE_SCRIPTS.to_vec(),
            HOOKS
                .iter()
                .filter(|h| h.scope == HookScope::Stem)
                .map(|h| h.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(WORKSPACE_LIFECYCLE_SCRIPTS, ["bootstrap", "teardown"]);
        assert!(is_lifecycle_script("pre_start"));
        assert!(!is_lifecycle_script("bootstrap"));
        assert!(!is_lifecycle_script("create-test-user"));
        assert_eq!(
            hook_named("teardown").map(|h| h.scope),
            Some(HookScope::Workspace)
        );
        assert_eq!(hook_named("nope"), None);
        assert!(summary().starts_with("setup, pre_start, start, health, "));
        assert!(summary().ends_with("; workspace: bootstrap, teardown"));
    }
}
