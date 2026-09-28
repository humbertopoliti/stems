//! Docker runtime tests. Everything except `docker_live_roundtrip` runs
//! without a Docker daemon.

use std::path::Path;

use serde_json::{Value, json};

use super::*;

/// Recursively sort object keys (bollard uses `HashMap`s) so goldens are stable.
fn canonical(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let sorted: std::collections::BTreeMap<String, Value> =
                m.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(canonical).collect()),
        other => other,
    }
}

fn golden(spec: &ContainerSpec) -> String {
    let (config, host, net) = to_bollard(spec);
    let v = json!({ "config": config, "host_config": host, "networking_config": net });
    serde_json::to_string_pretty(&canonical(v)).unwrap()
}

fn postgres() -> ContainerSpec {
    let mut s = ContainerSpec::new("hello-shop", "postgres", "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P");
    s.image = Some("postgres:16".into());
    s.ports = vec![PortMapping {
        host: 15432,
        container: 5432,
        proto: PortProto::Tcp,
    }];
    s.volumes = vec![
        VolumeMount::parse("pgdata:/var/lib/postgresql/data", Path::new("/ws")).unwrap(),
        VolumeMount::parse(
            "./init:/docker-entrypoint-initdb.d:ro",
            Path::new("/ws/hello-shop"),
        )
        .unwrap(),
    ];
    s.env = [
        ("POSTGRES_PASSWORD", "dev"),
        ("POSTGRES_DB", "shop"),
        ("POSTGRES_USER", "shop"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    s.labels.insert("team".into(), "payments".into());
    // stems' own labels win over user labels.
    s.labels.insert(LABEL_STEM.into(), "spoofed".into());
    s.healthcheck = Some(HealthcheckSpec {
        test: Some(vec!["CMD-SHELL".into(), "pg_isready -U shop".into()]),
        interval: Some(Duration::from_millis(500)),
        timeout: Some(Duration::from_secs(2)),
        retries: Some(20),
        start_period: None,
    });
    s.stop_grace = Duration::from_millis(1500);
    s
}

fn built() -> ContainerSpec {
    let mut s = ContainerSpec::new("hello-shop", "shop-api", "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P");
    s.build = Some(BuildSpec {
        context: "/repos/shop-api".into(),
        dockerfile: "/repos/shop-api/Dockerfile".into(),
    });
    s.ports = vec![
        PortMapping {
            host: 18080,
            container: 8080,
            proto: PortProto::Tcp,
        },
        PortMapping {
            host: 15353,
            container: 5353,
            proto: PortProto::Udp,
        },
    ];
    s.command = Some(vec!["serve".into(), "--port".into(), "8080".into()]);
    s.entrypoint = Some(vec!["/app/shop-api".into()]);
    s.network = Some("shared_net".into());
    s.env.insert(
        "DATABASE_URL".into(),
        "postgres://shop@postgres:5432/shop".into(),
    );
    s.stop_grace = Duration::from_secs(2);
    s
}

#[test]
fn to_bollard_postgres_golden() {
    insta::assert_snapshot!("to_bollard_postgres", golden(&postgres()));
}

#[test]
fn to_bollard_build_golden() {
    insta::assert_snapshot!("to_bollard_build", golden(&built()));
}

#[test]
fn create_body_embeds_host_and_network_config() {
    let body = create_body(&postgres());
    let host = body.host_config.unwrap();
    assert_eq!(host.auto_remove, Some(false));
    assert_eq!(host.network_mode.as_deref(), Some("hello-shop_net"));
    assert!(
        body.networking_config
            .unwrap()
            .endpoints_config
            .unwrap()
            .contains_key("hello-shop_net")
    );
}

#[test]
fn naming_and_prefixing() {
    let s = postgres();
    assert_eq!(s.container_name(), "hello-shop-postgres");
    assert_eq!(s.network_name(), "hello-shop_net");
    assert_eq!(s.volume_name("pgdata"), "hello-shop_pgdata");
    assert_eq!(
        s.volume_name("hello-shop_pgdata"),
        "hello-shop_pgdata",
        "not doubled"
    );
    assert_eq!(
        s.volume_name("hello-shop-pgdata"),
        "hello-shop_pgdata",
        "a `<ws>-` prefix is normalised too"
    );
    assert_eq!(s.volume_name("hello-shopper"), "hello-shop_hello-shopper");
    assert_eq!(s.volume_name("hello-shop-"), "hello-shop_hello-shop-");
    assert_eq!(s.build_tag(), None);
    assert_eq!(s.image_ref().as_deref(), Some("postgres:16"));

    let b = built();
    assert_eq!(b.network_name(), "shared_net");
    assert_eq!(
        b.build_tag().as_deref(),
        Some("stems/hello-shop/shop-api:01J8ZQ7Y6M5X4W3V2T1S0R9Q8P")
    );
    assert_eq!(b.image_ref(), b.build_tag());

    let mut upper = ContainerSpec::new("Shop", "API", "r1");
    upper.build = b.build.clone();
    assert_eq!(upper.build_tag().as_deref(), Some("stems/shop/api:r1"));
}

#[test]
fn volume_parsing() {
    let base = Path::new("/ws/app");
    assert_eq!(
        VolumeMount::parse("data:/data", base).unwrap(),
        VolumeMount::Named {
            name: "data".into(),
            target: "/data".into(),
            read_only: false
        }
    );
    assert_eq!(
        VolumeMount::parse("../seed:/seed:ro", base).unwrap(),
        VolumeMount::Bind {
            source: "/ws/seed".into(),
            target: "/seed".into(),
            read_only: true
        }
    );
    assert_eq!(
        VolumeMount::parse("/abs/x:/x:rw", base).unwrap(),
        VolumeMount::Bind {
            source: "/abs/x".into(),
            target: "/x".into(),
            read_only: false
        }
    );
    assert!(VolumeMount::parse("/only-one", base).is_err());
    assert!(VolumeMount::parse("data:relative", base).is_err());
    assert!(VolumeMount::parse("data:/d:zz", base).is_err());
}

#[test]
fn spec_hash_is_stable_and_ignores_run_id_and_grace() {
    let a = postgres();
    assert_eq!(a.spec_hash(), postgres().spec_hash());
    assert_eq!(a.spec_hash().len(), 32);

    let mut b = postgres();
    b.run_id = "another-run".into();
    b.stop_grace = Duration::from_secs(9);
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    b.progress = ProgressSink(Some(tx));
    assert_eq!(a.spec_hash(), b.spec_hash());

    let mut c = postgres();
    c.env.insert("POSTGRES_PASSWORD".into(), "changed".into());
    assert_ne!(a.spec_hash(), c.spec_hash());
    let mut d = postgres();
    d.ports[0].host = 15433;
    assert_ne!(a.spec_hash(), d.spec_hash());
    let mut e = postgres();
    e.image = Some("postgres:17".into());
    assert_ne!(a.spec_hash(), e.spec_hash());
    // An explicit default network is the same container.
    let mut f = postgres();
    f.network = Some("hello-shop_net".into());
    assert_eq!(a.spec_hash(), f.spec_hash());

    let labels = HashMap::from([(LABEL_SPEC_HASH.to_string(), a.spec_hash())]);
    assert!(!needs_recreate(Some(&labels), &b));
    assert!(needs_recreate(Some(&labels), &c));
    assert!(needs_recreate(None, &a));

    // The pull policy is not part of the container: changing it alone
    // restarts in place.
    let mut g = postgres();
    g.pull = PullPolicy::Always;
    assert_eq!(a.spec_hash(), g.spec_hash());
}

#[test]
fn a_newer_local_image_forces_a_recreate() {
    assert!(image_differs(Some("sha256:old"), Some("sha256:new")));
    assert!(!image_differs(Some("sha256:same"), Some("sha256:same")));
    assert!(!image_differs(None, Some("sha256:new")));
    assert!(!image_differs(Some("sha256:old"), None));
}

fn inspect(v: Value) -> ContainerInspectResponse {
    serde_json::from_value(v).expect("inspect json")
}

const ID: &str = "4f3c2b1a0e9d8c7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b";

fn fake_inspect(ws: &str, stem: &str, running: bool) -> ContainerInspectResponse {
    inspect(json!({
        "Id": ID,
        "Name": format!("/{ws}-{stem}"),
        "State": {
            "Status": if running { "running" } else { "exited" },
            "Running": running,
            "Pid": if running { 4242 } else { 0 },
            "ExitCode": if running { 0 } else { 137 },
            "Health": { "Status": "healthy", "FailingStreak": 0 }
        },
        "Config": {
            "Image": "postgres:16",
            "Labels": {
                "stems.workspace": ws,
                "stems.stem": stem,
                "stems.run_id": "01OLDRUN",
                "stems.spec_hash": "abc"
            }
        },
        "NetworkSettings": {
            "Ports": {
                "5432/tcp": [
                    { "HostIp": "0.0.0.0", "HostPort": "15432" },
                    { "HostIp": "::", "HostPort": "15432" }
                ],
                "9999/tcp": null
            }
        },
        "Mounts": [
            { "Type": "volume", "Name": format!("{ws}_pgdata"), "Destination": "/var/lib/postgresql/data" },
            { "Type": "volume", "Name": "users-own-volume", "Destination": "/other" },
            { "Type": "bind", "Source": "/ws/init", "Destination": "/docker-entrypoint-initdb.d" }
        ]
    }))
}

fn record(id: &str) -> AdoptRecord {
    AdoptRecord {
        pid: 0,
        pgid: 0,
        start_time: StartTime(0),
        container_id: Some(id.into()),
    }
}

#[test]
fn adoption_matches_labels_not_run_id() {
    let i = fake_inspect("hello-shop", "postgres", true);
    assert_eq!(
        verify_adoption(&i, &record(ID), "hello-shop", "postgres"),
        Ok(())
    );
    // Short ids (as `docker ps` prints them) match too.
    assert_eq!(
        verify_adoption(&i, &record(&ID[..12]), "hello-shop", "postgres"),
        Ok(())
    );
}

#[test]
fn adoption_rejects_wrong_workspace_stem_id_or_stopped() {
    let i = fake_inspect("other-ws", "postgres", true);
    assert_eq!(
        verify_adoption(&i, &record(ID), "hello-shop", "postgres"),
        Err(AdoptRejection::WrongWorkspace {
            found: Some("other-ws".into())
        })
    );
    let i = fake_inspect("hello-shop", "redis", true);
    assert!(matches!(
        verify_adoption(&i, &record(ID), "hello-shop", "postgres"),
        Err(AdoptRejection::WrongStem { .. })
    ));
    let i = fake_inspect("hello-shop", "postgres", false);
    assert_eq!(
        verify_adoption(&i, &record(ID), "hello-shop", "postgres"),
        Err(AdoptRejection::NotRunning)
    );
    let i = fake_inspect("hello-shop", "postgres", true);
    assert!(matches!(
        verify_adoption(&i, &record("deadbeef"), "hello-shop", "postgres"),
        Err(AdoptRejection::IdMismatch { .. })
    ));
    let mut no_id = record(ID);
    no_id.container_id = None;
    assert_eq!(
        verify_adoption(&i, &no_id, "hello-shop", "postgres"),
        Err(AdoptRejection::NoContainerId)
    );
    let unlabelled = inspect(json!({ "Id": ID, "State": { "Running": true } }));
    assert_eq!(
        verify_adoption(&unlabelled, &record(ID), "hello-shop", "postgres"),
        Err(AdoptRejection::WrongWorkspace { found: None })
    );
}

#[test]
fn stem_is_derived_from_the_container_name() {
    assert_eq!(
        stem_from_name("/hello-shop-postgres", "hello-shop"),
        Some("postgres")
    );
    assert_eq!(
        stem_from_name("hello-shop-shop-api", "hello-shop"),
        Some("shop-api")
    );
    assert_eq!(stem_from_name("/other-postgres", "hello-shop"), None);
    assert_eq!(stem_from_name("/hello-shop-", "hello-shop"), None);
}

#[test]
fn describe_facts_from_inspect() {
    let f = facts_from_inspect(&fake_inspect("hello-shop", "postgres", true), 3);
    assert_eq!(f.pid, 4242);
    assert_eq!(f.pgid, 0);
    assert_eq!(f.container_id.as_deref(), Some(ID));
    assert_eq!(f.ports, vec![15432]);
    assert_eq!(f.container_health.as_deref(), Some("healthy"));
    assert_eq!(f.dropped_lines, 3);
    let bare = facts_from_inspect(
        &inspect(json!({ "Id": "x", "State": { "Running": true } })),
        0,
    );
    assert_eq!(bare.container_health, None);
    assert!(bare.ports.is_empty());
}

#[test]
fn volumes_removed_only_with_workspace_prefix() {
    let i = fake_inspect("hello-shop", "postgres", false);
    assert_eq!(volumes_to_remove(&i), vec!["hello-shop_pgdata"]);
    let unlabelled = inspect(
        json!({ "Id": "x", "Mounts": [{ "Type": "volume", "Name": "hello-shop_pgdata" }] }),
    );
    assert!(volumes_to_remove(&unlabelled).is_empty());
}

#[test]
fn stop_classification() {
    assert_eq!(classify_stop(false, Some(0)), StopOutcome::Graceful);
    assert_eq!(classify_stop(false, Some(143)), StopOutcome::Graceful);
    assert_eq!(classify_stop(false, Some(137)), StopOutcome::Killed);
    assert_eq!(classify_stop(true, None), StopOutcome::Killed);
    assert_eq!(grace_secs(Duration::from_millis(0)), 0);
    assert_eq!(grace_secs(Duration::from_millis(500)), 1);
    assert_eq!(grace_secs(Duration::from_millis(2000)), 2);
    assert_eq!(grace_secs(Duration::from_millis(2001)), 3);
}

#[test]
fn orphans_are_labelled_containers_not_in_state() {
    let list: Vec<ContainerSummary> = serde_json::from_value(json!([
        { "Id": ID, "Names": ["/hello-shop-postgres"], "Image": "postgres:16", "State": "running",
          "Labels": { "stems.workspace": "hello-shop", "stems.stem": "postgres" } },
        { "Id": "aaa111", "Names": ["/hello-shop-redis"], "Image": "redis:7", "State": "exited",
          "Labels": { "stems.workspace": "hello-shop", "stems.stem": "redis" } },
        { "Id": "bbb222", "Names": ["/other-redis"], "Image": "redis:7", "State": "running",
          "Labels": { "stems.workspace": "other" } }
    ]))
    .unwrap();
    let scope = OrphanScope {
        workspace: "hello-shop".into(),
        known: vec![record(&ID[..12])],
    };
    let orphans = select_orphans(&list, &scope);
    assert_eq!(
        orphans,
        vec![ContainerOrphan {
            id: "aaa111".into(),
            name: "hello-shop-redis".into(),
            stem_label: Some("redis".into()),
            state: "exited".into(),
            image: Some("redis:7".into()),
        }]
    );
    let o = Orphan::from(orphans[0].clone());
    assert_eq!(o.kind, OrphanKind::Container);
    assert_eq!(o.container_id.as_deref(), Some("aaa111"));
    assert_eq!(o.stem.as_deref(), Some("redis"));
    assert!(o.matches_start_command);
    assert_eq!(o.command, "redis:7 (hello-shop-redis, exited)");
}

#[test]
fn pull_progress_is_coalesced_per_layer_status() {
    let feed = [
        (None, Some("Pulling from library/postgres")),
        (Some("a1"), Some("Pulling fs layer")),
        (Some("b2"), Some("Pulling fs layer")),
        (Some("a1"), Some("Downloading")),
        (Some("a1"), Some("Downloading")),
        (Some("a1"), Some("Downloading")),
        (Some("b2"), Some("Downloading")),
        (Some("a1"), Some("Download complete")),
        (Some("a1"), Some("Extracting")),
        (Some("a1"), Some("Extracting")),
        (Some("a1"), Some("Pull complete")),
        (Some("b2"), None),
        (Some("b2"), Some("  ")),
        (None, Some("Digest: sha256:abc")),
        (None, Some("Status: Downloaded newer image for postgres:16")),
    ];
    let mut c = PullCoalescer::new();
    let got: Vec<(String, String)> = feed
        .iter()
        .filter_map(|(l, s)| c.observe("postgres:16", *l, *s))
        .map(|p| {
            assert_eq!(p.image, "postgres:16");
            (p.layer, p.status)
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("", "Pulling from library/postgres"),
        ("a1", "Pulling fs layer"),
        ("b2", "Pulling fs layer"),
        ("a1", "Downloading"),
        ("b2", "Downloading"),
        ("a1", "Download complete"),
        ("a1", "Extracting"),
        ("a1", "Pull complete"),
        ("", "Digest: sha256:abc"),
        ("", "Status: Downloaded newer image for postgres:16"),
    ]
    .iter()
    .map(|(l, s)| (l.to_string(), s.to_string()))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn image_refs_split_for_pull() {
    let s = |r: &str| {
        let (a, b) = split_image_ref(r);
        (a, b)
    };
    assert_eq!(s("postgres:16"), ("postgres".into(), "16".into()));
    assert_eq!(s("postgres"), ("postgres".into(), "latest".into()));
    assert_eq!(
        s("localhost:5000/team/app"),
        ("localhost:5000/team/app".into(), "latest".into())
    );
    assert_eq!(
        s("localhost:5000/team/app:1.2"),
        ("localhost:5000/team/app".into(), "1.2".into())
    );
    assert_eq!(
        s("redis@sha256:0123"),
        ("redis".into(), "sha256:0123".into())
    );
}

#[test]
fn host_resolution_order() {
    let none = |_: &Path| false;
    assert_eq!(
        resolve_host(
            Some("tcp://1.2.3.4:2375"),
            Some("unix:///env.sock"),
            None,
            none
        ),
        "tcp://1.2.3.4:2375"
    );
    assert_eq!(
        resolve_host(None, Some("unix:///env.sock"), None, none),
        "unix:///env.sock"
    );
    assert_eq!(
        resolve_host(None, Some(" "), None, none),
        "unix:///var/run/docker.sock"
    );
    let home = Path::new("/Users/me");
    assert_eq!(
        resolve_host(None, None, Some(home), |p| p
            .ends_with(".colima/default/docker.sock")),
        "unix:///Users/me/.colima/default/docker.sock"
    );
    assert_eq!(
        resolve_host(None, None, Some(home), |_| true),
        "unix:///var/run/docker.sock"
    );
}

#[test]
fn log_lines_carry_docker_timestamps() {
    let l = parse_log_line(
        "2026-09-26T10:00:01.123456789Z LOG:  database system is ready to accept connections",
        OutputStreamKind::Err,
    );
    assert_eq!(l.stream, OutputStreamKind::Err);
    assert_eq!(
        l.text,
        "LOG:  database system is ready to accept connections"
    );
    let d = l.ts.duration_since(UNIX_EPOCH).unwrap();
    assert_eq!(d.as_secs(), 1_790_416_801);
    assert_eq!(d.subsec_nanos(), 123_456_789);

    let raw = parse_log_line("no timestamp here", OutputStreamKind::Out);
    assert_eq!(raw.text, "no timestamp here");
    let empty = parse_log_line("2026-09-26T10:00:01Z ", OutputStreamKind::Out);
    assert_eq!(empty.text, "");
}

#[test]
fn build_tail_keeps_last_lines() {
    let mut tail = Vec::new();
    for i in 0..30 {
        push_tail(&mut tail, &format!("step {i}\n"));
        push_tail(&mut tail, "   ");
    }
    assert_eq!(tail.len(), BUILD_TAIL_LINES);
    assert_eq!(tail[0], "step 10");
    assert_eq!(tail.last().unwrap(), "step 29");
    let e = RuntimeError::BuildFailed {
        tail: vec!["a".into(), "b".into()],
    };
    assert_eq!(e.to_string(), "image build failed:\na\nb");
}

#[test]
fn unavailability_classification() {
    assert!(is_unavailable(&DockerError::SocketNotFoundError(
        "/x".into()
    )));
    assert!(is_unavailable(&DockerError::RequestTimeoutError));
    assert!(is_unavailable(&DockerError::IOError {
        err: std::io::Error::from(std::io::ErrorKind::ConnectionRefused)
    }));
    let server = DockerError::DockerResponseServerError {
        status_code: 404,
        message: "pull access denied for stems-does-not-exist".into(),
    };
    assert!(!is_unavailable(&server));
    assert!(is_not_found(&server));
    assert_eq!(
        server_message(&server),
        "pull access denied for stems-does-not-exist"
    );
}

#[tokio::test]
async fn connect_to_missing_socket_is_docker_unavailable() {
    let err = DockerRuntime::connect(DockerOptions {
        host: Some("unix:///nonexistent/stems-test-docker.sock".into()),
        timeout: Duration::from_secs(2),
        workspace: None,
    })
    .await
    .unwrap_err();
    match err {
        RuntimeError::DockerUnavailable { hint } => {
            assert!(
                hint.contains("/nonexistent/stems-test-docker.sock"),
                "{hint}"
            );
            assert!(hint.contains("Docker Desktop"), "{hint}");
        }
        other => panic!("expected DockerUnavailable, got {other:?}"),
    }
    let err = DockerRuntime::connect(DockerOptions {
        host: Some("ssh://nope".into()),
        ..Default::default()
    })
    .await
    .unwrap_err();
    assert!(matches!(err, RuntimeError::DockerUnavailable { .. }));
}

#[test]
fn handles_and_specs_serialise() {
    let h = Handle::Container {
        id: HandleId(7),
        container_id: ID.into(),
        name: "hello-shop-postgres".into(),
    };
    assert_eq!((h.pid(), h.pgid(), h.start_time()), (0, 0, StartTime(0)));
    assert_eq!(h.container_id(), Some(ID));
    assert!(!h.is_adopted() && !h.is_external());
    let back: Handle = serde_json::from_str(&serde_json::to_string(&h).unwrap()).unwrap();
    assert_eq!(back, h);

    let spec = StartSpec::Docker(Box::new(postgres()));
    let back: StartSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
    assert_eq!(back, spec);
}

/// Live round trip against a real daemon: `STEMS_TEST_DOCKER=1 cargo test -p
/// stems-runtime docker_live_roundtrip -- --ignored`.
#[tokio::test]
#[ignore = "needs a Docker daemon; run with STEMS_TEST_DOCKER=1 and --ignored"]
async fn docker_live_roundtrip() {
    if std::env::var("STEMS_TEST_DOCKER").as_deref() != Ok("1") {
        eprintln!("skipped: set STEMS_TEST_DOCKER=1");
        return;
    }
    let ws = format!("stems-live-{}", std::process::id());
    let rt = DockerRuntime::connect(DockerOptions {
        workspace: Some(ws.clone()),
        ..Default::default()
    })
    .await
    .expect("docker available");
    let mut spec = ContainerSpec::new(&ws, "alpine", "01LIVE");
    spec.image = Some("alpine:3".into());
    spec.command = Some(vec![
        "sh".into(),
        "-c".into(),
        "echo hello-from-alpine; exec sleep 300".into(),
    ]);
    spec.volumes = vec![VolumeMount::parse("data:/data", Path::new("/")).unwrap()];
    spec.stop_grace = Duration::from_secs(1);

    let h = rt
        .start(&StartSpec::Docker(Box::new(spec.clone())))
        .await
        .unwrap();
    let mut out = rt.output_stream(&h).expect("output stream");
    assert!(rt.is_alive(&h).await);
    let line = tokio::time::timeout(Duration::from_secs(10), out.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(line.text, "hello-from-alpine");
    let facts = rt.describe(&h).await.unwrap();
    assert!(facts.pid > 0);
    assert_eq!(facts.container_id.as_deref(), h.container_id());

    // `system/df` reports the named volume's size (metrics `--disk`).
    let vol = format!("{ws}_data");
    let sizes = rt.volume_sizes(std::slice::from_ref(&vol)).await.unwrap();
    assert!(sizes.contains_key(&vol), "no size for {vol}: {sizes:?}");

    // Adoption and orphan scan see the same container.
    let rec = AdoptRecord {
        pid: 0,
        pgid: 0,
        start_time: StartTime(0),
        container_id: h.container_id().map(String::from),
    };
    let adopted = rt.adopt(&rec).await.expect("adopted");
    assert_eq!(adopted.container_id(), h.container_id());
    rt.release(&adopted);
    let orphans = rt
        .scan_orphans(&OrphanScope {
            workspace: ws.clone(),
            known: Vec::new(),
        })
        .await;
    assert_eq!(orphans.len(), 1);

    // `sleep` as pid 1 ignores SIGTERM: Docker kills it after `t`.
    let outcome = rt.stop(&h, spec.stop_grace).await.unwrap();
    assert_eq!(outcome, StopOutcome::Killed);
    assert!(!rt.is_alive(&h).await);
    assert_eq!(rt.wait(&h).await.unwrap().code, Some(137));

    // A fresh start replaces our stopped leftover (same name) and keeps the
    // network it is about to join (`stems stop`, then `stems start`).
    rt.release(&h);
    let h = rt
        .start(&StartSpec::Docker(Box::new(spec.clone())))
        .await
        .expect("start over a stopped leftover");
    assert!(rt.is_alive(&h).await);
    rt.stop(&h, spec.stop_grace).await.unwrap();
    rt.remove(&h, true).await.unwrap();
    assert!(
        rt.inspect(h.container_id().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    // Nothing left: the named volume and the now unused `<ws>_net`.
    assert!(!rt.remove_volume(&vol).await.unwrap(), "{vol} left behind");
    let net = rt
        .docker
        .inspect_network(&default_network(&ws), None::<InspectNetworkOptions>)
        .await;
    assert!(net.is_err(), "{} left behind", default_network(&ws));
}

#[test]
fn container_stats_follow_docker_formula() {
    // 2 online CPUs; the container used 25 % of the system delta → 50 %;
    // memory usage minus inactive_file (cgroup v2).
    let r: bollard::models::ContainerStatsResponse = serde_json::from_value(json!({
        "cpu_stats": {"cpu_usage": {"total_usage": 1_250}, "system_cpu_usage": 10_000, "online_cpus": 2},
        "precpu_stats": {"cpu_usage": {"total_usage": 1_000}, "system_cpu_usage": 9_000},
        "memory_stats": {"usage": 100_000_000u64, "stats": {"inactive_file": 20_000_000u64}},
        "pids_stats": {"current": 7}
    }))
    .unwrap();
    let s = container_stats(&r);
    assert_eq!(s.cpu_pct, 50.0);
    assert_eq!(s.mem_bytes, 80_000_000);
    assert_eq!(s.pids, 7);
    // cgroup v1 `cache`, percpu length as online CPUs, no precpu yet.
    let r: bollard::models::ContainerStatsResponse = serde_json::from_value(json!({
        "cpu_stats": {"cpu_usage": {"total_usage": 500, "percpu_usage": [1, 2, 3, 4]}, "system_cpu_usage": 1_000},
        "memory_stats": {"usage": 10_000u64, "stats": {"cache": 4_000u64}}
    }))
    .unwrap();
    let s = container_stats(&r);
    assert_eq!(s.cpu_pct, 200.0);
    assert_eq!(s.mem_bytes, 6_000);
    assert_eq!(s.pids, 1);
    assert_eq!(
        container_stats(&Default::default()),
        ContainerStats {
            cpu_pct: 0.0,
            mem_bytes: 0,
            pids: 1
        }
    );
}

#[test]
fn volume_sizes_from_df_items() {
    let items = vec![
        json!({"Name": "ws-pgdata", "UsageData": {"Size": 4096, "RefCount": 1}}),
        json!({"Name": "other", "UsageData": {"Size": 1}}),
        json!({"Name": "ws-unknown", "UsageData": {"Size": -1}}),
    ];
    let names = vec!["ws-pgdata".to_string(), "ws-unknown".to_string()];
    let m = volume_sizes_of(items, &names);
    assert_eq!(m.len(), 1);
    assert_eq!(m["ws-pgdata"], 4096);
}

/// Docker Engine 29 (API 1.53) only lists per-volume sizes in
/// `VolumeUsage.Items` for a verbose `system/df`; a `type` list cannot be
/// URL-encoded by bollard (the request fails before it is sent).
#[test]
fn df_asks_for_verbose_usage_without_a_type_list() {
    let o = df_volume_options();
    assert!(o.verbose);
    assert_eq!(o._type, None);
}

#[test]
fn removal_takes_anonymous_volumes() {
    let o = container_remove_options();
    assert!(o.force);
    assert!(o.v, "anonymous volumes (image VOLUMEs) must not leak");
}

#[test]
fn only_empty_stems_networks_of_the_workspace_are_removed() {
    let ours = HashMap::from([(LABEL_WORKSPACE.to_string(), "ws".to_string())]);
    let other = HashMap::from([(LABEL_WORKSPACE.to_string(), "other".to_string())]);
    let user = HashMap::from([("team".to_string(), "x".to_string())]);
    assert!(network_removable(Some(&ours), 0, "ws"));
    assert!(!network_removable(Some(&ours), 1, "ws"));
    assert!(!network_removable(Some(&other), 0, "ws"));
    assert!(!network_removable(Some(&user), 0, "ws"));
    assert!(!network_removable(None, 0, "ws"));
}

#[test]
fn network_gone_is_recognised() {
    let gone = RuntimeError::Container(
        "docker: failed to set up container networking: network ws_net not found".into(),
    );
    assert!(is_network_gone(&gone));
    assert!(!is_network_gone(&RuntimeError::Container(
        "No such image: x".into()
    )));
    assert!(!is_network_gone(&RuntimeError::Unsupported(
        "network not found".into()
    )));
}

#[test]
fn a_pull_changed_the_image_when_its_id_moved() {
    let o = |b: Option<&str>, a: Option<&str>| PullOutcome {
        before: b.map(Into::into),
        after: a.map(Into::into),
    };
    assert!(o(None, Some("sha256:a")).changed(), "new image");
    assert!(o(Some("sha256:a"), Some("sha256:b")).changed(), "moved tag");
    assert!(!o(Some("sha256:a"), Some("sha256:a")).changed());
}
