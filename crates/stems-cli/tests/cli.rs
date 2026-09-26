//! End-to-end checks of the output conventions through `stems_cli::run`
//! (no subprocess): envelope, TTY detection, usage errors, stubs.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;
use stems_cli::{Invocation, run};

fn invoke(args: &[&str], tty: bool, cwd: PathBuf) -> (i32, String, String) {
    let inv = Invocation {
        args: std::iter::once("stems")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect(),
        cwd,
        env: HashMap::from([("HOME".to_string(), "/nonexistent".to_string())]),
        stdout_is_tty: tty,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run(&inv, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("not JSON ({e}): {s}"))
}

#[test]
fn piped_validate_emits_the_envelope() {
    let ws = repo().join("examples/workspaces/minimal");
    let (code, out, _) = invoke(&["validate"], false, ws);
    assert_eq!(code, 0);
    let v = json(&out);
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["data"]["start_order"], serde_json::json!([["echo-svc"]]));
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn tty_validate_is_human() {
    let ws = repo().join("examples/workspaces/minimal");
    let (code, out, _) = invoke(&["validate"], true, ws);
    assert_eq!(code, 0);
    assert!(out.starts_with("minimal: valid"), "{out}");
}

#[test]
fn unknown_subcommand_is_usage_exit_2() {
    let (code, out, err) = invoke(&["frobnicate"], false, repo());
    assert_eq!(code, 2);
    assert!(err.is_empty());
    let v = json(&out);
    assert_eq!(v["errors"][0]["code"], "USAGE");
    let (code, out, err) = invoke(&["frobnicate", "--human"], false, repo());
    assert_eq!(code, 2);
    assert!(out.is_empty());
    assert!(err.contains("unrecognized subcommand"), "{err}");
}

#[test]
fn bad_flag_is_usage() {
    let (code, out, _) = invoke(&["validate", "--nope"], false, repo());
    assert_eq!(code, 2);
    assert_eq!(json(&out)["errors"][0]["code"], "USAGE");
}

#[test]
fn help_prints_normally() {
    let (code, out, _) = invoke(&["--help"], false, repo());
    assert_eq!(code, 0);
    assert!(out.contains("Usage: stems"), "{out}");
}

#[test]
fn stubs_are_not_implemented_with_their_deliverable() {
    let (code, out, _) = invoke(&["add", "x", "--type", "process"], false, repo());
    assert_eq!(code, 1);
    let v = json(&out);
    assert_eq!(v["errors"][0]["code"], "NOT_IMPLEMENTED");
    assert_eq!(v["errors"][0]["details"]["deliverable"], "unscheduled");
}

#[test]
fn no_command_is_usage() {
    let (code, out, _) = invoke(&[], false, repo());
    assert_eq!(code, 2);
    assert_eq!(json(&out)["errors"][0]["code"], "USAGE");
}

#[test]
fn completions_are_raw_unless_json_is_explicit() {
    let (code, out, _) = invoke(&["completions", "zsh"], false, repo());
    assert_eq!(code, 0);
    assert!(
        out.starts_with("#compdef stems"),
        "{}",
        &out[..40.min(out.len())]
    );
    let (_, out, _) = invoke(&["completions", "fish", "--json"], false, repo());
    assert_eq!(json(&out)["data"]["shell"], "fish");
}

#[test]
fn show_one_stem_and_unknown_stem() {
    let ws = repo().join("examples/workspaces/minimal");
    let (code, out, _) = invoke(&["show", "echo-svc", "--effective"], false, ws.clone());
    assert_eq!(code, 0);
    let v = json(&out);
    assert_eq!(v["data"]["name"], "echo-svc");
    assert_eq!(
        v["data"]["defaults"]["logs.keep"],
        stems_config::defaults::LOGS_KEEP.to_string()
    );
    let (code, out, _) = invoke(&["show", "nope"], false, ws);
    assert_eq!(code, 2);
    assert_eq!(json(&out)["errors"][0]["code"], "UNKNOWN_STEM");
}

#[test]
fn init_then_validate_then_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("new-ws");
    std::fs::create_dir_all(&root).unwrap();
    let (code, out, _) = invoke(&["init", "--from", "hello-shop"], false, root.clone());
    assert_eq!(code, 0, "{out}");
    assert_eq!(json(&out)["data"]["name"], "new-ws");
    let (code, out, _) = invoke(&["validate", "--skip-requires"], false, root.clone());
    assert_eq!(code, 0, "{out}");
    let (code, out, _) = invoke(&["init"], false, root.clone());
    assert_eq!(code, 1);
    assert_eq!(json(&out)["errors"][0]["code"], "ALREADY_INITIALISED");
    let (code, _, _) = invoke(
        &["init", "--force", "--name", "renamed"],
        false,
        root.clone(),
    );
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(root.join("stems.yaml")).unwrap();
    assert!(text.contains("\nname: renamed\n"));
    let gi = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(gi.matches("stems.local.yaml").count(), 1);
}
