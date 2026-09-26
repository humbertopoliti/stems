//! Watchdog actions through the supervisor with the fake runtime (24):
//! restart (bypass, not counted), rebuild (build then restart), script
//! (no restart), signal refusals, pause/resume/status RPCs, and one real
//! notify round trip (a file written in the workspace dir).

use super::*;
use crate::supervisor::watch::{WATCHDOG_ACTOR, run_action};
use stems_api::{WatchPauseParams, WatchStatusParams};
use stems_config::WatchAction;

/// One process stem `w` (no codebase: rules watch the workspace dir).
fn watch_yaml(rule: &str, scripts: &str) -> String {
    format!(
        "  w:\n    type: process\n    command: run\n    scripts: {{ {scripts} }}\n    watch:\n      - {rule}\n"
    )
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

fn status_w(r: &Rig) -> stems_api::StemStatus {
    r.sup.status(&StatusParams::default(), "t").unwrap().stems[0].clone()
}

#[tokio::test]
async fn restart_action_bypasses_the_policy() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], action: restart }", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    run_action(&r.sup, "w", &WatchAction::Restart, "watch: app.py changed")
        .await
        .unwrap();
    let st = status_w(&r);
    assert_eq!(st.state, StemState::Healthy);
    assert_ne!(st.pid, Some(pid));
    assert_eq!(st.restarts, 0, "a watchdog restart is not counted");
    let ev = kinds(&r, EventKind::STEM_RESTARTING);
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0].data["counted"], false);
    assert_eq!(ev[0].actor, WATCHDOG_ACTOR);
    assert_eq!(ev[0].data["reason"], "watch: app.py changed");
    assert_eq!(
        st.watch,
        Some(stems_api::WatchSummary {
            paused: false,
            rules: 1
        })
    );
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}

#[tokio::test]
async fn rebuild_runs_build_then_restarts() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], action: rebuild }", "build: make"),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    r.rt.actions.lock().unwrap().clear();
    run_action(&r.sup, "w", &WatchAction::Rebuild, "watch: x")
        .await
        .unwrap();
    let actions = r.rt.actions.lock().unwrap().clone();
    assert_eq!(
        actions,
        ["script w:build", "stop w", "start w"],
        "{actions:?}"
    );
    assert_ne!(status_w(&r).pid, Some(pid));
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}

#[tokio::test]
async fn a_failed_build_does_not_restart() {
    let mut rt = FakeRuntime::default();
    rt.script_exit.insert("w:build".into(), 2);
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], action: rebuild }", "build: make"),
        rt,
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    let e = run_action(&r.sup, "w", &WatchAction::Rebuild, "watch: x")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ScriptFailed, "{e:?}");
    assert_eq!(status_w(&r).pid, Some(pid));
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}

#[tokio::test]
async fn script_action_runs_without_a_restart() {
    let r = rig(
        &watch_yaml(
            "{ paths: ['*.py'], action: 'script:on-change' }",
            "on-change: echo changed",
        ),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    run_action(
        &r.sup,
        "w",
        &WatchAction::Script("on-change".into()),
        "watch: x",
    )
    .await
    .unwrap();
    assert!(
        r.rt.actions
            .lock()
            .unwrap()
            .contains(&"script w:on-change".to_string())
    );
    let fin: Vec<Event> = kinds(&r, EventKind::SCRIPT_FINISHED);
    assert_eq!(fin.last().map(|e| e.actor.as_str()), Some(WATCHDOG_ACTOR));
    assert_eq!(status_w(&r).pid, Some(pid));
    assert!(kinds(&r, EventKind::STEM_RESTARTING).is_empty());
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}

#[tokio::test]
async fn signal_needs_a_running_process_stem() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], action: 'signal:SIGHUP' }", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let e = run_action(&r.sup, "w", &WatchAction::Signal("HUP".into()), "x")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage, "{e:?}");
    let e = run_action(&r.sup, "w", &WatchAction::Signal("BOGUS".into()), "x")
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Usage, "{e:?}");
}

#[tokio::test]
async fn pause_resume_and_status() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], debounce: 50ms }", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    up_w(&r).await;
    let st = r
        .sup
        .watch_status(WatchStatusParams::default(), "t")
        .unwrap();
    assert_eq!(st.stems.len(), 1);
    assert!(st.stems[0].active);
    assert!(!st.stems[0].paused);
    assert_eq!(st.stems[0].rules[0].debounce_ms, 50);
    assert_eq!(st.stems[0].rules[0].action, "restart");
    assert!(
        st.stems[0].rules[0]
            .ignore
            .contains(&"**/node_modules/**".to_string())
    );

    let p = WatchPauseParams {
        stems: vec!["w".into()],
    };
    r.sup.watch_pause(p.clone(), true, "cli:t").unwrap();
    assert!(status_w(&r).watch.unwrap().paused);
    r.sup.watch_pause(p, false, "cli:t").unwrap();
    assert!(!status_w(&r).watch.unwrap().paused);

    let res = r
        .sup
        .watch_pause(WatchPauseParams::default(), true, "cli:t")
        .unwrap();
    assert!(res.global_paused);
    assert!(status_w(&r).watch.unwrap().paused);
    r.sup
        .watch_pause(WatchPauseParams::default(), false, "cli:t")
        .unwrap();
    assert!(!status_w(&r).watch.unwrap().paused);

    let paused: Vec<Event> = r
        .events
        .replay(0)
        .into_iter()
        .filter(|e| e.kind == EventKind::WATCH_PAUSED)
        .collect();
    assert_eq!(paused.len(), 2);
    let e = r
        .sup
        .watch_pause(
            WatchPauseParams {
                stems: vec!["nope".into()],
            },
            true,
            "cli:t",
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownStem);

    // Stopping the stem stops its watcher.
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
    let st = r
        .sup
        .watch_status(WatchStatusParams::default(), "t")
        .unwrap();
    assert!(!st.stems[0].active);
}

#[tokio::test]
async fn no_watch_starts_no_watcher() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'] }", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let res = r
        .sup
        .up(
            UpParams {
                no_watch: true,
                ..UpParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(res.ok);
    until_state(&r.sup, "w", StemState::Healthy).await;
    let st = r
        .sup
        .watch_status(WatchStatusParams::default(), "t")
        .unwrap();
    assert!(st.disabled);
    assert!(!st.stems[0].active);
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}

#[tokio::test]
async fn a_written_file_triggers_a_restart() {
    let r = rig(
        &watch_yaml("{ paths: ['*.py'], debounce: 100ms }", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let pid = up_w(&r).await;
    let root = r.host.ws.workspace.root.clone();
    // FSEvents may need a moment to start delivering: retry the write.
    let t0 = std::time::Instant::now();
    let mut n = 0;
    loop {
        if !kinds(&r, EventKind::WATCH_TRIGGERED).is_empty() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "no watch.triggered");
        if n % 10 == 0 {
            std::fs::write(root.join("app.py"), format!("# {n}\n")).unwrap();
            // An ignored write never triggers.
            std::fs::create_dir_all(root.join("node_modules")).unwrap();
            std::fs::write(root.join("node_modules/x.py"), "x").unwrap();
        }
        n += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ev = kinds(&r, EventKind::WATCH_TRIGGERED);
    assert_eq!(ev[0].data["paths"], serde_json::json!(["app.py"]));
    assert_eq!(ev[0].data["action"], "restart");
    assert_eq!(ev[0].data["rule_index"], 0);
    let t0 = std::time::Instant::now();
    while kinds(&r, EventKind::WATCH_ACTION_FINISHED).is_empty() {
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "action never finished"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        kinds(&r, EventKind::WATCH_ACTION_FINISHED)[0].data["ok"],
        true
    );
    let st = status_w(&r);
    assert_eq!(st.state, StemState::Healthy);
    assert_ne!(st.pid, Some(pid));
    r.sup.down(DownParams::default(), "cli:t").await.unwrap();
}
