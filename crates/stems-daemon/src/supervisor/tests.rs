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
    /// Pids `adopt` accepts (crash recovery, 11).
    adoptable: HashSet<i32>,
    /// Every start/stop and script run, in order: `start db`, `stop db`,
    /// `script db:setup`, `script ws:bootstrap` (16).
    actions: Mutex<Vec<String>>,
    /// Exit code of `<stem>:<script>` scripts (default 0).
    script_exit: HashMap<String, i32>,
    /// `<stem>:<script>` scripts that run until stopped.
    script_hang: HashSet<String>,
    /// Stems whose spawn fails from now on (set during a test; cascades).
    fail_spawn_now: Mutex<HashSet<String>>,
}

impl FakeRuntime {
    fn stem_of(spec: &StartSpec) -> String {
        let StartSpec::Process(p) = spec else {
            return String::new();
        };
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
        if let StartSpec::Process(p) = spec
            && let Some(script) = p.env.get("STEMS_SCRIPT")
        {
            // A script: exits right away (code from `script_exit`).
            let who = p.env.get("STEMS_STEM").map_or("ws", String::as_str);
            let key = format!("{who}:{script}");
            self.actions.lock().unwrap().push(format!("script {key}"));
            let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            let h = Handle::Process {
                id: HandleId(n as u64),
                pid: 800_000 + n,
                pgid: 800_000 + n,
                start_time: StartTime(1),
            };
            let code = self.script_exit.get(&key).copied().unwrap_or(0);
            let tx = watch::Sender::new((!self.script_hang.contains(&key)).then_some(ExitStatus {
                code: Some(code),
                signal: None,
            }));
            self.units.lock().unwrap().insert(h.id(), (key, tx));
            return Ok(h);
        }
        let stem = Self::stem_of(spec);
        if self.fail_spawn.contains(&stem) || self.fail_spawn_now.lock().unwrap().contains(&stem) {
            return Err(RuntimeError::Io(std::io::Error::other("no such binary")));
        }
        self.starts.lock().unwrap().push(stem.clone());
        self.actions.lock().unwrap().push(format!("start {stem}"));
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
        if let Some((name, tx)) = self.units.lock().unwrap().get(&h.id()) {
            if !name.contains(':') {
                self.actions.lock().unwrap().push(format!("stop {name}"));
            }
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
    async fn adopt(&self, r: &AdoptRecord) -> Option<Handle> {
        if !self.adoptable.contains(&r.pid) {
            return None;
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let h = Handle::Adopted {
            id: HandleId(n as u64),
            pid: r.pid,
            pgid: r.pgid,
            start_time: r.start_time,
        };
        let tx = watch::Sender::new(None);
        self.units
            .lock()
            .unwrap()
            .insert(h.id(), ("adopted".into(), tx));
        Some(h)
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
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        Some(self._dir.path().join("data"))
    }
}

fn host(yaml: &str) -> Arc<FakeHost> {
    host_doc(&format!("schema_version: 1\nname: t\nstems:\n{yaml}"))
}

/// A host for a whole `stems.yaml` document.
fn host_doc(doc: &str) -> Arc<FakeHost> {
    let yaml = doc;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("stems.yaml"), doc).unwrap();
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
    rig_host(host(yaml), rt, waiter)
}

fn rig_host(host: Arc<FakeHost>, rt: FakeRuntime, waiter: FakeWaiter) -> Rig {
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
    assert_eq!(e.code, ErrorCode::UnknownProfile);
}

#[tokio::test]
async fn recover_adopts_live_records_and_drops_dead_ones() {
    use crate::state::{DaemonRecord, PortRecord, StateFile, StemRecord};
    let yaml = format!(
        "{}{}",
        "  a: { type: process, command: run, ports: [{ name: http, port: auto }] }\n",
        proc("b", "a")
    );
    let rt = FakeRuntime {
        adoptable: [4242].into_iter().collect(),
        ..FakeRuntime::default()
    };
    let r = rig(&yaml, rt, FakeWaiter::default());
    let mut prev = StateFile::new("01PREV", DaemonRecord::default());
    let rec = |pid: i32, ports: Vec<PortRecord>| StemRecord {
        pid,
        pgid: pid,
        start_time: StartTime(7),
        container_id: None,
        ports,
        overlays: Vec::new(),
        state: StemState::Healthy,
        started_at: chrono::Utc::now(),
        log_file: None,
    };
    prev.stems.insert(
        "a".into(),
        rec(
            4242,
            vec![PortRecord {
                name: "http".into(),
                port: 45678,
                auto: true,
            }],
        ),
    );
    prev.stems.insert("b".into(), rec(4343, Vec::new()));
    let report = r.sup.recover(Some(prev), "daemon").await;
    assert_eq!(report.adopted, ["a"]);
    assert_eq!(report.dead, ["b"]);
    let a = r.sup.cell("a");
    assert_eq!(a.state(), StemState::Healthy);
    assert_eq!(r.sup.core.ports.allocated("a", "http"), Some(45678));
    let ev = r.events.replay(0);
    assert!(ev.iter().any(|e| e.kind == EventKind::STEM_ADOPTED
        && e.stem.as_deref() == Some("a")
        && e.data["pid"] == 4242));
    assert!(
        ev.iter()
            .any(|e| e.kind == EventKind::STEM_RECOVERED_DEAD && e.stem.as_deref() == Some("b"))
    );
    // `up` leaves the adopted stem alone and starts the other.
    let res = r.sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(*r.rt.starts.lock().unwrap(), ["b"]);
    let st = r.sup.status(&StatusParams::default(), "cli:test").unwrap();
    assert_eq!(st.stems[0].pid, Some(4242));
    assert_eq!(st.stems[0].reason.as_deref(), Some("adopted"));
    // Adopted stems stop like any other.
    let down = r
        .sup
        .stop(
            StopParams {
                stems: vec!["a".into()],
                cascade: true,
                timeout_ms: None,
            },
            "cli:test",
        )
        .await
        .unwrap();
    assert_eq!(down.stopped.len(), 2, "{down:?}");
    assert_eq!(a.state(), StemState::Stopped);
}

/// Deliverable 16: a workspace exercising every lifecycle script.
const SCRIPTED: &str = r#"schema_version: 1
name: t
scripts:
  bootstrap: echo boot
  teardown: echo bye
stems:
  db:
    type: process
    command: run
    scripts:
      setup: echo setup
      pre_start: echo pre
      post_start: echo post
      seed: echo seed
      pre_stop: echo pre-stop
      post_stop: echo post-stop
      reset: echo reset
  api:
    type: process
    command: run
    depends_on: [{ stem: db, condition: seeded }]
    scripts:
      setup: echo api-setup
  web:
    type: process
    command: run
    depends_on: [{ stem: db, condition: healthy }]
"#;

fn take_actions(r: &Rig) -> String {
    std::mem::take(&mut *r.rt.actions.lock().unwrap()).join("\n")
}

/// The sequence golden (plan 16): the exact ordered actions of `up` without
/// stamps, `down --all`, `up` with current stamps, and `up --fresh`.
#[tokio::test]
async fn script_sequence_golden() {
    let r = rig_host(
        host_doc(SCRIPTED),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let up = |fresh: bool| UpParams {
        fresh,
        max_parallel: Some(1),
        ..UpParams::default()
    };
    let res = r.sup.up(up(false), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    // `web` (condition: healthy) may start before db's seed finished, so
    // only the seeded chain is compared exactly.
    let first = take_actions(&r);
    let chain: Vec<&str> = first.lines().filter(|l| !l.contains("web")).collect();
    insta::assert_snapshot!(chain.join("\n"), @r"
    script ws:bootstrap
    script db:setup
    script db:pre_start
    start db
    script db:post_start
    script db:seed
    script api:setup
    start api
    ");
    assert!(first.contains("start web"));
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    assert!(st.stems.iter().find(|s| s.name == "db").unwrap().seeded);
    assert!(!st.stems.iter().find(|s| s.name == "api").unwrap().seeded);

    r.sup
        .down(
            DownParams {
                all: true,
                ..DownParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    let down = take_actions(&r);
    let down: Vec<&str> = down
        .lines()
        .filter(|l| !l.contains("web") && !l.contains("api"))
        .collect();
    insta::assert_snapshot!(down.join("\n"), @r"
    script db:pre_stop
    stop db
    script db:post_stop
    script ws:teardown
    ");

    // Stamps are current: no setup, no seed.
    let res = r.sup.up(up(false), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    let again = take_actions(&r);
    let again: Vec<&str> = again.lines().filter(|l| !l.contains("web")).collect();
    insta::assert_snapshot!(again.join("\n"), @r"
    script ws:bootstrap
    script db:pre_start
    start db
    script db:post_start
    start api
    ");
    assert!(
        r.sup.cell("db").info().seeded,
        "a current seed stamp counts as seeded"
    );

    // --fresh: stop, reset, clear stamps, then the full sequence again.
    let res = r.sup.up(up(true), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    let fresh = take_actions(&r);
    let fresh: Vec<&str> = fresh.lines().filter(|l| !l.contains("web")).collect();
    insta::assert_snapshot!(fresh.join("\n"), @r"
    script ws:bootstrap
    stop api
    script db:pre_stop
    stop db
    script db:post_stop
    script db:reset
    script db:setup
    script db:pre_start
    start db
    script db:post_start
    script db:seed
    script api:setup
    start api
    ");
    let ev = r.events.replay(0);
    let seeding = ev
        .iter()
        .rev()
        .find(|e| e.kind == EventKind::STEM_STATE && e.to.as_deref() == Some("seeding"))
        .unwrap()
        .seq;
    let api_start = ev
        .iter()
        .rev()
        .find(|e| {
            e.kind == EventKind::STEM_STATE
                && e.stem.as_deref() == Some("api")
                && e.to.as_deref() == Some("setup")
        })
        .unwrap()
        .seq;
    assert!(seeding < api_start, "condition: seeded waits for the seed");
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn setup_failure_fails_the_stem_with_setup_failed() {
    let mut rt = FakeRuntime::default();
    rt.script_exit.insert("db:setup".into(), 7);
    let r = rig_host(host_doc(SCRIPTED), rt, FakeWaiter::default());
    let res = r
        .sup
        .up(
            UpParams {
                stems: vec!["db".into()],
                ..UpParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].error.code, ErrorCode::SetupFailed);
    assert_eq!(res.failed[0].error.details["exit"], 7);
    assert_eq!(r.sup.cell("db").state(), StemState::Failed);
    assert!(!r.rt.starts.lock().unwrap().contains(&"db".to_string()));
    // No stamp was recorded: the next up runs setup again.
    let st = r
        .sup
        .stamps(stems_api::StampsParams::default(), "t")
        .unwrap();
    assert!(st.stamps.is_empty());
}

#[tokio::test]
async fn seed_failure_stops_the_process() {
    let mut rt = FakeRuntime::default();
    rt.script_exit.insert("db:seed".into(), 1);
    let r = rig_host(host_doc(SCRIPTED), rt, FakeWaiter::default());
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].stem, "db");
    assert_eq!(res.failed[0].error.code, ErrorCode::ScriptFailed);
    assert_eq!(r.sup.cell("db").state(), StemState::Failed);
    assert!(res.skipped.contains(&"api".to_string()));
    assert!(
        r.rt.actions
            .lock()
            .unwrap()
            .contains(&"stop db".to_string())
    );
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn bootstrap_failure_starts_nothing() {
    let mut rt = FakeRuntime::default();
    rt.script_exit.insert("ws:bootstrap".into(), 2);
    let r = rig_host(host_doc(SCRIPTED), rt, FakeWaiter::default());
    let e = r.sup.up(UpParams::default(), "cli:t").await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SetupFailed);
    assert!(r.rt.starts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn custom_stop_script_replaces_sigterm_and_kills_after_grace() {
    let doc = r#"schema_version: 1
name: t
stems:
  svc:
    type: process
    command: run
    stop_grace: 200ms
    scripts:
      stop: echo please-stop
"#;
    let r = rig_host(host_doc(doc), FakeRuntime::default(), FakeWaiter::default());
    assert!(r.sup.up(UpParams::default(), "cli:t").await.unwrap().ok);
    take_actions(&r);
    let t0 = std::time::Instant::now();
    let res = r
        .sup
        .stop(
            StopParams {
                stems: vec!["svc".into()],
                ..StopParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.stopped, ["svc"]);
    // The fake process ignores the stop script: after the grace it is killed.
    assert!(t0.elapsed() >= Duration::from_millis(200));
    insta::assert_snapshot!(take_actions(&r), @r"
    script svc:stop
    stop svc
    ");
    assert_eq!(r.sup.cell("svc").state(), StemState::Stopped);
}

#[tokio::test]
async fn down_during_setup_kills_the_script_at_once() {
    let mut rt = FakeRuntime::default();
    rt.script_hang.insert("db:setup".into());
    let r = rig_host(host_doc(SCRIPTED), rt, FakeWaiter::default());
    let sup = r.sup.clone();
    let up = tokio::spawn(async move {
        sup.up(
            UpParams {
                stems: vec!["db".into()],
                ..UpParams::default()
            },
            "cli:t",
        )
        .await
    });
    let mut ph = r.sup.cell("db").watch();
    ph.wait_for(|p| p.state == StemState::Setup).await.unwrap();
    let t0 = std::time::Instant::now();
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
    assert!(t0.elapsed() < Duration::from_secs(2));
    let res = up.await.unwrap().unwrap();
    assert_eq!(res.skipped, ["db"]);
    let cell = r.sup.cell("db");
    let mut ph = cell.watch();
    ph.wait_for(|p| p.state == StemState::Stopped)
        .await
        .unwrap();
    assert!(!r.rt.starts.lock().unwrap().contains(&"db".to_string()));
    let ev = r.events.replay(0);
    assert!(ev.iter().any(|e| e.kind == EventKind::SCRIPT_FINISHED
        && e.data["script"] == "setup"
        && e.data["cancelled"] == true));
}

// --- health probes (21) ------------------------------------------------------

/// A prober answering from a script, then `ok` forever.
struct ScriptedProber(Mutex<std::collections::VecDeque<bool>>);

#[async_trait::async_trait]
impl probes::Prober for ScriptedProber {
    async fn probe(&self) -> probes::ProbeResult {
        let t0 = std::time::Instant::now();
        let ok = self.0.lock().unwrap().pop_front().unwrap_or(true);
        if ok {
            probes::ProbeResult::pass(t0, "scripted ok")
        } else {
            probes::ProbeResult::fail(t0, "scripted failure")
        }
    }
}

/// Readiness and health from the probe loop, with scripted probers.
#[derive(Default)]
struct ScriptedWaiter(Mutex<HashMap<String, Vec<bool>>>);

#[async_trait::async_trait]
impl Waiter for ScriptedWaiter {
    async fn wait_condition(&self, _t: &WaitTarget, _c: Condition) -> Result<(), Error> {
        Err(Error::internal("probes() is true: never called"))
    }
    fn probes(&self) -> bool {
        true
    }
    fn prober(&self, t: &WaitTarget) -> Option<Arc<dyn probes::Prober>> {
        let seq = self.0.lock().unwrap().remove(&t.stem).unwrap_or_default();
        Some(Arc::new(ScriptedProber(Mutex::new(seq.into()))))
    }
}

fn probe_rig(
    yaml: &str,
    scripts: &[(&str, Vec<bool>)],
) -> (Arc<Supervisor>, Arc<EventBus>, Arc<FakeRuntime>) {
    let events = Arc::new(EventBus::default());
    let rt = Arc::new(FakeRuntime::default());
    let mut reg = RuntimeRegistry::default();
    reg.register(StemType::Process, rt.clone());
    let w = ScriptedWaiter(Mutex::new(
        scripts
            .iter()
            .map(|(s, v)| ((*s).to_string(), v.clone()))
            .collect(),
    ));
    let sup = Supervisor::new(events.clone(), host(yaml), reg, Arc::new(w));
    (sup, events, rt)
}

async fn until_state(sup: &Supervisor, stem: &str, want: StemState) -> stems_api::StatusResult {
    let t0 = std::time::Instant::now();
    loop {
        let st = sup.status(&StatusParams::default(), "t").unwrap();
        if st.stems.iter().any(|s| s.name == stem && s.state == want) {
            return st;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "{stem} never {want}: {st:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn probes_drive_health_transitions_and_degraded_dependants() {
    let yaml = "  db: { type: process, command: run, health: { type: tcp, port: 1, interval: 20ms, retries: 2 } }\n  api: { type: process, command: run, depends_on: [db], health: { type: tcp, port: 2, interval: 20ms, retries: 2 } }\n";
    // db: fails once while starting, passes, then fails long enough to be
    // seen unhealthy, then passes forever.
    let mut db = vec![false, true, true, true, false, false];
    db.extend(std::iter::repeat_n(false, 30));
    let (sup, events, _rt) = probe_rig(yaml, &[("db", db)]);
    let res = sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert!(res.ok, "{res:?}");
    let st = until_state(&sup, "db", StemState::Unhealthy).await;
    let db_st = st.stems.iter().find(|s| s.name == "db").unwrap();
    assert_eq!(db_st.reason.as_deref(), Some("scripted failure"));
    assert!(db_st.health.as_ref().unwrap().consecutive_failures >= 2);
    let api = st.stems.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.state, StemState::Healthy);
    assert!(api.degraded);
    assert_eq!(api.glyph, stems_core::Glyph::Degraded);
    assert_eq!(api.reason.as_deref(), Some("dependency db unhealthy"));
    assert_eq!(st.summary.degraded, 1);
    let st = until_state(&sup, "db", StemState::Healthy).await;
    let api = st.stems.iter().find(|s| s.name == "api").unwrap();
    assert!(!api.degraded);
    assert_eq!(api.reason, None);
    let db_st = st.stems.iter().find(|s| s.name == "db").unwrap();
    assert_eq!(db_st.reason, None, "a plain healthy stem has no reason");
    // One stem.health event per transition, none per probe.
    let health: Vec<(String, String)> = events
        .replay(0)
        .iter()
        .filter(|e| e.kind == EventKind::STEM_HEALTH && e.stem.as_deref() == Some("db"))
        .map(|e| {
            (
                e.from.clone().unwrap_or_default(),
                e.to.clone().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        health,
        [
            ("healthy".to_string(), "unhealthy".to_string()),
            ("unhealthy".to_string(), "healthy".to_string())
        ]
    );
    assert_eq!(
        states(&events.replay(0), "db"),
        ["starting", "healthy", "unhealthy", "healthy"]
    );
    let h = sup
        .health(&stems_api::HealthParams::default(), "t")
        .unwrap();
    let dbh = h.stems.iter().find(|s| s.name == "db").unwrap();
    assert_eq!(dbh.kind.as_deref(), Some("tcp"));
    assert_eq!(dbh.results.len(), 10);
    assert!(dbh.results.iter().all(|r| r.latency_ms < 1000));
    sup.down(DownParams::default(), "t").await.unwrap();
}

#[tokio::test]
async fn never_healthy_is_start_timeout_and_the_process_is_stopped() {
    let yaml = "  slow: { type: process, command: run, health: { type: tcp, port: 1, interval: 20ms, start_timeout: 200ms } }\n";
    let (sup, _events, rt) = probe_rig(yaml, &[("slow", vec![false; 1000])]);
    let res = sup.up(UpParams::default(), "cli:test").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].error.code, ErrorCode::StartTimeout);
    assert!(
        res.failed[0].error.message.contains("scripted failure"),
        "{:?}",
        res.failed[0].error
    );
    let st = sup.status(&StatusParams::default(), "t").unwrap();
    assert_eq!(st.stems[0].state, StemState::Failed);
    assert_eq!(st.stems[0].pid, None);
    assert!(
        rt.actions
            .lock()
            .unwrap()
            .contains(&"stop slow".to_string())
    );
}

// ------------------------------------------------------------------ outputs (26)

const WITH_OUTPUTS: &str = "  api:
    type: process
    command: run
    ports: [18181]
    outputs:
      URL: \"http://localhost:${stem.self.port}\"
      TOKEN: { command: \"echo tok\", secret: true }
  web:
    type: process
    command: run
    depends_on: [api]
    env:
      API_URL: \"${stem.api.outputs.URL}/v1\"
";

#[tokio::test]
async fn dependants_start_after_outputs_and_see_them() {
    let mut w = FakeWaiter::default();
    w.delay.insert("api".into(), 60);
    let r = rig(WITH_OUTPUTS, FakeRuntime::default(), w);
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");

    // The output command ran before `api` became healthy and before `web`
    // was started.
    let actions = r.rt.actions.lock().unwrap().clone();
    let pos = |a: &str| {
        actions
            .iter()
            .position(|x| x == a)
            .unwrap_or_else(|| panic!("{a}: {actions:?}"))
    };
    assert!(pos("script api:outputs") < pos("start web"), "{actions:?}");
    let ev = r.events.replay(0);
    let outputs_seq = ev
        .iter()
        .find(|e| e.kind == EventKind::STEM_OUTPUTS)
        .expect("stem.outputs event")
        .seq;
    assert!(outputs_seq < seq_of(&ev, "api", "healthy"));
    assert!(outputs_seq < seq_of(&ev, "web", "starting"));
    let data = &ev
        .iter()
        .find(|e| e.kind == EventKind::STEM_OUTPUTS)
        .unwrap()
        .data;
    assert_eq!(data["names"], serde_json::json!(["TOKEN", "URL"]));
    assert!(!data.to_string().contains("tok"), "never values: {data}");

    // web's env: the rendered reference and STEMS_API_OUTPUT_*; secrets are
    // redacted in status.
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
    let web = st.stems.iter().find(|s| s.name == "web").unwrap();
    let env = web.env.as_ref().unwrap();
    assert_eq!(env["API_URL"], "http://localhost:18181/v1");
    assert_eq!(env["STEMS_API_OUTPUT_URL"], "http://localhost:18181");
    // The fake runtime's scripts print nothing: TOKEN is "", still redacted.
    assert_eq!(env["STEMS_API_OUTPUT_TOKEN"], stems_api::REDACTED);
    let api = st.stems.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.outputs["URL"], "http://localhost:18181");
    assert_eq!(api.outputs["TOKEN"], stems_api::REDACTED);

    let o = r
        .sup
        .outputs(&stems_api::OutputsParams::default(), "t")
        .unwrap();
    assert_eq!(o.stems.len(), 1);
    assert_eq!(o.stems[0].outputs[0].name, "URL");
    assert_eq!(
        o.stems[0].outputs[1].value.as_deref(),
        Some(stems_api::REDACTED)
    );
    assert!(o.stems[0].outputs[1].secret);

    // Stopping drops them.
    r.sup
        .stop(
            StopParams {
                stems: vec!["api".into()],
                cascade: true,
                ..StopParams::default()
            },
            "t",
        )
        .await
        .unwrap();
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    assert!(st.stems[0].outputs.is_empty());
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn failing_output_command_fails_the_stem_and_skips_dependants() {
    let mut rt = FakeRuntime::default();
    rt.script_exit.insert("api:outputs".into(), 2);
    let r = rig(WITH_OUTPUTS, rt, FakeWaiter::default());
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].stem, "api");
    let e = &res.failed[0].error;
    assert_eq!(e.code, ErrorCode::ScriptFailed);
    assert_eq!(e.details["output"], "TOKEN");
    assert_eq!(e.details["tail"], serde_json::json!([]), "secret: no tail");
    assert!(res.skipped.contains(&"web".to_string()));
    assert_eq!(r.sup.cell("api").state(), StemState::Failed);
    assert!(
        r.rt.actions
            .lock()
            .unwrap()
            .contains(&"stop api".to_string())
    );
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn output_reference_without_value_is_unresolved_at_start() {
    let yaml = "  api: { type: process, command: run }
  web:
    type: process
    command: run
    depends_on: [api]
    env: { X: \"${stem.api.outputs.NOPE}\" }
";
    let r = rig(yaml, FakeRuntime::default(), FakeWaiter::default());
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].stem, "web");
    let e = &res.failed[0].error;
    assert_eq!(e.code, ErrorCode::UnresolvedVariable);
    assert!(
        e.hint
            .as_deref()
            .unwrap_or_default()
            .starts_with("outputs are available only from dependencies with condition: healthy"),
        "{e:?}"
    );
    assert!(!r.rt.starts.lock().unwrap().contains(&"web".to_string()));
    r.sup.stop_all("t", "test").await;
}

// --- restart policies (22) -------------------------------------------------

mod restart;

// --- watchdogs (24) ----------------------------------------------------------

mod watchdogs;

// --- config reload (33) -----------------------------------------------------

mod reload;

// --- cascading restarts (FR-LC-9) --------------------------------------------

mod cascade;
