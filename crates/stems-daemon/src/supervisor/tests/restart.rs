//! Restart policies through the actor (deliverable 22) with the fake
//! runtime: the decision table (policy × exit × intent), backoff delays,
//! `MAX_RESTARTS`, timer cancellation by a stop, hooks on a respawn, the
//! `restarts` degraded reason, `on_unhealthy` and the watchdog entry point.

use super::*;

/// One process stem `w` with `restart: {<restart>}`.
fn restart_yaml(restart: &str) -> String {
    format!("  w: {{ type: process, command: run, restart: {{ {restart} }} }}\n")
}

/// Make the live unit of `stem` exit with `code` (`None`: killed by SIGKILL).
fn exit_unit(rt: &FakeRuntime, stem: &str, code: Option<i32>) {
    let units = rt.units.lock().unwrap();
    let live: Vec<_> = units
        .iter()
        .filter(|(_, (name, tx))| name == stem && tx.borrow().is_none())
        .collect();
    assert_eq!(live.len(), 1, "{stem} has {} live units", live.len());
    live[0].1.1.send_replace(Some(ExitStatus {
        code,
        signal: code.is_none().then_some(9),
    }));
}

fn kinds(r: &Rig, kind: EventKind) -> Vec<Event> {
    r.events
        .replay(0)
        .into_iter()
        .filter(|e| e.kind == kind && e.stem.as_deref() == Some("w"))
        .collect()
}

async fn up_w(r: &Rig) -> i32 {
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    let st = until_state(&r.sup, "w", StemState::Healthy).await;
    st.stems[0].pid.unwrap()
}

/// Wait until `w` is healthy with a pid other than `old`.
async fn healthy_again(r: &Rig, old: i32) -> stems_api::StemStatus {
    let t0 = std::time::Instant::now();
    loop {
        let st = r.sup.status(&StatusParams::default(), "t").unwrap();
        let w = st.stems[0].clone();
        if w.state == StemState::Healthy && w.pid.is_some_and(|p| p != old) {
            return w;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "never restarted: {w:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Settle: give the actor time to act on anything queued.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(80)).await;
}

#[tokio::test]
async fn decision_table_policy_by_exit() {
    // (policy, exit code, expected end state, restarted?)
    let cases: &[(&str, Option<i32>, StemState, bool)] = &[
        ("never", Some(0), StemState::Stopped, false),
        ("never", Some(3), StemState::Failed, false),
        ("never", None, StemState::Failed, false),
        ("on-failure", Some(0), StemState::Stopped, false),
        ("on-failure", Some(3), StemState::Healthy, true),
        ("on-failure", None, StemState::Healthy, true),
        ("always", Some(0), StemState::Healthy, true),
        ("always", Some(3), StemState::Healthy, true),
    ];
    for (policy, code, want, restarted) in cases {
        let yaml = restart_yaml(&format!("policy: {policy}, backoff: {{ initial: 10ms }}"));
        let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
        let pid = up_w(&r).await;
        exit_unit(&r.rt, "w", *code);
        let case = format!("{policy} / {code:?}");
        let st = if *restarted {
            healthy_again(&r, pid).await
        } else {
            until_state(&r.sup, "w", *want).await.stems[0].clone()
        };
        assert_eq!(st.state, *want, "{case}");
        assert_eq!(st.restarts, u32::from(*restarted), "{case}");
        let restarting = kinds(&r, EventKind::STEM_RESTARTING);
        assert_eq!(restarting.len(), usize::from(*restarted), "{case}");
        if *restarted {
            let d = &restarting[0].data;
            assert_eq!(d["attempt"], 1, "{case}");
            assert_eq!(d["delay_ms"], 10, "{case}");
            assert_eq!(d["exit_code"], json!(code), "{case}");
            assert_eq!(d["reason"], "exit", "{case}");
            assert_eq!(
                states(&r.events.replay(0), "w"),
                ["starting", "healthy", "starting", "healthy"],
                "{case}"
            );
        }
        r.sup.stop_all("t", "test").await;
    }
}

#[tokio::test]
async fn user_stop_never_restarts() {
    for policy in ["on-failure", "always"] {
        let yaml = restart_yaml(&format!("policy: {policy}, backoff: {{ initial: 10ms }}"));
        let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
        up_w(&r).await;
        let res = r
            .sup
            .stop(
                StopParams {
                    stems: vec!["w".into()],
                    ..StopParams::default()
                },
                "cli:t",
            )
            .await
            .unwrap();
        assert_eq!(res.stopped, ["w"]);
        settle().await;
        assert_eq!(r.sup.cell("w").state(), StemState::Stopped, "{policy}");
        assert!(kinds(&r, EventKind::STEM_RESTARTING).is_empty(), "{policy}");
        assert_eq!(r.rt.starts.lock().unwrap().len(), 1, "{policy}");
    }
}

#[tokio::test]
async fn backoff_doubles_and_max_gives_up() {
    let yaml = restart_yaml("max: 2, backoff: { initial: 10ms, factor: 2 }");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    let mut pid = up_w(&r).await;
    for _ in 0..2 {
        exit_unit(&r.rt, "w", Some(3));
        pid = healthy_again(&r, pid).await.pid.unwrap();
    }
    let delays: Vec<Value> = kinds(&r, EventKind::STEM_RESTARTING)
        .iter()
        .map(|e| e.data["delay_ms"].clone())
        .collect();
    assert_eq!(delays, [json!(10), json!(20)]);
    exit_unit(&r.rt, "w", Some(3));
    let st = until_state(&r.sup, "w", StemState::Failed).await;
    let w = &st.stems[0];
    let e = w.error.as_ref().unwrap();
    assert_eq!(e.code, ErrorCode::MaxRestarts);
    assert_eq!(e.details["attempts"], 2);
    assert_eq!(e.details["max"], 2);
    assert_eq!(e.details["window_ms"], 600_000);
    assert_eq!(w.restarts, 2);
    assert_eq!(w.pid, None);
    assert_eq!(st.summary.failed, 1);
    let gave_up = kinds(&r, EventKind::STEM_GAVE_UP);
    assert_eq!(gave_up.len(), 1);
    assert_eq!(gave_up[0].data["exit_code"], 3);
    // A user start begins a new window; the lifetime count is kept.
    let pid = up_w(&r).await;
    exit_unit(&r.rt, "w", Some(3));
    let w = healthy_again(&r, pid).await;
    assert_eq!(w.restarts, 3);
    assert_eq!(w.restarts_in_window, 1);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn stop_cancels_the_backoff_timer() {
    let yaml = restart_yaml("backoff: { initial: 5s }");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    up_w(&r).await;
    exit_unit(&r.rt, "w", Some(3));
    let st = until_state(&r.sup, "w", StemState::Starting).await;
    assert_eq!(
        st.stems[0].reason.as_deref(),
        Some("restarting in 5s (attempt 1)")
    );
    let t0 = std::time::Instant::now();
    let res = r
        .sup
        .stop(
            StopParams {
                stems: vec!["w".into()],
                ..StopParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(res.stopped, ["w"]);
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "the stop waited for the timer"
    );
    assert_eq!(r.sup.cell("w").state(), StemState::Stopped);
    // The timer is gone: nothing respawns (would need 5 s; check the
    // token rather than sleeping).
    assert!(!r.sup.cell("w").info().restart.cancel_backoff());
    assert_eq!(r.rt.starts.lock().unwrap().len(), 1);
    assert_eq!(
        states(&r.events.replay(0), "w"),
        ["starting", "healthy", "starting", "stopping", "stopped"]
    );
}

#[tokio::test]
async fn shutdown_cancels_the_backoff_timer() {
    let yaml = restart_yaml("backoff: { initial: 5s }");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    up_w(&r).await;
    exit_unit(&r.rt, "w", Some(3));
    until_state(&r.sup, "w", StemState::Starting).await;
    r.sup.stop_all("t", "shutdown").await;
    assert_eq!(r.sup.cell("w").state(), StemState::Stopped);
    assert_eq!(r.rt.starts.lock().unwrap().len(), 1);
}

const HOOKED: &str = r#"schema_version: 1
name: t
stems:
  w:
    type: process
    command: run
    restart: { backoff: { initial: 10ms } }
    scripts:
      setup: echo setup
      pre_start: echo pre
      post_start: echo post
      seed: echo seed
      pre_stop: echo pre-stop
      post_stop: echo post-stop
"#;

#[tokio::test]
async fn respawn_runs_start_hooks_but_not_setup_or_seed() {
    let r = rig_host(
        host_doc(HOOKED),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    // Wait for the seed (after healthy) to finish.
    let t0 = std::time::Instant::now();
    while !r.sup.cell("w").info().seeded {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let first = take_actions(&r);
    assert!(first.contains("script w:setup") && first.contains("script w:seed"));
    exit_unit(&r.rt, "w", Some(3));
    healthy_again(&r, pid).await;
    let t0 = std::time::Instant::now();
    while r.sup.cell("w").info().pending {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    insta::assert_snapshot!(take_actions(&r), @r"
    script w:pre_start
    start w
    script w:post_start
    ");
    assert!(r.sup.cell("w").info().seeded, "seeded survives a respawn");
    // A user restart is a full start again (stamps are current: no setup).
    r.sup.stop_all("t", "test").await;
    let stops = take_actions(&r);
    assert!(stops.contains("script w:pre_stop") && stops.contains("script w:post_stop"));
}

#[tokio::test]
async fn three_restarts_in_the_window_degrade_until_it_passes() {
    let yaml = restart_yaml("window: 400ms, max: 10, backoff: { initial: 5ms, factor: 1 }");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    let mut pid = up_w(&r).await;
    for _ in 0..3 {
        exit_unit(&r.rt, "w", Some(1));
        pid = healthy_again(&r, pid).await.pid.unwrap();
    }
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    let w = &st.stems[0];
    assert_eq!(w.restarts_in_window, 3, "{w:?}");
    assert!(w.degraded, "{w:?}");
    assert_eq!(w.reason.as_deref(), Some("restarts (3 recently)"));
    assert_eq!(st.summary.degraded, 1);
    // The window passes: healthy, no reason.
    let t0 = std::time::Instant::now();
    loop {
        let st = r.sup.status(&StatusParams::default(), "t").unwrap();
        if !st.stems[0].degraded {
            assert_eq!(st.stems[0].reason, None);
            assert_eq!(st.stems[0].restarts, 3);
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn bypass_restarts_now_without_counting() {
    let yaml = restart_yaml("max: 1, backoff: { initial: 5s }");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    let pid = up_w(&r).await;
    let cell = r.sup.cell("w");
    cell.restart_bypassing_policy("watchdog:t", "watchdog: src/app.py changed")
        .await
        .unwrap();
    let w = healthy_again(&r, pid).await;
    assert_eq!((w.restarts, w.restarts_in_window), (0, 0));
    let ev = kinds(&r, EventKind::STEM_RESTARTING);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].data["counted"], false);
    assert_eq!(ev[0].data["delay_ms"], 0);
    assert_eq!(ev[0].data["reason"], "watchdog: src/app.py changed");
    assert!(take_actions(&r).contains("stop w"));
    r.sup.stop_all("t", "test").await;
    let e = cell
        .restart_bypassing_policy("watchdog:t", "x")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage);
}

#[tokio::test]
async fn unhealthy_past_the_grace_is_restarted() {
    let yaml = "  w: { type: process, command: run, health: { type: tcp, port: 1, interval: 20ms, retries: 1 }, restart: { on_unhealthy: true, unhealthy_grace: 100ms, backoff: { initial: 10ms } } }\n";
    // Healthy twice, then failing for good (this prober); the respawn
    // gets a fresh prober that always passes.
    let mut seq = vec![true, true];
    seq.extend(std::iter::repeat_n(false, 500));
    let (sup, events, rt) = probe_rig(yaml, &[("w", seq)]);
    assert!(sup.up(UpParams::default(), "cli:t").await.unwrap().ok);
    let pid = until_state(&sup, "w", StemState::Healthy).await.stems[0]
        .pid
        .unwrap();
    until_state(&sup, "w", StemState::Unhealthy).await;
    let t0 = std::time::Instant::now();
    let w = loop {
        let st = sup.status(&StatusParams::default(), "t").unwrap();
        let w = st.stems[0].clone();
        if w.state == StemState::Healthy && w.pid != Some(pid) {
            break w;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "{w:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    assert_eq!(w.restarts, 1);
    let ev: Vec<_> = events
        .replay(0)
        .into_iter()
        .filter(|e| e.kind == EventKind::STEM_RESTARTING)
        .collect();
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].data["reason"], "unhealthy");
    assert!(rt.actions.lock().unwrap().contains(&"stop w".to_string()));
    let states = states(&events.replay(0), "w");
    assert_eq!(
        states,
        ["starting", "healthy", "unhealthy", "starting", "healthy"]
    );
    sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn unhealthy_that_recovers_within_the_grace_is_left_alone() {
    let yaml = "  w: { type: process, command: run, health: { type: tcp, port: 1, interval: 20ms, retries: 1 }, restart: { on_unhealthy: true, unhealthy_grace: 2s } }\n";
    let (sup, events, _rt) = probe_rig(yaml, &[("w", vec![true, false, false, true])]);
    assert!(sup.up(UpParams::default(), "cli:t").await.unwrap().ok);
    until_state(&sup, "w", StemState::Unhealthy).await;
    until_state(&sup, "w", StemState::Healthy).await;
    // The grace timer was cancelled when the stem left `unhealthy`.
    assert!(sup.cell("w").info().restart.unhealthy_timer_cancelled());
    assert!(
        !events
            .replay(0)
            .iter()
            .any(|e| e.kind == EventKind::STEM_RESTARTING)
    );
    sup.stop_all("t", "test").await;
}
