//! Smoke test: `stems --version` prints `stems <semver>` and exits 0.

use std::process::Command;

fn is_semver(s: &str) -> bool {
    let core = s.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn version_flag_prints_name_and_semver() {
    let output = Command::new(env!("CARGO_BIN_EXE_stems"))
        .arg("--version")
        .output()
        .expect("failed to run stems binary");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is not utf-8");
    let line = stdout.lines().next().unwrap_or("");
    let version = line
        .strip_prefix("stems ")
        .unwrap_or_else(|| panic!("stdout does not start with `stems `: {stdout:?}"));
    assert!(is_semver(version), "not a semver version: {version:?}");
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
}
