//! Help text generated from `stems_config::lifecycle::HOOKS` at startup
//! (36): the `after_help` of `stems --help` and the `after_long_help` of
//! `run`, `scripts`, `build` and `reset`, so the hook names in the help
//! cannot drift from the table the daemon and `stems scripts --lifecycle`
//! use. Golden-tested through `tests/help.rs` and `docs/cli.md`.

use std::sync::LazyLock;

use stems_config::lifecycle::{HOOKS, HookScope, summary};

static AFTER_HELP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "Lifecycle hooks (fixed `scripts:` names stems runs itself): {}.\n\
         Any other script name is a custom script (`stems run`). `stems scripts --lifecycle`\n\
         lists every hook with when it runs and who defines it.",
        summary()
    )
});

static HOOKS_LONG_HELP: LazyLock<String> = LazyLock::new(|| {
    let width = HOOKS.iter().map(|h| h.name.len()).max().unwrap_or(0);
    let mut s = String::from(
        "Lifecycle hooks, in lifecycle order (every other `scripts:` name is a custom script):\n",
    );
    for h in HOOKS {
        let scope = match h.scope {
            HookScope::Stem => "",
            HookScope::Workspace => "(workspace) ",
        };
        s.push_str(&format!("  {:<width$}  {scope}{}\n", h.name, h.when));
    }
    s.push_str(
        "`stems scripts --lifecycle` prints this table with the stems that define each hook.",
    );
    s
});

/// The paragraph under `stems --help`: the hook names and the pointer.
pub fn after_help() -> &'static str {
    &AFTER_HELP
}

/// The hook table under the long help of `run`, `scripts`, `build`, `reset`.
pub fn hooks_long_help() -> &'static str {
    &HOOKS_LONG_HELP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hook_is_named_in_both_texts() {
        for h in HOOKS {
            assert!(after_help().contains(h.name), "{} missing", h.name);
            assert!(hooks_long_help().contains(h.name), "{} missing", h.name);
            assert!(
                hooks_long_help().contains(h.when),
                "{} when missing",
                h.name
            );
        }
        assert!(after_help().contains("stems scripts --lifecycle"));
        insta::assert_snapshot!("help-after-help", after_help());
        insta::assert_snapshot!("help-hooks-long-help", hooks_long_help());
    }
}
