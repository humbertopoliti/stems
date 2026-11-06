//! Which `STEMS_*` variables of a caller's environment are kept.
//!
//! Stems injects `STEMS_STEM`, `STEMS_WORKSPACE`, `STEMS_CODEBASE`,
//! `STEMS_RUN_ID`, `STEMS_STATE_DIR`, `STEMS_SCRIPT`, `STEMS_ARG_*`,
//! `STEMS_IMAGE(S)`, `STEMS_<DEP>_PORT*`, `STEMS_<DEP>_CODEBASE` and
//! `STEMS_<DEP>_OUTPUT_*` for the stem being run; those must not leak from one
//! context into another, so they are dropped from a caller's environment.
//! The variables that configure the `stems` CLI itself pass through, so a
//! script running `stems status` uses the caller's home and profile.

/// Exact `STEMS_*` names that configure the CLI, daemon or TUI. No prefixes:
/// a stem named `tui` gets injected `STEMS_TUI_PORT` etc., which must not leak.
const CLI_CONFIG: &[&str] = &[
    "STEMS_HOME",
    "STEMS_NO_COLOR",
    "STEMS_ASCII",
    "STEMS_DAEMON_LOG",
    "STEMS_DEBUG_RPC",
    "STEMS_NOTIFICATIONS",
    "STEMS_FAKE_VERSION",
    "STEMS_FAKE_INSTALL_METHOD",
    "STEMS_PROFILE",
    "STEMS_LOG",
    "STEMS_UI_CONFIG",
    "STEMS_DOCKER_CLI",
    "STEMS_TUI",
    "STEMS_TUI_FORCE",
    "STEMS_TUI_SCRIPT",
    "STEMS_TUI_SIZE",
    "STEMS_TUI_FRAMES_OUT",
    "STEMS_TUI_PANIC_TEST",
];

/// Is `name` a variable stems injects for a stem (so it must not be
/// inherited from a caller)? True for every `STEMS_*` name that is not CLI
/// configuration.
pub fn is_stems_injected(name: &str) -> bool {
    name.starts_with("STEMS_") && !CLI_CONFIG.contains(&name)
}

/// `env` without the stems-injected variables; CLI configuration
/// (`STEMS_HOME`, ...) is kept.
pub fn without_injected<I, K, V>(env: I) -> impl Iterator<Item = (K, V)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
{
    env.into_iter()
        .filter(|(k, _)| !is_stems_injected(k.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_cli_config_strips_injected() {
        for k in [
            "STEMS_HOME",
            "STEMS_NO_COLOR",
            "STEMS_TUI_SIZE",
            "PATH",
            "HOME",
        ] {
            assert!(!is_stems_injected(k), "{k}");
        }
        for k in [
            "STEMS_STEM",
            "STEMS_WORKSPACE",
            "STEMS_CODEBASE",
            "STEMS_RUN_ID",
            "STEMS_STATE_DIR",
            "STEMS_SCRIPT",
            "STEMS_ARG_EMAIL",
            "STEMS_IMAGE",
            "STEMS_IMAGES",
            "STEMS_SHOP_API_PORT",
            "STEMS_SHOP_API_CODEBASE",
            "STEMS_HOMEWORK_PORT",
        ] {
            assert!(is_stems_injected(k), "{k}");
        }
    }

    #[test]
    fn tui_named_stem_variables_are_stripped() {
        for k in [
            "STEMS_TUI_PORT",
            "STEMS_TUI_CODEBASE",
            "STEMS_TUI_OUTPUT_TOKEN",
        ] {
            assert!(is_stems_injected(k), "{k}");
        }
        assert!(!is_stems_injected("STEMS_PROFILE"));
    }

    /// Variables that are read by stems but deliberately not passed through
    /// (injected by stems for the stem or its hooks, and also read by the
    /// workspace discovery of a stem's own nested `stems`).
    const NOT_PASSED: &[&str] = &["STEMS_NOTIFY_BODY", "STEMS_WORKSPACE"];

    /// Every `STEMS_*` variable declared as a constant or a clap `env =`
    /// attribute in the source must be allowlisted or listed in `NOT_PASSED`.
    #[test]
    fn every_declared_env_read_is_allowlisted() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for c in std::fs::read_dir(&crates).unwrap().flatten() {
            walk(&c.path().join("src"), &mut files);
        }
        let mut missing = Vec::new();
        let mut seen = 0;
        for f in files {
            if f.ends_with("callerenv.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            for line in text.lines() {
                let t = line.trim();
                let declared = (t.starts_with("pub const") && t.contains(": &str = \"STEMS_"))
                    || t.starts_with("env = \"STEMS_");
                if !declared {
                    continue;
                }
                let start = t.find("\"STEMS_").unwrap() + 1;
                let name: String = t[start..].chars().take_while(|c| *c != '"').collect();
                if name.ends_with('_') {
                    continue;
                }
                seen += 1;
                if CLI_CONFIG.contains(&name.as_str()) || NOT_PASSED.contains(&name.as_str()) {
                    continue;
                }
                missing.push(format!("{name} ({})", f.display()));
            }
        }
        assert!(seen >= 10, "scan found only {seen} declarations");
        assert!(missing.is_empty(), "not allowlisted: {missing:?}");
    }

    #[test]
    fn filters_pairs() {
        let env = [("STEMS_HOME", "/h"), ("STEMS_STEM", "api"), ("A", "1")];
        let kept: Vec<_> = without_injected(env).map(|(k, _)| k).collect();
        assert_eq!(kept, ["STEMS_HOME", "A"]);
    }
}
