//! Compose runtime tests: all pure, no Docker needed (the live round trip
//! is `tests/compose_live.rs`, ignored by default).

use std::ffi::OsStr;

use bollard::models::ContainerInspectResponse;
use serde_json::{Value, json};

use super::*;

fn redis() -> ComposeSpec {
    let mut s = ComposeSpec::new(
        "hello-shop",
        "redis",
        "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P",
        "/work/hello-shop/compose/redis.yml",
        "redis",
    );
    s.stop_grace = Duration::from_millis(1500);
    s
}

fn env_dir() -> PathBuf {
    PathBuf::from("/home/dev/.stems/hello-shop/compose")
}

fn target() -> ComposeTarget {
    ComposeTarget::for_spec(&redis(), &env_dir())
}

fn show(args: Vec<OsString>) -> String {
    display_command(OsStr::new("docker"), &args)
}

// --- command goldens ---------------------------------------------------------

#[test]
fn golden_up() {
    insta::assert_snapshot!("cmd_up", show(ComposeCommand::up(&target())));
}

#[test]
fn golden_stop() {
    insta::assert_snapshot!(
        "cmd_stop",
        show(ComposeCommand::stop(&target(), redis().stop_grace))
    );
}

#[test]
fn golden_rm() {
    insta::assert_snapshot!("cmd_rm", show(ComposeCommand::rm(&target())));
}

#[test]
fn golden_ps() {
    insta::assert_snapshot!("cmd_ps", show(ComposeCommand::ps(&target())));
}

#[test]
fn golden_project_ps() {
    insta::assert_snapshot!(
        "cmd_project_ps",
        show(ComposeCommand::ps(&ComposeTarget::project(
            "stems-hello-shop"
        )))
    );
}

#[test]
fn golden_version() {
    insta::assert_snapshot!("cmd_version", show(ComposeCommand::version()));
}

#[test]
fn golden_config() {
    insta::assert_snapshot!("cmd_config", show(ComposeCommand::config(&target())));
}

#[test]
fn paths_with_spaces_are_quoted() {
    let mut s = redis();
    s.file = "/work/my shop/compose/redis.yml".into();
    let t = ComposeTarget::for_spec(&s, &env_dir());
    assert!(show(ComposeCommand::up(&t)).contains("-f '/work/my shop/compose/redis.yml'"));
}

#[test]
fn project_name_defaults_to_stems_ws() {
    assert_eq!(redis().project_name, "stems-hello-shop");
    assert_eq!(default_project_name("My Shop"), "stems-my-shop");
}

#[test]
fn stop_grace_rounds_up() {
    let args = ComposeCommand::stop(&target(), Duration::from_millis(200));
    let s = show(args);
    assert!(s.contains("stop -t 1 redis"), "{s}");
}

// --- env file ----------------------------------------------------------------

#[test]
fn env_file_quoting() {
    let env: BTreeMap<String, String> = [
        ("REDIS_ARGS", "--save '' --appendonly no"),
        ("PLAIN", "abc"),
        ("DOLLAR", "p@$$w0rd #1"),
        ("EMPTY", ""),
        ("MULTI", "a\nb \"c\" \\ $HOME"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    insta::assert_snapshot!("env_file", render_env_file(&env).unwrap());
}

#[test]
fn env_file_rejects_bad_keys() {
    for bad in ["1X", "A-B", "", "A B"] {
        let env = BTreeMap::from([(bad.to_string(), "v".to_string())]);
        assert!(render_env_file(&env).is_err(), "{bad:?}");
    }
}

#[test]
fn env_file_is_written_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = env_file_path(&dir.path().join("compose"), "redis");
    let env = BTreeMap::from([("REDIS_ARGS".to_string(), "--maxmemory 64mb".to_string())]);
    write_env_file(&path, &env).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("REDIS_ARGS='--maxmemory 64mb'\n"));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Rewriting replaces the file.
    write_env_file(&path, &BTreeMap::new()).unwrap();
    assert!(
        !std::fs::read_to_string(&path)
            .unwrap()
            .contains("REDIS_ARGS")
    );
}

#[test]
fn compose_env_exports_stem_env_and_docker_host() {
    let env = BTreeMap::from([("REDIS_ARGS".to_string(), "x".to_string())]);
    let out = compose_env(&env, Some("unix:///var/run/docker.sock"));
    assert_eq!(out["REDIS_ARGS"], "x");
    assert_eq!(out["DOCKER_HOST"], "unix:///var/run/docker.sock");
    let own = BTreeMap::from([("DOCKER_HOST".to_string(), "tcp://h:2375".to_string())]);
    assert_eq!(
        compose_env(&own, Some("unix:///x"))["DOCKER_HOST"],
        "tcp://h:2375"
    );
}

// --- ps parsing ----------------------------------------------------------------

const PS_NDJSON: &str = include_str!("fixtures/ps_ndjson.txt");
const PS_ARRAY: &str = include_str!("fixtures/ps_array.json");

#[test]
fn ps_ndjson() {
    let v = parse_ps_json(PS_NDJSON);
    assert_eq!(v.len(), 2);
    assert_eq!(
        v[0],
        PsEntry {
            id: "4f3c2b1a0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b".into(),
            name: "stems-hello-shop-redis-1".into(),
            service: "redis".into(),
            state: "running".into(),
            project: "stems-hello-shop".into(),
            health: Some("healthy".into()),
            image: Some("redis:7".into()),
        }
    );
    assert_eq!(v[1].state, "exited");
    assert_eq!(v[1].health, None);
}

#[test]
fn ps_array() {
    let v = parse_ps_json(PS_ARRAY);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].service, "redis");
    assert_eq!(v[1].service, "worker");
    assert!(v[0].is_running());
}

#[test]
fn ps_empty_forms() {
    assert!(parse_ps_json("").is_empty());
    assert!(parse_ps_json("  \n").is_empty());
    assert!(parse_ps_json("[]").is_empty());
    // Warnings interleaved with NDJSON are skipped.
    let noisy = "WARN[0000] the attribute `version` is obsolete\n{\"ID\":\"abc\",\"Service\":\"redis\",\"State\":\"running\"}\n";
    let v = parse_ps_json(noisy);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].id, "abc");
}

#[test]
fn pick_prefers_running_container_of_the_service() {
    let v = parse_ps_json(PS_NDJSON);
    let e = pick_container(&v, "stems-hello-shop", "redis").unwrap();
    assert!(e.is_running());
    assert!(pick_container(&v, "stems-hello-shop", "nope").is_none());
    assert!(pick_container(&v, "other", "redis").is_none());
}

// --- version gating ------------------------------------------------------------

#[test]
fn version_parsing() {
    let v = |s: &str| parse_version(s).map(|v| v.to_string());
    assert_eq!(v(r#"{"version":"v2.24.5"}"#).as_deref(), Some("2.24.5"));
    assert_eq!(v(r#"{"version":"2.24.5"}"#).as_deref(), Some("2.24.5"));
    assert_eq!(
        v(r#"{"version":"2.29.1-desktop.1"}"#).as_deref(),
        Some("2.29.1-desktop.1")
    );
    assert_eq!(
        v("Docker Compose version v2.24.5").as_deref(),
        Some("2.24.5")
    );
    assert_eq!(
        v("docker-compose version 1.29.2, build 5becea4c").as_deref(),
        Some("1.29.2")
    );
    assert_eq!(v("Docker Compose version v2.3").as_deref(), Some("2.3.0"));
    assert_eq!(v("garbage"), None);
    assert_eq!(v(""), None);
}

#[test]
fn version_gate_requires_v2() {
    assert_eq!(
        require_v2(r#"{"version":"2.24.5"}"#).unwrap(),
        Version::new(2, 24, 5)
    );
    assert!(require_v2(r#"{"version":"v5.0.0"}"#).is_ok());
    for bad in ["docker-compose version 1.29.2, build 5becea4c", "nonsense"] {
        match require_v2(bad) {
            Err(RuntimeError::ComposeUnavailable { hint }) => {
                assert!(hint.contains("Compose v2"), "{hint}")
            }
            other => panic!("{bad}: {other:?}"),
        }
    }
}

// --- project in use ------------------------------------------------------------

fn entry(project: &str, service: &str, id: &str) -> PsEntry {
    PsEntry {
        id: id.into(),
        name: format!("{project}-{service}-1"),
        service: service.into(),
        state: "running".into(),
        project: project.into(),
        health: None,
        image: None,
    }
}

#[test]
fn project_in_use_table() {
    use ProjectDecision::*;
    let some = vec![entry("hello-shop", "redis", "c1")];
    let none: Vec<PsEntry> = vec![];
    // (entries, is_default, marker, adopt) -> decision
    let table = [
        (&none, false, false, false, Free),
        (&none, false, false, true, Free),
        (&some, false, true, false, Owned),
        (&some, false, true, true, Owned),
        (&some, false, false, false, InUse),
        (&some, false, false, true, Adopt),
        (&some, true, false, false, Owned),
    ];
    for (entries, is_default, marker, adopt, want) in table {
        let got = project_decision(entries, "hello-shop", is_default, marker, adopt);
        assert_eq!(
            got, want,
            "default={is_default} marker={marker} adopt={adopt}"
        );
        assert_eq!(got.allows_start(), want != InUse);
    }
    // Containers of another project do not count.
    let other = vec![entry("elsewhere", "redis", "c2")];
    assert_eq!(
        project_decision(&other, "hello-shop", false, false, false),
        Free
    );
}

#[test]
fn project_in_use_error_names_the_project() {
    let e = RuntimeError::ComposeProjectInUse {
        project: "hello-shop".into(),
    };
    let msg = e.to_string();
    assert!(
        msg.contains("hello-shop") && msg.contains("adopt: true"),
        "{msg}"
    );
}

// --- orphans ---------------------------------------------------------------------

#[test]
fn orphan_selection_skips_known_containers() {
    let entries = vec![
        entry("stems-hello-shop", "redis", "aaaa1111"),
        entry("stems-hello-shop", "cache", "bbbb2222"),
        PsEntry {
            id: String::new(),
            ..entry("stems-hello-shop", "ghost", "")
        },
    ];
    let scope = OrphanScope {
        workspace: "hello-shop".into(),
        known: vec![AdoptRecord {
            pid: 0,
            pgid: 0,
            start_time: crate::os::StartTime(0),
            // A short id prefix still matches.
            container_id: Some("aaaa".into()),
        }],
    };
    let orphans = select_compose_orphans(&entries, &scope);
    assert_eq!(orphans.len(), 1);
    let o = &orphans[0];
    assert_eq!(o.container_id.as_deref(), Some("bbbb2222"));
    assert_eq!(o.kind, crate::runtime::OrphanKind::Container);
    assert!(o.matches_start_command);
    assert!(
        o.command.contains("stems-hello-shop/cache"),
        "{}",
        o.command
    );
}

#[test]
fn marked_projects_are_listed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(marker_path(dir.path(), "hello-shop"), "x").unwrap();
    std::fs::write(dir.path().join("redis.env"), "x").unwrap();
    assert_eq!(marked_projects(dir.path()), vec!["hello-shop".to_string()]);
    assert!(marked_projects(&dir.path().join("missing")).is_empty());
}

// --- adoption --------------------------------------------------------------------

fn inspect(project: &str, service: &str, running: bool) -> ContainerInspectResponse {
    let v: Value = json!({
        "Id": "4f3c2b1a0e9d",
        "Name": format!("/{project}-{service}-1"),
        "State": { "Running": running, "ExitCode": 0 },
        "Config": { "Labels": {
            "com.docker.compose.project": project,
            "com.docker.compose.service": service,
        }}
    });
    serde_json::from_value(v).unwrap()
}

fn record(id: Option<&str>) -> AdoptRecord {
    AdoptRecord {
        pid: 0,
        pgid: 0,
        start_time: crate::os::StartTime(0),
        container_id: id.map(str::to_string),
    }
}

#[test]
fn adoption_checks_compose_labels() {
    use ComposeAdoptRejection::*;
    let ok = inspect("stems-hello-shop", "redis", true);
    let r = record(Some("4f3c2b1a0e9d"));
    assert_eq!(
        verify_compose_adoption(&ok, &r, "stems-hello-shop", Some("redis")),
        Ok(())
    );
    assert_eq!(
        verify_compose_adoption(&ok, &r, "stems-hello-shop", None),
        Ok(())
    );
    assert_eq!(
        verify_compose_adoption(&ok, &record(None), "stems-hello-shop", None),
        Err(NoContainerId)
    );
    assert!(matches!(
        verify_compose_adoption(&ok, &record(Some("ffff")), "stems-hello-shop", None),
        Err(IdMismatch { .. })
    ));
    assert_eq!(
        verify_compose_adoption(&ok, &r, "other", None),
        Err(WrongProject {
            found: Some("stems-hello-shop".into())
        })
    );
    assert_eq!(
        verify_compose_adoption(&ok, &r, "stems-hello-shop", Some("cache")),
        Err(WrongService {
            found: Some("redis".into())
        })
    );
    let stopped = inspect("stems-hello-shop", "redis", false);
    assert_eq!(
        verify_compose_adoption(&stopped, &r, "stems-hello-shop", Some("redis")),
        Err(NotRunning)
    );
}

// --- failures --------------------------------------------------------------------

#[test]
fn failure_keeps_last_20_lines() {
    let stderr: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    match failure_error("docker compose up".into(), Some(1), "", &stderr) {
        RuntimeError::ComposeFailed {
            command,
            exit,
            tail,
        } => {
            assert_eq!(command, "docker compose up");
            assert_eq!(exit, Some(1));
            assert_eq!(tail.len(), COMPOSE_TAIL_LINES);
            assert_eq!(tail[0], "line 11");
            assert_eq!(tail[19], "line 30");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn failure_maps_engine_down_and_missing_plugin() {
    let down = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";
    assert!(matches!(
        failure_error("x".into(), Some(1), "", down),
        RuntimeError::DockerUnavailable { .. }
    ));
    let missing = "docker: 'compose' is not a docker command.\nSee 'docker --help'";
    assert!(matches!(
        failure_error("x".into(), Some(1), "", missing),
        RuntimeError::ComposeUnavailable { .. }
    ));
    let bad_image = "Error response from daemon: pull access denied for nope/nope";
    let e = failure_error("x".into(), Some(18), "", bad_image);
    assert!(matches!(e, RuntimeError::ComposeFailed { .. }));
    assert!(e.to_string().contains("pull access denied"), "{e}");
}

#[test]
fn start_spec_round_trips_through_serde() {
    let spec = StartSpec::Compose(Box::new(redis()));
    let v = serde_json::to_value(&spec).unwrap();
    assert_eq!(v["kind"], "compose");
    let back: StartSpec = serde_json::from_value(v).unwrap();
    assert_eq!(back, spec);
}

// --- live ------------------------------------------------------------------------

/// End to end against a real Docker daemon with the hello-shop redis
/// compose file: up (with env overrides), describe/logs, project-in-use
/// refusal from a second stems home, adoption opt-in, stop, rm. Run with
/// `STEMS_TEST_DOCKER=1 cargo test -p stems-runtime compose_live_roundtrip -- --ignored`.
#[tokio::test]
#[ignore = "needs a Docker daemon; run with STEMS_TEST_DOCKER=1 and --ignored"]
async fn compose_live_roundtrip() {
    use crate::docker::DockerOptions;
    if std::env::var("STEMS_TEST_DOCKER").as_deref() != Ok("1") {
        eprintln!("skipped: set STEMS_TEST_DOCKER=1");
        return;
    }
    let ws = format!("stems-live-{}", std::process::id());
    let docker = Arc::new(
        DockerRuntime::connect(DockerOptions {
            workspace: Some(ws.clone()),
            ..Default::default()
        })
        .await
        .expect("docker"),
    );
    let home = tempfile::tempdir().unwrap();
    let rt = ComposeRuntime::new(
        docker.clone(),
        ComposeOptions::new(&ws, home.path().join(&ws).join("compose")),
    );
    let version = rt.ensure_version().await.expect("compose v2");
    assert!(version.major >= 2);

    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/workspaces/hello-shop/compose/redis.yml")
        .canonicalize()
        .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut spec = ComposeSpec::new(&ws, "redis", "live", &file, "redis");
    spec.project_name = format!("live-{}", std::process::id());
    spec.stop_grace = Duration::from_secs(2);
    spec.env.insert("REDIS_PORT".into(), port.to_string());
    spec.env
        .insert("REDIS_ARGS".into(), "--maxmemory 64mb".into());

    let h = rt
        .start(&StartSpec::Compose(Box::new(spec.clone())))
        .await
        .expect("compose up");
    let id = h.container_id().unwrap().to_string();
    assert!(rt.is_alive(&h).await);
    let facts = rt.describe(&h).await.unwrap();
    assert!(facts.ports.contains(&port), "{:?}", facts.ports);
    let inspect = docker.inspect(&id).await.unwrap().unwrap();
    let cmd = inspect.config.unwrap().cmd.unwrap_or_default().join(" ");
    assert!(cmd.contains("--maxmemory 64mb"), "{cmd}");
    assert!(marker_path(&rt.options().env_dir, &spec.project_name).exists());

    // A second stems home has no ownership marker: refused, then adopted on opt-in.
    let other_home = tempfile::tempdir().unwrap();
    let rt2 = ComposeRuntime::new(docker.clone(), ComposeOptions::new(&ws, other_home.path()));
    match rt2.start(&StartSpec::Compose(Box::new(spec.clone()))).await {
        Err(RuntimeError::ComposeProjectInUse { project }) => {
            assert_eq!(project, spec.project_name)
        }
        other => panic!("expected COMPOSE_PROJECT_IN_USE, got {other:?}"),
    }
    let mut adopting = spec.clone();
    adopting.adopt = true;
    let h2 = rt2
        .start(&StartSpec::Compose(Box::new(adopting)))
        .await
        .expect("adopt");
    assert_eq!(h2.container_id(), Some(id.as_str()));
    rt2.release(&h2);

    // Crash-recovery adoption by record.
    let rec = AdoptRecord {
        pid: 0,
        pgid: 0,
        start_time: crate::os::StartTime(0),
        container_id: Some(id.clone()),
    };
    let h3 = rt.adopt_service(&rec, &spec).await.expect("adopt_service");
    rt.release(&h3);

    let outcome = rt.stop(&h, spec.stop_grace).await.unwrap();
    assert_ne!(outcome, StopOutcome::AlreadyDead);
    assert!(!rt.is_alive(&h).await);
    rt.remove(&h).await.unwrap();
    assert!(rt.project_ps(&spec.project_name).await.unwrap().is_empty());
    // Leave nothing behind: compose's default network.
    let _ = std::process::Command::new("docker")
        .args(["compose", "-p", &spec.project_name, "down"])
        .status();
}
