//! Reducer tests and frame goldens of the TUI redesign: the action bar,
//! the Detail sections (Scripts, Variants, Watchdog) and their rows, the
//! variant picker (`v`), the table's SCRIPTS / VARIANT / WATCH columns and
//! the unhealthy count.

use serde_json::{Value, json};
use stems_api::{StemWatchStatus, VariantChoice, WatchRuleStatus, WatchSummary};

use super::*;
use crate::actions::{Action, MenuEntry};
use crate::detail::DetailRow;

fn actions(cmds: &[Cmd]) -> Vec<Action> {
    cmds.iter()
        .filter_map(|c| match c {
            Cmd::Action(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

fn arg(v: Value) -> stems_config::ScriptArg {
    serde_json::from_value(v).expect("script arg")
}

/// `b`'s scripts: two custom ones (one with args), one lifecycle one.
fn catalog() -> Vec<MenuEntry> {
    use stems_core::scriptargs::ScriptKind;
    vec![
        MenuEntry {
            stem: None,
            name: "needs-api".into(),
            description: Some("A workspace script".into()),
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
        MenuEntry {
            stem: Some("b".into()),
            name: "flaky".into(),
            description: Some("Fails on its first two attempts".into()),
            kind: ScriptKind::Custom,
            args: vec![],
        },
        MenuEntry {
            stem: Some("b".into()),
            name: "create-test-user".into(),
            description: Some("Create a user with a known password".into()),
            kind: ScriptKind::Custom,
            args: vec![
                arg(json!({"name": "email", "type": "string", "required": true,
                    "default": null, "description": "Email address", "values": []})),
                arg(json!({"name": "role", "type": "enum", "required": false,
                    "default": "admin", "description": "Role", "values": ["admin", "user"]})),
            ],
        },
    ]
}

fn choices(active: &str) -> Vec<VariantChoice> {
    [
        ("local", "process"),
        ("slow", "process"),
        ("docker", "docker"),
    ]
    .into_iter()
    .map(|(n, t)| VariantChoice {
        name: n.into(),
        kind: Some(t.into()),
        active: n == active,
    })
    .collect()
}

fn watch_status(paused: bool) -> StemWatchStatus {
    StemWatchStatus {
        name: "b".into(),
        rules: vec![WatchRuleStatus {
            paths: vec!["*.py".into()],
            ignore: vec![],
            action: "restart".into(),
            debounce_ms: 200,
            settle_ms: 0,
            root: "codebase".into(),
            dir: None,
        }],
        paused,
        active: true,
        last_triggered: None,
        pending: false,
        busy: false,
    }
}

/// The chain (table view) where `b` has scripts, variants and a watchdog,
/// selected.
fn rich() -> Model {
    let mut m = Model::new(AttachMode::Attach, Prefs::default(), Some(ViewKind::Table));
    let mut d = daemon();
    d.workspace_name = Some("process-chain".into());
    update(&mut m, Msg::Rpc(RpcResult::Daemon(Box::new(d))));
    let mut b = stem("b", "healthy", "healthy", Some(102), 18302, Some(8));
    b.variant = Some("local".into());
    b.watch = Some(WatchSummary {
        paused: false,
        rules: 1,
    });
    let cmds = update(
        &mut m,
        Msg::Status(Box::new(status(vec![
            stem("a", "healthy", "healthy", Some(101), 18301, Some(9)),
            b,
            stem("c", "starting", "transitioning", Some(103), 18303, Some(1)),
            stem("d", "stopped", "stopped", None, 18304, None),
        ]))),
    );
    assert_eq!(
        cmds,
        vec![Cmd::LoadVariants("b".into())],
        "choices load once"
    );
    assert!(
        update(&mut m, Msg::Tick)
            .iter()
            .all(|c| !matches!(c, Cmd::LoadVariants(_))),
        "not again while pending"
    );
    update(&mut m, Msg::Rpc(RpcResult::Catalog(Ok(catalog()))));
    update(
        &mut m,
        Msg::Rpc(RpcResult::Variants {
            stem: "b".into(),
            result: Ok(choices("local")),
        }),
    );
    update(&mut m, ch('j'));
    assert_eq!(m.selected.as_deref(), Some("b"));
    m
}

/// `rich()` in the Detail view of `b`, its data loaded.
fn rich_detail() -> Model {
    let mut m = rich();
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.view, ViewKind::Detail);
    let mut want = crate::update::detail_cmds("b");
    want.push(Cmd::LoadWatch("b".into()));
    assert_eq!(cmds, want, "a stem with watch rules loads its watchdogs");
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemConfig {
            stem: "b".into(),
            result: Ok(super::stem_config()),
        }),
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::StemEvents {
            stem: "b".into(),
            result: Ok(vec![event(4, "b", "starting", "healthy")]),
        }),
    );
    update(
        &mut m,
        Msg::Rpc(RpcResult::Watch {
            stem: "b".into(),
            result: Ok(watch_status(false)),
        }),
    );
    m
}

fn bar(m: &Model, w: u16) -> String {
    crate::bar::text(m, w)
}

// --- the action bar -------------------------------------------------------------

#[test]
fn action_bar_follows_the_selected_stem() {
    let mut m = rich();
    let text = bar(&m, 120);
    assert_eq!(
        text,
        " ■ x stop  ↻ r restart  : scripts (2+1)  v variant local ▸ slow  p watch on  o editor  ? more"
    );
    // A stem without variants, watch rules or scripts (catalogue loaded):
    // the workspace's script keeps the menu worth opening.
    update(&mut m, ch('k'));
    let text = bar(&m, 120);
    assert_eq!(
        text,
        " ■ x stop  ↻ r restart  : scripts (0+1)  o editor  ? more"
    );
    let scripts = |m: &Model| {
        let segs = crate::bar::segments(m);
        segs.into_iter().find(|s| s.key == ':').unwrap()
    };
    assert!(scripts(&m).enabled, "the workspace has a script");
    // Without workspace scripts: the stem's count alone, dimmed at 0.
    let mut own = m.clone();
    let stems_only: Vec<_> = catalog().into_iter().filter(|e| e.stem.is_some()).collect();
    update(&mut own, Msg::Rpc(RpcResult::Catalog(Ok(stems_only))));
    assert_eq!(scripts(&own).label, "scripts (0)");
    assert!(!scripts(&own).enabled, "dimmed");
    // No stem selected: the workspace's scripts still run.
    own.selected = None;
    assert!(!scripts(&own).enabled);
    m.selected = None;
    assert_eq!(scripts(&m).label, "scripts (1)");
    assert!(scripts(&m).enabled);
    m.selected = Some("a".into());
    // Stopped: start instead of stop / restart.
    update(&mut m, ch('G'));
    assert_eq!(m.selected.as_deref(), Some("d"));
    assert!(bar(&m, 120).starts_with(" ▶ s start  : scripts (0+1)"));
    // `x` then the stem's stop event turns the bar to `s start`.
    update(&mut m, ch('g'));
    update(&mut m, ch('j'));
    update(
        &mut m,
        Msg::Event(Box::new(event(20, "b", "healthy", "stopped"))),
    );
    assert!(bar(&m, 120).starts_with(" ▶ s start  : scripts (2+1)  v variant local ▸ slow"));
    // Paused watchdog.
    let mut st = m.stems.clone();
    st[1].watch = Some(WatchSummary {
        paused: true,
        rules: 1,
    });
    update(&mut m, Msg::Status(Box::new(status(st))));
    assert!(
        bar(&m, 120).contains("p watch ⏸ paused"),
        "{}",
        bar(&m, 120)
    );
    // ASCII: no icons, `>` and `||`.
    m.ascii = true;
    assert!(
        bar(&m, 120)
            .starts_with(" s start  : scripts (2+1)  v variant local > slow  p watch || paused")
    );
}

#[test]
fn action_bar_is_cut_from_the_right_on_narrow_terminals() {
    let m = rich();
    let text = bar(&m, 50);
    assert_eq!(text, " ■ x stop  ↻ r restart  : scripts (2+1) …");
    assert!(crate::view::str_width(&text) <= 50);
    // Every segment fits at 120: no ellipsis.
    assert!(!bar(&m, 120).contains('…'));
    // The frame at 40x10 still draws a bar.
    let frame = render_text(&m, 40, 10);
    assert!(
        frame.lines().nth(8).unwrap().starts_with(" ■ x stop"),
        "{frame}"
    );
}

#[test]
fn action_bar_only_in_stem_views() {
    let mut m = rich();
    for (v, has) in [
        (ViewKind::Table, true),
        (ViewKind::Graph, true),
        (ViewKind::Detail, true),
        (ViewKind::Logs, false),
        (ViewKind::Events, false),
    ] {
        update(&mut m, Msg::SetView(v));
        let text = render_text(&m, 80, 24);
        assert_eq!(
            text.contains(" ■ x stop  ↻ r restart"),
            has,
            "{v:?}\n{text}"
        );
    }
}

#[test]
fn clicking_an_action_bar_segment_presses_its_key() {
    let mut m = rich();
    m.prefs.mouse = true;
    m.size = (120, 40);
    let _ = render_text(&m, 120, 40);
    let hits = m.bar_hits.borrow().clone();
    let (_, row, start, _) = *hits.iter().find(|h| h.0 == 'x').expect("x stop hit");
    assert_eq!(row, 38, "the row above the status bar");
    let click = |col: u16, row: u16| {
        Msg::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let cmds = update(&mut m, click(start + 1, row));
    assert_eq!(
        actions(&cmds),
        [Action::Stop {
            stem: "b".into(),
            cascade: false
        }]
    );
    let (_, _, v, _) = *hits.iter().find(|h| h.0 == 'v').unwrap();
    update(&mut m, click(v, row));
    assert!(matches!(m.modal, Modal::VariantPicker { .. }));
    update(&mut m, k(KeyCode::Esc));
    let (_, _, q, _) = *hits.iter().find(|h| h.0 == '?').unwrap();
    update(&mut m, click(q, row));
    assert!(m.help);
}

// --- the variant picker -----------------------------------------------------------

#[test]
fn v_opens_the_variant_picker_and_switches_after_confirming() {
    let mut m = rich();
    assert!(update(&mut m, ch('v')).is_empty(), "choices already loaded");
    assert_eq!(
        m.modal,
        Modal::VariantPicker {
            stem: "b".into(),
            selected: 0
        },
        "on the active choice"
    );
    // Enter on the active one: a toast, nothing sent.
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(m.modal, Modal::None);
    assert!(
        m.toasts
            .items
            .last()
            .unwrap()
            .text
            .contains("already local")
    );
    update(&mut m, ch('v'));
    update(&mut m, ch('j'));
    update(&mut m, ch('j'));
    update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        m.modal,
        Modal::SwitchConfirm {
            stem: "b".into(),
            from: "local".into(),
            variant: "docker".into(),
            kind: Some("docker".into()),
            running: true,
        }
    );
    let text = render_text(&m, 80, 24);
    assert!(text.contains("restart b as docker? [y/N]"), "{text}");
    // n cancels; y sends switch_variant.
    update(&mut m, ch('n'));
    assert_eq!(m.modal, Modal::None);
    update(&mut m, ch('v'));
    update(&mut m, k(KeyCode::Down));
    update(&mut m, k(KeyCode::Enter));
    let cmds = update(&mut m, ch('y'));
    let a = Action::SwitchVariant {
        stem: "b".into(),
        variant: "slow".into(),
    };
    assert_eq!(actions(&cmds), std::slice::from_ref(&a));
    assert_eq!(a.method(), stems_api::Method::SWITCH_VARIANT);
    assert_eq!(a.params(), json!({"stem": "b", "variant": "slow"}));
    assert_eq!(m.message.as_deref(), Some("switch b to slow…"));
    // The result: a toast, the choices reload, a status refresh.
    let cmds = update(
        &mut m,
        Msg::Rpc(RpcResult::Action {
            action: a,
            result: Ok(json!({"from": "local", "to": "slow",
                "applied": {"applied": [{"stem": "b", "action": "restart", "result": "restarted"}]}})),
        }),
    );
    assert!(cmds.contains(&Cmd::LoadVariants("b".into())), "{cmds:?}");
    assert!(cmds.contains(&Cmd::RefreshStatus), "{cmds:?}");
    assert!(m.variant_choices("b").is_none(), "stale choices dropped");
    assert_eq!(
        m.toasts.items.last().unwrap().text,
        "b: local -> slow (restarted)"
    );
}

#[test]
fn v_on_a_stem_without_variants_says_so() {
    let mut m = rich();
    update(&mut m, ch('k'));
    assert!(update(&mut m, ch('v')).is_empty());
    assert_eq!(m.modal, Modal::None);
    assert!(
        m.toasts
            .items
            .last()
            .unwrap()
            .text
            .contains("a has no variants"),
    );
    // Not loaded yet: the picker opens and asks for them.
    let mut m = rich();
    m.variants.clear();
    m.variants_pending.clear();
    assert_eq!(update(&mut m, ch('v')), vec![Cmd::LoadVariants("b".into())]);
    let text = render_text(&m, 80, 24);
    assert!(text.contains("Variant of b"), "{text}");
    update(
        &mut m,
        Msg::Rpc(RpcResult::Variants {
            stem: "b".into(),
            result: Ok(choices("slow")),
        }),
    );
    assert_eq!(
        m.modal,
        Modal::VariantPicker {
            stem: "b".into(),
            selected: 1
        },
        "the active choice once loaded"
    );
    // Only in the stem views; the palette has it for stems with variants.
    update(&mut m, k(KeyCode::Esc));
    let labels: Vec<String> = crate::actions::palette_items(&m)
        .into_iter()
        .map(|i| i.label)
        .collect();
    assert!(labels.contains(&"switch variant of b".to_string()));
    assert!(!labels.contains(&"switch variant of a".to_string()));
}

// --- the Detail sections ------------------------------------------------------------

#[test]
fn detail_rows_span_scripts_variants_and_the_watchdog() {
    let m = rich_detail();
    assert_eq!(
        crate::detail::rows(&m, "b"),
        [
            DetailRow::Script("create-test-user".into()),
            DetailRow::Script("flaky".into()),
            DetailRow::Script("start".into()),
            DetailRow::Variant("local".into()),
            DetailRow::Variant("slow".into()),
            DetailRow::Variant("docker".into()),
            DetailRow::Watch,
        ]
    );
    // A stem without scripts, variants or watch rules has no rows: j/k
    // scroll as before.
    assert!(crate::detail::rows(&m, "a").is_empty());
}

#[test]
fn j_k_move_over_rows_across_sections_and_enter_acts() {
    let mut m = rich_detail();
    assert_eq!(m.detail_row, 0);
    // Enter on a script with args opens its form.
    update(&mut m, k(KeyCode::Enter));
    assert!(matches!(&m.modal, Modal::ScriptForm(f) if f.script == "create-test-user"));
    update(&mut m, k(KeyCode::Esc));
    // Enter on a script without args runs it.
    update(&mut m, ch('j'));
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        actions(&cmds),
        [Action::RunScript {
            stem: Some("b".into()),
            script: "flaky".into(),
            args: serde_json::Map::new(),
        }]
    );
    // Down into Variants: Enter asks to switch.
    update(&mut m, ch('j'));
    update(&mut m, ch('j'));
    update(&mut m, k(KeyCode::Down));
    assert_eq!(m.detail_row, 4);
    update(&mut m, k(KeyCode::Enter));
    assert!(matches!(&m.modal, Modal::SwitchConfirm { variant, .. } if variant == "slow"));
    update(&mut m, k(KeyCode::Esc));
    // The watchdog row: Enter pauses it (as `p`).
    update(&mut m, ch('j'));
    update(&mut m, ch('j'));
    assert_eq!(m.detail_row, 6);
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert_eq!(
        actions(&cmds),
        [Action::Watch {
            stem: "b".into(),
            pause: true
        }]
    );
    // Past the last row j scrolls; k comes back through the rows.
    let before = m.detail_scroll;
    update(&mut m, ch('j'));
    assert_eq!(m.detail_row, 6);
    assert!(m.detail_scroll > before || crate::view::detail_extent(&m).unwrap().0 <= 21);
    update(&mut m, ch('k'));
    assert_eq!(m.detail_row, 5);
    // Another stem starts on its first row again.
    update(&mut m, ch(']'));
    assert_eq!(m.detail_row, 0);
}

#[test]
fn detail_scroll_follows_the_selected_row() {
    let mut m = rich_detail();
    m.size = (80, 14);
    let (c, h) = crate::view::detail_content(&m).unwrap();
    let watch_line = c.rows.last().unwrap().1;
    assert!(watch_line >= h, "the watchdog row starts below the fold");
    for _ in 0..6 {
        update(&mut m, ch('j'));
    }
    assert_eq!(m.detail_row, 6);
    assert!(
        m.detail_scroll <= watch_line && watch_line < m.detail_scroll + h,
        "row {watch_line} visible in {}..{}",
        m.detail_scroll,
        m.detail_scroll + h
    );
    let text = render_text(&m, 80, 14);
    assert!(text.contains("› watch on"), "{text}");
    for _ in 0..6 {
        update(&mut m, ch('k'));
    }
    assert_eq!(m.detail_row, 0);
    assert_eq!(
        m.detail_scroll, 4,
        "the Scripts heading shows above its first row"
    );
}

#[test]
fn scripts_show_their_last_run_inline() {
    let mut m = rich_detail();
    let ev = |seq: u64, kind: &str, data: Value| {
        Msg::Event(Box::new(
            serde_json::from_value(json!({
                "ts": "2026-09-26T12:00:10Z", "seq": seq, "kind": kind, "stem": "b",
                "actor": "cli:bob", "data": data
            }))
            .expect("event"),
        ))
    };
    update(
        &mut m,
        ev(
            30,
            "script.started",
            json!({"script": "flaky", "run_id": "r1"}),
        ),
    );
    assert!(render_text(&m, 120, 40).contains("flaky            custom    running…"));
    update(
        &mut m,
        ev(
            31,
            "script.finished",
            json!({"script": "flaky", "run_id": "r1", "ok": false, "exit": 3}),
        ),
    );
    assert!(render_text(&m, 120, 40).contains("flaky            custom    ✗ exit 3"));
    update(
        &mut m,
        ev(
            32,
            "script.finished",
            json!({"script": "create-test-user", "ok": true, "duration_ms": 1200}),
        ),
    );
    let text = render_text(&m, 120, 40);
    assert!(
        text.contains("create-test-user custom    ✓ 1.2s (email*, role)"),
        "{text}"
    );
}

#[test]
fn watch_events_reload_the_watchdog_section() {
    let mut m = rich_detail();
    let e: stems_api::Event = serde_json::from_value(json!({
        "ts": "2026-09-26T12:00:10Z", "seq": 40, "kind": "watch.triggered", "stem": "b",
        "actor": "daemon", "data": {"paths": ["app.py"], "action": "restart"}
    }))
    .unwrap();
    let cmds = update(&mut m, Msg::Event(Box::new(e)));
    assert_eq!(cmds, vec![Cmd::LoadWatch("b".into())]);
    let text = render_text(&m, 80, 40);
    assert!(text.contains("*.py → restart · debounce 200ms"), "{text}");
    assert!(text.contains("watch on · last trigger never"), "{text}");
}

#[test]
fn catalog_reloads_on_tools_changed_and_config_applied() {
    let mut m = rich();
    let ev = |kind: &str| {
        Msg::Event(Box::new(
            serde_json::from_value(json!({
                "ts": "2026-09-26T12:00:10Z", "seq": 50, "kind": kind,
                "actor": "daemon", "data": {}
            }))
            .unwrap(),
        ))
    };
    assert_eq!(update(&mut m, ev("tools.changed")), vec![Cmd::LoadCatalog]);
    let cmds = update(&mut m, ev("config.applied"));
    assert!(cmds.contains(&Cmd::LoadCatalog));
    assert!(m.variants.is_empty(), "choices reload with the next status");
}

// --- the table's extra columns ---------------------------------------------------

#[test]
fn wide_tables_show_scripts_variant_and_watch() {
    let mut m = rich();
    let wide = render_text(&m, 120, 20);
    let header = wide.lines().nth(1).unwrap();
    assert!(header.contains("SCRIPTS VARIANT WATCH"), "{header}");
    let row_b = wide.lines().find(|l| l.starts_with("› b")).unwrap();
    assert!(row_b.ends_with("2       local   on"), "{row_b}");
    let row_a = wide.lines().find(|l| l.starts_with("  a")).unwrap();
    assert!(row_a.ends_with("0       -       -"), "{row_a}");
    for w in [80, 100, 119] {
        let text = render_text(&m, w, 20);
        assert!(!text.contains("SCRIPTS"), "{w} columns");
    }
    // A paused watchdog shows ⏸; the scripts count waits for the catalogue.
    m.catalog = None;
    let mut st = m.stems.clone();
    st[1].watch = Some(WatchSummary {
        paused: true,
        rules: 1,
    });
    update(&mut m, Msg::Status(Box::new(status(st))));
    let wide = render_text(&m, 120, 20);
    let row_b = wide.lines().find(|l| l.starts_with("› b")).unwrap();
    assert!(row_b.ends_with("-       local   ⏸"), "{row_b}");
}

// --- unhealthy counted apart -----------------------------------------------------

#[test]
fn unhealthy_is_counted_apart_from_failed() {
    let mut m = chain();
    update(
        &mut m,
        Msg::Event(Box::new(event(8, "a", "healthy", "unhealthy"))),
    );
    update(
        &mut m,
        Msg::Event(Box::new(event(9, "d", "stopped", "failed"))),
    );
    assert_eq!(m.summary.unhealthy, 1);
    assert_eq!(m.summary.failed, 1);
    let text = render_text(&m, 80, 24);
    assert!(text.contains("✗1 ↯1"), "{text}");
}

// --- frames --------------------------------------------------------------------------

#[test]
fn frame_action_bar_table_graph_detail() {
    let mut m = rich();
    crate::assert_frame!(m, "bar-table");
    update(&mut m, Msg::SetView(ViewKind::Graph));
    update(&mut m, Msg::Rpc(super::chain_graph()));
    crate::assert_frame!(m, "bar-graph");
}

#[test]
fn frame_detail_sections() {
    let mut m = rich_detail();
    crate::assert_frame!(m, "detail-sections");
    for _ in 0..4 {
        update(&mut m, ch('j'));
    }
    crate::assert_frame!(m, "detail-sections-variant-row", 80, 24);
    update(&mut m, k(KeyCode::Enter));
    crate::assert_frame!(m, "switch-confirm", 80, 24);
    update(&mut m, k(KeyCode::Esc));
    update(&mut m, ch('v'));
    crate::assert_frame!(m, "variant-picker", 80, 24);
}

// --- the Scripts view ---------------------------------------------------------------

/// `rich()` in the Scripts view.
fn rich_scripts() -> Model {
    let mut m = rich();
    assert_eq!(update(&mut m, ch('6')), vec![Cmd::LoadCatalog]);
    assert_eq!(m.view, ViewKind::Scripts);
    m
}

fn listed(m: &Model) -> Vec<(Option<&str>, &str)> {
    crate::scripts::entries(m)
        .into_iter()
        .map(|e| (e.stem.as_deref(), e.name.as_str()))
        .collect()
}

#[test]
fn scripts_view_lists_the_workspace_then_each_stem() {
    let m = rich_scripts();
    assert_eq!(
        listed(&m),
        vec![
            (None, "needs-api"),
            (Some("b"), "create-test-user"),
            (Some("b"), "flaky"),
            (Some("b"), "start"),
        ]
    );
    assert!(
        !crate::view::has_bar(&m),
        "the stem actions are not its keys"
    );
    let text = render_text(&m, 80, 24);
    assert!(text.contains("6 [Scripts]"), "{text}");
    assert!(text.contains("workspace · global"), "{text}");
    assert!(text.contains("› needs-api"), "{text}");
}

#[test]
fn scripts_view_selection_follows_into_the_stem() {
    let mut m = rich_scripts();
    assert_eq!(m.selected.as_deref(), Some("b"));
    update(&mut m, ch('['));
    assert_eq!(m.selected.as_deref(), Some("a"));
    // A workspace script leaves the stem selection alone.
    update(&mut m, ch('k'));
    assert_eq!(m.scripts_view.selected, 0);
    assert_eq!(m.selected.as_deref(), Some("a"));
    // Onto b's first script: b is selected everywhere.
    update(&mut m, ch('j'));
    assert_eq!(m.scripts_view.selected, 1);
    assert_eq!(m.selected.as_deref(), Some("b"));
    update(&mut m, ch('G'));
    assert_eq!(crate::scripts::selected(&m).unwrap().name, "start");
    update(&mut m, ch('j'));
    assert_eq!(m.scripts_view.selected, 3, "stays on the last row");
    update(&mut m, ch('g'));
    assert_eq!(m.scripts_view.selected, 0);
}

#[test]
fn scripts_view_enter_runs_or_opens_the_form() {
    let mut m = rich_scripts();
    // The workspace script runs at once; its output owns the split pane.
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert!(cmds.contains(&Cmd::Action(Action::RunScript {
        stem: None,
        script: "needs-api".into(),
        args: serde_json::Map::new(),
    })));
    assert!(m.log_pane.visible);
    assert_eq!(
        m.log_focus.as_deref(),
        Some(crate::actions::WORKSPACE_LOG_STEM)
    );
    assert!(
        crate::view::body_areas(&m).unwrap().1.is_some(),
        "the split pane shows under the Scripts view"
    );
    // A script with `args` opens its form first.
    update(&mut m, ch('j'));
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert!(cmds.iter().all(|c| !matches!(c, Cmd::Action(_))));
    assert!(matches!(&m.modal, Modal::ScriptForm(f) if f.script == "create-test-user"));
}

#[test]
fn scripts_view_filters_by_script_or_owner() {
    let mut m = rich_scripts();
    update(&mut m, ch('/'));
    for c in "flk".chars() {
        update(&mut m, ch(c));
    }
    assert!(m.scripts_view.editing);
    assert_eq!(m.view, ViewKind::Scripts, "typed text is not a view key");
    assert_eq!(listed(&m), vec![(Some("b"), "flaky")]);
    update(&mut m, k(KeyCode::Enter));
    assert!(!m.scripts_view.editing);
    let cmds = update(&mut m, k(KeyCode::Enter));
    assert!(cmds.contains(&Cmd::Action(Action::RunScript {
        stem: Some("b".into()),
        script: "flaky".into(),
        args: serde_json::Map::new(),
    })));
    // By owner; Esc clears.
    update(&mut m, ch('/'));
    for c in "workspace".chars() {
        update(&mut m, ch(c));
    }
    assert_eq!(listed(&m), vec![(None, "needs-api")]);
    update(&mut m, k(KeyCode::Esc));
    assert_eq!(listed(&m).len(), 4);
}

#[test]
fn frame_scripts_view() {
    let mut m = rich_scripts();
    let ev = |seq: u64, kind: &str, stem: Value, data: Value| {
        Msg::Event(Box::new(
            serde_json::from_value(json!({
                "ts": "2026-09-26T12:00:10Z", "seq": seq, "kind": kind,
                "actor": "cli:me", "stem": stem, "data": data
            }))
            .unwrap(),
        ))
    };
    update(
        &mut m,
        ev(
            60,
            "script.finished",
            Value::Null,
            json!({"script": "needs-api", "ok": true, "duration_ms": 1200}),
        ),
    );
    update(
        &mut m,
        ev(61, "script.started", json!("b"), json!({"script": "flaky"})),
    );
    update(&mut m, ch('j'));
    crate::assert_frame!(m, "scripts-view");
}

// --- `up.finished` -------------------------------------------------------------------

#[test]
fn up_finished_of_another_actor_is_a_toast() {
    let mut m = rich();
    let ev = |actor: &str, data: Value| {
        Msg::Event(Box::new(
            serde_json::from_value(json!({
                "ts": "2026-09-26T12:00:10Z", "seq": 70, "kind": "up.finished",
                "actor": actor, "data": data
            }))
            .unwrap(),
        ))
    };
    let ok = json!({"started": ["a", "b"], "failed": [], "skipped": [], "ok": true});
    update(&mut m, ev("tui:me", ok.clone()));
    assert!(
        m.toasts.is_empty(),
        "the dashboard's own `u` reports itself"
    );
    update(&mut m, ev("cli:me", ok));
    let text = render_text(&m, 80, 24);
    assert!(text.contains("up: 2 ready"), "{text}");
    update(
        &mut m,
        ev(
            "cli:me",
            json!({"started": ["a"], "failed": ["b"], "skipped": ["d"], "ok": false}),
        ),
    );
    let text = render_text(&m, 80, 24);
    assert!(text.contains("up: 1 ready, 1 failed, 1 skipped"), "{text}");
}
