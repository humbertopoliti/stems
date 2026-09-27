//! Cascading restarts (FR-LC-9) with the fake runtime: the diamond is
//! restarted once in dependency order, flag/config precedence, the abort
//! path, policy-triggered cascades, the four loop guards and queueing.

use stems_api::{RestartParams, UpResult};

use super::*;

/// a ← b, a ← c, b/c ← d; `a_restart` goes into a's `restart:` map.
fn diamond(a_restart: &str, b_restart: &str) -> String {
    format!(
        "  a: {{ type: process, command: run, restart: {{ {a_restart} }} }}\n  b: {{ type: process, command: run, depends_on: [a], restart: {{ {b_restart} }} }}\n{}{}",
        proc("c", "a"),
        proc("d", "b, c"),
    )
}

fn slow(stem: &str, ms: u64) -> FakeWaiter {
    FakeWaiter {
        delay: [(stem.to_string(), ms)].into_iter().collect(),
        ..FakeWaiter::default()
    }
}

fn of_kind(r: &Rig, kind: &EventKind) -> Vec<Event> {
    r.events
        .replay(0)
        .into_iter()
        .filter(|e| &e.kind == kind)
        .collect()
}

fn restarting(r: &Rig, stem: &str) -> Vec<Event> {
    of_kind(r, &EventKind::STEM_RESTARTING)
        .into_iter()
        .filter(|e| e.stem.as_deref() == Some(stem))
        .collect()
}

fn pids(r: &Rig) -> HashMap<String, Option<i32>> {
    r.sup
        .status(&StatusParams::default(), "t")
        .unwrap()
        .stems
        .into_iter()
        .map(|s| (s.name, s.pid))
        .collect()
}

async fn up_all(r: &Rig) -> HashMap<String, Option<i32>> {
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    for s in ["a", "b", "c", "d"] {
        until_state(&r.sup, s, StemState::Healthy).await;
    }
    pids(r)
}

async fn restart(r: &Rig, stems: &[&str], cascade: Option<bool>) -> UpResult {
    r.sup
        .restart(
            RestartParams {
                stems: stems.iter().map(|s| s.to_string()).collect(),
                cascade,
                ..RestartParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap()
}

/// Make the live unit of `stem` exit with code 3.
fn crash(rt: &FakeRuntime, stem: &str) {
    let units = rt.units.lock().unwrap();
    let live: Vec<_> = units
        .values()
        .filter(|(name, tx)| name == stem && tx.borrow().is_none())
        .collect();
    assert_eq!(live.len(), 1, "{stem} has {} live units", live.len());
    live[0].1.send_replace(Some(ExitStatus {
        code: Some(3),
        signal: None,
    }));
}

/// Poll until at least `n` events of `kind` exist.
async fn until_events(r: &Rig, kind: &EventKind, n: usize) -> Vec<Event> {
    let t0 = std::time::Instant::now();
    loop {
        let ev = of_kind(r, kind);
        if ev.len() >= n {
            return ev;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "fewer than {n} {kind} events: {ev:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn layers(v: &[&[&str]]) -> Vec<Vec<String>> {
    v.iter()
        .map(|l| l.iter().map(|x| x.to_string()).collect())
        .collect()
}

/// The `n`-th (0-based) `stem.state → to` event of `stem`.
fn nth_state(r: &Rig, stem: &str, to: &str, n: usize) -> u64 {
    r.events
        .replay(0)
        .into_iter()
        .filter(|e| {
            e.kind == EventKind::STEM_STATE
                && e.stem.as_deref() == Some(stem)
                && e.to.as_deref() == Some(to)
        })
        .nth(n)
        .unwrap_or_else(|| panic!("no {stem} -> {to} #{n}"))
        .seq
}

#[tokio::test]
async fn restart_cascade_restarts_the_diamond_once_in_order() {
    let r = rig(
        &diamond("", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let before = up_all(&r).await;
    let res = restart(&r, &["a"], Some(true)).await;
    assert!(res.ok, "{res:?}");
    let c = res.cascade.expect("a cascade report");
    assert_eq!(c.restarted, layers(&[&["b", "c"], &["d"]]));
    assert!(c.failed.is_empty() && c.skipped.is_empty() && !c.aborted);
    assert_eq!(c.origin, "a");

    // Every pid changed; d restarted exactly once (guard #2 / the diamond).
    let after = pids(&r);
    for s in ["a", "b", "c", "d"] {
        assert_ne!(before[s], after[s], "{s} kept its pid");
    }
    for s in ["b", "c", "d"] {
        let ev = restarting(&r, s);
        assert_eq!(ev.len(), 1, "{s}: {ev:?}");
        assert_eq!(ev[0].data["cascade"], json!(c.id));
        assert_eq!(ev[0].data["counted"], json!(false));
        assert_eq!(ev[0].actor, "cli:t");
    }
    assert!(restarting(&r, "a").is_empty());

    // Order: a healthy again, then b and c, then d once both are healthy.
    let a_back = nth_state(&r, "a", "healthy", 1);
    let b_restart = restarting(&r, "b")[0].seq;
    let c_restart = restarting(&r, "c")[0].seq;
    let d_restart = restarting(&r, "d")[0].seq;
    assert!(a_back < b_restart && a_back < c_restart);
    assert!(nth_state(&r, "b", "healthy", 1) < d_restart);
    assert!(nth_state(&r, "c", "healthy", 1) < d_restart);

    let started = of_kind(&r, &EventKind::CASCADE_STARTED);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].data["stems"], json!([["b", "c"], ["d"]]));
    assert_eq!(started[0].data["reason"], json!("restart"));
    let finished = of_kind(&r, &EventKind::CASCADE_FINISHED);
    assert_eq!(finished[0].data["restarted"], json!(["b", "c", "d"]));
    assert!(started[0].seq < a_back && d_restart < finished[0].seq);

    // Bypass: no policy restart counted; nobody is in a cascade any more.
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    for s in &st.stems {
        assert_eq!(s.restarts, 0, "{}", s.name);
        assert!(s.cascade.is_none(), "{}", s.name);
    }
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn without_the_flag_only_the_stem_restarts() {
    let r = rig(
        &diamond("", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let before = up_all(&r).await;
    let res = restart(&r, &["a"], None).await;
    assert!(res.ok && res.cascade.is_none(), "{res:?}");
    let after = pids(&r);
    assert_ne!(before["a"], after["a"]);
    for s in ["b", "c", "d"] {
        assert_eq!(before[s], after[s], "{s} was restarted");
    }
    assert!(of_kind(&r, &EventKind::CASCADE_STARTED).is_empty());
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn config_default_cascades_and_the_flag_overrides_it() {
    let r = rig(
        &diamond("cascade: true", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let before = up_all(&r).await;
    let res = restart(&r, &["a"], Some(false)).await;
    assert!(res.cascade.is_none());
    assert_eq!(before["d"], pids(&r)["d"]);
    let res = restart(&r, &["a"], None).await;
    assert_eq!(
        res.cascade.unwrap().restarted,
        layers(&[&["b", "c"], &["d"]])
    );
    assert_ne!(before["d"], pids(&r)["d"]);
    // A dependant restarted with its origin is not restarted twice.
    let before = pids(&r);
    let res = restart(&r, &["a", "b"], Some(true)).await;
    assert_eq!(res.cascade.unwrap().restarted, layers(&[&["c"], &["d"]]));
    assert_ne!(before["b"], pids(&r)["b"]);
    assert_eq!(restarting(&r, "b").len(), 1, "b restarted by the cascade");
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn an_origin_that_fails_aborts_the_cascade() {
    let r = rig(
        &diamond("", ""),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let before = up_all(&r).await;
    r.rt.fail_spawn_now.lock().unwrap().insert("a".into());
    let res = restart(&r, &["a"], Some(true)).await;
    assert!(!res.ok);
    let c = res.cascade.expect("report");
    assert!(c.aborted && c.restarted.is_empty());
    let ab = of_kind(&r, &EventKind::CASCADE_ABORTED);
    assert_eq!(ab.len(), 1);
    assert_eq!(ab[0].stem.as_deref(), Some("a"));
    assert_eq!(ab[0].data["error"]["code"], json!("START_FAILED"));
    assert!(of_kind(&r, &EventKind::CASCADE_FINISHED).is_empty());
    let after = pids(&r);
    for s in ["b", "c", "d"] {
        assert_eq!(before[s], after[s], "{s} was touched");
        assert!(restarting(&r, s).is_empty());
    }
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn members_are_marked_and_their_watch_triggers_suppressed_while_it_runs() {
    let yaml = format!("{}{}", diamond("", ""), proc("x", ""));
    let r = rig(&yaml, FakeRuntime::default(), slow("d", 300));
    up_all(&r).await;
    let sup = r.sup.clone();
    let task = tokio::spawn(async move {
        sup.restart(
            RestartParams {
                stems: vec!["a".into()],
                cascade: Some(true),
                ..RestartParams::default()
            },
            "cli:t",
        )
        .await
    });
    let started = until_events(&r, &EventKind::CASCADE_STARTED, 1).await;
    let id = started[0].data["id"].as_str().unwrap().to_string();
    let hub = &r.sup.core.cascade;
    // Guard #4: members' triggers are dropped; others are not.
    for s in ["a", "b", "c", "d"] {
        assert_eq!(hub.suppresses(s).as_deref(), Some(id.as_str()), "{s}");
    }
    assert!(hub.suppresses("x").is_none());
    // Guard #1: dependants' restarts never cascade; the origin's may queue.
    assert!(hub.restarting_dependant("b").is_some());
    assert!(hub.restarting_dependant("a").is_none());
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    let d = st.stems.iter().find(|s| s.name == "d").unwrap();
    assert_eq!(d.cascade.as_ref().map(|c| c.id.as_str()), Some(id.as_str()));
    assert_eq!(d.cascade.as_ref().unwrap().origin, "a");
    let res = task.await.unwrap().unwrap();
    assert!(res.ok, "{res:?}");
    assert!(hub.suppresses("d").is_none());
    assert!(
        r.sup
            .status(&StatusParams::default(), "t")
            .unwrap()
            .stems
            .iter()
            .all(|s| s.cascade.is_none())
    );
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn a_policy_restart_with_cascade_restarts_the_dependants_once_healthy() {
    let r = rig(
        &diamond(
            "policy: on-failure, cascade: true, backoff: { initial: 10ms }",
            "",
        ),
        FakeRuntime::default(),
        FakeWaiter::default(),
    );
    let before = up_all(&r).await;
    crash(&r.rt, "a");
    let fin = until_events(&r, &EventKind::CASCADE_FINISHED, 1).await;
    assert_eq!(fin[0].data["restarted"], json!(["b", "c", "d"]));
    let started = of_kind(&r, &EventKind::CASCADE_STARTED);
    assert_eq!(started[0].data["reason"], json!("policy"));
    assert_eq!(started[0].actor, stems_api::DAEMON_ACTOR);
    // Only after a is healthy again.
    assert!(nth_state(&r, "a", "healthy", 1) < started[0].seq);
    let after = pids(&r);
    let st = r.sup.status(&StatusParams::default(), "t").unwrap();
    for s in &st.stems {
        assert_ne!(before[&s.name], after[&s.name], "{}", s.name);
        let want = u32::from(s.name == "a");
        assert_eq!(s.restarts, want, "{}", s.name);
    }
    assert_eq!(restarting(&r, "d").len(), 1);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn a_crash_of_a_dependant_during_a_cascade_does_not_cascade() {
    // Guard #1: b (restart.cascade) crashes while the cascade restarts d.
    let r = rig(
        &diamond(
            "",
            "policy: on-failure, cascade: true, backoff: { initial: 10ms }",
        ),
        FakeRuntime::default(),
        slow("d", 300),
    );
    up_all(&r).await;
    let sup = r.sup.clone();
    let task = tokio::spawn(async move {
        sup.restart(
            RestartParams {
                stems: vec!["a".into()],
                cascade: Some(true),
                ..RestartParams::default()
            },
            "cli:t",
        )
        .await
    });
    // d's restart is under way: b is healthy again.
    let t0 = std::time::Instant::now();
    while restarting(&r, "d").is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    crash(&r.rt, "b");
    let res = task.await.unwrap().unwrap();
    assert!(res.cascade.is_some());
    let t0 = std::time::Instant::now();
    while restarting(&r, "b").len() < 2 || r.sup.cell("b").state() != StemState::Healthy {
        assert!(t0.elapsed() < Duration::from_secs(5));
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(of_kind(&r, &EventKind::CASCADE_STARTED).len(), 1);
    assert!(!r.sup.core.cascade.armed.lock().unwrap().contains("b"));
    assert_eq!(restarting(&r, "d").len(), 1);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn a_second_cascade_waits_for_the_first() {
    let r = rig(&diamond("", ""), FakeRuntime::default(), slow("d", 150));
    up_all(&r).await;
    let spawn = |r: &Rig| {
        let sup = r.sup.clone();
        tokio::spawn(async move {
            sup.restart(
                RestartParams {
                    stems: vec!["a".into()],
                    cascade: Some(true),
                    ..RestartParams::default()
                },
                "cli:t",
            )
            .await
        })
    };
    let first = spawn(&r);
    until_events(&r, &EventKind::CASCADE_STARTED, 1).await;
    let second = spawn(&r);
    let queued = until_events(&r, &EventKind::CASCADE_QUEUED, 1).await;
    let started = until_events(&r, &EventKind::CASCADE_STARTED, 1).await;
    assert_eq!(queued[0].data["behind"], started[0].data["id"]);
    assert!(first.await.unwrap().unwrap().ok);
    assert!(second.await.unwrap().unwrap().ok);
    let started = of_kind(&r, &EventKind::CASCADE_STARTED);
    let finished = of_kind(&r, &EventKind::CASCADE_FINISHED);
    assert_eq!((started.len(), finished.len()), (2, 2));
    assert_eq!(queued[0].data["id"], started[1].data["id"]);
    // Never nested: the second starts after the first finished.
    assert!(finished[0].seq < started[1].seq);
    assert_eq!(restarting(&r, "d").len(), 2);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn a_second_crash_during_a_policy_cascade_queues_one_more() {
    let r = rig(
        &diamond(
            "policy: on-failure, cascade: true, backoff: { initial: 10ms }",
            "",
        ),
        FakeRuntime::default(),
        slow("d", 300),
    );
    up_all(&r).await;
    crash(&r.rt, "a");
    until_events(&r, &EventKind::CASCADE_STARTED, 1).await;
    until_state(&r.sup, "a", StemState::Healthy).await;
    crash(&r.rt, "a");
    let queued = until_events(&r, &EventKind::CASCADE_QUEUED, 1).await;
    let finished = until_events(&r, &EventKind::CASCADE_FINISHED, 2).await;
    let started = of_kind(&r, &EventKind::CASCADE_STARTED);
    assert_eq!(started.len(), 2);
    assert_eq!(queued[0].data["id"], started[1].data["id"]);
    assert!(finished[0].seq < started[1].seq, "nested cascade");
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn watch_rules_cascade_through_the_watchdog_action() {
    let yaml = diamond("", "");
    let r = rig(&yaml, FakeRuntime::default(), FakeWaiter::default());
    let before = up_all(&r).await;
    crate::supervisor::watch::run_rule_action(
        &r.sup,
        "a",
        &stems_config::WatchAction::Restart,
        "watch: app.py changed",
        Some(true),
    )
    .await
    .unwrap();
    let started = of_kind(&r, &EventKind::CASCADE_STARTED);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].actor, crate::supervisor::watch::WATCHDOG_ACTOR);
    assert_eq!(started[0].data["reason"], json!("watch: app.py changed"));
    let after = pids(&r);
    for s in ["a", "b", "c", "d"] {
        assert_ne!(before[s], after[s], "{s}");
    }
    // `cascade: false` on the rule overrides nothing-set: only a.
    crate::supervisor::watch::run_rule_action(
        &r.sup,
        "a",
        &stems_config::WatchAction::Restart,
        "watch: x",
        Some(false),
    )
    .await
    .unwrap();
    assert_eq!(of_kind(&r, &EventKind::CASCADE_STARTED).len(), 1);
    assert_eq!(after["d"], pids(&r)["d"]);
    r.sup.stop_all("t", "test").await;
}
