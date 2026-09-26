use std::time::Instant;

use serde_json::json;

use super::*;

fn input(dir: &std::path::Path) -> DoctorInput {
    DoctorInput {
        paths: DaemonPaths::in_dir(dir.join("d")),
        home: dir.to_path_buf(),
        load: LoadOptions {
            workspace: Some(dir.join("ws")),
            cwd: dir.to_path_buf(),
            env: Default::default(),
            skip_local: true,
        },
        daemon: DaemonProbe::NotRunning,
        docker_host: Some("unix:///nonexistent/docker.sock".into()),
        ignore_pids: vec![std::process::id() as i32],
    }
}

fn sample() -> DoctorReport {
    let mut r = DoctorReport::new(
        vec![
            CheckResult::ok("config", "stems.yaml is valid (1 stem)"),
            CheckResult::fail(
                "daemon",
                "stale lock: the daemon (pid 42) exited without cleaning up",
            )
            .hint("`stems doctor --fix` removes it")
            .fixable(),
            CheckResult::fail(
                "requires.node",
                "`node` 20.1.0 does not satisfy the required range >=99",
            )
            .hint("install node >=99")
            .code(ErrorCode::ToolVersion),
            CheckResult::warn(
                "ports.echo-svc.http",
                "port 18090 is held by pid 7 (python3 -m http.server)",
            ),
            CheckResult::ok("disk.home", "12.0 GB free under /tmp"),
        ],
        false,
    );
    r.fixed.push(FixOutcome {
        id: "daemon".into(),
        action: "remove_stale_lock".into(),
        ok: true,
        message: "removed the stale lock of pid 42".into(),
    });
    r
}

#[test]
fn report_rendering_golden() {
    insta::assert_snapshot!(sample().render_human(), @r"
    CHECK                STATUS  MESSAGE
    config               ✓ ok    stems.yaml is valid (1 stem)
    daemon               ✗ fail  stale lock: the daemon (pid 42) exited without cleaning up (fixable)
    requires.node        ✗ fail  `node` 20.1.0 does not satisfy the required range >=99
    ports.echo-svc.http  ! warn  port 18090 is held by pid 7 (python3 -m http.server)
    disk.home            ✓ ok    12.0 GB free under /tmp

    hints:
      daemon: `stems doctor --fix` removes it
      requires.node: install node >=99

    fixed:
      ✓ daemon: removed the stale lock of pid 42 (remove_stale_lock)

    2 ok, 1 warn, 2 fail
    ");
}

#[test]
fn report_json_shape_and_exit() {
    let r = sample();
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["summary"], json!({ "ok": 2, "warn": 1, "fail": 2 }));
    assert_eq!(v["checks"][1]["fixable"], json!(true));
    assert_eq!(v["checks"][2]["code"], json!("TOOL_VERSION"));
    assert!(v["checks"][0].get("hint").is_none());
    assert_eq!(r.exit_code(), 1);
    assert_eq!(r.fixable().count(), 1);
    let e = r.checks[2].to_error();
    assert_eq!(e.code, ErrorCode::ToolVersion);
    assert_eq!(e.details["check"], json!("requires.node"));

    let warn_only = DoctorReport::new(vec![CheckResult::warn("x", "w")], false);
    assert!(warn_only.ok);
    assert_eq!(warn_only.exit_code(), 0);
    let strict = DoctorReport::new(vec![CheckResult::warn("x", "w")], true);
    assert!(!strict.ok);
    assert_eq!(strict.exit_code(), 1);
}

struct Sleepy;

#[async_trait]
impl Check for Sleepy {
    fn id(&self) -> String {
        "sleepy".into()
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        vec![CheckResult::ok("sleepy", "woke up")]
    }
}

struct Quick;

#[async_trait]
impl Check for Quick {
    fn id(&self) -> String {
        "quick".into()
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        vec![CheckResult::ok("quick", "fine")]
    }
}

#[tokio::test]
async fn a_check_that_hangs_times_out() {
    let d = tempfile::tempdir().unwrap();
    let cx = Arc::new(DoctorCtx::load(input(d.path())));
    let checks: Vec<Box<dyn Check>> = vec![Box::new(Sleepy), Box::new(Quick)];
    let start = Instant::now();
    let r = run_checks(&checks, cx, CHECK_TIMEOUT).await;
    let took = start.elapsed();
    assert!(took < Duration::from_millis(3000), "took {took:?}");
    assert!(took >= Duration::from_millis(1900), "took {took:?}");
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].id, "sleepy");
    assert_eq!(r[0].status, CheckStatus::Fail);
    assert!(r[0].message.contains("timed out"), "{}", r[0].message);
    assert_eq!(r[1].status, CheckStatus::Ok);
}

#[tokio::test]
async fn without_a_workspace_config_fails_and_daemon_is_ok() {
    let d = tempfile::tempdir().unwrap();
    let r = run(input(d.path()), false).await;
    let ids: Vec<&str> = r.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["config", "daemon", "disk.home"]);
    assert_eq!(r.checks[0].status, CheckStatus::Fail);
    assert_eq!(r.checks[1].status, CheckStatus::Ok);
    assert!(!r.ok);
}

#[test]
fn hot_reload_detection() {
    for (cmd, want) in [
        ("vite", Some("vite")),
        ("npx vite --port 3000", Some("vite")),
        ("./node_modules/.bin/vite", Some("vite")),
        ("next dev -p 3000", Some("next")),
        ("nodemon server.js", Some("nodemon")),
        ("cargo watch -x run", Some("cargo")),
        ("air", Some("air")),
        ("uvicorn app:app --reload", Some("uvicorn")),
        ("flask run --port 5000 --reload", Some("flask")),
        ("webpack serve --watch", Some("webpack")),
        ("python3 app.py", None),
        ("next build", None),
        ("uvicorn app:app", None),
        ("repair.sh", None),
        ("invite", None),
    ] {
        assert_eq!(hot_reload_command(cmd).as_deref(), want, "{cmd}");
    }
    let w = |paths: &[&str]| -> Vec<String> { paths.iter().map(|s| s.to_string()).collect() };
    assert!(watch_overlaps_sources(&w(&["src/**"])));
    assert!(watch_overlaps_sources(&w(&["./src/**/*.ts"])));
    assert!(watch_overlaps_sources(&w(&["**"])));
    assert!(!watch_overlaps_sources(&w(&["config/*.yaml"])));
    assert!(!watch_overlaps_sources(&w(&["*.py"])));
}
