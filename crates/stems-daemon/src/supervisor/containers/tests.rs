//! Docker/compose wiring tests that need no Docker: config → spec goldens,
//! the error mapping table, progress events, status decoration and lazy
//! connection (a counting fake connector).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use stems_api::{DownParams, StatusParams, UpParams};
use stems_config::{Condition, Resolved, StemType};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::docker::{ImageProgress, PullProgress, to_bollard};
use stems_runtime::os::StartTime;
use stems_runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, OutputStream, Runtime, RuntimeError, RuntimeFacts,
    StartSpec, StopOutcome,
};

use super::*;
use crate::events::EventBus;
use crate::supervisor::env::{EnvInputs, build_env};
use crate::supervisor::ports::PortBook;
use crate::supervisor::{Host, RuntimeRegistry, Supervisor, WaitTarget, Waiter};

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn hello_shop() -> Resolved {
    let dir = repo_root().join("examples/workspaces/hello-shop");
    stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.clone()),
        cwd: dir,
        env: Default::default(),
        skip_local: true,
    })
    .unwrap()
}

/// The stem's env through env.rs (no daemon env, no pass-env), with the
/// machine-specific paths masked.
fn env_of(r: &Resolved, stem: &str) -> BTreeMap<String, String> {
    let ws = &r.workspace;
    let base = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
    let mut allocated = Vec::new();
    let env = build_env(
        ws,
        ws.stem(stem).unwrap(),
        &EnvInputs {
            base: &base,
            pass_env: &BTreeMap::new(),
            run_id: "RUN",
            ports: &PortBook::default(),
            outputs: None,
            strict_outputs: false,
        },
        &mut allocated,
    )
    .unwrap();
    assert!(
        !env.own.contains_key("PATH"),
        "the daemon env is not passed"
    );
    mask(env.own, &ws.root)
}

fn mask(mut env: BTreeMap<String, String>, root: &Path) -> BTreeMap<String, String> {
    let root = root.display().to_string();
    for v in env.values_mut() {
        *v = v.replace(&root, "<ws>");
    }
    env
}

#[test]
fn hello_shop_postgres_container_spec_golden() {
    let r = hello_shop();
    let ws = &r.workspace;
    let stem = ws.stem("postgres").unwrap();
    // Host ports come from the port book (the same one env.rs used).
    let spec = container_spec(
        ws,
        stem,
        "RUN",
        &env_of(&r, "postgres"),
        &[("pg".into(), 15432)],
    )
    .unwrap();
    assert_eq!(spec.container_name(), "hello-shop-postgres");
    let (body, host, _) = to_bollard(&spec);
    let golden = json!({
        "name": spec.container_name(),
        "image": spec.image,
        "ports": spec.ports,
        "env": spec.env,
        "binds": host.binds,
        // HashMap in bollard: sorted for a stable golden.
        "labels": body.labels.map(|l| l.into_iter().collect::<BTreeMap<_, _>>()),
        "stop_timeout": body.stop_timeout,
        "network": host.network_mode,
    });
    let mut golden = golden;
    golden["labels"]["stems.spec_hash"] = json!("[hash]");
    insta::assert_json_snapshot!("hello_shop_postgres", golden);
    // The named volume is not doubled: `hello-shop-pgdata` → `hello-shop_pgdata`.
    assert_eq!(
        host.binds.unwrap(),
        ["hello-shop_pgdata:/var/lib/postgresql/data"]
    );
    assert_eq!(named_volumes(ws, stem), ["hello-shop_pgdata"]);
    // PORT set by stems points at the container port inside the container.
    assert_eq!(spec.env["PORT"], "5432");
}

#[test]
fn hello_shop_redis_compose_spec_golden() {
    let r = hello_shop();
    let ws = &r.workspace;
    let spec = compose_spec(ws, ws.stem("redis").unwrap(), "RUN", &env_of(&r, "redis")).unwrap();
    assert!(spec.file.is_absolute());
    let mut v = serde_json::to_value(&spec).unwrap();
    v["file"] = json!(spec.file.strip_prefix(&ws.root).unwrap());
    insta::assert_json_snapshot!("hello_shop_redis", v);
    assert_eq!(
        spec.project_name, "hello-shop",
        "explicit project_name kept"
    );
    // Compose interpolates the host-side port (PORT is the host port).
    assert_eq!(spec.env["PORT"], "16379");
}

#[test]
fn compose_project_defaults_to_stems_ws() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("stems.yaml"),
        "schema_version: 1\nname: Shop\nstems:\n  cache: { type: compose, file: dc.yml, service: redis, adopt: true }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("dc.yml"), "services: {}\n").unwrap();
    let r = stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.path().to_path_buf()),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap();
    let ws = &r.workspace;
    let spec = compose_spec(ws, ws.stem("cache").unwrap(), "RUN", &BTreeMap::new()).unwrap();
    assert_eq!(spec.project_name, "stems-shop", "default, lower-cased");
    assert_eq!(spec.service, "redis");
    assert!(spec.adopt);
    assert_eq!(spec.file, ws.root.join("dc.yml"));
}

#[test]
fn docker_build_ports_labels_and_healthcheck() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("api")).unwrap();
    std::fs::write(
        dir.path().join("stems.yaml"),
        "schema_version: 1\nname: w\nstems:\n  api:\n    type: docker\n    build: { context: api }\n    ports: [{ name: http, port: 18080, container_port: 8080 }, { name: dbg, port: 9229 }]\n    volumes: [\"./data:/data:ro\", \"w_cache:/cache\"]\n    labels: { team: shop }\n    command: [python3, app.py]\n    healthcheck: { disable: true }\n    stop_grace: 1500ms\n",
    )
    .unwrap();
    let r = stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.path().to_path_buf()),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap();
    let ws = &r.workspace;
    let stem = ws.stem("api").unwrap();
    let env = BTreeMap::from([("PORT".to_string(), "18080".to_string())]);
    let spec = container_spec(ws, stem, "01RUN", &env, &[]).unwrap();
    let b = spec.build.as_ref().unwrap();
    assert_eq!(b.context, ws.root.join("api"));
    assert_eq!(b.dockerfile, ws.root.join("api/Dockerfile"));
    assert_eq!(spec.image_ref().unwrap(), "stems/w/api:01RUN");
    let ports: Vec<(u16, u16)> = spec.ports.iter().map(|p| (p.host, p.container)).collect();
    assert_eq!(ports, [(18080, 8080), (9229, 9229)]);
    assert_eq!(spec.env["PORT"], "8080");
    assert_eq!(spec.labels["team"], "shop");
    assert_eq!(spec.command.as_deref().unwrap(), ["python3", "app.py"]);
    assert_eq!(
        spec.healthcheck.as_ref().unwrap().test.as_deref().unwrap(),
        ["NONE"]
    );
    assert_eq!(spec.stop_grace, std::time::Duration::from_millis(1500));
    let (_, host, _) = to_bollard(&spec);
    assert_eq!(
        host.binds.unwrap(),
        [
            format!("{}:/data:ro", ws.root.join("data").display()),
            "w_cache:/cache".to_string()
        ]
    );
    assert_eq!(named_volumes(ws, stem), ["w_cache"]);
    // A user-set PORT that is not the host port is left alone.
    let env = BTreeMap::from([("PORT".to_string(), "3000".to_string())]);
    let spec = container_spec(ws, stem, "01RUN", &env, &[]).unwrap();
    assert_eq!(spec.env["PORT"], "3000");
}

#[test]
fn error_mapping_table() {
    let table: Vec<(RuntimeError, ErrorCode)> = vec![
        (
            RuntimeError::DockerUnavailable {
                hint: "start Docker Desktop (or Colima)".into(),
            },
            ErrorCode::DockerUnavailable,
        ),
        (
            RuntimeError::ComposeUnavailable { hint: "h".into() },
            ErrorCode::DockerUnavailable,
        ),
        (
            RuntimeError::ImagePullFailed {
                image: "stems-does-not-exist:1".into(),
                message: "pull access denied for stems-does-not-exist".into(),
            },
            ErrorCode::ImagePullFailed,
        ),
        (
            RuntimeError::BuildFailed {
                tail: vec!["Step 3/5".into(), "COPY failed".into()],
            },
            ErrorCode::SetupFailed,
        ),
        (
            RuntimeError::ComposeFailed {
                command: "docker compose up".into(),
                exit: Some(1),
                tail: vec!["manifest unknown".into()],
            },
            ErrorCode::ComposeFailed,
        ),
        (
            RuntimeError::ComposeProjectInUse {
                project: "stems-w".into(),
            },
            ErrorCode::ComposeProjectInUse,
        ),
        (
            RuntimeError::Container(
                "driver failed programming external connectivity: Bind for 0.0.0.0:15432 failed: port is already allocated".into(),
            ),
            ErrorCode::PortInUse,
        ),
        (
            RuntimeError::Container("conflict: name in use".into()),
            ErrorCode::StartFailed,
        ),
        (RuntimeError::Unsupported("x".into()), ErrorCode::StartFailed),
    ];
    for (e, code) in table {
        let shown = e.to_string();
        let err = runtime_error("db", e);
        assert_eq!(err.code, code, "{shown}");
        assert_eq!(err.details["stem"], "db", "{shown}");
        assert!(err.hint.is_some(), "{shown}");
        assert!(err.message.contains("`db`"), "{}", err.message);
    }
    let e = runtime_error(
        "db",
        RuntimeError::DockerUnavailable {
            hint: stems_runtime::docker::UNAVAILABLE_HINT.into(),
        },
    );
    assert_eq!(e.exit_code(), 1);
    assert!(e.hint.unwrap().contains("start Docker Desktop"));
    assert_eq!(e.details["stems"], json!(["db"]));
    let e = runtime_error(
        "db",
        RuntimeError::ImagePullFailed {
            image: "i".into(),
            message: "pull access denied".into(),
        },
    );
    assert_eq!(e.details["message"], "pull access denied");
    assert!(e.message.contains("pull access denied"));
    let e = runtime_error(
        "db",
        RuntimeError::BuildFailed {
            tail: vec!["COPY failed".into()],
        },
    );
    assert_eq!(e.details["tail"], json!(["COPY failed"]));
    let e = runtime_error(
        "db",
        RuntimeError::ComposeFailed {
            command: "c".into(),
            exit: None,
            tail: vec!["t".into()],
        },
    );
    assert_eq!(e.details["tail"], json!(["t"]));
    assert!(e.message.contains("exit none"), "{}", e.message);
}

#[test]
fn progress_events() {
    let bus = EventBus::default();
    bus.emit(progress_event(
        "db",
        ImageProgress::Pull(PullProgress {
            image: "postgres:16".into(),
            layer: "a1b2".into(),
            status: "Downloading".into(),
        }),
    ));
    bus.emit(progress_event(
        "api",
        ImageProgress::Build {
            stem: "api".into(),
            line: "Step 1/7 : FROM python:3-slim".into(),
        },
    ));
    let ev = bus.replay(0);
    assert_eq!(ev[0].kind, EventKind::DOCKER_PULL);
    assert_eq!(ev[0].kind.to_string(), "docker.pull");
    assert_eq!(ev[0].stem.as_deref(), Some("db"));
    assert_eq!(
        ev[0].data,
        json!({ "stem": "db", "image": "postgres:16", "layer": "a1b2", "status": "Downloading" })
    );
    assert_eq!(ev[1].kind.to_string(), "docker.build");
    assert_eq!(ev[1].data["line"], "Step 1/7 : FROM python:3-slim");
}

#[test]
fn compose_dir_is_next_to_the_state_file() {
    assert_eq!(
        compose_dir_for(Some(Path::new("/h/abc/state.json"))),
        Path::new("/h/abc/compose")
    );
    assert!(compose_dir_for(None).ends_with("stems-compose"));
}

// ---------------------------------------------------------------------------
// Lazy connection through the supervisor
// ---------------------------------------------------------------------------

/// A process runtime whose units run until stopped.
#[derive(Default)]
struct Procs {
    next: AtomicI32,
    started: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Runtime for Procs {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        if let StartSpec::Process(p) = spec {
            self.started
                .lock()
                .unwrap()
                .push(p.env.get("STEMS_STEM").cloned().unwrap_or_default());
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Handle::Process {
            id: HandleId(n as u64),
            pid: 900_000 + n,
            pgid: 900_000 + n,
            start_time: StartTime(1),
        })
    }
    async fn stop(
        &self,
        _h: &Handle,
        _g: std::time::Duration,
    ) -> Result<StopOutcome, RuntimeError> {
        Ok(StopOutcome::Graceful)
    }
    async fn is_alive(&self, _h: &Handle) -> bool {
        true
    }
    async fn describe(&self, _h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        Err(RuntimeError::Unsupported("fake".into()))
    }
    fn output_stream(&self, _h: &Handle) -> Option<OutputStream> {
        None
    }
    async fn wait(&self, _h: &Handle) -> Result<ExitStatus, RuntimeError> {
        std::future::pending().await
    }
    async fn adopt(&self, _r: &AdoptRecord) -> Option<Handle> {
        None
    }
    fn release(&self, _h: &Handle) {}
}

struct Ready;

#[async_trait::async_trait]
impl Waiter for Ready {
    async fn wait_condition(&self, _t: &WaitTarget, _c: Condition) -> Result<(), Error> {
        Ok(())
    }
}

struct OneWs {
    ws: Arc<Resolved>,
    dir: tempfile::TempDir,
}

impl Host for OneWs {
    fn resolved(&self) -> Option<Arc<Resolved>> {
        Some(self.ws.clone())
    }
    fn reload(&self, _actor: &str) -> Result<Arc<Resolved>, Error> {
        Ok(self.ws.clone())
    }
    fn request_shutdown(&self, _actor: &str, _reason: &str) {}
    fn started_by_up(&self) -> bool {
        false
    }
    fn set_started_by_up(&self) {}
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.dir.path().join("data"))
    }
}

/// A connector that counts calls and always fails like a stopped Docker.
fn unavailable(calls: Arc<AtomicUsize>) -> Connector {
    Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(RuntimeError::DockerUnavailable {
                hint: format!(
                    "cannot reach unix:///nonexistent; {}",
                    stems_runtime::docker::UNAVAILABLE_HINT
                ),
            })
        })
    })
}

fn mixed() -> (
    Arc<Supervisor>,
    Arc<Containers>,
    Arc<AtomicUsize>,
    Arc<Procs>,
) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dc.yml"), "services: {}\n").unwrap();
    std::fs::write(
        dir.path().join("stems.yaml"),
        "schema_version: 1\nname: mixed\nstems:\n  p: { type: process, command: run }\n  d: { type: docker, image: 'postgres:16', ports: [{ port: 47913, container_port: 5432 }], volumes: ['pgdata:/data'] }\n  c: { type: compose, file: dc.yml, service: redis }\n",
    )
    .unwrap();
    let ws = stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.path().to_path_buf()),
        cwd: dir.path().to_path_buf(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = Arc::new(Containers::new(
        unavailable(calls.clone()),
        dir.path().join("compose"),
    ));
    let procs = Arc::new(Procs::default());
    let mut reg = RuntimeRegistry::default();
    reg.register(StemType::Process, procs.clone());
    let reg = reg.with_containers(c.clone());
    let host = Arc::new(OneWs {
        ws: Arc::new(ws),
        dir,
    });
    let sup = Supervisor::new(Arc::new(EventBus::default()), host, reg, Arc::new(Ready));
    (sup, c, calls, procs)
}

fn up(stems: &[&str]) -> UpParams {
    UpParams {
        stems: stems.iter().map(|s| s.to_string()).collect(),
        detach: true,
        ..UpParams::default()
    }
}

#[tokio::test]
async fn process_only_selections_never_connect() {
    let (sup, c, calls, procs) = mixed();
    let res = sup.up(up(&["p"]), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(*procs.started.lock().unwrap(), ["p"]);
    let st = sup.status(&StatusParams::default(), "cli:t").unwrap();
    assert_eq!(st.stems.len(), 3);
    let down = sup.down(DownParams::default(), "cli:t").await.unwrap();
    assert_eq!(down.stopped, ["p"]);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "never connected");
    assert_eq!(c.connects(), 0);
    assert!(c.docker_if_connected().is_none());
}

#[tokio::test]
async fn a_docker_stem_connects_and_reports_docker_unavailable() {
    let (sup, c, calls, _) = mixed();
    let res = sup.up(up(&["d"]), "cli:t").await.unwrap();
    assert!(!res.ok);
    let f = &res.failed[0];
    assert_eq!(f.stem, "d");
    assert_eq!(f.error.code, ErrorCode::DockerUnavailable);
    assert_eq!(f.error.exit_code(), 1);
    assert!(
        f.error
            .hint
            .as_deref()
            .unwrap()
            .contains("start Docker Desktop")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let st = sup.status(&StatusParams::default(), "cli:t").unwrap();
    let d = st.stems.iter().find(|s| s.name == "d").unwrap();
    assert_eq!(d.state, StemState::Failed);
    assert_eq!(d.pid, None);
    // A failure is not cached: the next start tries again.
    let res = sup.up(up(&["c"]), "cli:t").await.unwrap();
    assert_eq!(res.failed[0].error.code, ErrorCode::DockerUnavailable);
    assert_eq!(c.connects(), 2);
}

#[tokio::test]
async fn down_volumes_needs_docker_only_for_docker_stems() {
    let (sup, c, _, _) = mixed();
    sup.up(up(&["p"]), "cli:t").await.unwrap();
    let res = sup
        .down(
            DownParams {
                stems: vec!["p".into()],
                volumes: true,
                ..DownParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(
        c.connects(),
        0,
        "a process-only down --volumes never connects"
    );
    let res = sup
        .down(
            DownParams {
                volumes: true,
                ..DownParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed[0].stem, "d");
    assert_eq!(res.failed[0].error.code, ErrorCode::DockerUnavailable);
    assert!(res.volumes_removed.is_empty());
    assert_eq!(c.connects(), 1);
}

#[test]
fn status_gets_container_health_and_no_pid() {
    let c = Containers::new(unavailable(Arc::new(AtomicUsize::new(0))), "/tmp/x");
    c.set_health("db", Some("healthy".into()));
    let mut st: StemStatus = serde_json::from_value(json!({
        "name": "db", "type": "docker", "state": "healthy", "glyph": "healthy",
        "reason": null, "pid": 0, "pgid": 0, "ports": [], "uptime_s": 1,
        "started_at": null, "restarts": 0,
        "health": { "type": "docker", "last": null, "consecutive_failures": 0, "transitions_60s": 0 },
        "error": null
    }))
    .unwrap();
    decorate_status(Some(&c), StemType::Docker, &mut st);
    assert_eq!(st.pid, None);
    assert_eq!(st.pgid, None);
    assert_eq!(
        st.health.as_ref().unwrap().container.as_deref(),
        Some("healthy")
    );
    let v = serde_json::to_value(&st).unwrap();
    assert_eq!(v["health"]["container"], "healthy");
    // No health block (no check configured): nothing to fill in.
    st.health = None;
    decorate_status(Some(&c), StemType::Compose, &mut st);
    assert!(st.health.is_none());
    // Process stems are left alone.
    let mut p = st.clone();
    p.name = "p".into();
    p.pid = Some(7);
    decorate_status(Some(&c), StemType::Process, &mut p);
    assert_eq!(p.pid, Some(7));
}
