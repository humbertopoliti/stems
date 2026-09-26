//! Reducer tests (every key) and frame goldens.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::{Value, json};
use stems_api::{DaemonStatus, Event, StatusResult, StatusSummary, StemStatus};

use crate::model::*;
use crate::prefs::Prefs;
use crate::update::{init, update};
use crate::view::render_text;

fn stem(
    name: &str,
    state: &str,
    glyph: &str,
    pid: Option<i32>,
    port: u16,
    up: Option<u64>,
) -> StemStatus {
    serde_json::from_value(json!({
        "name": name, "type": "process", "state": state, "glyph": glyph,
        "reason": if state == "healthy" { json!("ready (tcp)") } else { Value::Null },
        "pid": pid, "pgid": pid,
        "ports": [{"name": "http", "port": port, "auto": false}],
        "uptime_s": up, "started_at": null, "restarts": 0,
        "health": null, "error": null
    }))
    .expect("stem status")
}

fn status(stems: Vec<StemStatus>) -> StatusResult {
    let summary = StatusSummary::of(&stems);
    StatusResult { stems, summary }
}

fn daemon() -> DaemonStatus {
    serde_json::from_value(json!({
        "version": "0.1.0", "api_version": 1, "workspace": "/w", "pid": 4242,
        "start_time": 1, "uptime_s": 5, "started_at": "2026-09-26T12:00:00Z",
        "workspace_name": "minimal", "stem_count": 1, "last_seq": 7,
        "subscribers": 1, "debug_rpc": false
    }))
    .expect("daemon status")
}

fn event(seq: u64, stem: &str, from: &str, to: &str) -> Event {
    serde_json::from_value(json!({
        "ts": format!("2026-09-26T12:00:{:02}Z", seq % 60), "seq": seq,
        "kind": "stem.state", "stem": stem, "from": from, "to": to,
        "reason": null, "actor": "daemon", "data": {}
    }))
    .expect("event")
}

fn minimal(mode: AttachMode) -> Model {
    let mut m = Model::new(mode, Prefs::default(), None);
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(daemon()))));
    update(
        &mut m,
        Msg::Status(Box::new(status(vec![stem(
            "echo-svc",
            "healthy",
            "healthy",
            Some(12345),
            18090,
            Some(42),
        )]))),
    );
    m
}

/// The process-chain stems in the table view (`--view table`).
fn chain() -> Model {
    chain_in(Some(ViewKind::Table))
}

fn chain_in(view: Option<ViewKind>) -> Model {
    let mut m = Model::new(AttachMode::Attach, Prefs::default(), view);
    let mut d = daemon();
    d.workspace_name = Some("process-chain".into());
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(d))));
    update(
        &mut m,
        Msg::Status(Box::new(status(vec![
            stem("a", "healthy", "healthy", Some(101), 18301, Some(9)),
            stem("b", "healthy", "healthy", Some(102), 18302, Some(8)),
            stem("c", "starting", "transitioning", Some(103), 18303, Some(1)),
            stem("d", "stopped", "stopped", None, 18304, None),
        ]))),
    );
    m
}

fn k(c: KeyCode) -> Msg {
    Msg::Key(KeyEvent::new(c, KeyModifiers::NONE))
}

fn ch(c: char) -> Msg {
    k(KeyCode::Char(c))
}

fn ctrl(c: char) -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn stem_config() -> Value {
    json!({"name": "b", "type": "process", "description": null, "enabled": true,
           "env": {"SHOP_SLEEP_START": "0.5"}, "ports": [{"name": "http", "port": 18302}],
           "depends_on": [{"stem": "a", "condition": "healthy"}],
           "scripts": {"start": {"command": "python3 app.py"}, "seed": {"command": "./seed.sh"}},
           "stop_grace": "1s", "tags": []})
}

// --- reducer --------------------------------------------------------------

#[test]
fn init_loads_daemon_and_status() {
    let mut m = Model::new(AttachMode::Attach, Prefs::default(), None);
    assert_eq!(init(&mut m), vec![Cmd::LoadDaemon, Cmd::RefreshStatus]);
    assert!(m.refresh_pending);
}

#[test]
fn status_selects_the_first_stem() {
    let m = chain();
    assert_eq!(m.selected.as_deref(), Some("a"));
    assert_eq!(m.workspace, "process-chain");
    assert_eq!(m.daemon_pid, Some(4242));
}

#[test]
fn j_k_and_arrows_move() {
    let mut m = chain();
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("b"));
    update(&mut m, k(KeyCode::Down));
    assert_eq!(m.selected.as_deref(), Some("c"));
    update(&mut m, ch('k'));
    update(&mut m, k(KeyCode::Up));
    update(&mut m, k(KeyCode::Up));
    assert_eq!(m.selected.as_deref(), Some("a"), "clamped at the top");
    update(&mut m, ch('G'));
    assert_eq!(m.selected.as_deref(), Some("d"));
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("d"), "clamped at the bottom");
    update(&mut m, ch('g'));
    assert_eq!(m.selected.as_deref(), Some("a"));
    update(&mut m, k(KeyCode::End));
    update(&mut m, k(KeyCode::Home));
    assert_eq!(m.selected.as_deref(), Some("a"));
}

#[test]
fn enter_opens_detail_and_loads_it() {
    let mut m = chain();
    update(&mut m, ch('j'));
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.view, ViewKind::Detail);
    assert_eq!(cmds, crate::update::detail_cmds("b"));
    assert!(
        update(&mut m, k(KeyCode::Enter)).is_empty(),
        "already there"
    );
    // Moving in the detail view loads the next stem.
    assert_eq!(update(&mut m, ch('j')), crate::update::detail_cmds("c"));
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.view, ViewKind::Table);
}

#[test]
fn tab_cycles_views() {
    let mut m = chain();
    let mut seen = vec![m.view];
    for _ in 0..5 {
        update(&mut m, k(KeyCode::Tab));
        seen.push(m.view);
    }
    assert_eq!(
        seen,
        vec![
            ViewKind::Table,
            ViewKind::Detail,
            ViewKind::Logs,
            ViewKind::Events,
            ViewKind::Graph,
            ViewKind::Table
        ]
    );
    update(
        &mut m,
        Msg::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
    );
    assert_eq!(m.view, ViewKind::Graph);
    assert_eq!(
        ViewKind::ALL[0],
        ViewKind::Graph,
        "Tab order starts at Graph"
    );
}

#[test]
fn slash_filters() {
    let mut m = chain();
    update(&mut m, ch('/'));
    assert!(m.filter_editing);
    update(&mut m, ch('b'));
    assert_eq!(m.filter, "b");
    assert_eq!(m.visible().len(), 1);
    assert_eq!(
        m.selected.as_deref(),
        Some("b"),
        "selection follows the filter"
    );
    update(&mut m, ch('x'));
    update(&mut m, k(KeyCode::Backspace));
    update(&mut m, k(KeyCode::Enter));
    assert!(!m.filter_editing);
    assert_eq!(m.filter, "b");
    // q while filtering types; after Enter it quits.
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.filter, "", "Esc clears a kept filter");
    update(&mut m, ch('/'));
    update(&mut m, ch('q'));
    assert_eq!(m.filter, "q");
    assert!(m.exit.is_none());
    update(&mut m, k(KeyCode::Esc));
    assert!(!m.filter_editing);
    assert_eq!(m.filter, "");
}

#[test]
fn capital_o_cycles_sort() {
    let mut m = chain();
    assert_eq!(m.sort, SortKey::Declared);
    update(&mut m, ch('O'));
    assert_eq!(m.sort, SortKey::Name);
    update(&mut m, ch('O'));
    assert_eq!(m.sort, SortKey::State);
    let order: Vec<_> = m.visible().iter().map(|s| s.name.clone()).collect();
    assert_eq!(order, ["c", "a", "b", "d"]);
    update(&mut m, ch('O'));
    assert_eq!(m.sort, SortKey::Uptime);
    let order: Vec<_> = m.visible().iter().map(|s| s.name.clone()).collect();
    assert_eq!(order, ["a", "b", "c", "d"]);
    update(&mut m, ch('O'));
    assert_eq!(m.sort, SortKey::Declared);
}

#[test]
fn help_toggles_and_swallows_keys() {
    let mut m = chain();
    update(&mut m, ch('?'));
    assert!(m.help);
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("a"));
    update(&mut m, ch('?'));
    assert!(!m.help);
    update(&mut m, ch('?'));
    update(&mut m, k(KeyCode::Esc));
    assert!(!m.help);
    update(&mut m, ch('?'));
    update(&mut m, ch('q'));
    assert!(!m.help);
    assert!(m.exit.is_none(), "q closes help first");
}

#[test]
fn q_in_attach_mode_detaches() {
    let mut m = chain();
    assert_eq!(update(&mut m, ch('q')), vec![Cmd::Exit(Outcome::Detach)]);
    let mut m = chain();
    assert_eq!(update(&mut m, ctrl('c')), vec![Cmd::Exit(Outcome::Detach)]);
}

#[test]
fn q_in_up_mode_asks() {
    let mut m = minimal(AttachMode::Up);
    assert!(update(&mut m, ch('q')).is_empty());
    assert_eq!(m.modal, Modal::QuitConfirm);
    update(&mut m, ch('n'));
    assert_eq!(m.modal, Modal::None);
    update(&mut m, ctrl('c'));
    assert_eq!(m.modal, Modal::QuitConfirm);
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.modal, Modal::None);
    update(&mut m, ch('q'));
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.modal, Modal::None, "Enter = the default No");
    update(&mut m, ch('q'));
    assert_eq!(update(&mut m, ch('y')), vec![Cmd::Exit(Outcome::StopAll)]);
    assert_eq!(m.exit, Some(Outcome::StopAll));
    assert!(update(&mut m, ch('j')).is_empty(), "nothing after exit");

    let mut m = minimal(AttachMode::Up);
    update(&mut m, ch('q'));
    assert_eq!(update(&mut m, ch('d')), vec![Cmd::Exit(Outcome::Detach)]);

    let mut m = minimal(AttachMode::Attach);
    m.owns_daemon = true;
    update(&mut m, ch('q'));
    assert_eq!(
        m.modal,
        Modal::QuitConfirm,
        "an attach that started the daemon asks too"
    );
}

#[test]
fn signals() {
    let mut m = minimal(AttachMode::Up);
    assert_eq!(
        update(&mut m, Msg::Signal(SignalKind::Hangup)),
        vec![Cmd::Exit(Outcome::StopAll)]
    );
    let mut m = minimal(AttachMode::Attach);
    assert_eq!(
        update(&mut m, Msg::Signal(SignalKind::Terminate)),
        vec![Cmd::Exit(Outcome::Detach)]
    );
    let mut m = minimal(AttachMode::Up);
    update(&mut m, Msg::Signal(SignalKind::Interrupt));
    assert_eq!(m.modal, Modal::QuitConfirm);
    let mut m = minimal(AttachMode::Up);
    assert_eq!(
        update(&mut m, Msg::Disconnected),
        vec![Cmd::Exit(Outcome::Detach)]
    );
}

#[test]
fn ctrl_l_toggles_the_reserved_log_pane() {
    let mut m = chain();
    update(&mut m, ctrl('l'));
    assert!(m.log_pane.visible);
    update(&mut m, ctrl('l'));
    assert!(!m.log_pane.visible);
}

#[test]
fn events_patch_state_and_request_a_refresh() {
    let mut m = minimal(AttachMode::Attach);
    let cmds = update(
        &mut m,
        Msg::Event(Box::new(event(8, "echo-svc", "healthy", "failed"))),
    );
    assert_eq!(cmds, vec![Cmd::RefreshStatus]);
    assert_eq!(m.stems[0].state.to_string(), "failed");
    assert_eq!(m.stems[0].glyph, stems_core::Glyph::Failed);
    assert_eq!(m.summary.failed, 1);
    // A second event while the refresh is in flight does not stack refreshes.
    assert!(
        update(
            &mut m,
            Msg::Event(Box::new(event(9, "echo-svc", "failed", "starting")))
        )
        .is_empty()
    );
    assert_eq!(m.events.len(), 2);
}

#[test]
fn tick_refreshes_about_every_second() {
    let mut m = minimal(AttachMode::Attach);
    let mut n = 0;
    for _ in 0..8 {
        n += update(&mut m, Msg::Tick).len();
        m.refresh_pending = false;
    }
    assert_eq!(n, 2, "250 ms ticks: every 4th");
}

#[test]
fn mouse_click_selects_when_enabled() {
    let click = |row| {
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let mut m = chain();
    update(&mut m, click(4));
    assert_eq!(m.selected.as_deref(), Some("a"), "mouse off by default");
    m.prefs.mouse = true;
    update(&mut m, click(4));
    assert_eq!(m.selected.as_deref(), Some("c"));
    update(&mut m, click(20));
    assert_eq!(m.selected.as_deref(), Some("c"), "below the rows");
}

#[test]
fn default_view_from_prefs_and_flag() {
    let prefs = Prefs {
        default_view: Some(ViewKind::Detail),
        ..Prefs::default()
    };
    let mut m = Model::new(AttachMode::Attach, prefs.clone(), None);
    assert_eq!(m.view, ViewKind::Detail);
    let cmds = update(
        &mut m,
        Msg::Status(Box::new(status(vec![stem(
            "x", "healthy", "healthy", None, 1, None,
        )]))),
    );
    assert_eq!(cmds, crate::update::detail_cmds("x"));
    let m = Model::new(AttachMode::Attach, prefs, Some(ViewKind::Table));
    assert_eq!(m.view, ViewKind::Table, "--view wins");
}

#[test]
fn rpc_results_fill_the_detail() {
    let mut m = chain();
    update(&mut m, ch('j'));
    update(&mut m, k(KeyCode::Enter));
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemConfig {
            stem: "b".into(),
            result: Ok(stem_config()),
        }),
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemEvents {
            stem: "b".into(),
            result: Ok(vec![event(3, "b", "starting", "healthy")]),
        }),
    );
    let d = m.detail.as_ref().unwrap();
    assert!(d.config.as_ref().unwrap().is_ok());
    assert_eq!(d.events.len(), 1);
    // A late result for another stem is ignored.
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemConfig {
            stem: "a".into(),
            result: Err("x".into()),
        }),
    );
    assert!(m.detail.as_ref().unwrap().config.as_ref().unwrap().is_ok());
    update(
        &mut m,
        Msg::Rpc(RpcResult::Failed {
            what: "status".into(),
            message: "boom".into(),
        }),
    );
    assert_eq!(m.message.as_deref(), Some("status: boom"));
}

// --- frames ---------------------------------------------------------------

fn detail_model() -> Model {
    let mut m = chain();
    update(&mut m, ch('j'));
    update(&mut m, k(KeyCode::Enter));
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemConfig {
            stem: "b".into(),
            result: Ok(stem_config()),
        }),
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemHealth {
            stem: "b".into(),
            result: Ok(vec![
                json!({"ts": "2026-09-26T12:00:05Z", "ok": true, "outcome": "ok", "latency_ms": 1, "detail": "connected"}),
                json!({"ts": "2026-09-26T12:00:06Z", "ok": false, "outcome": "fail", "latency_ms": 150, "detail": "connection refused"}),
            ]),
        }),
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemEvents {
            stem: "b".into(),
            result: Ok(vec![
                event(3, "b", "stopped", "starting"),
                event(4, "b", "starting", "healthy"),
            ]),
        }),
    );
    m
}

#[test]
fn frame_table_minimal() {
    crate::assert_frame!(minimal(AttachMode::Attach), "table-minimal");
}

#[test]
fn frame_table_process_chain() {
    let mut m = chain();
    update(&mut m, ch('j'));
    crate::assert_frame!(m, "table-process-chain");
}

#[test]
fn frame_table_filtered() {
    let mut m = chain();
    for c in "/b".chars() {
        update(&mut m, ch(c));
    }
    crate::assert_frame!(m, "table-filtered", 80, 24);
}

#[test]
fn frame_detail() {
    crate::assert_frame!(detail_model(), "detail");
}

#[test]
fn frame_help_overlay() {
    let mut m = chain();
    update(&mut m, ch('?'));
    crate::assert_frame!(m, "help");
}

#[test]
fn frame_quit_modal() {
    let mut m = minimal(AttachMode::Up);
    update(&mut m, ch('q'));
    crate::assert_frame!(m, "quit-modal");
}

#[test]
fn frame_too_small() {
    crate::assert_frame!(chain(), "too-small", 39, 9);
    let text = render_text(&chain(), 10, 3);
    assert!(text.contains("terminal"), "{text}");
    let text = render_text(&chain(), 1, 1);
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn frame_placeholders_and_ascii() {
    let mut m = chain();
    let cmds = update(&mut m, Msg::SetView(ViewKind::Graph));
    assert_eq!(
        cmds,
        vec![Cmd::LoadGraph(vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into()
        ])]
    );
    assert!(render_text(&m, 80, 24).contains("loading"));
    update(&mut m, Msg::SetView(ViewKind::Logs));
    assert!(render_text(&m, 80, 24).contains("[Logs]"));
    update(&mut m, Msg::SetView(ViewKind::Table));
    m.ascii = true;
    let text = render_text(&m, 80, 24);
    assert!(text.contains("OK healthy"), "{text}");
    assert!(text.contains("> a"), "{text}");
}

#[test]
fn detail_narrow_and_loading() {
    let mut m = chain();
    update(&mut m, k(KeyCode::Enter));
    let text = render_text(&m, 50, 20);
    assert!(text.contains("Detail: a"), "{text}");
    assert!(text.contains("loading"), "{text}");
}

// --- graph view (28) --------------------------------------------------------

/// The process-chain diamond's graph as `stem_config` would describe it.
fn chain_graph() -> RpcResult {
    let cfg = |deps: Value| json!({"type": "process", "depends_on": deps});
    let stems = vec![
        crate::graph::GraphStem::from_config("a", &cfg(json!([]))),
        crate::graph::GraphStem::from_config(
            "b",
            &cfg(json!([{"stem": "a", "condition": "healthy"}])),
        ),
        crate::graph::GraphStem::from_config(
            "c",
            &cfg(json!([{"stem": "a", "condition": "healthy", "protocol": "http"}])),
        ),
        crate::graph::GraphStem::from_config("d", &cfg(json!(["b", "c"]))),
    ];
    RpcResult::Graph {
        names: vec!["a".into(), "b".into(), "c".into(), "d".into()],
        result: Ok(stems),
    }
}

/// The chain opened with no view asked for: the graph is the default.
fn graph_model() -> Model {
    let mut m = Model::new(AttachMode::Attach, Prefs::default(), None);
    let mut d = daemon();
    d.workspace_name = Some("process-chain".into());
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(d))));
    let cmds = update(
        &mut m,
        Msg::Status(Box::new(status(vec![
            stem("a", "unhealthy", "failed", Some(101), 18301, Some(9)),
            stem("b", "healthy", "healthy", Some(102), 18302, Some(8)),
            stem("c", "starting", "transitioning", Some(103), 18303, Some(1)),
            stem("d", "stopped", "stopped", None, 18304, None),
        ]))),
    );
    assert_eq!(m.view, ViewKind::Graph);
    assert_eq!(
        cmds,
        vec![Cmd::LoadGraph(vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into()
        ])]
    );
    update(&mut m, Msg::Rpc(chain_graph()));
    m
}

#[test]
fn default_view_is_graph_for_several_stems_table_for_one() {
    let m = graph_model();
    assert_eq!(m.view, ViewKind::Graph);
    assert_eq!(m.selected.as_deref(), Some("d"), "first box");
    let m = minimal(AttachMode::Attach);
    assert_eq!(m.view, ViewKind::Table);
    // An explicit view (or ui.default_view) wins.
    let m = chain_in(Some(ViewKind::Detail));
    assert_eq!(m.view, ViewKind::Detail);
    let prefs = Prefs {
        default_view: Some(ViewKind::Table),
        ..Prefs::default()
    };
    let mut m = Model::new(AttachMode::Attach, prefs, None);
    update(
        &mut m,
        Msg::Status(Box::new(status(vec![
            stem("x", "healthy", "healthy", None, 1, None),
            stem("y", "healthy", "healthy", None, 2, None),
        ]))),
    );
    assert_eq!(m.view, ViewKind::Table);
}

#[test]
fn graph_is_cached_on_the_stem_set() {
    let mut m = graph_model();
    assert!(m.graph.layout.is_some());
    let same = status(m.stems.clone());
    assert!(
        update(&mut m, Msg::Status(Box::new(same))).is_empty(),
        "no re-layout"
    );
    let mut more = m.stems.clone();
    more.push(stem("e", "healthy", "healthy", None, 1, None));
    let cmds = update(&mut m, Msg::Status(Box::new(status(more.clone()))));
    assert_eq!(cmds.len(), 1);
    assert!(matches!(&cmds[0], Cmd::LoadGraph(n) if n.len() == 5));
    assert!(
        update(&mut m, Msg::Status(Box::new(status(more)))).is_empty(),
        "one load in flight"
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::Graph {
            names: vec!["x".into()],
            result: Err("boom".into()),
        }),
    );
    assert!(!m.graph.pending);
    assert_eq!(m.graph.error.as_deref(), Some("boom"));
}

#[test]
fn graph_keys_move_focus_label_and_zoom() {
    let mut m = graph_model();
    update(&mut m, ch('l'));
    assert_eq!(m.selected.as_deref(), Some("b"));
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("c"));
    update(&mut m, k(KeyCode::Right));
    assert_eq!(m.selected.as_deref(), Some("a"));
    update(&mut m, k(KeyCode::Left));
    update(&mut m, ch('h'));
    assert_eq!(m.selected.as_deref(), Some("d"));
    update(&mut m, ch('l'));
    update(&mut m, ch('f'));
    let f = m.graph.shown().unwrap();
    let mut names: Vec<&str> = f.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["a", "b", "d"]);
    update(&mut m, ch('f'));
    assert_eq!(m.graph.shown().unwrap().nodes.len(), 4);
    update(&mut m, ch('e'));
    assert!(m.graph.edge_labels);
    update(&mut m, ch('-'));
    assert_eq!(m.graph.compact, Some(true));
    update(&mut m, ch('+'));
    assert_eq!(m.graph.compact, Some(false));
    update(&mut m, ch('0'));
    assert_eq!(m.graph.compact, None);
    // Ctrl-L still toggles the log pane in the graph.
    update(&mut m, ctrl('l'));
    assert!(m.log_pane.visible);
    // Enter opens the detail, Esc comes back to the graph.
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.view, ViewKind::Detail);
    assert_eq!(cmds, crate::update::detail_cmds("b"));
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.view, ViewKind::Graph);
}

#[test]
fn frame_graph() {
    let m = graph_model();
    crate::assert_frame!(m, "graph-process-chain");
    let mut m = graph_model();
    update(&mut m, ch('-'));
    update(&mut m, ch('l'));
    crate::assert_frame!(m, "graph-compact", 80, 24);
    let mut m = graph_model();
    update(&mut m, ch('e'));
    update(&mut m, ch('l'));
    update(&mut m, ch('j'));
    update(&mut m, ch('f'));
    crate::assert_frame!(m, "graph-focus-labels", 80, 24);
    let mut m = graph_model();
    m.ascii = true;
    crate::assert_frame!(m, "graph-ascii", 80, 24);
}

#[test]
fn graph_shows_degraded_reason_lines() {
    let mut m = graph_model();
    let mut stems = m.stems.clone();
    let mut b: StemStatus = stems[1].clone();
    b.glyph = stems_core::Glyph::Degraded;
    b.degraded = true;
    b.reason = Some("dependency a unhealthy".into());
    stems[1] = b;
    update(&mut m, Msg::Status(Box::new(status(stems))));
    let text = render_text(&m, 120, 40);
    assert!(text.contains("│ b   ! ├"), "{text}");
    assert!(text.contains("dep a"), "{text}");
    assert!(text.contains("a ✗"), "{text}");
}

#[test]
fn metrics_feed_sparkline_history_from_status() {
    let mut m = minimal(AttachMode::Attach);
    let at = |secs: i64, cpu: f64| {
        let mut s = stem("echo-svc", "healthy", "healthy", Some(1), 18090, Some(1));
        s.metrics = Some(stems_api::MetricsSummary {
            ts: chrono::DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap(),
            cpu_pct: cpu,
            rss_bytes: 1 << 20,
            children: 1,
        });
        Msg::Status(Box::new(status(vec![s])))
    };
    update(&mut m, at(0, 5.0));
    update(&mut m, at(0, 5.0)); // same sample again: not appended
    update(&mut m, at(2, 50.0));
    let h = &m.metrics["echo-svc"];
    assert_eq!(h.cpu, [5.0, 50.0]);
    assert_eq!(h.mem.len(), 2);
    for i in 3..40 {
        update(&mut m, at(i, 1.0));
    }
    assert_eq!(
        m.metrics["echo-svc"].cpu.len(),
        stems_api::metrics::SPARK_SAMPLES
    );
}

// --- logs, events, split (29) ---------------------------------------------

fn log(stem: &str, ms: i64, level: Option<&str>, text: &str) -> stems_core::logs::LogRecord {
    serde_json::from_value(json!({
        "ts": (chrono::DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z").unwrap()
            + chrono::Duration::milliseconds(ms)).to_rfc3339(),
        "stem": stem, "stream": "out", "tag": null, "level": level,
        "text": text, "fields": null
    }))
    .expect("log record")
}

fn feed(m: &mut Model, rec: stems_core::logs::LogRecord) {
    let generation = m.log_pane.generation;
    update(
        m,
        Msg::Log {
            generation,
            record: Box::new(rec),
        },
    );
}

fn sub(generation: u64, stems: &[&str], since: Option<&str>) -> Cmd {
    Cmd::SubscribeLogs(LogSubscription {
        generation,
        stems: stems.iter().map(|s| s.to_string()).collect(),
        since: since.map(str::to_string),
        tail: if since.is_none() {
            Some(crate::logs::LOG_REPLAY)
        } else {
            None
        },
    })
}

fn logs_model() -> Model {
    let mut m = minimal(AttachMode::Attach);
    let cmds = update(&mut m, Msg::SetView(ViewKind::Logs));
    assert_eq!(cmds, vec![sub(1, &["echo-svc"], None)]);
    for i in 0..6 {
        let (lvl, text) = if i % 2 == 0 {
            (Some("info"), format!("INFO chaos log line {i}"))
        } else {
            (Some("error"), format!("ERROR chaos log line {i}"))
        };
        feed(&mut m, log("echo-svc", i * 10, lvl, &text));
    }
    m
}

#[test]
fn logs_view_subscribes_to_the_selected_stem_once() {
    let mut m = logs_model();
    assert_eq!(m.log_pane.ring.len(), 6);
    // Same target: no new subscription; records of an old generation are dropped.
    assert!(update(&mut m, Msg::SetView(ViewKind::Logs)).is_empty());
    update(
        &mut m,
        Msg::Log {
            generation: 0,
            record: Box::new(log("echo-svc", 0, None, "stale")),
        },
    );
    assert!(!m.log_pane.contains("stale"));
    // Other views: the pane is not needed, nothing is (re)subscribed.
    assert!(update(&mut m, Msg::SetView(ViewKind::Table)).is_empty());
    // m: merged (every stem); again: back to the selected stem.
    update(&mut m, Msg::SetView(ViewKind::Logs));
    assert_eq!(update(&mut m, ch('m')), vec![sub(2, &[], None)]);
    assert!(m.log_pane.ring.is_empty(), "a new source starts empty");
    assert_eq!(update(&mut m, ch('m')), vec![sub(3, &["echo-svc"], None)]);
}

#[test]
fn logs_keys_pause_search_level_copy() {
    let mut m = logs_model();
    update(&mut m, k(KeyCode::Char(' ')));
    assert!(m.log_pane.paused);
    feed(&mut m, log("echo-svc", 100, Some("warn"), "WARN later"));
    assert!(!render_text(&m, 80, 24).contains("WARN later"));
    assert!(render_text(&m, 80, 24).contains("paused (+1)"));
    update(&mut m, k(KeyCode::Char(' ')));
    assert!(render_text(&m, 80, 24).contains("WARN later"));
    for c in "/line 3".chars() {
        update(&mut m, ch(c));
    }
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        m.log_pane.selected_line().unwrap().record.text,
        "ERROR chaos log line 3"
    );
    // y copies the selected line (OSC 52 by default).
    assert_eq!(
        update(&mut m, ch('y')),
        vec![Cmd::Copy {
            text: "ERROR chaos log line 3".into(),
            via: crate::prefs::Clipboard::Osc52
        }]
    );
    assert_eq!(m.message.as_deref(), Some("copied 1 line"));
    // L cycles the level; q still quits, Tab still switches.
    let shift_l = Msg::Key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
    update(&mut m, shift_l.clone());
    update(&mut m, shift_l);
    let text = render_text(&m, 80, 24);
    assert!(!text.contains("INFO chaos"), "{text}");
    assert!(text.contains("ERROR chaos log line 5"), "{text}");
    update(&mut m, k(KeyCode::Tab));
    assert_eq!(m.view, ViewKind::Events);
}

#[test]
fn frame_logs_view() {
    let mut m = logs_model();
    let shift_l = Msg::Key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
    crate::assert_frame!(m, "logs-view");
    for c in "/line 1<".chars().take(7) {
        update(&mut m, ch(c));
    }
    update(&mut m, k(KeyCode::Enter));
    update(&mut m, shift_l.clone());
    update(&mut m, shift_l);
    crate::assert_frame!(m, "logs-view-search-warn", 80, 24);
    m.help = true;
    crate::assert_frame!(m, "logs-help", 80, 24);
}

fn events_model() -> Model {
    let mut m = minimal(AttachMode::Attach);
    let mut restart = event(4, "echo-svc", "failed", "restarting");
    restart.kind = stems_api::EventKind::STEM_RESTARTING;
    restart.from = None;
    restart.to = None;
    restart.reason = Some("failed: exit code 1 (restart 1/5)".into());
    for e in [
        event(1, "echo-svc", "stopped", "starting"),
        event(2, "echo-svc", "starting", "healthy"),
        event(3, "echo-svc", "healthy", "failed"),
        restart,
    ] {
        update(&mut m, Msg::Event(Box::new(e)));
    }
    m
}

#[test]
fn events_view_loads_filters_and_jumps() {
    let mut m = events_model();
    let cmds = update(&mut m, Msg::SetView(ViewKind::Events));
    assert_eq!(cmds, vec![Cmd::LoadEvents]);
    // The buffered events are merged by seq.
    update(
        &mut m,
        Msg::Rpc(RpcResult::Events(Ok(vec![
            event(0, "echo-svc", "-", "stopped"),
            event(2, "echo-svc", "starting", "healthy"),
        ]))),
    );
    assert_eq!(
        m.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    assert!(update(&mut m, Msg::SetView(ViewKind::Events)).is_empty());
    // / filters (kind, stem, states, reason).
    for c in "/failed".chars() {
        update(&mut m, ch(c));
    }
    update(&mut m, k(KeyCode::Enter));
    let rows = m.events_view.rows(&m.events);
    assert_eq!(rows.len(), 2, "healthy→failed and failed→restarting");
    update(&mut m, ch('k'));
    let e = m.events_view.selected_event(&m.events).unwrap();
    assert_eq!(e.seq, 3);
    // Enter: the stem's logs, replayed from a minute before the event.
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.view, ViewKind::Logs);
    assert_eq!(
        cmds,
        vec![sub(1, &["echo-svc"], Some("2026-09-26T11:59:03.000Z"))]
    );
    // Replayed lines: the selection lands on the last one before the event.
    feed(&mut m, log("echo-svc", 1_000, None, "before"));
    feed(&mut m, log("echo-svc", 3_000, Some("error"), "ERROR crash"));
    feed(&mut m, log("echo-svc", 9_000, None, "after"));
    assert!(!m.log_pane.follow);
    assert_eq!(
        m.log_pane.selected_line().unwrap().record.text,
        "ERROR crash"
    );
}

#[test]
fn frame_events_view() {
    let mut m = events_model();
    update(&mut m, Msg::SetView(ViewKind::Events));
    crate::assert_frame!(m, "events-view");
    for c in "/restart".chars() {
        update(&mut m, ch(c));
    }
    crate::assert_frame!(m, "events-view-filtered", 80, 24);
}

#[test]
fn ctrl_l_split_pane_follows_the_selection_and_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ui.toml");
    let prefs = Prefs {
        path: Some(path.clone()),
        ..Prefs::default()
    };
    let mut m = Model::new(AttachMode::Attach, prefs, Some(ViewKind::Table));
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(daemon()))));
    update(
        &mut m,
        Msg::Status(Box::new(status(vec![
            stem("a", "healthy", "healthy", Some(101), 18301, Some(9)),
            stem("b", "healthy", "healthy", Some(102), 18302, Some(8)),
        ]))),
    );
    let cmds = update(&mut m, ctrl('l'));
    assert_eq!(
        cmds,
        vec![
            Cmd::SavePref {
                path: path.clone(),
                key: "split_logs".into(),
                value: "true".into()
            },
            sub(1, &["a"], None)
        ]
    );
    assert!(m.prefs.split_logs);
    // j: the pane follows the selected stem.
    assert_eq!(update(&mut m, ch('j')), vec![sub(2, &["b"], None)]);
    // Off: saved again, no subscription.
    assert_eq!(
        update(&mut m, ctrl('l')),
        vec![Cmd::SavePref {
            path,
            key: "split_logs".into(),
            value: "false".into()
        }]
    );
}

#[test]
fn frame_split_layout() {
    let prefs = Prefs {
        split_logs: true,
        ..Prefs::default()
    };
    let mut m = Model::new(AttachMode::Attach, prefs, Some(ViewKind::Table));
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(daemon()))));
    let cmds = update(
        &mut m,
        Msg::Status(Box::new(status(vec![stem(
            "echo-svc",
            "healthy",
            "healthy",
            Some(12345),
            18090,
            Some(42),
        )]))),
    );
    assert!(cmds.contains(&sub(1, &["echo-svc"], None)), "{cmds:?}");
    feed(
        &mut m,
        log(
            "echo-svc",
            0,
            Some("info"),
            "INFO listening on 127.0.0.1:18090",
        ),
    );
    feed(
        &mut m,
        log(
            "echo-svc",
            5,
            Some("warn"),
            "WARN chaos endpoints enabled under /__chaos/",
        ),
    );
    crate::assert_frame!(m, "split-table");
    update(&mut m, Msg::SetView(ViewKind::Detail));
    crate::assert_frame!(m, "split-detail", 120, 40);
}

mod actions;
