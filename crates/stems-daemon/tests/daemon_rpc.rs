//! End-to-end tests of the daemon over its real Unix socket, in-process.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use stems_api::client::{Client, ClientOptions, EventStream};
use stems_api::{API_VERSION, DaemonStatus, Event, EventKind, EventsResult, Method, VERSION};
use stems_core::ErrorCode;
use stems_daemon::{Daemon, DaemonPaths, RunOptions, wait_for_socket};
use stems_runtime::ProcessSpec;
use tokio::task::JoinHandle;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
    opts: RunOptions,
    paths: DaemonPaths,
}

/// A temp home and a temp copy of `examples/workspaces/minimal` (with the
/// `../../repos/shop-api` codebase directory it points at).
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/workspaces/minimal");
    let ws = tmp.path().join("examples/workspaces/minimal");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(tmp.path().join("examples/repos/shop-api")).unwrap();
    std::fs::copy(root.join("stems.yaml"), ws.join("stems.yaml")).unwrap();
    let mut opts = RunOptions::new(tmp.path().join("h"));
    opts.workspace = Some(ws.clone());
    opts.debug_rpc = true;
    opts.handle_signals = false;
    let paths = opts.paths();
    Fixture {
        _tmp: tmp,
        ws,
        opts,
        paths,
    }
}

async fn start(f: &Fixture) -> JoinHandle<Result<(), stems_core::Error>> {
    let task = tokio::spawn(Daemon::run(f.opts.clone()));
    wait_for_socket(&f.paths, T).await.unwrap();
    task
}

fn opts() -> ClientOptions {
    let mut o = ClientOptions::new("cli:test");
    o.client_version = VERSION.to_string();
    o
}

async fn next_of(stream: &mut EventStream, kind: EventKind) -> Event {
    tokio::time::timeout(T, async {
        loop {
            let ev = stream.next().await.expect("event stream ended early");
            if ev.kind == kind {
                return ev;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} event"))
}

/// Raw line-level connection (what `nc -U` does).
async fn raw_line(conn: &mut tokio::io::BufStream<tokio::net::UnixStream>, line: &str) -> Value {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    conn.write_all(line.as_bytes()).await.unwrap();
    conn.write_all(b"\n").await.unwrap();
    conn.flush().await.unwrap();
    let mut out = String::new();
    tokio::time::timeout(T, conn.read_line(&mut out))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_lifecycle_over_the_socket() {
    let f = fixture();
    let task = start(&f).await;

    // --- lock + socket on disk, socket 0600 ---------------------------------
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&f.paths.socket)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let lock = stems_daemon::lock::read(&f.paths.lock).unwrap().unwrap();
    assert_eq!(lock.pid, std::process::id() as i32);

    // --- connect, ping, info -------------------------------------------------
    let client = Client::connect(&f.paths.socket, opts()).await.unwrap();
    let pong: Value = client.call(Method::PING, json!({})).await.unwrap();
    assert_eq!(pong, json!({"pong": true}));
    let info = client.info();
    assert_eq!(info.pid, std::process::id());
    assert_eq!(info.version, VERSION);
    assert_eq!(info.api_version, API_VERSION);
    assert_eq!(
        info.workspace.as_deref(),
        Some(std::fs::canonicalize(&f.ws).unwrap().as_path())
    );
    let status: DaemonStatus = client.call(Method::DAEMON_STATUS, json!({})).await.unwrap();
    assert_eq!(status.workspace_name.as_deref(), Some("minimal"));
    assert_eq!(status.stem_count, 1);
    assert!(status.debug_rpc);

    // --- a second daemon for the same workspace is refused ---------------------
    let err = Daemon::run(f.opts.clone()).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::LockHeld);
    assert!(f.paths.socket.exists(), "loser must not remove the socket");

    // --- subscribe with replay sees daemon.started -----------------------------
    let mut sub = client.subscribe_events(Some(0)).await.unwrap();
    let started = next_of(&mut sub, EventKind::DAEMON_STARTED).await;
    assert_eq!(started.seq, 1);
    assert_eq!(started.actor, "daemon");
    next_of(&mut sub, EventKind::WORKSPACE_LOADED).await;

    // --- events since_seq ------------------------------------------------------
    let all: EventsResult = client
        .call(Method::EVENTS, json!({"since_seq": 0}))
        .await
        .unwrap();
    assert_eq!(all.events[0].kind, EventKind::DAEMON_STARTED);
    assert!(
        all.events
            .iter()
            .any(|e| e.kind == EventKind::WORKSPACE_LOADED)
    );
    let after: EventsResult = client
        .call(Method::EVENTS, json!({"since_seq": 1}))
        .await
        .unwrap();
    assert!(after.events.iter().all(|e| e.seq > 1));
    assert_eq!(after.events.len(), all.events.len() - 1);
    let one: EventsResult = client
        .call(Method::EVENTS, json!({"since_seq": 0, "limit": 1}))
        .await
        .unwrap();
    assert_eq!(one.events.len(), 1);

    // --- load_workspace RPC ------------------------------------------------------
    let loaded: stems_api::WorkspaceLoaded = client
        .call(Method::LOAD_WORKSPACE, json!({"path": f.ws}))
        .await
        .unwrap();
    assert_eq!(loaded.name, "minimal");
    assert_eq!(loaded.stems, vec!["echo-svc".to_string()]);
    let ev = next_of(&mut sub, EventKind::WORKSPACE_LOADED).await;
    assert_eq!(ev.actor, "cli:test");

    // --- unknown method -> NOT_IMPLEMENTED ----------------------------------------
    let e = client
        .call::<Value>("frobnicate", json!({}))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotImplemented);
    assert!(e.hint.as_deref().unwrap().contains("frobnicate"));

    // --- client version skew -> DAEMON_VERSION_MISMATCH ----------------------------
    let mut old = opts();
    old.client_version = "0.0.1".into();
    let e = Client::connect(&f.paths.socket, old).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DaemonVersionMismatch);
    assert_eq!(e.exit_code(), 4);
    assert_eq!(
        e.hint.as_deref(),
        Some("restart the daemon: stems daemon stop && stems daemon start")
    );

    // --- raw frames: api_version skew, malformed JSON, connection survives ---------
    let s = tokio::net::UnixStream::connect(&f.paths.socket)
        .await
        .unwrap();
    let mut raw = tokio::io::BufStream::new(s);
    let r = raw_line(
        &mut raw,
        r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":{},"meta":{"actor":"cli:raw","client_version":"0.1.0","api_version":99}}"#,
    )
    .await;
    assert_eq!(r["id"], json!(7));
    assert_eq!(r["error"]["code"], json!(-32001));
    assert_eq!(r["error"]["data"]["code"], json!("DAEMON_VERSION_MISMATCH"));
    let r = raw_line(&mut raw, "{this is not json").await;
    assert_eq!(r["id"], Value::Null);
    assert_eq!(r["error"]["code"], json!(-32700));
    let r = raw_line(&mut raw, r#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#).await;
    assert_eq!(r["error"]["code"], json!(-32600));
    let r = raw_line(
        &mut raw,
        &format!(
            r#"{{"jsonrpc":"2.0","id":9,"method":"ping","params":{{}},"meta":{{"actor":"cli:raw","client_version":"{VERSION}","api_version":1}}}}"#
        ),
    )
    .await;
    assert_eq!(r, json!({"jsonrpc":"2.0","id":9,"result":{"pong":true}}));
    drop(raw);

    // --- debug RPCs: start a process, see its output and exit --------------------
    let spec = ProcessSpec::shell("echo hi; exit 3", f.ws.clone());
    let handle: stems_runtime::Handle = client
        .call("_debug.start_raw", json!({ "spec": spec }))
        .await
        .unwrap();
    assert!(handle.pid() > 0);
    let out = next_of(&mut sub, EventKind::PROCESS_OUTPUT).await;
    assert_eq!(out.data["text"], json!("hi"));
    assert_eq!(out.data["stream"], json!("stdout"));
    let exited = next_of(&mut sub, EventKind::PROCESS_EXITED).await;
    assert_eq!(exited.data["code"], json!(3));
    assert_eq!(exited.data["signal"], Value::Null);
    assert_eq!(exited.data["pid"], json!(handle.pid()));
    let outcome: Value = client
        .call(
            "_debug.stop_raw",
            json!({ "handle": handle, "grace_ms": 500 }),
        )
        .await
        .unwrap();
    assert_eq!(outcome, json!("already_dead"));

    // a long-running one is stopped by the shutdown path
    let sleeper: stems_runtime::Handle = client
        .call(
            "_debug.start_raw",
            json!({ "spec": ProcessSpec::shell("exec sleep 30", f.ws.clone()) }),
        )
        .await
        .unwrap();
    let facts: stems_runtime::RuntimeFacts = client
        .call("_debug.describe", json!({ "handle": sleeper.id().0 }))
        .await
        .unwrap();
    assert_eq!(facts.pid, sleeper.pid());

    // --- shutdown: ok, then socket + lock removed, stream ends -------------------
    client.shutdown().await.unwrap();
    let stopping = next_of(&mut sub, EventKind::DAEMON_STOPPING).await;
    assert_eq!(stopping.actor, "cli:test");
    next_of(&mut sub, EventKind::DAEMON_STOPPED).await;
    let end = tokio::time::timeout(T, sub.next()).await.unwrap();
    assert!(end.is_none(), "stream should end after daemon.stopped");
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!f.paths.socket.exists());
    assert!(!f.paths.lock.exists());
    assert!(!stems_runtime::os::is_alive(
        sleeper.pid(),
        sleeper.start_time()
    ));

    // --- now: DAEMON_NOT_RUNNING ------------------------------------------------
    let e = Client::connect(&f.paths.socket, opts()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DaemonNotRunning);
    assert_eq!(
        stems_daemon::lock::probe(&f.paths),
        stems_daemon::LockState::Free
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nc_style_plain_unix_stream() {
    let f = fixture();
    let task = start(&f).await;
    let sock = f.paths.socket.clone();
    let line = tokio::task::spawn_blocking(move || {
        let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
        s.set_read_timeout(Some(T)).unwrap();
        s.write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{},\"meta\":{\"actor\":\"cli:nc\",\"client_version\":\"0.1.0\",\"api_version\":1}}\n",
        )
        .unwrap();
        let mut out = String::new();
        BufReader::new(s).read_line(&mut out).unwrap();
        out
    })
    .await
    .unwrap();
    assert_eq!(
        line,
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"pong\":true}}\n"
    );

    Client::connect(&f.paths.socket, opts())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!f.paths.socket.exists() && !f.paths.lock.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_lock_and_socket_are_reclaimed() {
    let f = fixture();
    // A lock from a dead process and a leftover socket file.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id() as i32;
    child.wait().unwrap();
    f.paths.ensure_dir().unwrap();
    std::fs::write(
        &f.paths.lock,
        json!({"pid": dead, "start_time": 1, "version": "0.0.1", "created_at": "2026-01-01T00:00:00Z"})
            .to_string(),
    )
    .unwrap();
    std::fs::write(&f.paths.socket, "").unwrap();
    assert_eq!(
        stems_daemon::lock::probe(&f.paths),
        stems_daemon::LockState::Stale { pid: dead }
    );

    let task = start(&f).await;
    let client = Client::connect(&f.paths.socket, opts()).await.unwrap();
    let ev: EventsResult = client.call(Method::EVENTS, json!({})).await.unwrap();
    assert_eq!(ev.events[0].kind, EventKind::DAEMON_STARTED);
    assert_eq!(ev.events[0].data["reclaimed_stale_lock_of"], json!(dead));
    client.shutdown().await.unwrap();
    tokio::time::timeout(T, task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!f.paths.lock.exists());
}
