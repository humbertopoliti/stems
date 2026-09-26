//! Reducer tests and frame goldens of deliverable 30: action keys, the
//! modals, the script menu and form, the palette and toasts.

use serde_json::{Map, Value, json};
use stems_core::{Error, ErrorCode};

use super::*;
use crate::actions::{Action, MenuEntry};
use crate::toast::{TOAST_TTL_MS, ToastKind};

fn actions(cmds: &[Cmd]) -> Vec<Action> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Action(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

fn press(m: &mut Model, keys: &str) -> Vec<Cmd> {
    let mut out = Vec::new();
    for c in keys.chars() {
        out.extend(update(m, ch(c)));
    }
    out
}

fn arg(v: Value) -> stems_config::ScriptArg {
    serde_json::from_value(v).expect("script arg")
}

/// The run-scripts catalogue: two scripts of `b`, one workspace script.
fn catalog() -> Vec<MenuEntry> {
    use stems_core::scriptargs::ScriptKind;
    vec![
        MenuEntry {
            stem: None,
            name: "needs-api".into(),
            description: Some("A workspace script that needs shop-api healthy".into()),
            kind: ScriptKind::Custom,
            args: vec![],
        },
        MenuEntry {
            stem: Some("b".into()),
            name: "create-test-user".into(),
            description: Some("Create a user with a known password for manual testing".into()),
            kind: ScriptKind::Custom,
            args: vec![
                arg(json!({"name": "email", "type": "string", "required": true,
                    "default": null, "description": "Email address of the test user", "values": []})),
                arg(json!({"name": "role", "type": "enum", "required": false,
                    "default": "admin", "description": "Role to grant", "values": ["admin", "user"]})),
            ],
        },
        MenuEntry {
            stem: Some("b".into()),
            name: "flaky".into(),
            description: Some("Fails on its first two attempts".into()),
            kind: ScriptKind::Custom,
            args: vec![],
        },
        MenuEntry {
            stem: Some("b".into()),
            name: "start".into(),
            description: None,
            kind: ScriptKind::Lifecycle,
            args: vec![],
        },
    ]
}

/// The chain with `b` selected (a running dependant: d).
fn chain_b() -> Model {
    let mut m = chain();
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("b"));
    m
}

fn has_dependants() -> Error {
    Error::new(
        ErrorCode::HasDependants,
        "cannot stop b: running stems depend on it: d",
    )
    .with_hint("stop them too with `stems stop b --cascade`, or stop d first")
    .with_details(json!({"stems": ["b"], "dependants": ["d"]}))
}

fn result(action: Action, r: Result<Value, Error>) -> Msg {
    Msg::Rpc(RpcResult::Action {
        action,
        result: r.map_err(Box::new),
    })
}

fn script_event(seq: u64, kind: &str, data: Value) -> Msg {
    Msg::Event(Box::new(
        serde_json::from_value(json!({
            "ts": "2026-09-26T12:00:10Z", "seq": seq, "kind": kind, "stem": "b",
            "actor": "tui:alice", "data": data
        }))
        .expect("event"),
    ))
}

// --- action keys --------------------------------------------------------------

#[test]
fn stem_action_keys_send_their_rpcs() {
    let mut m = chain_b();
    let b = || "b".to_string();
    assert_eq!(actions(&update(&mut m, ch('s'))), [Action::Start(b())]);
    assert_eq!(m.message.as_deref(), Some("start b…"));
    assert_eq!(
        actions(&update(&mut m, ch('x'))),
        [Action::Stop {
            stem: b(),
            cascade: false
        }]
    );
    assert_eq!(
        actions(&update(&mut m, ch('X'))),
        [Action::Stop {
            stem: b(),
            cascade: true
        }]
    );
    assert_eq!(
        actions(&update(&mut m, ch('r'))),
        [Action::Restart {
            stem: b(),
            build: false
        }]
    );
    assert_eq!(
        actions(&update(&mut m, ch('R'))),
        [Action::Restart {
            stem: b(),
            build: true
        }]
    );
    assert_eq!(
        actions(&update(&mut m, ch('p'))),
        [Action::Watch {
            stem: b(),
            pause: true
        }]
    );
    m.profile = Some("web".into());
    assert_eq!(
        actions(&update(&mut m, ch('u'))),
        [Action::UpAll {
            profile: Some("web".into())
        }]
    );
    assert_eq!(m.modal, Modal::None, "no action key opened a dialog");
}

#[test]
fn p_resumes_a_paused_watchdog() {
    let mut m = chain_b();
    m.stems[1].watch = Some(serde_json::from_value(json!({"paused": true, "rules": 1})).unwrap());
    assert!(m.watch_paused("b"));
    assert_eq!(
        actions(&update(&mut m, ch('p'))),
        [Action::Watch {
            stem: "b".into(),
            pause: false
        }]
    );
}

#[test]
fn actions_only_in_stem_views() {
    for v in [ViewKind::Logs, ViewKind::Events] {
        let mut m = chain_b();
        update(&mut m, Msg::SetView(v));
        let cmds = press(&mut m, "sxrRpSu");
        assert!(actions(&cmds).is_empty(), "{v:?}: {cmds:?}");
        assert_eq!(m.modal, Modal::None);
    }
    // Graph and Detail act like the table.
    let mut m = chain_b();
    update(&mut m, Msg::SetView(ViewKind::Detail));
    assert_eq!(
        actions(&update(&mut m, ch('s'))),
        [Action::Start("b".into())]
    );
    let mut m = graph_model();
    assert_eq!(
        actions(&update(&mut m, ch('s'))),
        [Action::Start("d".into())]
    );
}

#[test]
fn o_opens_the_editor() {
    let mut m = chain_b();
    assert_eq!(
        update(&mut m, ch('o')),
        vec![Cmd::OpenEditor {
            stem: "b".into(),
            editor: "vi".into()
        }]
    );
    m.editor = Some("code -w".into());
    assert_eq!(
        update(&mut m, ch('o')),
        vec![Cmd::OpenEditor {
            stem: "b".into(),
            editor: "code -w".into()
        }]
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::Editor {
            stem: "b".into(),
            result: Ok("/src/b".into()),
        }),
    );
    assert_eq!(m.toasts.items[0].text, "editor closed (b: /src/b)");
    update(
        &mut m,
        Msg::Rpc(RpcResult::Editor {
            stem: "b".into(),
            result: Err("vi exited with 1".into()),
        }),
    );
    assert_eq!(m.toasts.items[1].kind, ToastKind::Error);
}

#[test]
fn x_with_running_dependants_asks_then_cascades() {
    let mut m = chain_b();
    let stop = Action::Stop {
        stem: "b".into(),
        cascade: false,
    };
    update(&mut m, ch('x'));
    let cmds = update(&mut m, result(stop.clone(), Err(has_dependants())));
    assert_eq!(cmds, vec![Cmd::RefreshStatus]);
    assert_eq!(
        m.modal,
        Modal::StopConfirm {
            stem: "b".into(),
            dependants: vec!["d".into()]
        }
    );
    assert!(m.toasts.is_empty(), "the dialog replaces the error toast");
    // Other keys are swallowed; n cancels.
    assert!(actions(&update(&mut m, ch('r'))).is_empty());
    update(&mut m, ch('n'));
    assert_eq!(m.modal, Modal::None);
    // Again, X cascades.
    update(&mut m, result(stop.clone(), Err(has_dependants())));
    assert_eq!(
        actions(&update(&mut m, ch('X'))),
        [Action::Stop {
            stem: "b".into(),
            cascade: true
        }]
    );
    assert_eq!(m.modal, Modal::None);
    // y too.
    update(&mut m, result(stop, Err(has_dependants())));
    assert_eq!(actions(&update(&mut m, ch('y'))).len(), 1);
}

#[test]
fn results_become_toasts_and_errors_expand() {
    let mut m = chain_b();
    m.refresh_pending = false;
    let r = Action::Restart {
        stem: "b".into(),
        build: false,
    };
    let cmds = update(&mut m, result(r.clone(), Ok(json!({"ok": true}))));
    assert_eq!(cmds, vec![Cmd::RefreshStatus], "a refresh follows");
    assert!(m.message.is_none());
    assert_eq!(m.toasts.items[0].lines(false), ["✓ restarted b"]);
    let e = Error::new(ErrorCode::UnknownStem, "no stem `zz`").with_hint("see `stems status`");
    update(&mut m, result(Action::Start("zz".into()), Err(e)));
    let t = m.toasts.last_error().unwrap();
    assert_eq!(t.code.as_deref(), Some("UNKNOWN_STEM"));
    assert_eq!(t.lines(false)[1], "  hint: see `stems status`");
    // e: details; Esc closes the dialog, a second Esc dismisses the toasts.
    update(&mut m, ch('e'));
    match &m.modal {
        Modal::ErrorDetails { title, text } => {
            assert_eq!(title, "UNKNOWN_STEM");
            assert!(text.contains("hint: see `stems status`"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.modal, Modal::None);
    assert_eq!(m.view, ViewKind::Table);
    update(&mut m, k(KeyCode::Esc));
    assert!(m.toasts.is_empty());
    // Without an error toast, `e` in the graph still toggles the labels.
    let mut g = graph_model();
    update(&mut g, result(r, Ok(json!({}))));
    update(&mut g, ch('e'));
    assert!(g.graph.edge_labels);
    assert_eq!(g.modal, Modal::None);
}

#[test]
fn toasts_expire_on_the_tick_clock() {
    let mut m = chain_b();
    update(&mut m, result(Action::Start("b".into()), Ok(json!({}))));
    let ticks = TOAST_TTL_MS / m.prefs.refresh_ms;
    for _ in 0..ticks - 1 {
        update(&mut m, Msg::Tick);
    }
    assert_eq!(m.toasts.items.len(), 1);
    update(&mut m, Msg::Tick);
    assert!(m.toasts.is_empty(), "gone after 5 s");
}

#[test]
fn reset_needs_the_typed_word() {
    let mut m = chain_b();
    assert!(update(&mut m, ch('S')).is_empty());
    assert!(matches!(m.modal, Modal::ResetTyped { .. }));
    press(&mut m, "rest");
    assert!(update(&mut m, k(KeyCode::Enter)).is_empty(), "wrong word");
    assert_eq!(
        m.modal,
        Modal::ResetTyped {
            stem: "b".into(),
            buffer: String::new()
        }
    );
    press(&mut m, "resett");
    update(&mut m, k(KeyCode::Backspace));
    assert_eq!(
        actions(&update(&mut m, k(KeyCode::Enter))),
        [Action::Reset("b".into())]
    );
    assert_eq!(m.modal, Modal::None);
    update(&mut m, ch('S'));
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.modal, Modal::None);
}

#[test]
fn d_asks_then_downs_or_quits() {
    let mut m = chain_b();
    update(&mut m, ch('d'));
    assert_eq!(m.modal, Modal::DownConfirm);
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.modal, Modal::None, "Enter = No");
    update(&mut m, ch('d'));
    assert_eq!(actions(&update(&mut m, ch('y'))), [Action::DownAll]);
    let mut m = minimal(AttachMode::Up);
    update(&mut m, ch('d'));
    assert_eq!(
        update(&mut m, ch('y')),
        vec![Cmd::Exit(Outcome::StopAll)],
        "attached up: like q then y"
    );
}

// --- script menu and form -------------------------------------------------------

#[test]
fn colon_opens_the_menu_and_runs_a_script_through_the_form() {
    let mut m = chain_b();
    assert_eq!(update(&mut m, ch(':')), vec![Cmd::LoadCatalog]);
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    let Modal::ScriptMenu(menu) = &m.modal else {
        panic!("{:?}", m.modal)
    };
    let names: Vec<_> = menu
        .visible(m.catalog.as_deref().unwrap())
        .iter()
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(names, ["start", "create-test-user", "flaky", "needs-api"]);
    // Down to create-test-user, Enter: its form (it has args).
    update(&mut m, k(KeyCode::Down));
    assert!(update(&mut m, k(KeyCode::Enter)).is_empty());
    let Modal::ScriptForm(f) = &m.modal else {
        panic!("{:?}", m.modal)
    };
    assert_eq!(f.script, "create-test-user");
    // Required email empty: inline error, nothing sent.
    assert!(update(&mut m, k(KeyCode::Enter)).is_empty());
    let Modal::ScriptForm(f) = &m.modal else {
        panic!()
    };
    assert!(f.fields[0].error.as_deref().unwrap().contains("required"));
    // Type it, run: the split pane subscribes first, then run_script.
    press(&mut m, "a@b.c");
    let cmds = update(&mut m, k(KeyCode::Enter));
    let mut args = Map::new();
    args.insert("email".into(), json!("a@b.c"));
    args.insert("role".into(), json!("admin"));
    let run = Action::RunScript {
        stem: Some("b".into()),
        script: "create-test-user".into(),
        args,
    };
    assert_eq!(cmds.last(), Some(&Cmd::Action(run.clone())));
    assert!(matches!(cmds[0], Cmd::SubscribeLogs(_)), "{cmds:?}");
    assert!(m.log_pane.visible);
    assert_eq!(m.modal, Modal::None);
    update(
        &mut m,
        result(
            run,
            Ok(json!({"run_id": "r1", "stem": "b", "script": "create-test-user"})),
        ),
    );
    assert_eq!(m.script_runs[0].run_id.as_deref(), Some("r1"));
    // Another run's finish is ignored; ours becomes a toast.
    update(
        &mut m,
        script_event(
            40,
            "script.finished",
            json!({"script": "create-test-user", "run_id": "r0", "ok": true, "duration_ms": 5}),
        ),
    );
    assert!(m.toasts.is_empty());
    update(
        &mut m,
        script_event(
            41,
            "script.finished",
            json!({"script": "create-test-user", "run_id": "r1", "ok": true, "duration_ms": 1234}),
        ),
    );
    assert_eq!(
        m.toasts.items[0].lines(false),
        ["✓ create-test-user finished in 1.2s"]
    );
    assert!(m.script_runs.is_empty());
}

#[test]
fn menu_filters_and_runs_scripts_without_args_at_once() {
    let mut m = chain_b();
    update(&mut m, ch(':'));
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    press(&mut m, "flk");
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        actions(&cmds),
        [Action::RunScript {
            stem: Some("b".into()),
            script: "flaky".into(),
            args: Map::new()
        }]
    );
    // A failed run: ✗ with the exit code.
    update(
        &mut m,
        script_event(
            50,
            "script.finished",
            json!({"script": "flaky", "ok": false, "exit": 3, "duration_ms": 10}),
        ),
    );
    assert_eq!(m.toasts.items[0].lines(false)[0], "✗ flaky failed: exit 3");
    // Esc clears the filter first, then closes.
    update(&mut m, ch(':'));
    press(&mut m, "zz");
    update(&mut m, k(KeyCode::Esc));
    assert!(matches!(&m.modal, Modal::ScriptMenu(x) if x.filter.is_empty()));
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.modal, Modal::None);
}

#[test]
fn workspace_scripts_show_the_workspace_log() {
    let mut m = chain_b();
    update(&mut m, ch(':'));
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    press(&mut m, "needs");
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.log_focus.as_deref(), Some("_workspace"));
    assert!(
        matches!(&cmds[0], Cmd::SubscribeLogs(s) if s.stems == ["_workspace"]),
        "{cmds:?}"
    );
    // Moving the selection gives the pane back to the selected stem.
    update(&mut m, ch('j'));
    assert!(m.log_focus.is_none());
}

// --- palette -------------------------------------------------------------------

#[test]
fn palette_runs_the_best_match() {
    let mut m = chain();
    assert_eq!(update(&mut m, ctrl('p')), vec![Cmd::LoadCatalog]);
    assert!(matches!(m.modal, Modal::Palette(_)));
    press(&mut m, "rest b");
    let top = crate::actions::palette_matches(
        &m,
        match &m.modal {
            Modal::Palette(p) => p,
            _ => unreachable!(),
        },
    );
    assert_eq!(top[0].label, "restart b");
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        actions(&cmds),
        [Action::Restart {
            stem: "b".into(),
            build: false
        }]
    );
    assert_eq!(m.selected.as_deref(), Some("b"));
    assert_eq!(m.modal, Modal::None);
    // Views, and Down to the second match.
    update(&mut m, ctrl('p'));
    press(&mut m, "view logs d");
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.view, ViewKind::Logs);
    assert_eq!(m.selected.as_deref(), Some("d"));
    update(&mut m, ctrl('p'));
    press(&mut m, "stop c");
    update(&mut m, k(KeyCode::Down));
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        actions(&cmds),
        [Action::Stop {
            stem: "c".into(),
            cascade: true
        }],
        "the second match is the cascade stop; actions run from any view"
    );
    assert_eq!(m.view, ViewKind::Logs);
    // Scripts appear once the catalogue is loaded.
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    update(&mut m, ctrl('p'));
    press(&mut m, "run flaky");
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(actions(&cmds).len(), 1);
    update(&mut m, ctrl('p'));
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(m.modal, Modal::None);
}

// --- config reload (33) ----------------------------------------------------------

#[test]
fn config_changed_toast_applies_and_shows_the_plan() {
    let mut m = chain_b();
    let plan = json!({"plan": {"stems": [
        {"name": "a", "action": "restart_required", "hot": false},
        {"name": "b", "action": "unchanged", "hot": true},
        {"name": "c", "action": "health_changed", "hot": true}
    ], "workspace": [], "catalog_changed": false}});
    update(&mut m, script_event(60, "config.changed", plan));
    let t = m.toasts.config_changed().unwrap();
    assert_eq!(
        t.lines(false),
        [
            "• config changed: 2 stems affected",
            "  a apply · v view · Esc dismiss"
        ]
    );
    assert_eq!(update(&mut m, ch('v')), vec![Cmd::LoadPlan]);
    assert_eq!(m.modal, Modal::Plan(None));
    update(
        &mut m,
        Msg::Rpc(RpcResult::Plan(Ok(vec!["a  restart_required".into()]))),
    );
    assert!(matches!(&m.modal, Modal::Plan(Some(Ok(l))) if l.len() == 1));
    assert_eq!(actions(&update(&mut m, ch('a'))), [Action::ConfigApply]);
    assert!(m.toasts.config_changed().is_none());
    // `a` without the toast is not an action key.
    assert!(update(&mut m, ch('a')).is_empty());
    update(
        &mut m,
        script_event(61, "config.changed", json!({"plan": {"stems": []}})),
    );
    assert_eq!(actions(&update(&mut m, ch('a'))), [Action::ConfigApply]);
    update(&mut m, script_event(62, "config.changed", json!({})));
    update(&mut m, script_event(63, "config.applied", json!({})));
    assert!(m.toasts.config_changed().is_none());
}

// --- frames ------------------------------------------------------------------------

fn frame(m: &Model, name: &str) {
    crate::assert_frame!(m, name, 80, 24);
}

#[test]
fn frame_stop_confirm() {
    let mut m = chain_b();
    update(
        &mut m,
        result(
            Action::Stop {
                stem: "b".into(),
                cascade: false,
            },
            Err(has_dependants()),
        ),
    );
    frame(&m, "modal-stop-confirm");
}

#[test]
fn frame_down_and_reset() {
    let mut m = chain_b();
    update(&mut m, ch('d'));
    frame(&m, "modal-down-confirm");
    let mut m = chain_b();
    update(&mut m, ch('S'));
    press(&mut m, "res");
    frame(&m, "modal-reset-typed");
}

#[test]
fn frame_script_menu_and_form() {
    let mut m = chain_b();
    update(&mut m, ch(':'));
    let text = render_text(&m, 80, 24);
    assert!(text.contains("loading"), "{text}");
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    update(&mut m, k(KeyCode::Down));
    frame(&m, "modal-script-menu");
    update(&mut m, k(KeyCode::Enter));
    update(&mut m, k(KeyCode::Enter));
    frame(&m, "modal-script-form-error");
}

#[test]
fn frame_palette() {
    let mut m = chain();
    update(&mut m, ctrl('p'));
    press(&mut m, "rest");
    frame(&m, "modal-palette");
}

#[test]
fn frame_error_details_plan_and_toasts() {
    let mut m = chain_b();
    update(&mut m, result(Action::Start("b".into()), Ok(json!({}))));
    let e = Error::new(ErrorCode::UnknownStem, "no stem `zz`").with_hint("see `stems status`");
    update(&mut m, result(Action::Start("zz".into()), Err(e)));
    frame(&m, "toasts");
    update(&mut m, ch('e'));
    frame(&m, "modal-error-details");
    let mut m = chain_b();
    update(
        &mut m,
        script_event(60, "config.changed", json!({"plan": {"stems": []}})),
    );
    update(&mut m, ch('v'));
    update(
        &mut m,
        Msg::Rpc(RpcResult::Plan(Ok(vec![
            "a                restart_required (env)".into(),
            "workspace        profiles_changed".into(),
        ]))),
    );
    frame(&m, "modal-plan");
}

#[test]
fn frame_paused_marker_in_table_and_graph() {
    let paused = || Some(serde_json::from_value(json!({"paused": true, "rules": 1})).unwrap());
    let mut m = chain_b();
    m.stems[1].watch = paused();
    frame(&m, "table-watch-paused");
    let mut g = graph_model();
    g.stems[1].watch = paused();
    let text = render_text(&g, 80, 24);
    assert!(text.contains('⏸'), "{text}");
    frame(&g, "graph-watch-paused");
    g.ascii = true;
    m.ascii = true;
    assert!(render_text(&g, 80, 24).contains("||"));
    assert!(render_text(&m, 80, 24).contains("b ||"));
}
