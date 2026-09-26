//! Supervisor and scheduler tests with a fake runtime and a fake waiter.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use stems_api::{Event, EventKind, StartParams, StatusParams, StopParams, UpParams};
use stems_config::{Condition, Resolved, StemType};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::os::StartTime;
use stems_runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, OutputStream, Runtime, RuntimeError, RuntimeFacts,
    StartSpec, StopOutcome,
};
use tokio::sync::watch;

use super::*;

type Units = Mutex<HashMap<HandleId, (String, watch::Sender<Option<ExitStatus>>)>>;

#[derive(Default)]
struct FakeRuntime {
    next: AtomicI32,
    /// handle id -> (stem, exit channel)
    units: Units,
    /// Stems whose spawn fails.
    fail_spawn: HashSet<String>,
    /// Stems whose process exits right away with this code.
    crash: HashMap<String, i32>,
    starts: Mutex<Vec<String>>,
}

impl FakeRuntime {
    fn stem_of(spec: &StartSpec) -> String {
        let StartSpec::Process(p) = spec;
        p.env.get("STEMS_STEM").cloned().unwrap_or_default()
    }
    fn exit_rx(&self, h: &Handle) -> Option<watch::Receiver<Option<ExitStatus>>> {
        self.units
            .lock()
            .unwrap()
            .get(&h.id())
            .map(|(_, tx)| tx.subscribe())
    }
}

#[async_trait::async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        let stem = Self::stem_of(spec);
        if self.fail_spawn.contains(&stem) {
            return Err(RuntimeError::Io(std::io::Error::other("no such binary")));
        }
        self.starts.lock().unwrap().push(stem.clone());
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let h = Handle::Process {
            id: HandleId(n as u64),
            pid: 900_000 + n,
            pgid: 900_000 + n,
            start_time: StartTime(1),
        };
        let tx = watch::Sender::new(None);
        if let Some(code) = self.crash.get(&stem) {
            tx.send_replace(Some(ExitStatus {
                code: Some(*code),
                signal: None,
            }));
        }
        self.units.lock().unwrap().insert(h.id(), (stem, tx));
        Ok(h)
    }
    async fn stop(&self, h: &Handle, _grace: Duration) -> Result<StopOutcome, RuntimeError> {
        if let Some((_, tx)) = self.units.lock().unwrap().get(&h.id()) {
            tx.send_replace(Some(ExitStatus {
                code: None,
                signal: Some(15),
            }));
        }
        Ok(StopOutcome::Graceful)
    }
    async fn is_alive(&self, h: &Handle) -> bool {
        self.exit_rx(h).is_some_and(|rx| rx.borrow().is_none())
    }
    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        Err(RuntimeError::NotFound(h.id()))
    }
    fn output_stream(&self, _h: &Handle) -> Option<OutputStream> {
        None
    }
    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        let mut rx = self.exit_rx(h).ok_or(RuntimeError::NotFound(h.id()))?;
        let s = rx
            .wait_for(Option::is_some)
            .await
            .map(|s| s.unwrap_or(ExitStatus::UNKNOWN))
            .unwrap_or(ExitStatus::UNKNOWN);
        Ok(s)
    }
    async fn adopt(&self, _r: &AdoptRecord) -> Option<Handle> {
        None
    }
    fn release(&self, _h: &Handle) {}
}

/// Ready after a per-stem delay; tracks how many readiness waits overlap.
#[derive(Default)]
struct FakeWaiter {
    delay: HashMap<String, u64>,
    fail: HashSet<String>,
    now: AtomicUsize,
    max: AtomicUsize,
}

#[async_trait::async_trait]
impl Waiter for FakeWaiter {
    async fn wait_condition(&self, t: &WaitTarget, _c: Condition) -> Result<(), Error> {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(n, Ordering::SeqCst);
        let ms = self.delay.get(&t.stem).copied().unwrap_or(30);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        if !t.runtime.is_alive(&t.handle).await {
            let st = t
                .runtime
                .wait(&t.handle)
                .await
                .unwrap_or(ExitStatus::UNKNOWN);
            return Err(waiter::exited_error(&t.stem, st));
        }
        if self.fail.contains(&t.stem) {
            return Err(Error::new(ErrorCode::HealthTimeout, "never healthy"));
        }
        Ok(())
    }
}

struct FakeHost {
    ws: Arc<Resolved>,
    shutdown: AtomicBool,
    by_up: AtomicBool,
    _dir: tempfile::TempDir,
}

impl Host for FakeHost {
    fn resolved(&self) -> Option<Arc<Resolved>> {
        Some(self.ws.clone())
    }
    fn reload(&self, _actor: &str) -> Result<Arc<Resolved>, Error> {
        Ok(self.ws.clone())
    }
    fn request_shutdown(&self, _actor: &str, _reason: &str) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
    fn started_by_up(&self) -> bool {
        self.by_up.load(Ordering::SeqCst)
    }
    fn set_started_by_up(&self) {
        self.by_up.store(true, Ordering::SeqCst);
    }
}

fn host(yaml: &str) -> Arc<FakeHost> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("stems.yaml"),
        format!("schema_version: 1\nname: t\nstems:\n{yaml}"),
    )
    .unwrap();
    let ws = stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.path().to_path_buf()),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap_or_else(|e| panic!("{e}\n{yaml}"));
    Arc::new(FakeHost {
        ws: Arc::new(ws),
        shutdown: AtomicBool::new(false),
        by_up: AtomicBool::new(false),
        _dir: dir,
    })
}

struct Rig {
    sup: Arc<Supervisor>,
    events: Arc<EventBus>,
    host: Arc<FakeHost>,
    rt: Arc<FakeRuntime>,
    waiter: Arc<FakeWaiter>,
}

fn rig(yaml: &str, rt: FakeRuntime, waiter: FakeWaiter) -> Rig {
    let host = host(yaml);
    let events = Arc::new(EventBus::default());
    let rt = Arc::new(rt);
    let waiter = Arc::new(waiter);
    let mut reg = RuntimeRegistry::default();
    reg.register(StemType::Process, rt.clone());
    let sup = Supervisor::new(events.clone(), host.clone(), reg, waiter.clone());
    Rig {
        sup,
        events,
        host,
        rt,
        waiter,
    }
}

fn proc(name: &str, deps: &str) -> String {
    format!("  {name}: {{ type: process, command: run, depends_on: [{deps}] }}\n")
}

fn states(events: &[Event], stem: &str) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.kind == EventKind::STEM_STATE && e.stem.as_deref() == Some(stem))
        .map(|e| e.to.clone().unwrap_or_default())
        .collect()
}

fn seq_of(events: &[Event], stem: &str, to: &str) -> u64 {
    events
        .iter()
        .find(|e| {
            e.kind == EventKind::STEM_STATE
                && e.stem.as_deref() == Some(stem)
                && e.to.as_deref() == Some(to)
        })
        .unwrap_or_else(|| panic!("no {stem} -> {to} event"))
        .seq
}

#[tokio::test]
async fn parallelism_is_bounded() {
    let yaml: String = (0..6).map(|i| proc(&format!("s{i}"), "")).collect();
    let mut w = FakeWaiter::default();
    for i in 0..6 {
        w.delay.insert(format!("s{i}"), 80);
    }
    let r = rig(&yaml, FakeRuntime::default(), w);
    let res = r
        .sup
        .up(
            UpParams {
                max_parallel: Some(2),
                ..UpParams::default()
            },
            "cli:test",
        )
        .await
        .unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(res.ready.len(), 6);
    assert_eq!(r.waiter.max.load(Ordering::SeqCst), 2);

    // Unbounded enough: all six overlap.
    let r2 = rig(
        &yaml,
        FakeRuntime::default(),
        FakeWaiter {
            delay: (0..6).map(|i| (format!("s{i}"), 80)).collect(),
            ..FakeWaiter::default()
        },
    );
    r2.sup
        .up(
            UpParams {
                max_parallel: Some(8),
                ..UpParams::default()
            },
            "cli:test",
        )
        .await
        .unwrap();
    assert_eq!(r2.waiter.max.load(Ordering::SeqCst), 6);
}

#[tokio::test]
async fn conditions_are_waited_per_edge() {
    let yaml = format!(
        "{}{}  early: {{ type: process, command: run, depends_on: [{{ stem: a, condition: started }}] }}\n",
        proc("a", ""),
        proc("late", "a"),
    );
    let w = FakeWaiter {
        delay: [("a".to_string(), 200)].into_iter().collect(),
        ..FakeWaiter::default()
    };
    let r = rig(&yaml, FakeRuntime::default(), w);
    let res = r.sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert!(res.ok, "{res:?}");
    let ev = r.events.replay(0);
    let a_healthy = seq_of(&ev, "a", "healthy");
    assert!(
        seq_of(&ev, "early", "starting") < a_healthy,
        "started edge waits only for spawn"
    );
    assert!(
        seq_of(&ev, "late", "starting") > a_healthy,
        "healthy edge waits for healthy"
    );
    let up: Vec<_> = ev
        .iter()
        .filter(|e| e.kind == EventKind::UP_FINISHED)
        .collect();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].data["ok"], true);
}

fn failing_workspace() -> String {
    // layers: [a] [bad, x] [y]
    format!(
        "{}{}{}{}",
        proc("a", ""),
        proc("bad", "a"),
        proc("x", "a"),
        proc("y", "x")
    )
}

#[tokio::test]
async fn fail_fast_skips_what_has_not_started() {
    let rt = FakeRuntime {
        fail_spawn: ["bad".to_string()].into_iter().collect(),
        ..FakeRuntime::default()
    };
    let w = FakeWaiter {
        delay: [("x".to_string(), 150)].into_iter().collect(),
        ..FakeWaiter::default()
    };
    let r = rig(&failing_workspace(), rt, w);
    let res = r.sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.ready, ["a", "x"]);
    assert_eq!(res.failed.len(), 1);
    assert_eq!(res.failed[0].stem, "bad");
    assert_eq!(res.failed[0].error.code, ErrorCode::StartFailed);
    assert_eq!(res.skipped, ["y"]);
    assert!(!r.rt.starts.lock().unwrap().contains(&"y".to_string()));
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    let bad = st.stems.iter().find(|s| s.name == "bad").unwrap();
    assert_eq!(bad.state, StemState::Failed);
    assert_eq!(st.summary.failed, 1);
    assert_eq!(st.summary.healthy, 2);
    assert_eq!(st.summary.stopped, 1);
}

#[tokio::test]
async fn without_fail_fast_only_dependants_are_skipped() {
    let rt = FakeRuntime {
        crash: [("bad".to_string(), 3)].into_iter().collect(),
        ..FakeRuntime::default()
    };
    let r = rig(&failing_workspace(), rt, FakeWaiter::default());
    let res = r
        .sup
        .up(
            UpParams {
                fail_fast: false,
                ..UpParams::default()
            },
            "cli:test",
        )
        .await
        .unwrap();
    assert_eq!(res.ready, ["a", "x", "y"]);
    assert_eq!(res.failed[0].stem, "bad");
    assert_eq!(res.failed[0].error.code, ErrorCode::StartFailed);
    assert_eq!(res.failed[0].error.details["exit_code"], 3);
    assert!(res.skipped.is_empty());
    assert_eq!(states(&r.events.replay(0), "bad"), ["starting", "failed"]);
}

#[tokio::test]
async fn health_failure_stops_the_process_and_fails() {
    let w = FakeWaiter {
        fail: ["a".to_string()].into_iter().collect(),
        ..FakeWaiter::default()
    };
    let r = rig(
        &format!("{}{}", proc("a", ""), proc("b", "a")),
        FakeRuntime::default(),
        w,
    );
    let res = r.sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert_eq!(res.failed[0].error.code, ErrorCode::HealthTimeout);
    assert_eq!(res.skipped, ["b"]);
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    assert_eq!(st.stems[0].pid, None);
}

#[tokio::test]
async fn stop_refuses_with_running_dependants_unless_cascade() {
    let yaml = format!(
        "{}{}{}",
        proc("db", ""),
        proc("api", "db"),
        proc("web", "api")
    );
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    assert!(r.sup.up(UpParams::default(), "cli:t").await.unwrap().ok);
    let e = r
        .sup
        .stop(
            StopParams {
                stems: vec!["db".into()],
                ..StopParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::HasDependants);
    assert_eq!(e.details["dependants"], serde_json::json!(["api", "web"]));
    let res = r
        .sup
        .stop(
            StopParams {
                stems: vec!["db".into()],
                cascade: true,
                ..StopParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.stopped.len(), 3);
    let ev = r.events.replay(0);
    assert!(seq_of(&ev, "web", "stopped") < seq_of(&ev, "api", "stopping"));
    assert!(seq_of(&ev, "api", "stopped") < seq_of(&ev, "db", "stopping"));

    // start pulls dependencies back in; --no-deps does not.
    let res = r
        .sup
        .start(
            StartParams {
                stems: vec!["api".into()],
                no_deps: true,
                ..StartParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.ready, ["api"]);
    let res = r
        .sup
        .start(
            StartParams {
                stems: vec!["web".into()],
                ..StartParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.ready, ["db", "api", "web"]);
}

#[tokio::test]
async fn restart_keeps_auto_ports_and_changes_pid() {
    let yaml = "  web: { type: process, command: run, ports: [{ name: http, port: auto }], env: { SELF: \"${stem.self.port}\" } }\n  cli: { type: process, command: run, depends_on: [web], env: { URL: \"http://localhost:${stem.web.port}/\" } }\n";
    let r = rig(yaml, FakeRuntime::default(), FakeWaiter::default());
    assert!(r.sup.up(UpParams::default(), "cli:t").await.unwrap().ok);
    let st = r
        .sup
        .status(
            &StatusParams {
                verbose: true,
                ..StatusParams::default()
            },
            "t",
        )
        .unwrap();
    let web = &st.stems[0];
    let port = web.ports[0].port.unwrap();
    assert!(port > 1024 && web.ports[0].auto);
    let env = web.env.as_ref().unwrap();
    assert_eq!(env["PORT"], port.to_string());
    assert_eq!(env["SELF"], port.to_string());
    assert_eq!(env["STEMS_STEM"], "web");
    let cli_env = st.stems[1].env.as_ref().unwrap();
    assert_eq!(cli_env["URL"], format!("http://localhost:{port}/"));
    assert_eq!(cli_env["STEMS_WEB_PORT"], port.to_string());
    assert!(!cli_env.contains_key("PORT"));
    let allocated: Vec<_> = r
        .events
        .replay(0)
        .into_iter()
        .filter(|e| e.kind == EventKind::STEM_PORT_ALLOCATED)
        .collect();
    assert_eq!(allocated.len(), 1);
    assert_eq!(allocated[0].data["port"], port);

    let pid = web.pid.unwrap();
    let res = r
        .sup
        .restart(
            stems_api::RestartParams {
                stems: vec!["web".into()],
                ..Default::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(res.ok);
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    assert_ne!(st.stems[0].pid.unwrap(), pid);
    assert_eq!(st.stems[0].ports[0].port, Some(port));
    assert!(st.stems[0].env.is_none(), "env only with verbose");
}

#[tokio::test]
async fn externals_are_unknown_and_not_managed() {
    let yaml = format!(
        "  ext: {{ type: external }}\n{}",
        proc("app", "{ stem: ext, condition: healthy }")
    );
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    assert_eq!(st.stems[0].state, StemState::Unknown);
    assert_eq!(st.summary.unknown, 1);
    for e in [
        r.sup
            .start(
                StartParams {
                    stems: vec!["ext".into()],
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap_err(),
        r.sup
            .stop(
                StopParams {
                    stems: vec!["ext".into()],
                    ..Default::default()
                },
                "t",
            )
            .await
            .unwrap_err(),
    ] {
        assert_eq!(e.code, ErrorCode::NotManaged);
    }
}

#[tokio::test]
async fn down_all_stops_everything_and_requests_shutdown() {
    let yaml = format!("{}{}", proc("db", ""), proc("api", "db"));
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    r.sup
        .up(
            UpParams {
                daemon_auto_started: true,
                ..UpParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    // Stopping one stem of two keeps the daemon.
    let res = r
        .sup
        .down(
            DownParams {
                stems: vec!["api".into()],
                ..DownParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.stopped, ["api"]);
    assert!(!res.daemon_stopping);
    // The last one: started by up, nothing left -> shutdown.
    let res = r.sup.down(DownParams::default(), "cli:t").await.unwrap();
    assert_eq!(res.stopped, ["db"]);
    assert!(res.daemon_stopping);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(r.host.shutdown.load(Ordering::SeqCst));
    let ev = r.events.replay(0);
    assert!(ev.iter().any(|e| e.kind == EventKind::DOWN_FINISHED));
}

#[tokio::test]
async fn shutdown_hook_stops_running_stems_and_cancels_up() {
    let yaml = format!("{}{}", proc("slow", ""), proc("after", "slow"));
    let w = FakeWaiter {
        delay: [("slow".to_string(), 5_000)].into_iter().collect(),
        ..FakeWaiter::default()
    };
    let r = rig(&yaml, FakeRuntime::default(), w);
    let sup = r.sup.clone();
    let up = tokio::spawn(async move { sup.up(UpParams::default(), "cli:t").await });
    // Wait until `slow` is starting.
    let mut ph = r.sup.cell("slow").watch();
    ph.wait_for(|p| p.state == StemState::Starting)
        .await
        .unwrap();
    let t0 = std::time::Instant::now();
    r.sup.shutdown().await;
    let res = up.await.unwrap().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(res.skipped, ["slow", "after"]);
    assert_eq!(r.sup.cell("slow").state(), StemState::Stopped);
    assert_eq!(r.rt.starts.lock().unwrap().as_slice(), ["slow"]);
}

#[tokio::test]
async fn unknown_stems_and_profiles_are_refused() {
    let r = rig(
        &proc("a", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let e = r
        .sup
        .up(
            UpParams {
                stems: vec!["nope".into()],
                ..UpParams::default()
            },
            "t",
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownStem);
    let e = r
        .sup
        .up(
            UpParams {
                profile: Some("x".into()),
                ..UpParams::default()
            },
            "t",
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotImplemented);
}
