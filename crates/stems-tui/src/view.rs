//! Rendering: `view(&Model, &mut Frame)`, a function of the model (it only
//! records the header tabs' columns in `Model::tab_hits` for mouse clicks).
//!
//! Layout: a title line (workspace + view tabs, the current one in
//! brackets), the body (the current view), a status bar (workspace,
//! profile, daemon pid, counts, key hints or the filter). Overlays: help
//! (`?`) and the quit confirmation. Below 40x10 only a "terminal too small"
//! notice is drawn. With the split layout (`Ctrl-L`, 29) the bottom 40 % of
//! Table/Graph/Detail is the log pane.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row, Table, TableState,
};
use serde_json::Value;
use stems_api::StemStatus;
use stems_core::Glyph;

use crate::graph::glyph_color;
use crate::model::{Model, SortKey, ViewKind};
use crate::prefs::Theme;

/// Smallest usable terminal.
pub const MIN_SIZE: (u16, u16) = (40, 10);

/// Table column headers.
pub const COLUMNS: [&str; 10] = [
    "STEM", "TYPE", "STATUS", "REASON", "PID", "PORTS", "UPTIME", "RESTARTS", "CPU", "MEM",
];

/// Render the whole dashboard.
pub fn view(model: &Model, f: &mut Frame) {
    let area = f.area();
    model.tab_hits.borrow_mut().clear();
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        too_small(f, area);
        return;
    }
    let [title, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    title_bar(model, f, title);
    let split = model.log_pane.visible
        && matches!(
            model.view,
            ViewKind::Table | ViewKind::Graph | ViewKind::Detail
        );
    let (main, pane) = if split {
        let [a, b] =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
        (a, Some(b))
    } else {
        (body, None)
    };
    match model.view {
        ViewKind::Table => table(model, f, main),
        ViewKind::Detail => detail(model, f, main),
        ViewKind::Graph => crate::graph::render(model, f, main),
        ViewKind::Logs => log_pane(model, f, main),
        ViewKind::Events => crate::events::render(
            &model.events_view,
            &model.events,
            model.ascii,
            accent(model),
            f,
            main,
        ),
    }
    if let Some(p) = pane {
        log_pane(model, f, p);
    }
    status_bar(model, f, status);
    crate::modals::toasts(model, f, area);
    if model.help {
        help(model, f, area);
    }
    crate::modals::render(model, f, area);
}

/// The watchdog-paused marker (30): `⏸` (`||` in ASCII).
pub fn paused_marker(ascii: bool) -> &'static str {
    if ascii { "||" } else { "⏸" }
}

/// The log pane (Logs view or split pane) with its title.
fn log_pane(model: &Model, f: &mut Frame, area: Rect) {
    let title = if model.log_pane.merged && model.view == ViewKind::Logs {
        "Logs: all stems".to_string()
    } else {
        match model.selected.as_deref() {
            Some(s) => format!("Logs: {s}"),
            None => "Logs".to_string(),
        }
    };
    let style = crate::logs::PaneStyle {
        title,
        ascii: model.ascii,
        accent: accent(model),
    };
    model.log_pane.render(f, area, &style);
}

/// The accent colour of the theme.
pub fn accent(model: &Model) -> Color {
    match model.prefs.theme {
        Theme::Dark => Color::Cyan,
        Theme::Light => Color::Blue,
    }
}

/// `text` cut to `max` characters, ending in `…` (`...` in ASCII).
pub fn truncate(text: &str, max: usize, ascii: bool) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let ell = if ascii { "..." } else { "…" };
    let keep = max.saturating_sub(ell.chars().count());
    if keep == 0 {
        return text.chars().take(max).collect();
    }
    let mut s: String = text.chars().take(keep).collect();
    s.push_str(ell);
    s
}

/// `3s`, `2m05s`, `1h02m`, `-`.
pub fn uptime(s: Option<u64>) -> String {
    match s {
        None => "-".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        Some(s) => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn too_small(f: &mut Frame, area: Rect) {
    let msg = vec![
        Line::from("terminal too small"),
        Line::from(format!(
            "{}x{} (need {}x{})",
            area.width, area.height, MIN_SIZE.0, MIN_SIZE.1
        )),
    ];
    let y = area.height / 2;
    let r = Rect::new(
        area.x,
        area.y + y.saturating_sub(1),
        area.width,
        2.min(area.height),
    );
    f.render_widget(Paragraph::new(msg).centered(), r);
}

/// The header's view tabs, `1 Graph  2 [Table]  3 Detail  4 Logs  5 Events`
/// (the digit is the view's direct key; the current view in brackets), or
/// the plain ` Graph [Table] Detail ` when the header is too narrow for the
/// digits. `None` segments are the gaps between tabs.
fn tab_segments(model: &Model, width: u16) -> Vec<(Option<ViewKind>, Vec<Span<'static>>)> {
    const GAP: &str = "  ";
    let titles: u16 = ViewKind::ALL
        .iter()
        .map(|v| v.title().chars().count() as u16)
        .sum();
    let n = ViewKind::ALL.len() as u16;
    // digit + space per tab, the brackets, the gaps, a leading space.
    let numbered_w = titles + 2 * n + 2 + GAP.len() as u16 * (n - 1) + 1;
    // Keep room for " stems ·" on the left (and the right margin).
    let numbered = width > numbered_w + 8;
    let current = Style::default()
        .fg(accent(model))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut out = Vec::new();
    if numbered {
        out.push((None, vec![Span::raw(" ")]));
    }
    for (i, v) in ViewKind::ALL.into_iter().enumerate() {
        let active = v == model.view;
        let label = if active {
            Span::styled(format!("[{}]", v.title()), current)
        } else if numbered {
            Span::raw(v.title())
        } else {
            Span::raw(format!(" {} ", v.title()))
        };
        if numbered {
            if i > 0 {
                out.push((None, vec![Span::raw(GAP)]));
            }
            let digit = if active { current } else { dim };
            out.push((
                Some(v),
                vec![Span::styled(format!("{} ", v.digit()), digit), label],
            ));
        } else {
            out.push((Some(v), vec![label]));
        }
    }
    out
}

fn title_bar(model: &Model, f: &mut Frame, area: Rect) {
    let segs = tab_segments(model, area.width);
    let width = |spans: &[Span]| spans.iter().map(|s| s.width() as u16).sum::<u16>();
    let tabs_w: u16 = segs.iter().map(|(_, s)| width(s)).sum::<u16>() + 1;
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(tabs_w)]).areas(area);
    let ws = if model.workspace.is_empty() {
        "…"
    } else {
        &model.workspace
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" stems", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(" · {ws}")),
        ])),
        left,
    );
    // Record where each tab landed: a mouse click on it switches views.
    let end = right.x + right.width;
    let mut x = right.x;
    let mut hits = Vec::new();
    let mut spans = Vec::new();
    for (v, s) in segs {
        let w = width(&s);
        if let Some(v) = v
            && x < end
        {
            hits.push((v, x, (x + w).min(end)));
        }
        x += w;
        spans.extend(s);
    }
    *model.tab_hits.borrow_mut() = hits;
    f.render_widget(Paragraph::new(Line::from(spans)), right);
}

fn status_bar(model: &Model, f: &mut Frame, area: Rect) {
    let s = &model.summary;
    let g = |g: Glyph| g.symbol(!model.ascii);
    let mut left = format!(
        " {} · profile {} · daemon pid {} · {}{} {}{} {}{} {}{} {}{} {}{}",
        if model.workspace.is_empty() {
            "-"
        } else {
            &model.workspace
        },
        model.profile.as_deref().unwrap_or("-"),
        model
            .daemon_pid
            .map_or_else(|| "-".into(), |p| p.to_string()),
        g(Glyph::Healthy),
        s.healthy,
        g(Glyph::Degraded),
        s.degraded,
        g(Glyph::Failed),
        s.failed,
        g(Glyph::Stopped),
        s.stopped,
        g(Glyph::Unknown),
        s.unknown,
        g(Glyph::Transitioning),
        s.starting,
    );
    if model.filter_editing || !model.filter.is_empty() {
        left.push_str(&format!(
            " · /{}{}",
            model.filter,
            if model.filter_editing { "_" } else { "" }
        ));
    }
    let right = if let Some(m) = &model.message {
        m.clone()
    } else {
        match (model.view, model.sort) {
            (ViewKind::Detail, _) => "Esc back · ? help · q quit ".to_string(),
            (ViewKind::Logs, _) => "Space pause · ? help · q quit ".to_string(),
            (ViewKind::Events, _) => "Enter logs · ? help · q quit ".to_string(),
            (_, SortKey::Declared) => "? help · q quit ".to_string(),
            (_, sort) => format!("sort:{} · ? help · q quit ", sort.label()),
        }
    };
    let w = area.width as usize;
    let lw = left.chars().count();
    let room = w.saturating_sub(lw + 1);
    let text = if room >= 12.min(right.chars().count()) && room > 0 {
        let right = truncate(&right, room, model.ascii);
        format!(
            "{left}{}{right}",
            " ".repeat(w - lw - right.chars().count())
        )
    } else {
        truncate(&left, w, model.ascii)
    };
    f.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn ports_text(s: &StemStatus, wide: bool) -> String {
    let v: Vec<String> = s
        .ports
        .iter()
        .map(|p| {
            let n = p.port.map_or_else(|| "auto".to_string(), |n| n.to_string());
            if wide { format!("{}:{n}", p.name) } else { n }
        })
        .collect();
    if v.is_empty() {
        "-".into()
    } else {
        v.join(",")
    }
}

/// Column widths for a table `width` columns wide (highlight gutter
/// included): fixed columns, `STEM` up to its longest name, `REASON` the rest.
pub fn column_widths(model: &Model, width: u16) -> [u16; 10] {
    let wide = width >= 100;
    let mark = paused_marker(model.ascii).chars().count() + 1;
    let longest = model
        .stems
        .iter()
        .map(|s| s.name.chars().count() + if model.watch_paused(&s.name) { mark } else { 0 })
        .max()
        .unwrap_or(4)
        .max(4) as u16;
    let (stem_max, ty, st, pid, ports, up, rs, cpu, mem) = if wide {
        (20, 8, 12, 7, 14, 7, 8, 10, 10)
    } else {
        (12, 7, 10, 6, 6, 6, 8, 5, 5)
    };
    let stem = longest.min(stem_max);
    let avail = width.saturating_sub(2 + 9);
    let fixed = stem + ty + st + pid + ports + up + rs + cpu + mem;
    let reason = avail.saturating_sub(fixed);
    [stem, ty, st, reason, pid, ports, up, rs, cpu, mem]
}

fn table(model: &Model, f: &mut Frame, area: Rect) {
    let widths = column_widths(model, area.width);
    let wide = area.width >= 100;
    let header = Row::new(COLUMNS.map(Cell::from)).style(
        Style::default()
            .fg(accent(model))
            .add_modifier(Modifier::BOLD),
    );
    let visible = model.visible();
    let rows: Vec<Row> = visible
        .iter()
        .map(|s| {
            let a = model.ascii;
            let reason = s
                .reason
                .as_deref()
                .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "-".into());
            let hist = model.metrics.get(&s.name);
            let spark = |v: Option<&Vec<f64>>, w: u16| {
                let w = usize::from(w);
                let vals: &[f64] = v.map_or(&[], |v| v.as_slice());
                if a {
                    stems_core::metrics::sparkline_ascii(vals, w)
                } else {
                    stems_core::metrics::sparkline(vals, w)
                }
            };
            let status = format!("{} {}", s.glyph.symbol(!a), s.state);
            let name = if s.watch.as_ref().is_some_and(|w| w.paused) {
                // Keep the marker visible when the name is cut.
                let m = paused_marker(a);
                let room = usize::from(widths[0]).saturating_sub(m.chars().count() + 1);
                format!("{} {m}", truncate(&s.name, room, a))
            } else {
                truncate(&s.name, usize::from(widths[0]), a)
            };
            Row::new(vec![
                Cell::from(name),
                Cell::from(truncate(&s.kind, usize::from(widths[1]), a)),
                Cell::from(Span::styled(
                    truncate(&status, usize::from(widths[2]), a),
                    Style::default().fg(glyph_color(s.glyph)),
                )),
                Cell::from(truncate(&reason, usize::from(widths[3]), a)),
                Cell::from(s.pid.map_or_else(|| "-".into(), |p| p.to_string())),
                Cell::from(truncate(&ports_text(s, wide), usize::from(widths[5]), a)),
                Cell::from(uptime(s.uptime_s)),
                Cell::from(s.restarts.to_string()),
                Cell::from(spark(hist.map(|h| &h.cpu), widths[8])),
                Cell::from(spark(hist.map(|h| &h.mem), widths[9])),
            ])
        })
        .collect();
    let empty = rows.is_empty();
    let t = Table::new(rows, widths.map(Constraint::Length))
        .header(header)
        .column_spacing(1)
        .highlight_symbol(if model.ascii { "> " } else { "› " })
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = TableState::default().with_selected(model.selected_index());
    f.render_stateful_widget(t, area, &mut state);
    if empty && area.height > 2 {
        let msg = if !model.loaded {
            "loading…".to_string()
        } else if !model.filter.is_empty() {
            format!("no stem matches /{}", model.filter)
        } else {
            "no stems".to_string()
        };
        let r = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(2), 1);
        f.render_widget(Paragraph::new(msg), r);
    }
}

/// YAML-ish `key: value` lines of the resolved config (name dropped).
pub fn config_lines(v: &Value) -> Vec<String> {
    let mut v = v.clone();
    if let Value::Object(m) = &mut v {
        m.remove("name");
        m.remove("local_env");
        // Empty collections and nulls are noise in a small pane.
        m.retain(|_, x| match x {
            Value::Null => false,
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        });
    }
    serde_yaml_ng::to_string(&v)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn detail(model: &Model, f: &mut Frame, area: Rect) {
    let Some(name) = model.selected.as_deref() else {
        let block = Block::default().borders(Borders::ALL).title(" Detail ");
        f.render_widget(Paragraph::new(" no stem selected").block(block), area);
        return;
    };
    let st = model.selected_stem();
    let a = model.ascii;
    let title = match st {
        Some(s) => format!(" Detail: {name} · {} {} ", s.glyph.symbol(!a), s.state),
        None => format!(" Detail: {name} "),
    };
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let d = model.detail.as_ref().filter(|d| d.stem == name);

    // Left: the effective config.
    let mut cfg: Vec<Line> = vec![Line::styled(
        "Config",
        Style::default()
            .fg(accent(model))
            .add_modifier(Modifier::BOLD),
    )];
    match d.and_then(|d| d.config.as_ref()) {
        None => cfg.push(Line::from("loading…")),
        Some(Err(e)) => cfg.push(Line::from(format!("unavailable: {e}"))),
        Some(Ok(v)) => cfg.extend(config_lines(v).into_iter().map(Line::from)),
    }

    // Right: runtime state, scripts, health, metrics, events.
    let head = |t: &str| {
        Line::styled(
            t.to_string(),
            Style::default()
                .fg(accent(model))
                .add_modifier(Modifier::BOLD),
        )
    };
    let mut rt: Vec<Line> = vec![head("Status")];
    if let Some(s) = st {
        rt.push(Line::from(vec![
            Span::styled(
                format!("{} {}", s.glyph.symbol(!a), s.state),
                Style::default().fg(glyph_color(s.glyph)),
            ),
            Span::raw(
                s.reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default(),
            ),
        ]));
        rt.push(Line::from(format!(
            "pid {} · up {} · restarts {}",
            s.pid.map_or_else(|| "-".into(), |p| p.to_string()),
            uptime(s.uptime_s),
            s.restarts
        )));
        rt.push(Line::from(format!("ports {}", ports_text(s, true))));
    }
    rt.push(head("Scripts"));
    let scripts: Vec<String> = d
        .and_then(|d| d.config.as_ref())
        .and_then(|c| c.as_ref().ok())
        .and_then(|c| c.get("scripts"))
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    rt.push(Line::from(if scripts.is_empty() {
        "-".to_string()
    } else {
        scripts.join(", ")
    }));
    rt.push(head("Health"));
    rt.extend(health_lines(model, st, d).into_iter().map(Line::from));
    rt.push(head("Metrics"));
    rt.push(Line::from(if model.metrics.contains_key(name) {
        "see CPU/MEM in the table"
    } else {
        "n/a (no samples yet)"
    }));
    rt.push(head("Events"));
    let events = d.map(|d| d.events.as_slice()).unwrap_or_default();
    if events.is_empty() {
        rt.push(Line::from("-"));
    }
    for e in events.iter().rev().take(10).rev() {
        let mut s = format!("{} {}", e.ts.format("%H:%M:%S"), e.kind);
        if let (Some(from), Some(to)) = (&e.from, &e.to) {
            s.push_str(&format!(" {from}→{to}"));
        }
        rt.push(Line::from(s));
    }

    if inner.width >= 60 {
        let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(1)
            .areas(inner);
        f.render_widget(Paragraph::new(cfg), l);
        f.render_widget(Paragraph::new(rt), r);
    } else {
        let mut all = rt;
        all.push(Line::from(""));
        all.extend(cfg);
        f.render_widget(Paragraph::new(all), inner);
    }
}

/// The health section: the probe summary from `status`, then the last
/// probe results from the `health` RPC (newest last, at most 3), or `n/a`.
fn health_lines(
    model: &Model,
    st: Option<&StemStatus>,
    d: Option<&crate::model::DetailData>,
) -> Vec<String> {
    let mut out = Vec::new();
    let summary = st
        .and_then(|s| s.health.as_ref())
        .and_then(|h| serde_json::to_value(h).ok())
        .filter(|v| !v.is_null());
    if let Some(v) = &summary {
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("?");
        let fails = v
            .get("consecutive_failures")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        out.push(format!("{kind} probe · {fails} failing in a row"));
    }
    let records: &[Value] = d
        .and_then(|d| d.health.as_ref())
        .and_then(|h| h.as_ref().ok())
        .map_or(&[], Vec::as_slice);
    for r in records.iter().rev().take(3).rev() {
        let ok = r.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let g = if ok { Glyph::Healthy } else { Glyph::Failed };
        let ts = r
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.format("%H:%M:%S").to_string())
            .unwrap_or_default();
        out.push(format!(
            "{ts} {} {} ({}ms)",
            g.symbol(!model.ascii),
            r.get("detail").and_then(Value::as_str).unwrap_or(""),
            r.get("latency_ms").and_then(Value::as_u64).unwrap_or(0)
        ));
    }
    if out.is_empty() {
        out.push("n/a".into());
    }
    out
}

/// A `w`x`h` rectangle centred in `area` (clipped to it).
pub fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

/// Key help lines (key, what) of Table/Graph/Detail: navigation, then
/// the actions on the selected stem (30).
pub const HELP: &[(&str, &str)] = &[
    ("j/k ↓/↑", "move the selection"),
    ("h/l ←/→", "graph: across columns"),
    ("f e + -", "graph: focus labels zoom"),
    ("g / G", "first / last stem"),
    ("Enter", "open the detail view"),
    ("Tab S-Tab", "next / previous view"),
    ("1-5", "go to a view (header #)"),
    ("/", "filter stems by name"),
    ("O", "cycle the sort order"),
    ("Ctrl-L", "split: logs below"),
    ("click", "row / tab (mouse = true)"),
    ("s", "start the stem"),
    ("x / X", "stop / with dependants"),
    ("r / R", "restart / rebuild"),
    ("p", "pause/resume watchdog"),
    ("o", "open code in $EDITOR"),
    ("S", "reset (type `reset`)"),
    (":", "script menu"),
    ("u / d", "up all / down all"),
    ("Ctrl-P", "command palette"),
    ("Esc", "dismiss toasts, back"),
    ("e", "error toast details"),
    ("?", "toggle this help"),
    ("q Ctrl-C", "quit"),
];

/// Key help of the Logs view.
pub const LOGS_HELP: &[(&str, &str)] = &[
    ("Space", "pause / resume following"),
    ("/  n / N", "search (Enter keeps) · next / previous match"),
    ("L", "level: all, info+, warn+, error"),
    ("s", "show / hide script output lines"),
    ("m", "merged: every stem, with prefixes"),
    ("[ / ]", "previous / next stem"),
    ("j / k, g / G", "move · top / bottom (follows)"),
    ("Enter", "expand the fields of a structured line"),
    ("V, y", "visual range · copy line or range"),
    ("w / t", "wrap long lines / timestamps (UTC)"),
    ("Tab, Ctrl-L", "next view · split pane elsewhere"),
    ("1-5, click", "go to a view (header number or tab)"),
    ("q", "quit"),
];

/// Key help of the Events view.
pub const EVENTS_HELP: &[(&str, &str)] = &[
    ("j / k, g / G", "move · first / newest (follows)"),
    ("/", "filter by stem, kind, state or reason"),
    ("Enter", "the stem's logs at the event time"),
    ("Esc", "clear the filter"),
    ("Tab, 1-5", "next view · go to a view"),
    ("?", "toggle this help"),
    ("q", "quit"),
];

fn help(model: &Model, f: &mut Frame, area: Rect) {
    let keys = match model.view {
        ViewKind::Logs => LOGS_HELP,
        ViewKind::Events => EVENTS_HELP,
        _ => HELP,
    };
    // One column when it fits the height, else two side by side.
    let two = keys.len() as u16 + 2 > area.height.saturating_sub(2);
    let kw = keys
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let kw = if two { kw } else { kw.max(16) };
    let dw = keys
        .iter()
        .map(|(_, v)| v.chars().count())
        .max()
        .unwrap_or(0);
    let cell = |(k, v): &(&str, &str)| {
        vec![
            Span::styled(
                format!(" {k:<kw$}"),
                Style::default()
                    .fg(accent(model))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{v:<dw$}")),
        ]
    };
    let lines: Vec<Line> = if two {
        let half = keys.len().div_ceil(2);
        (0..half)
            .map(|i| {
                let mut spans = cell(&keys[i]);
                if let Some(right) = keys.get(half + i) {
                    spans.push(Span::raw(" "));
                    spans.extend(cell(right));
                }
                Line::from(spans)
            })
            .collect()
    } else {
        keys.iter().map(|e| Line::from(cell(e))).collect()
    };
    let col = (1 + kw + dw) as u16;
    let w = if two { 2 * col + 3 } else { (col + 2).max(68) };
    let r = centered(area, w, lines.len() as u16 + 2);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Help ")),
        r,
    );
}

/// Render `model` at `w`x`h` into text (one line per row, trailing spaces
/// trimmed): what headless mode prints and the goldens hold.
pub fn render_text(model: &Model, w: u16, h: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(w, h);
    let mut term = ratatui::Terminal::new(backend).expect("test backend");
    let _ = term.draw(|f| view(model, f));
    buffer_text(term.backend().buffer())
}

/// A buffer as text (one line per row, trailing spaces trimmed).
pub fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(area.x + x, area.y + y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
