//! The reducer: `update(&mut Model, Msg) -> Vec<Cmd>`. Pure: no I/O, no
//! clock; every effect is a returned [`Cmd`].

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use stems_api::EventKind;
use stems_core::StemState;

use crate::actions::{Action, Palette, PaletteTarget, ScriptMenu, palette_matches};
use crate::form::{FormAction, ScriptForm};
use crate::logs::{LOG_REPLAY, LogJump, LogTarget, PaneAction};
use crate::model::{
    AttachMode, Cmd, DetailData, EVENT_RING, LogSubscription, Modal, Model, Msg, Outcome,
    RpcResult, ScriptRun, SignalKind, ViewKind,
};
use crate::toast::{Toast, ToastKind};

/// Detail events shown.
pub const DETAIL_EVENTS: usize = 10;

/// Commands to run once at start-up.
pub fn init(model: &mut Model) -> Vec<Cmd> {
    model.refresh_pending = true;
    vec![Cmd::LoadDaemon, Cmd::RefreshStatus]
}

/// Apply `msg` to `model`; returns the effects to run.
pub fn update(model: &mut Model, msg: Msg) -> Vec<Cmd> {
    if model.exit.is_some() {
        return Vec::new();
    }
    let mut cmds = step(model, msg);
    if model.exit.is_none() {
        cmds.extend(sync_logs(model));
    }
    cmds
}

/// What the log pane should show now: the Logs view, or the split pane
/// under Table/Graph/Detail; `None` when neither is on screen.
pub fn log_target(model: &Model) -> Option<LogTarget> {
    let split = model.log_pane.visible
        && matches!(
            model.view,
            ViewKind::Table | ViewKind::Graph | ViewKind::Detail
        );
    if model.view != ViewKind::Logs && !split {
        return None;
    }
    if model.view == ViewKind::Logs && model.log_pane.merged {
        return Some(LogTarget {
            stem: None,
            since: None,
        });
    }
    let stem = if model.view == ViewKind::Logs {
        model.selected.clone()?
    } else {
        model.log_focus.clone().or_else(|| model.selected.clone())?
    };
    let since = model
        .log_pane
        .jump
        .as_ref()
        .filter(|j| j.stem == stem)
        .map(|j| j.since.clone());
    Some(LogTarget {
        stem: Some(stem),
        since,
    })
}

/// (Re)subscribe the log pane when what it should show changed. The
/// previous subscription keeps feeding the pane while it is hidden, so
/// coming back to the same stem keeps its lines.
fn sync_logs(model: &mut Model) -> Vec<Cmd> {
    let Some(t) = log_target(model) else {
        return Vec::new();
    };
    if model.log_pane.source.as_ref() == Some(&t) {
        return Vec::new();
    }
    let at = model
        .log_pane
        .jump
        .as_ref()
        .filter(|j| t.since.is_some() && t.stem.as_deref() == Some(j.stem.as_str()))
        .map(|j| j.at);
    if at.is_none() {
        model.log_pane.jump = None;
    }
    let generation = model.log_pane.reset(Some(t.clone()));
    if let Some(at) = at {
        model.log_pane.anchor_at = Some(at);
        model.log_pane.follow = false;
    }
    vec![Cmd::SubscribeLogs(LogSubscription {
        generation,
        stems: t.stem.into_iter().collect(),
        tail: if t.since.is_none() {
            Some(LOG_REPLAY)
        } else {
            None
        },
        since: t.since,
    })]
}

fn step(model: &mut Model, msg: Msg) -> Vec<Cmd> {
    match msg {
        Msg::Key(k) => key(model, k),
        Msg::Mouse(m) => {
            if model.prefs.mouse
                && model.view == ViewKind::Table
                && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && m.row >= 2
            {
                let i = usize::from(m.row - 2);
                let name = model.visible().get(i).map(|s| s.name.clone());
                if name.is_some() {
                    model.selected = name;
                    model.touched = true;
                }
            }
            Vec::new()
        }
        Msg::Tick => {
            model.ticks += 1;
            model.clock_ms += model.prefs.refresh_ms;
            model.toasts.expire(model.clock_ms);
            let every = (1000 / model.prefs.refresh_ms.max(1)).max(1);
            if model.ticks.is_multiple_of(every) {
                refresh(model)
            } else {
                Vec::new()
            }
        }
        Msg::Event(e) => {
            let mut cmds = Vec::new();
            if e.kind == EventKind::STEM_STATE
                && let (Some(stem), Some(to)) = (&e.stem, &e.to)
                && let Ok(state) = to.parse::<StemState>()
                && let Some(s) = model.stems.iter_mut().find(|s| &s.name == stem)
            {
                s.state = state;
                s.glyph = state.glyph(false);
                s.reason = e.reason.clone();
                model.summary = stems_api::StatusSummary::of(&model.stems);
                cmds.extend(refresh(model));
            } else if e.kind == EventKind::DAEMON_STOPPING {
                model.message = Some("daemon stopping".into());
            } else if e.kind == EventKind::SCRIPT_FINISHED {
                script_finished(model, &e);
            } else if e.kind == EventKind::CONFIG_CHANGED {
                config_changed(model, &e);
            } else if e.kind == EventKind::CONFIG_APPLIED {
                model.toasts.clear_config();
            } else if matches!(
                e.kind.as_str(),
                "watch.paused" | "watch.resumed" | "stem.restarting"
            ) {
                cmds.extend(refresh(model));
            }
            if let Some(d) = model.detail.as_mut()
                && e.stem.as_deref() == Some(d.stem.as_str())
                && !d.events.iter().any(|x| x.seq == e.seq)
            {
                d.events.push((*e).clone());
                let n = d.events.len();
                d.events.drain(..n.saturating_sub(DETAIL_EVENTS));
            }
            model.events.push_back(*e);
            while model.events.len() > EVENT_RING {
                model.events.pop_front();
            }
            cmds
        }
        Msg::Status(st) => {
            model.refresh_pending = false;
            crate::model::MetricHistory::feed(&mut model.metrics, &st);
            model.stems = st.stems;
            model.summary = st.summary;
            model.loaded = true;
            if std::mem::take(&mut model.auto_view) {
                model.view = if model.stems.len() > 1 {
                    ViewKind::Graph
                } else {
                    ViewKind::Table
                };
            }
            fix_selection(model);
            let mut cmds = ensure_graph(model);
            cmds.extend(ensure_detail(model));
            cmds
        }
        Msg::Rpc(r) => {
            match r {
                RpcResult::Daemon(d) => {
                    model.daemon_pid = Some(d.info.pid);
                    if let Some(n) = d.workspace_name {
                        model.workspace = n;
                    }
                }
                RpcResult::StemConfig { stem, result } => {
                    if let Some(d) = model.detail.as_mut().filter(|d| d.stem == stem) {
                        d.config = Some(result);
                    }
                }
                RpcResult::StemHealth { stem, result } => {
                    if let Some(d) = model.detail.as_mut().filter(|d| d.stem == stem) {
                        d.health = Some(result);
                    }
                }
                RpcResult::StemEvents { stem, result } => {
                    if let Some(d) = model.detail.as_mut().filter(|d| d.stem == stem) {
                        match result {
                            Ok(mut evs) => {
                                for e in std::mem::take(&mut d.events) {
                                    if !evs.iter().any(|x| x.seq == e.seq) {
                                        evs.push(e);
                                    }
                                }
                                evs.sort_by_key(|e| e.seq);
                                let n = evs.len();
                                evs.drain(..n.saturating_sub(DETAIL_EVENTS));
                                d.events = evs;
                            }
                            Err(e) => model.message = Some(format!("events: {e}")),
                        }
                    }
                }
                RpcResult::Graph { names, result } => {
                    match result {
                        Ok(stems) => model.graph.set(names, &stems),
                        Err(e) => {
                            model.graph.names = names;
                            model.graph.pending = false;
                            model.graph.error = Some(e);
                        }
                    }
                    graph_selection(model);
                }
                RpcResult::Events(Ok(evs)) => {
                    let mut all: Vec<_> = std::mem::take(&mut model.events).into();
                    for e in evs {
                        if !all.iter().any(|x| x.seq == e.seq) {
                            all.push(e);
                        }
                    }
                    all.sort_by_key(|e| e.seq);
                    let n = all.len();
                    all.drain(..n.saturating_sub(EVENT_RING));
                    model.events = all.into();
                }
                RpcResult::Events(Err(e)) => model.message = Some(format!("events: {e}")),
                RpcResult::Catalog(Ok(entries)) => {
                    model.catalog = Some(entries);
                    if let Modal::ScriptMenu(m) = &mut model.modal {
                        m.selected = 0;
                    }
                }
                RpcResult::Catalog(Err(e)) => {
                    model.toasts.push(Toast::failure(
                        format!("script_catalog: {e}"),
                        model.clock_ms,
                    ));
                    if matches!(model.modal, Modal::ScriptMenu(_)) && model.catalog.is_none() {
                        model.modal = Modal::None;
                    }
                }
                RpcResult::Action { action, result } => {
                    return action_result(model, action, result);
                }
                RpcResult::Editor { stem, result } => {
                    let now = model.clock_ms;
                    match result {
                        Ok(dir) => model
                            .toasts
                            .push(Toast::ok(format!("editor closed ({stem}: {dir})"), now)),
                        Err(e) => model
                            .toasts
                            .push(Toast::failure(format!("open {stem}: {e}"), now)),
                    }
                }
                RpcResult::Plan(r) => {
                    if let Modal::Plan(p) = &mut model.modal {
                        *p = Some(r);
                    }
                }
                RpcResult::Failed { what, message } => {
                    if what == "status" {
                        model.refresh_pending = false;
                    }
                    model.message = Some(format!("{what}: {message}"));
                }
            }
            Vec::new()
        }
        Msg::Resize(w, h) => {
            model.size = (w, h);
            Vec::new()
        }
        Msg::Signal(SignalKind::Interrupt) => quit_request(model),
        Msg::Signal(SignalKind::Terminate | SignalKind::Hangup) => {
            let outcome = match model.mode {
                AttachMode::Up => Outcome::StopAll,
                AttachMode::Attach if model.owns_daemon => Outcome::StopAll,
                AttachMode::Attach => Outcome::Detach,
            };
            exit(model, outcome)
        }
        Msg::Disconnected => {
            model.message = Some("the daemon stopped".into());
            exit(model, Outcome::Detach)
        }
        Msg::SetView(v) => set_view(model, v),
        Msg::Log { generation, record } => {
            if generation == model.log_pane.generation {
                model.log_pane.push(*record);
            }
            Vec::new()
        }
    }
}

fn exit(model: &mut Model, outcome: Outcome) -> Vec<Cmd> {
    model.exit = Some(outcome);
    vec![Cmd::Exit(outcome)]
}

fn refresh(model: &mut Model) -> Vec<Cmd> {
    if model.refresh_pending {
        return Vec::new();
    }
    model.refresh_pending = true;
    vec![Cmd::RefreshStatus]
}

/// Keep the selection on a visible stem (the first one by default).
fn fix_selection(model: &mut Model) {
    let visible = model.visible();
    let ok = model
        .selected
        .as_deref()
        .is_some_and(|n| visible.iter().any(|s| s.name == n));
    if !ok {
        let first = visible.first().map(|s| s.name.clone());
        model.selected = first;
    }
}

/// In the detail view, load the selected stem's data if not loaded yet.
fn ensure_detail(model: &mut Model) -> Vec<Cmd> {
    if model.view != ViewKind::Detail {
        return Vec::new();
    }
    let Some(stem) = model.selected.clone() else {
        return Vec::new();
    };
    if model.detail.as_ref().is_some_and(|d| d.stem == stem) {
        return Vec::new();
    }
    let events = model
        .stem_events(&stem, DETAIL_EVENTS)
        .into_iter()
        .cloned()
        .collect();
    model.detail = Some(DetailData {
        stem: stem.clone(),
        config: None,
        events,
        health: None,
    });
    detail_cmds(&stem)
}

/// The commands loading the detail view of `stem`.
pub fn detail_cmds(stem: &str) -> Vec<Cmd> {
    vec![
        Cmd::LoadStemConfig(stem.to_string()),
        Cmd::LoadStemEvents(stem.to_string()),
        Cmd::LoadStemHealth(stem.to_string()),
    ]
}

fn set_view(model: &mut Model, v: ViewKind) -> Vec<Cmd> {
    model.view = v;
    let mut cmds = ensure_graph(model);
    if v == ViewKind::Events && !model.events_view.loaded {
        model.events_view.loaded = true;
        cmds.push(Cmd::LoadEvents);
    }
    graph_selection(model);
    cmds.extend(ensure_detail(model));
    cmds
}

/// In the graph view, (re)load the graph when the set of stems changed.
fn ensure_graph(model: &mut Model) -> Vec<Cmd> {
    if model.view != ViewKind::Graph || !model.loaded || model.graph.pending {
        return Vec::new();
    }
    let mut names: Vec<String> = model.stems.iter().map(|s| s.name.clone()).collect();
    names.sort();
    if (model.graph.layout.is_some() || model.graph.error.is_some()) && model.graph.names == names {
        return Vec::new();
    }
    model.graph.pending = true;
    model.graph.names = names.clone();
    vec![Cmd::LoadGraph(names)]
}

/// In the graph view, start on the first box until the user moves, and
/// keep the selection on a box.
fn graph_selection(model: &mut Model) {
    if model.view != ViewKind::Graph {
        return;
    }
    let Some(l) = model.graph.shown() else {
        return;
    };
    let on_box = model
        .selected
        .as_deref()
        .is_some_and(|s| l.node(s).is_some());
    if (!model.touched || !on_box)
        && let Some(n) = crate::graph::first(l)
    {
        model.selected = Some(l.nodes[n].name.clone());
    }
}

/// A graph key; `None` when the key is not a graph key.
fn graph_key(model: &mut Model, code: KeyCode) -> Option<Vec<Cmd>> {
    use crate::graph::{Move, step};
    let mv = match code {
        KeyCode::Char('h') | KeyCode::Left => Some(Move::Left),
        KeyCode::Char('l') | KeyCode::Right => Some(Move::Right),
        KeyCode::Char('j') | KeyCode::Down => Some(Move::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Move::Up),
        _ => None,
    };
    if let Some(mv) = mv {
        if let Some(l) = model.graph.shown() {
            let from = model.selected.as_deref().and_then(|s| l.node(s));
            if let Some(n) = step(l, from, mv) {
                model.selected = Some(l.nodes[n].name.clone());
                model.touched = true;
            }
        }
        return Some(Vec::new());
    }
    match code {
        KeyCode::Char('f') => {
            let sel = model.selected.clone();
            model.graph.toggle_focus(sel.as_deref());
            graph_selection(model);
        }
        KeyCode::Char('e') => model.graph.edge_labels = !model.graph.edge_labels,
        KeyCode::Char('+' | '=') => model.graph.compact = Some(false),
        KeyCode::Char('-' | '_') => model.graph.compact = Some(true),
        KeyCode::Char('0') => model.graph.compact = None,
        _ => return None,
    }
    Some(Vec::new())
}

fn quit_request(model: &mut Model) -> Vec<Cmd> {
    if model.mode == AttachMode::Up || model.owns_daemon {
        model.help = false;
        model.modal = Modal::QuitConfirm;
        Vec::new()
    } else {
        exit(model, Outcome::Detach)
    }
}

fn move_selection(model: &mut Model, delta: isize) -> Vec<Cmd> {
    let visible = model.visible();
    if visible.is_empty() {
        return Vec::new();
    }
    let cur = model.selected_index();
    let last = visible.len() as isize - 1;
    let next = match cur {
        None => 0,
        Some(i) => (i as isize + delta).clamp(0, last),
    };
    let name = visible[next as usize].name.clone();
    model.selected = Some(name);
    model.touched = true;
    model.log_focus = None;
    ensure_detail(model)
}

fn key(model: &mut Model, k: KeyEvent) -> Vec<Cmd> {
    if k.kind == KeyEventKind::Release {
        return Vec::new();
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let ctrl_c = ctrl && matches!(k.code, KeyCode::Char('c' | 'C'));

    // 1. Modal dialogs take every key.
    if model.modal.is_open() {
        return modal_key(model, k);
    }
    if ctrl_c {
        model.filter_editing = false;
        return quit_request(model);
    }
    // 2. The help overlay: `?`, `Esc` or `q` close it.
    if model.help {
        if matches!(k.code, KeyCode::Char('?' | 'q') | KeyCode::Esc) {
            model.help = false;
        }
        return Vec::new();
    }
    // 3. Typing a filter.
    if model.filter_editing {
        match k.code {
            KeyCode::Enter => model.filter_editing = false,
            KeyCode::Esc => {
                model.filter_editing = false;
                model.filter.clear();
            }
            KeyCode::Backspace => {
                model.filter.pop();
            }
            KeyCode::Char(c) if !ctrl => model.filter.push(c),
            _ => {}
        }
        model.touched = true;
        fix_selection(model);
        return ensure_detail(model);
    }
    // 4. The palette (any view), then the toasts' keys (not while typing a
    // search in Logs/Events).
    if ctrl && matches!(k.code, KeyCode::Char('p' | 'P')) {
        return open_palette(model);
    }
    let typing = (model.view == ViewKind::Logs && model.log_pane.search_editing)
        || (model.view == ViewKind::Events && model.events_view.editing);
    if !typing
        && !ctrl
        && let Some(cmds) = toast_key(model, k.code)
    {
        return cmds;
    }
    // 5. Logs and Events keys.
    if model.view == ViewKind::Logs
        && !ctrl
        && let Some(cmds) = logs_key(model, k)
    {
        return cmds;
    }
    if model.view == ViewKind::Events
        && !ctrl
        && let Some(cmds) = events_key(model, k)
    {
        return cmds;
    }
    // 6. Graph keys.
    if model.view == ViewKind::Graph
        && !ctrl
        && let Some(cmds) = graph_key(model, k.code)
    {
        return cmds;
    }
    // 7. Actions on the selected stem (Table, Graph, Detail).
    if matches!(
        model.view,
        ViewKind::Table | ViewKind::Graph | ViewKind::Detail
    ) && !ctrl
        && let KeyCode::Char(c) = k.code
        && let Some(cmds) = action_key(model, c)
    {
        return cmds;
    }
    // 8. Normal keys.
    match k.code {
        KeyCode::Char('q') => quit_request(model),
        KeyCode::Char('?') => {
            model.help = true;
            Vec::new()
        }
        KeyCode::Char('l' | 'L') if ctrl => {
            let on = !model.log_pane.visible;
            model.log_pane.visible = on;
            model.prefs.split_logs = on;
            match &model.prefs.path {
                Some(path) => vec![Cmd::SavePref {
                    path: path.clone(),
                    key: "split_logs".into(),
                    value: on.to_string(),
                }],
                None => Vec::new(),
            }
        }
        KeyCode::Esc => {
            if model.view == ViewKind::Detail {
                model.view = model.return_view;
            } else if !model.filter.is_empty() {
                model.filter.clear();
                fix_selection(model);
            }
            model.message = None;
            Vec::new()
        }
        KeyCode::Char('j') | KeyCode::Down => move_selection(model, 1),
        KeyCode::Char('k') | KeyCode::Up => move_selection(model, -1),
        KeyCode::Char('g') | KeyCode::Home => move_selection(model, isize::MIN / 2),
        KeyCode::Char('G') | KeyCode::End => move_selection(model, isize::MAX / 2),
        KeyCode::Enter => {
            if model.view != ViewKind::Detail {
                model.return_view = if model.view == ViewKind::Graph {
                    ViewKind::Graph
                } else {
                    ViewKind::Table
                };
                set_view(model, ViewKind::Detail)
            } else {
                Vec::new()
            }
        }
        KeyCode::Tab => {
            let v = model.view.next();
            set_view(model, v)
        }
        KeyCode::BackTab => {
            let v = model.view.prev();
            set_view(model, v)
        }
        KeyCode::Char('/') => {
            model.filter_editing = true;
            Vec::new()
        }
        KeyCode::Char('O') => {
            model.sort = model.sort.next();
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// A key in the Logs view; `None` falls through to the normal keys.
fn logs_key(model: &mut Model, k: KeyEvent) -> Option<Vec<Cmd>> {
    if !model.log_pane.search_editing {
        match k.code {
            KeyCode::Char('m') => {
                model.log_pane.merged = !model.log_pane.merged;
                model.log_pane.jump = None;
                return Some(Vec::new());
            }
            KeyCode::Char(']') => {
                model.log_pane.merged = false;
                return Some(move_selection(model, 1));
            }
            KeyCode::Char('[') => {
                model.log_pane.merged = false;
                return Some(move_selection(model, -1));
            }
            _ => {}
        }
    }
    match model.log_pane.key(k) {
        PaneAction::Ignored => None,
        PaneAction::Handled => Some(Vec::new()),
        PaneAction::Copy { text, lines } => {
            model.message = Some(format!(
                "copied {lines} line{}",
                if lines == 1 { "" } else { "s" }
            ));
            Some(vec![Cmd::Copy {
                text,
                via: model.prefs.clipboard,
            }])
        }
    }
}

/// A key in the Events view; `None` falls through to the normal keys.
fn events_key(model: &mut Model, k: KeyEvent) -> Option<Vec<Cmd>> {
    let ev = &mut model.events_view;
    if ev.editing {
        match k.code {
            KeyCode::Enter => ev.editing = false,
            KeyCode::Esc => {
                ev.editing = false;
                ev.filter.clear();
            }
            KeyCode::Backspace => {
                ev.filter.pop();
            }
            KeyCode::Char(c) => ev.filter.push(c),
            _ => {}
        }
        ev.selected = None;
        return Some(Vec::new());
    }
    match k.code {
        KeyCode::Char('/') => {
            ev.editing = true;
            ev.filter.clear();
            ev.selected = None;
        }
        KeyCode::Char('j') | KeyCode::Down => ev.step(&model.events, 1),
        KeyCode::Char('k') | KeyCode::Up => ev.step(&model.events, -1),
        KeyCode::PageDown => ev.step(&model.events, 10),
        KeyCode::PageUp => ev.step(&model.events, -10),
        KeyCode::Char('g') | KeyCode::Home => ev.step(&model.events, isize::MIN / 2),
        KeyCode::Char('G') | KeyCode::End => ev.selected = None,
        KeyCode::Esc if !ev.filter.is_empty() => {
            ev.filter.clear();
            ev.selected = None;
        }
        KeyCode::Enter => {
            let Some(e) = ev.selected_event(&model.events) else {
                return Some(Vec::new());
            };
            let Some(stem) = e.stem.clone() else {
                model.message = Some(format!("{} has no stem", e.kind));
                return Some(Vec::new());
            };
            let at = e.ts;
            let since = (at - chrono::Duration::seconds(60))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            model.selected = Some(stem.clone());
            model.touched = true;
            model.log_pane.merged = false;
            model.log_pane.jump = Some(LogJump { stem, since, at });
            // Force a new subscription even when the same stem was shown.
            model.log_pane.source = None;
            return Some(set_view(model, ViewKind::Logs));
        }
        _ => return None,
    }
    Some(Vec::new())
}

// ---------------------------------------------------------------------------
// Actions (30)
// ---------------------------------------------------------------------------

/// An action key on the selected stem; `None` when `c` is not one.
fn action_key(model: &mut Model, c: char) -> Option<Vec<Cmd>> {
    // Keys that need no stem.
    match c {
        'u' => {
            let profile = model.profile.clone();
            return Some(act(model, Action::UpAll { profile }));
        }
        'd' => {
            model.modal = Modal::DownConfirm;
            return Some(Vec::new());
        }
        ':' => return Some(open_menu(model, model.selected.clone())),
        's' | 'x' | 'X' | 'r' | 'R' | 'p' | 'o' | 'S' => {}
        _ => return None,
    }
    let Some(stem) = model.selected.clone() else {
        model
            .toasts
            .push(Toast::info("no stem selected", model.clock_ms));
        return Some(Vec::new());
    };
    Some(match c {
        's' => act(model, Action::Start(stem)),
        'x' => act(
            model,
            Action::Stop {
                stem,
                cascade: false,
            },
        ),
        'X' => act(
            model,
            Action::Stop {
                stem,
                cascade: true,
            },
        ),
        'r' => act(model, Action::Restart { stem, build: false }),
        'R' => act(model, Action::Restart { stem, build: true }),
        'p' => {
            let pause = !model.watch_paused(&stem);
            act(model, Action::Watch { stem, pause })
        }
        'o' => {
            let editor = model
                .editor
                .clone()
                .filter(|e| !e.trim().is_empty())
                .unwrap_or_else(|| "vi".into());
            model.message = Some(format!("opening {stem} in {editor}…"));
            vec![Cmd::OpenEditor { stem, editor }]
        }
        'S' => {
            model.modal = Modal::ResetTyped {
                stem,
                buffer: String::new(),
            };
            Vec::new()
        }
        _ => Vec::new(),
    })
}

/// Send an action: a notice while it runs; a script run also opens the
/// split log pane (subscribed before the script starts).
pub fn act(model: &mut Model, action: Action) -> Vec<Cmd> {
    model.message = Some(format!("{}…", action.describe()));
    let mut cmds = Vec::new();
    if let Action::RunScript { stem, script, .. } = &action {
        model.script_runs.push(ScriptRun {
            stem: stem.clone(),
            script: script.clone(),
            run_id: None,
        });
        model.log_pane.visible = true;
        model.log_focus = match stem {
            Some(s) if model.selected.as_deref() == Some(s.as_str()) => None,
            Some(s) => Some(s.clone()),
            None => Some(crate::actions::WORKSPACE_LOG_STEM.to_string()),
        };
        if model.log_focus.is_none() && model.selected.is_none() {
            model.selected = stem.clone();
        }
        cmds.extend(sync_logs(model));
    }
    cmds.push(Cmd::Action(action));
    cmds
}

/// An action's result: a toast, the stop confirmation on
/// `HAS_DEPENDANTS`, and a status refresh.
fn action_result(
    model: &mut Model,
    action: Action,
    result: Result<serde_json::Value, Box<stems_core::Error>>,
) -> Vec<Cmd> {
    model.message = None;
    let now = model.clock_ms;
    match (&action, result) {
        (Action::RunScript { stem, script, .. }, Ok(v)) => {
            let run_id = v.get("run_id").and_then(|r| r.as_str()).map(str::to_string);
            if let Some(r) = model
                .script_runs
                .iter_mut()
                .find(|r| r.run_id.is_none() && &r.stem == stem && &r.script == script)
            {
                r.run_id = run_id;
            }
            model.message = Some(format!("{script} running…"));
        }
        (Action::RunScript { stem, script, .. }, Err(e)) => {
            model
                .script_runs
                .retain(|r| !(r.run_id.is_none() && &r.stem == stem && &r.script == script));
            model.toasts.push(Toast::error(&action.describe(), &e, now));
        }
        (
            Action::Stop {
                stem,
                cascade: false,
            },
            Err(e),
        ) if e.code == stems_core::ErrorCode::HasDependants => {
            let dependants: Vec<String> = e
                .details
                .get("dependants")
                .and_then(|d| d.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            model.help = false;
            model.modal = Modal::StopConfirm {
                stem: stem.clone(),
                dependants,
            };
        }
        (_, Ok(v)) => model.toasts.push(Toast::ok(action.done(&v), now)),
        (_, Err(e)) => model.toasts.push(Toast::error(&action.describe(), &e, now)),
    }
    if action == Action::ConfigApply {
        model.toasts.clear_config();
    }
    refresh(model)
}

/// `script.finished` of a run started here: `✓ <script> finished in 1.2s`
/// or `✗ <script> failed: exit 3`.
fn script_finished(model: &mut Model, e: &stems_api::Event) {
    let d = &e.data;
    let script = d.get("script").and_then(|v| v.as_str()).unwrap_or_default();
    let run_id = d.get("run_id").and_then(|v| v.as_str());
    let pos = model
        .script_runs
        .iter()
        .position(|r| r.run_id.is_some() && r.run_id.as_deref() == run_id)
        .or_else(|| {
            model
                .script_runs
                .iter()
                .position(|r| r.run_id.is_none() && r.stem == e.stem && r.script == script)
        });
    let Some(pos) = pos else {
        return;
    };
    model.script_runs.remove(pos);
    if model.script_runs.is_empty() {
        model.message = None;
    }
    let now = model.clock_ms;
    let ok = d.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if ok {
        let ms = d.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let secs = ms as f64 / 1000.0;
        model
            .toasts
            .push(Toast::ok(format!("{script} finished in {secs:.1}s"), now));
        return;
    }
    let why = if d.get("timed_out").and_then(|v| v.as_bool()) == Some(true) {
        "timed out".to_string()
    } else if let Some(c) = d.get("exit").and_then(|v| v.as_i64()) {
        format!("exit {c}")
    } else if let Some(s) = d.get("signal").and_then(|v| v.as_i64()) {
        format!("signal {s}")
    } else {
        "cancelled".to_string()
    };
    model
        .toasts
        .push(Toast::failure(format!("{script} failed: {why}"), now));
}

/// `config.changed` (33): a sticky toast, `a` applies, `v` shows the plan.
fn config_changed(model: &mut Model, e: &stems_api::Event) {
    let stems = e
        .data
        .pointer("/plan/stems")
        .and_then(|v| v.as_array())
        .map_or(0, |a| {
            a.iter()
                .filter(|s| s.get("action").and_then(|x| x.as_str()) != Some("unchanged"))
                .count()
        });
    let text = format!(
        "config changed: {stems} stem{} affected",
        if stems == 1 { "" } else { "s" }
    );
    model.toasts.push(Toast {
        kind: ToastKind::ConfigChanged,
        ..Toast::info(text, model.clock_ms)
    });
}

/// Keys of the toasts: `Esc` dismisses, `e` expands the newest error, `a`
/// / `v` act on the config-changed toast. `None` when not applicable.
fn toast_key(model: &mut Model, code: KeyCode) -> Option<Vec<Cmd>> {
    if model.toasts.is_empty() {
        return None;
    }
    match code {
        KeyCode::Esc => {
            model.toasts.dismiss();
            Some(Vec::new())
        }
        KeyCode::Char('e') => {
            let t = model.toasts.last_error()?;
            model.modal = Modal::ErrorDetails {
                title: t.code.clone().unwrap_or_else(|| "error".into()),
                text: t.details.clone().unwrap_or_else(|| t.text.clone()),
            };
            Some(Vec::new())
        }
        KeyCode::Char('a') => {
            model.toasts.config_changed()?;
            model.toasts.clear_config();
            Some(act(model, Action::ConfigApply))
        }
        KeyCode::Char('v') => {
            model.toasts.config_changed()?;
            model.modal = Modal::Plan(None);
            Some(vec![Cmd::LoadPlan])
        }
        _ => None,
    }
}

/// Open the script menu for `stem` (refreshing the catalogue).
fn open_menu(model: &mut Model, stem: Option<String>) -> Vec<Cmd> {
    model.help = false;
    model.modal = Modal::ScriptMenu(ScriptMenu {
        stem,
        ..ScriptMenu::default()
    });
    vec![Cmd::LoadCatalog]
}

fn open_palette(model: &mut Model) -> Vec<Cmd> {
    model.help = false;
    model.filter_editing = false;
    model.modal = Modal::Palette(Palette::default());
    if model.catalog.is_none() {
        vec![Cmd::LoadCatalog]
    } else {
        Vec::new()
    }
}

/// Run `script` of `stem`: its form when it declares `args`, else at once.
fn open_script(model: &mut Model, stem: Option<String>, script: &str) -> Vec<Cmd> {
    let entry = model
        .catalog
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|e| e.stem == stem && e.name == script)
        .cloned();
    match entry {
        Some(e) if !e.args.is_empty() => {
            model.modal = Modal::ScriptForm(ScriptForm::new(stem, script, &e.args));
            Vec::new()
        }
        _ => {
            model.modal = Modal::None;
            act(
                model,
                Action::RunScript {
                    stem,
                    script: script.to_string(),
                    args: serde_json::Map::new(),
                },
            )
        }
    }
}

/// Every key while a modal is open.
fn modal_key(model: &mut Model, k: KeyEvent) -> Vec<Cmd> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let ctrl_c = ctrl && matches!(k.code, KeyCode::Char('c' | 'C'));
    let close = |model: &mut Model| {
        model.modal = Modal::None;
        Vec::new()
    };
    if ctrl_c {
        return close(model);
    }
    match std::mem::take(&mut model.modal) {
        Modal::None => Vec::new(),
        Modal::QuitConfirm => match k.code {
            KeyCode::Char('y' | 'Y') => exit(model, Outcome::StopAll),
            KeyCode::Char('d' | 'D') => exit(model, Outcome::Detach),
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Vec::new(),
            _ => {
                model.modal = Modal::QuitConfirm;
                Vec::new()
            }
        },
        Modal::StopConfirm { stem, dependants } => match k.code {
            KeyCode::Char('y' | 'Y' | 'X') => act(
                model,
                Action::Stop {
                    stem,
                    cascade: true,
                },
            ),
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Vec::new(),
            _ => {
                model.modal = Modal::StopConfirm { stem, dependants };
                Vec::new()
            }
        },
        Modal::DownConfirm => match k.code {
            KeyCode::Char('y' | 'Y') => {
                if model.mode == AttachMode::Up {
                    exit(model, Outcome::StopAll)
                } else {
                    act(model, Action::DownAll)
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc | KeyCode::Enter => Vec::new(),
            _ => {
                model.modal = Modal::DownConfirm;
                Vec::new()
            }
        },
        Modal::ResetTyped { stem, mut buffer } => match k.code {
            KeyCode::Esc => Vec::new(),
            KeyCode::Enter if buffer == "reset" => act(model, Action::Reset(stem)),
            KeyCode::Enter => {
                buffer.clear();
                model.modal = Modal::ResetTyped { stem, buffer };
                Vec::new()
            }
            KeyCode::Backspace => {
                buffer.pop();
                model.modal = Modal::ResetTyped { stem, buffer };
                Vec::new()
            }
            KeyCode::Char(c) if !ctrl => {
                buffer.push(c);
                model.modal = Modal::ResetTyped { stem, buffer };
                Vec::new()
            }
            _ => {
                model.modal = Modal::ResetTyped { stem, buffer };
                Vec::new()
            }
        },
        Modal::ScriptMenu(mut m) => {
            let catalog = model.catalog.clone().unwrap_or_default();
            let n = m.visible(&catalog).len();
            match k.code {
                KeyCode::Esc if !m.filter.is_empty() => {
                    m.filter.clear();
                    m.selected = 0;
                }
                KeyCode::Esc => return Vec::new(),
                KeyCode::Enter => {
                    let Some(e) = m.visible(&catalog).get(m.selected).map(|e| (*e).clone()) else {
                        model.modal = Modal::ScriptMenu(m);
                        return Vec::new();
                    };
                    return open_script(model, e.stem.clone(), &e.name);
                }
                KeyCode::Down | KeyCode::Tab if n > 0 => m.selected = (m.selected + 1) % n,
                KeyCode::Up | KeyCode::BackTab if n > 0 => m.selected = (m.selected + n - 1) % n,
                KeyCode::Char('n') if ctrl && n > 0 => m.selected = (m.selected + 1) % n,
                KeyCode::Char('p') if ctrl && n > 0 => m.selected = (m.selected + n - 1) % n,
                KeyCode::Backspace => {
                    m.filter.pop();
                    m.selected = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    m.filter.push(c);
                    m.selected = 0;
                }
                _ => {}
            }
            model.modal = Modal::ScriptMenu(m);
            Vec::new()
        }
        Modal::ScriptForm(mut f) => match f.key(k) {
            FormAction::Cancel => Vec::new(),
            FormAction::Handled => {
                model.modal = Modal::ScriptForm(f);
                Vec::new()
            }
            FormAction::Submit(args) => act(
                model,
                Action::RunScript {
                    stem: f.stem,
                    script: f.script,
                    args,
                },
            ),
        },
        Modal::Palette(mut p) => {
            let n = palette_matches(model, &p).len();
            match k.code {
                KeyCode::Esc => return Vec::new(),
                KeyCode::Enter => {
                    let Some(item) = palette_matches(model, &p).into_iter().nth(p.selected) else {
                        model.modal = Modal::Palette(p);
                        return Vec::new();
                    };
                    return run_palette(model, item.target);
                }
                KeyCode::Down | KeyCode::Tab if n > 0 => p.selected = (p.selected + 1) % n,
                KeyCode::Up | KeyCode::BackTab if n > 0 => p.selected = (p.selected + n - 1) % n,
                KeyCode::Char('n') if ctrl && n > 0 => p.selected = (p.selected + 1) % n,
                KeyCode::Char('p') if ctrl && n > 0 => p.selected = (p.selected + n - 1) % n,
                KeyCode::Backspace => {
                    p.query.pop();
                    p.selected = 0;
                }
                KeyCode::Char(c) if !ctrl => {
                    p.query.push(c);
                    p.selected = 0;
                }
                _ => {}
            }
            model.modal = Modal::Palette(p);
            Vec::new()
        }
        Modal::ErrorDetails { title, text } => match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'e') => Vec::new(),
            _ => {
                model.modal = Modal::ErrorDetails { title, text };
                Vec::new()
            }
        },
        Modal::Plan(plan) => match k.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'v') => Vec::new(),
            KeyCode::Char('a') => {
                model.toasts.clear_config();
                act(model, Action::ConfigApply)
            }
            _ => {
                model.modal = Modal::Plan(plan);
                Vec::new()
            }
        },
    }
}

/// Do what a palette item says.
fn run_palette(model: &mut Model, target: PaletteTarget) -> Vec<Cmd> {
    match target {
        PaletteTarget::StemKey { stem, key } => {
            select(model, &stem);
            let mut cmds = ensure_detail(model);
            if key == ':' {
                cmds.extend(open_menu(model, Some(stem)));
                return cmds;
            }
            // Act as the key would in a stem view (from any view).
            cmds.extend(action_key(model, key).unwrap_or_default());
            cmds
        }
        PaletteTarget::Script { stem, script } => {
            if let Some(s) = &stem {
                select(model, s);
            }
            open_script(model, stem, &script)
        }
        PaletteTarget::View { stem, view } => {
            if let Some(s) = &stem {
                select(model, s);
            }
            if view == ViewKind::Detail && model.view != ViewKind::Detail {
                model.return_view = if model.view == ViewKind::Graph {
                    ViewKind::Graph
                } else {
                    ViewKind::Table
                };
            }
            if view == ViewKind::Logs {
                model.log_pane.merged = false;
            }
            set_view(model, view)
        }
        PaletteTarget::Key('?') => {
            model.help = true;
            Vec::new()
        }
        PaletteTarget::Key(c) => action_key(model, c).unwrap_or_default(),
    }
}

/// Select `stem` (clearing a filter that hides it).
fn select(model: &mut Model, stem: &str) {
    if !model.visible().iter().any(|s| s.name == stem) {
        model.filter.clear();
    }
    model.selected = Some(stem.to_string());
    model.touched = true;
    model.log_focus = None;
}
