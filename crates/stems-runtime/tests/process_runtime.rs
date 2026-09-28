//! Integration tests for `ProcessRuntime` against real processes.
//!
//! Every test ends by asserting the process group it created is empty, so a
//! regression in the kill model shows up as a failing test, not a leak.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use stems_runtime::os;
use stems_runtime::{
    AdoptRecord, Handle, OutputStream, OutputStreamKind, ProcessRuntime, ProcessSpec, Runtime,
    RuntimeError, StartSpec, StartTime, StopOutcome,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sh(script: &str) -> StartSpec {
    StartSpec::Process(ProcessSpec::shell(script, std::env::temp_dir()))
}

fn shop_api_spec() -> StartSpec {
    let dir = repo_root().join("examples/repos/shop-api");
    let mut env = BTreeMap::new();
    env.insert("SHOP_CHAOS".into(), "1".into());
    env.insert("PORT".into(), "0".into());
    env.insert("HOST".into(), "127.0.0.1".into());
    env.insert("PYTHONUNBUFFERED".into(), "1".into());
    StartSpec::Process(ProcessSpec {
        command: "python3".into(),
        args: vec!["app.py".into()],
        shell: false,
        cwd: dir,
        env,
        clear_env: false,
    })
}

/// Poll `f` until it returns `Some`, failing after `TIMEOUT`.
async fn poll_until<T>(what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(POLL).await;
    }
}

/// Read lines until one satisfies `pred`; returns it.
async fn wait_line(out: &mut OutputStream, pred: impl Fn(&str) -> bool) -> String {
    let mut seen = Vec::new();
    let found = tokio::time::timeout(TIMEOUT, async {
        loop {
            let line = out.recv().await?;
            if pred(&line.text) {
                return Some(line.text);
            }
            seen.push(line.text);
        }
    })
    .await;
    match found {
        Ok(Some(line)) => line,
        Ok(None) => panic!("output closed before expected line; saw: {seen:#?}"),
        Err(_) => panic!("timed out waiting for output line; saw: {seen:#?}"),
    }
}

async fn collect_all(out: &mut OutputStream) -> Vec<(OutputStreamKind, String)> {
    tokio::time::timeout(TIMEOUT, async {
        let mut v = Vec::new();
        while let Some(l) = out.recv().await {
            v.push((l.stream, l.text));
        }
        v
    })
    .await
    .expect("output did not close")
}

async fn assert_group_empty(pgid: i32) {
    poll_until("process group to empty", async || {
        os::process_tree(pgid).is_empty().then_some(())
    })
    .await;
}

async fn http_get(port: u16, path: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    s.write_all(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = String::new();
    let _ = tokio::time::timeout(TIMEOUT, s.read_to_string(&mut buf)).await;
    buf
}

/// Start shop-api and return (handle, output, port).
async fn start_shop(rt: &ProcessRuntime) -> (Handle, OutputStream, u16) {
    let h = rt.start(&shop_api_spec()).await.expect("start shop-api");
    let mut out = rt.output_stream(&h).expect("output stream");
    let line = wait_line(&mut out, |l| l.contains("listening on")).await;
    let port = line
        .rsplit(':')
        .next()
        .and_then(|p| p.trim().parse().ok())
        .expect("port in listening line");
    (h, out, port)
}

#[tokio::test(flavor = "multi_thread")]
async fn whole_group_dies_on_stop() {
    let rt = ProcessRuntime::new();
    let h = rt.start(&sh("sleep 30 & sleep 30 & wait")).await.unwrap();
    assert_eq!(h.pgid(), h.pid());
    let facts = poll_until("3 processes", async || {
        let f = rt.describe(&h).await.unwrap();
        (f.children.len() == 3).then_some(f)
    })
    .await;
    let pids: Vec<i32> = facts.children.iter().map(|p| p.pid).collect();
    assert!(pids.contains(&h.pid()));
    assert!(facts.children.iter().all(|p| p.pgid == h.pgid()));

    let outcome = rt.stop(&h, Duration::from_secs(2)).await.unwrap();
    assert_eq!(outcome, StopOutcome::Graceful);
    assert!(!rt.is_alive(&h).await);
    assert_group_empty(h.pgid()).await;
    let status = rt.wait(&h).await.unwrap();
    assert_eq!(status.signal, Some(libc_sigterm()));
}

fn libc_sigterm() -> i32 {
    os::Signal::SIGTERM as i32
}

#[tokio::test(flavor = "multi_thread")]
async fn children_ignoring_sigterm_are_killed_via_group() {
    let rt = ProcessRuntime::new();
    // Ignored dispositions are inherited across fork/exec, so all three ignore TERM.
    let h = rt
        .start(&sh("trap '' TERM; sleep 30 & sleep 30 & wait"))
        .await
        .unwrap();
    poll_until("3 processes", async || {
        (rt.describe(&h).await.unwrap().children.len() == 3).then_some(())
    })
    .await;
    let t0 = Instant::now();
    let outcome = rt.stop(&h, Duration::from_millis(300)).await.unwrap();
    assert_eq!(outcome, StopOutcome::Killed);
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    assert_group_empty(h.pgid()).await;
    assert_eq!(
        rt.wait(&h).await.unwrap().signal,
        Some(os::Signal::SIGKILL as i32)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shop_api_fork_then_graceful_stop() {
    let rt = ProcessRuntime::new();
    let (h, _out, port) = start_shop(&rt).await;
    let resp = http_get(port, "/__chaos/fork?n=3").await;
    assert!(resp.contains("200"), "{resp}");
    let facts = poll_until("4 processes", async || {
        let f = rt.describe(&h).await.unwrap();
        (f.children.len() == 4).then_some(f)
    })
    .await;
    assert!(facts.ports.contains(&port), "ports {:?}", facts.ports);
    assert!(facts.children.iter().all(|p| p.rss_bytes > 0));
    let tracked: Vec<(i32, StartTime)> = facts
        .children
        .iter()
        .map(|p| (p.pid, os::process_start_time(p.pid).unwrap()))
        .collect();

    let outcome = rt.stop(&h, Duration::from_secs(2)).await.unwrap();
    assert_eq!(outcome, StopOutcome::Graceful);
    for (pid, st) in tracked {
        assert!(!os::is_alive(pid, st), "pid {pid} survived");
    }
    assert_group_empty(h.pgid()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shop_api_hang_on_stop_is_killed_after_grace() {
    let rt = ProcessRuntime::new();
    let (h, mut out, port) = start_shop(&rt).await;
    let resp = http_get(port, "/__chaos/hang-on-stop").await;
    assert!(resp.contains("200"), "{resp}");
    let t0 = Instant::now();
    let outcome = rt.stop(&h, Duration::from_millis(500)).await.unwrap();
    let took = t0.elapsed();
    assert_eq!(outcome, StopOutcome::Killed);
    assert!(took >= Duration::from_millis(500), "{took:?}");
    assert!(took < Duration::from_millis(1500), "{took:?}");
    // The app logged that it ignored our SIGTERM.
    let lines = collect_all(&mut out).await;
    assert!(lines.iter().any(|(_, l)| l.contains("ignoring SIGTERM")));
    assert_group_empty(h.pgid()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shop_api_crash_exit_code() {
    let rt = ProcessRuntime::new();
    let (h, _out, port) = start_shop(&rt).await;
    let _ = http_get(port, "/__chaos/crash?code=3").await;
    let status = tokio::time::timeout(TIMEOUT, rt.wait(&h))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code, Some(3));
    assert_eq!(status.signal, None);
    assert!(!rt.is_alive(&h).await);
    assert_eq!(
        rt.stop(&h, Duration::from_millis(200)).await.unwrap(),
        StopOutcome::AlreadyDead
    );
    assert_group_empty(h.pgid()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn env_and_cwd_are_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut spec = ProcessSpec::shell("pwd; echo \"$FOO\"", dir.path());
    spec.env.insert("FOO".into(), "bar baz".into());
    let rt = ProcessRuntime::new();
    let h = rt.start(&StartSpec::Process(spec)).await.unwrap();
    let mut out = rt.output_stream(&h).unwrap();
    let lines = collect_all(&mut out).await;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(
        Path::new(&lines[0].1).canonicalize().unwrap(),
        dir.path().canonicalize().unwrap()
    );
    assert_eq!(lines[1].1, "bar baz");
    assert!(rt.wait(&h).await.unwrap().success());
    assert_group_empty(h.pgid()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_env_and_shell_args() {
    let mut spec = ProcessSpec::shell("echo \"${HOME:-unset} $FOO $0 $1\"", std::env::temp_dir());
    spec.clear_env = true;
    spec.env.insert("FOO".into(), "x".into());
    spec.args = vec!["zero".into(), "one".into()];
    let rt = ProcessRuntime::new();
    let h = rt.start(&StartSpec::Process(spec)).await.unwrap();
    let mut out = rt.output_stream(&h).unwrap();
    let lines = collect_all(&mut out).await;
    assert_eq!(
        lines,
        vec![(OutputStreamKind::Out, "unset x zero one".to_string())]
    );
    rt.wait(&h).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_exec_without_shell() {
    let rt = ProcessRuntime::new();
    let spec = ProcessSpec {
        command: "echo".into(),
        args: vec!["a b".into(), "$HOME".into()],
        shell: false,
        cwd: std::env::temp_dir(),
        env: BTreeMap::new(),
        clear_env: false,
    };
    let h = rt.start(&StartSpec::Process(spec)).await.unwrap();
    let mut out = rt.output_stream(&h).unwrap();
    let lines = collect_all(&mut out).await;
    assert_eq!(lines[0].1, "a b $HOME");
}

#[tokio::test(flavor = "multi_thread")]
async fn stdout_stderr_partial_lines_and_late_first_subscriber() {
    let rt = ProcessRuntime::new();
    let h = rt
        .start(&sh("printf 'e1\\r\\n' >&2; printf 'o1\\no2'"))
        .await
        .unwrap();
    // Subscribe only after the process has exited: the first subscriber still sees everything.
    assert!(rt.wait(&h).await.unwrap().success());
    let mut out = rt.output_stream(&h).unwrap();
    let mut lines = collect_all(&mut out).await;
    lines.sort();
    assert_eq!(
        lines,
        vec![
            (OutputStreamKind::Out, "o1".to_string()),
            (OutputStreamKind::Out, "o2".to_string()),
            (OutputStreamKind::Err, "e1".to_string()),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_subscriber_drops_are_counted() {
    let rt = ProcessRuntime::new();
    let h = rt.start(&sh("seq 1 20000")).await.unwrap();
    let mut out = rt.output_stream(&h).unwrap();
    rt.wait(&h).await.unwrap();
    // Nothing read yet: only the last OUTPUT_CHANNEL_CAPACITY lines are retained.
    let received = collect_all(&mut out).await.len() as u64;
    let dropped = out.dropped();
    assert!(dropped > 0);
    assert_eq!(received + dropped, 20000);
    assert_eq!(rt.describe(&h).await.unwrap().dropped_lines, dropped);
    assert_eq!(rt.dropped_lines(&h), dropped);
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_failures() {
    let rt = ProcessRuntime::new();
    let missing = StartSpec::Process(ProcessSpec {
        command: "/definitely/not/a/program".into(),
        args: vec![],
        shell: false,
        cwd: std::env::temp_dir(),
        env: BTreeMap::new(),
        clear_env: false,
    });
    assert!(matches!(
        rt.start(&missing).await,
        Err(RuntimeError::SpawnFailed { .. })
    ));
    let bad_cwd = StartSpec::Process(ProcessSpec::shell("true", "/definitely/not/a/dir"));
    assert!(matches!(
        rt.start(&bad_cwd).await,
        Err(RuntimeError::SpawnFailed { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn adopt_after_restart_and_stop_through_adopted_handle() {
    let original = ProcessRuntime::new();
    let h = original
        .start(&sh("sleep 30 & sleep 30 & wait"))
        .await
        .unwrap();
    let facts = poll_until("3 processes", async || {
        let f = original.describe(&h).await.unwrap();
        (f.children.len() == 3).then_some(f)
    })
    .await;
    let record = AdoptRecord {
        pid: facts.pid,
        pgid: facts.pgid,
        start_time: facts.start_time,
        container_id: None,
    };
    // Round-trips through JSON like the state file would.
    let record: AdoptRecord =
        serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();

    // A fresh runtime stands in for the restarted daemon.
    let restarted = ProcessRuntime::new();
    let adopted = restarted.adopt(&record).await.expect("adopted");
    assert!(adopted.is_adopted());
    assert_eq!(adopted.pid(), h.pid());
    assert!(restarted.is_alive(&adopted).await);
    assert!(restarted.output_stream(&adopted).is_none());
    assert_eq!(
        restarted.describe(&adopted).await.unwrap().children.len(),
        3
    );

    let outcome = restarted
        .stop(&adopted, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::Graceful);
    assert!(!restarted.is_alive(&adopted).await);
    assert_group_empty(h.pgid()).await;
    let _ = tokio::time::timeout(TIMEOUT, restarted.wait(&adopted))
        .await
        .expect("adopted wait resolves");
}

#[tokio::test(flavor = "multi_thread")]
async fn adopt_rejects_mismatches() {
    let rt = ProcessRuntime::new();
    let h = rt.start(&sh("sleep 30")).await.unwrap();
    let good = AdoptRecord {
        pid: h.pid(),
        pgid: h.pgid(),
        start_time: h.start_time(),
        container_id: None,
    };
    let other = ProcessRuntime::new();
    let wrong_start = AdoptRecord {
        start_time: StartTime(h.start_time().0 + 1),
        ..good.clone()
    };
    assert!(other.adopt(&wrong_start).await.is_none());
    let wrong_pgid = AdoptRecord {
        pgid: h.pgid() + 1,
        ..good.clone()
    };
    assert!(other.adopt(&wrong_pgid).await.is_none());
    let container = AdoptRecord {
        container_id: Some("abc".into()),
        ..good.clone()
    };
    assert!(other.adopt(&container).await.is_none());
    assert!(other.adopt(&good).await.is_some());

    assert_eq!(
        rt.stop(&h, Duration::from_secs(2)).await.unwrap(),
        StopOutcome::Graceful
    );
    assert!(other.adopt(&good).await.is_none(), "dead process adopted");
    assert_group_empty(h.pgid()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_adopted_handle_never_signals_a_reused_pid() {
    // A handle whose start time does not match the live process at that pid must
    // be treated as dead: stop is a no-op and the process is untouched.
    let rt = ProcessRuntime::new();
    let victim = rt.start(&sh("sleep 30")).await.unwrap();
    let stale = Handle::Adopted {
        id: stems_runtime::HandleId(999),
        pid: victim.pid(),
        pgid: victim.pgid(),
        start_time: StartTime(victim.start_time().0 + 1),
    };
    assert!(!rt.is_alive(&stale).await);
    assert_eq!(
        rt.stop(&stale, Duration::from_millis(100)).await.unwrap(),
        StopOutcome::AlreadyDead
    );
    assert!(rt.is_alive(&victim).await);
    rt.stop(&victim, Duration::from_secs(2)).await.unwrap();
    assert_group_empty(victim.pgid()).await;
    rt.release(&victim);
    assert!(matches!(
        rt.wait(&victim).await,
        Err(RuntimeError::NotFound(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn handle_serialises_identifying_parts() {
    let h = Handle::Process {
        id: stems_runtime::HandleId(7),
        pid: 42,
        pgid: 42,
        start_time: StartTime(1234),
    };
    let json = serde_json::to_value(&h).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"kind": "process", "id": 7, "pid": 42, "pgid": 42, "start_time": 1234})
    );
    let back: Handle = serde_json::from_value(json).unwrap();
    assert_eq!(back, h);
}
