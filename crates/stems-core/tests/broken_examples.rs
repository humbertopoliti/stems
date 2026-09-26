//! Every `examples/workspaces/broken/*` workspace produces exactly its
//! `EXPECTED.json` as the first (location-sorted) error, and the good
//! examples validate clean. Adding a broken workspace needs no change here.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use stems_core::{Errors, ValidateOptions, WorkspaceGraph, load_and_validate};

#[derive(Debug, Deserialize)]
struct Expected {
    code: String,
    path: String,
    exit: i32,
    message_contains: String,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn load_opts(dir: &Path) -> stems_config::LoadOptions {
    stems_config::LoadOptions {
        workspace: Some(dir.to_path_buf()),
        cwd: repo_root(),
        // The real environment: PATH is needed for `requires:` probes.
        env: std::env::vars().collect(),
        // Ignore any developer stems.local.yaml.
        skip_local: true,
    }
}

fn check(dir: &Path) -> Result<stems_config::Resolved, Errors> {
    load_and_validate(load_opts(dir), &ValidateOptions::default())
}

fn broken_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(repo_root().join("examples/workspaces/broken"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("EXPECTED.json").is_file())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_broken_example_reports_its_expected_first_error() {
    let dirs = broken_dirs();
    assert!(
        dirs.len() >= 10,
        "found only {} broken examples",
        dirs.len()
    );
    let mut failures = Vec::new();
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let expected: Expected =
            serde_json::from_str(&fs::read_to_string(dir.join("EXPECTED.json")).unwrap())
                .unwrap_or_else(|e| panic!("{name}/EXPECTED.json: {e}"));
        let errors = match check(&dir) {
            Ok(_) => {
                failures.push(format!("{name}: validated clean"));
                continue;
            }
            Err(e) => e,
        };
        let first = &errors.0[0];
        let path = first
            .path
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let mut problems = Vec::new();
        if first.code.as_str() != expected.code {
            problems.push(format!("code {} != {}", first.code, expected.code));
        }
        if path != expected.path {
            problems.push(format!("path `{path}` != `{}`", expected.path));
        }
        if !first.message.contains(&expected.message_contains) {
            problems.push(format!(
                "message `{}` lacks `{}`",
                first.message, expected.message_contains
            ));
        }
        if errors.exit_code() != expected.exit {
            problems.push(format!("exit {} != {}", errors.exit_code(), expected.exit));
        }
        if first.span.is_none() {
            problems.push("no source location".into());
        }
        if first.hint.as_deref().unwrap_or("").is_empty() {
            problems.push("no hint".into());
        }
        if !problems.is_empty() {
            failures.push(format!(
                "{name}: {}\n  all errors:\n{errors}",
                problems.join("; ")
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// Human rendering of every broken example's errors (reviewed wording).
/// `tool-version` is excluded: its message names the installed node version.
#[test]
fn broken_examples_human_rendering() {
    let root = repo_root().display().to_string();
    let mut out = String::new();
    for dir in broken_dirs() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if name == "tool-version" {
            continue;
        }
        let errors = check(&dir).expect_err(&name);
        out.push_str(&format!(
            "## {name} (exit {})\n{errors}\n\n",
            errors.exit_code()
        ));
    }
    insta::assert_snapshot!(out.replace(&root, "<repo>"));
}

#[test]
fn tool_version_example_names_the_range_whether_node_is_absent_or_old() {
    let dir = repo_root().join("examples/workspaces/broken/tool-version");
    let errors = check(&dir).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors}");
    let e = &errors.0[0];
    assert!(e.message.contains(">=99"), "{e}");
    assert_eq!(e.details["required"], ">=99");
    // With --skip-requires the workspace is otherwise valid.
    let skip = ValidateOptions {
        skip_requires: true,
        ..Default::default()
    };
    load_and_validate(load_opts(&dir), &skip).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn hello_shop_validates_clean_with_the_expected_start_order() {
    let r = check(&repo_root().join("examples/workspaces/hello-shop"))
        .unwrap_or_else(|e| panic!("hello-shop should validate:\n{e}"));
    let order = r.workspace.start_order().unwrap();
    assert_eq!(
        order,
        vec![
            vec!["httpbin", "postgres", "redis"],
            vec!["shop-api", "shop-worker"],
            vec!["shop-web"],
        ]
    );
    let stop = r.workspace.stop_order().unwrap();
    assert_eq!(stop[0], ["shop-web"]);
}

#[test]
fn minimal_and_fixture_workspaces_validate_clean() {
    for ws in [
        "examples/workspaces/minimal",
        "tests/fixtures/workspaces/include-demo",
    ] {
        let r = check(&repo_root().join(ws)).unwrap_or_else(|e| panic!("{ws}:\n{e}"));
        assert!(!r.workspace.start_order().unwrap().is_empty());
    }
}

#[test]
fn two_errors_fixture_reports_both() {
    let errors = check(&repo_root().join("tests/fixtures/workspaces/two-errors")).unwrap_err();
    let codes: Vec<&str> = errors.iter().map(|e| e.code.as_str()).collect();
    assert_eq!(codes, ["UNKNOWN_DEPENDENCY", "PORT_CONFLICT"], "{errors}");
    assert_eq!(errors.exit_code(), 2);
}
