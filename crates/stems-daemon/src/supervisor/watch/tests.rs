//! Pure watchdog parts: glob/ignore matching and the coalescing state
//! machine with a fake clock (instants are built from a fixed origin).

use super::*;

fn defaults() -> Vec<String> {
    stems_config::defaults::WATCH_IGNORE
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn matcher(paths: &[&str], user_ignore: &[&str]) -> Matcher {
    let mut ignore = defaults();
    ignore.extend(user_ignore.iter().map(|s| s.to_string()));
    let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
    Matcher::new(Path::new("/w/api"), &paths, &ignore_patterns(&ignore)).unwrap()
}

#[test]
fn glob_and_ignore_table() {
    let py = matcher(&["*.py"], &[]);
    let all = matcher(&["**"], &["secrets/**", "tmp", "*.bak"]);
    let src = matcher(&["src/**", "Dockerfile"], &[]);
    // (matcher, relative path, watched?)
    let table: &[(&Matcher, &str, bool)] = &[
        (&py, "app.py", true),
        (&py, "src/deep/mod.py", true), // `*` crosses directories
        (&py, "README.md", false),
        (&py, "node_modules/x.py", false), // default ignore wins
        (&py, "a/node_modules/b/c.py", false),
        (&py, "__pycache__/app.cpython-313.pyc", false),
        (&py, "__pycache__/x.py", false),
        (&py, ".venv/lib/site.py", false), // extra default
        (&py, ".git/hooks/pre-commit.py", false),
        (&py, "target/gen.py", false),
        (&py, "dist/x.py", false),
        (&py, "build/x.py", false),
        (&py, ".stems/state.py", false),
        (&all, "server.log", false), // `*.log` extra default
        (&all, "logs/today.log", false),
        (&all, "secrets/key.pem", false), // user ignore
        (&all, "tmp/scratch.txt", false), // bare directory name
        (&all, "a/tmp/scratch.txt", false),
        (&all, "notes.bak", false),
        (&all, "src/main.rs", true),
        (&all, "node_modules", false), // the ignored directory itself
        (&src, "src/a/b.ts", true),
        (&src, "Dockerfile", true),
        (&src, "docs/x.md", false),
    ];
    for (m, rel, want) in table {
        assert_eq!(m.matches_rel(rel), *want, "{rel}");
    }
}

#[test]
fn absolute_paths_are_made_relative_to_the_root() {
    let m = matcher(&["*.py"], &[]);
    assert_eq!(
        m.matches(Path::new("/w/api/app.py")).as_deref(),
        Some("app.py")
    );
    assert_eq!(
        m.matches(Path::new("/w/api/src/x.py")).as_deref(),
        Some("src/x.py")
    );
    assert_eq!(m.matches(Path::new("/w/other/app.py")), None);
    assert_eq!(m.matches(Path::new("/w/api")), None);
    assert_eq!(m.matches(Path::new("/w/api/node_modules/x.py")), None);
}

#[test]
fn canonical_root_also_matches() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let m = Matcher::new(&link, &["*.py".into()], &ignore_patterns(&defaults())).unwrap();
    let canon = real.canonicalize().unwrap();
    assert_eq!(m.matches(&canon.join("a.py")).as_deref(), Some("a.py"));
    assert_eq!(m.matches(&link.join("a.py")).as_deref(), Some("a.py"));
}

#[test]
fn defaults_are_merged_with_user_ignores_once() {
    let mut rule = defaults();
    rule.push("*.bak".into());
    rule.push("**/.venv/**".into()); // duplicate of an extra default
    let merged = ignore_patterns(&rule);
    for d in stems_config::defaults::WATCH_IGNORE {
        assert!(merged.iter().any(|m| m == d), "{d} missing");
    }
    for d in EXTRA_IGNORE {
        assert_eq!(merged.iter().filter(|m| *m == d).count(), 1, "{d}");
    }
    assert!(merged.contains(&"*.bak".to_string()));
}

#[test]
fn workspace_root_rules_use_the_integration_repo() {
    let dir = tempfile::tempdir().unwrap();
    let ws_dir = dir.path().join("ws");
    let code = dir.path().join("api");
    std::fs::create_dir_all(&ws_dir).unwrap();
    std::fs::create_dir_all(&code).unwrap();
    std::fs::write(
        ws_dir.join("stems.yaml"),
        "schema_version: 1\nname: t\nstems:\n  api:\n    type: process\n    command: run\n    codebase: ../api\n    watch:\n      - { paths: ['*.py'] }\n      - { paths: ['proto/**'], root: workspace, action: rebuild }\n",
    )
    .unwrap();
    let ws = stems_config::load(stems_config::LoadOptions {
        workspace: Some(ws_dir.clone()),
        cwd: ws_dir.clone(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap();
    let stem = ws.workspace.stem("api").unwrap();
    let r0 = rule_root(&ws, stem, &stem.watch[0]);
    let r1 = rule_root(&ws, stem, &stem.watch[1]);
    assert_eq!(r0.canonicalize().unwrap(), code.canonicalize().unwrap());
    assert_eq!(r1.canonicalize().unwrap(), ws_dir.canonicalize().unwrap());
    let m1 = Matcher::new(
        &r1,
        &stem.watch[1].paths,
        &ignore_patterns(&stem.watch[1].ignore),
    )
    .unwrap();
    assert!(m1.matches(&r1.join("proto/a.proto")).is_some());
    assert!(m1.matches(&code.join("proto/a.proto")).is_none());
}

// --- coalescing ------------------------------------------------------------

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

#[test]
fn debounce_coalesces_a_burst_into_one_batch() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(Duration::from_millis(200), Duration::ZERO);
    assert_eq!(c.deadline(), None);
    for i in 0..10u64 {
        c.event(at(t0, i * 10), format!("f{i}.py"));
    }
    c.event(at(t0, 95), "f0.py"); // duplicates count once
    assert_eq!(c.deadline(), Some(at(t0, 200)));
    assert_eq!(c.take_due(at(t0, 199)), None);
    let b = c.take_due(at(t0, 200)).expect("due");
    assert_eq!(b.paths.len(), 10);
    assert_eq!(b.paths.first().map(String::as_str), Some("f0.py"));
    assert!(!c.pending());
    assert_eq!(c.take_due(at(t0, 1000)), None);
}

#[test]
fn settle_waits_for_quiet() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(Duration::from_millis(100), Duration::from_millis(500));
    c.event(at(t0, 0), "a");
    assert_eq!(c.deadline(), Some(at(t0, 500)));
    // Changes keep coming every 300 ms: never quiet for 500 ms.
    for t in [300, 600, 900] {
        c.event(at(t0, t), "a");
        assert_eq!(c.take_due(at(t0, t + 499)), None);
    }
    assert_eq!(c.deadline(), Some(at(t0, 1400)));
    assert!(c.take_due(at(t0, 1400)).is_some());
}

#[test]
fn a_debounce_longer_than_settle_still_holds() {
    let t0 = Instant::now();
    let mut c = Coalescer::new(Duration::from_millis(800), Duration::from_millis(100));
    c.event(at(t0, 0), "a");
    c.event(at(t0, 50), "b");
    assert_eq!(c.deadline(), Some(at(t0, 800)));
}

#[test]
fn one_pending_action_while_busy() {
    let t0 = Instant::now();
    let mut s = Scheduler::new([(Duration::from_millis(100), Duration::ZERO)]);
    s.event(0, at(t0, 0), "app.py");
    assert_eq!(s.poll(at(t0, 50)), None);
    let (rule, b) = s.poll(at(t0, 100)).expect("fires");
    assert_eq!(rule, 0);
    assert_eq!(b.paths.len(), 1);
    assert!(s.busy());
    // 50 changes while the action runs: nothing fires, one batch waits.
    for i in 0..50u64 {
        s.event(0, at(t0, 200 + i * 20), format!("f{}.py", i % 5));
    }
    assert!(s.pending());
    assert_eq!(s.next_deadline(), None);
    assert_eq!(s.poll(at(t0, 5000)), None);
    s.finished();
    // The pending batch is overdue: it fires at once, exactly once.
    let (_, b) = s.poll(at(t0, 5000)).expect("pending batch fires");
    assert_eq!(b.paths.len(), 5);
    s.finished();
    assert_eq!(s.poll(at(t0, 10_000)), None);
    assert!(!s.pending());
}

#[test]
fn rules_fire_one_at_a_time() {
    let t0 = Instant::now();
    let mut s = Scheduler::new([
        (Duration::from_millis(100), Duration::ZERO),
        (Duration::from_millis(100), Duration::ZERO),
    ]);
    s.event(0, at(t0, 0), "a.py");
    s.event(1, at(t0, 0), "Dockerfile");
    assert_eq!(s.next_deadline(), Some(at(t0, 100)));
    assert_eq!(s.poll(at(t0, 100)).map(|(r, _)| r), Some(0));
    assert_eq!(s.poll(at(t0, 100)), None);
    s.finished();
    assert_eq!(s.poll(at(t0, 100)).map(|(r, _)| r), Some(1));
}

#[test]
fn clear_drops_waiting_batches() {
    let t0 = Instant::now();
    let mut s = Scheduler::new([(Duration::from_millis(100), Duration::ZERO)]);
    s.event(0, at(t0, 0), "a");
    s.clear();
    assert!(!s.pending());
    assert_eq!(s.poll(at(t0, 1000)), None);
}

// --- actions ---------------------------------------------------------------

#[test]
fn signals_parse() {
    use stems_runtime::os::Signal;
    assert_eq!(parse_signal("SIGHUP").unwrap(), Signal::SIGHUP);
    assert_eq!(parse_signal("hup").unwrap(), Signal::SIGHUP);
    assert_eq!(parse_signal("USR1").unwrap(), Signal::SIGUSR1);
    assert_eq!(parse_signal("15").unwrap(), Signal::SIGTERM);
    assert_eq!(parse_signal("NOPE").unwrap_err().code, ErrorCode::Usage);
}

#[test]
fn signals_only_reach_process_stems() {
    assert!(check_signal_target("api", StemType::Process).is_ok());
    let e = check_signal_target("db", StemType::Docker).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotImplemented);
    assert!(e.hint.as_deref().unwrap_or("").contains("restart"), "{e:?}");
}
