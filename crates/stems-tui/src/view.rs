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
use crate::model::{Model, SortKey, StemHit, ViewKind};
use crate::prefs::Theme;

/// Smallest usable terminal.
pub const MIN_SIZE: (u16, u16) = (40, 10);

/// Table column headers.
pub const COLUMNS: [&str; 10] = [
    "STEM", "TYPE", "STATUS", "REASON", "PID", "PORTS", "UPTIME", "RESTARTS", "CPU", "MEM",
];

/// The columns added when the table is at least [`EXTRA_COLUMNS_MIN`]
/// wide: custom scripts, active variant, watchdog.
pub const EXTRA_COLUMNS: [&str; 3] = ["SCRIPTS", "VARIANT", "WATCH"];

/// Width from which the table shows [`EXTRA_COLUMNS`] (below it the
/// REASON column would have no room left).
pub const EXTRA_COLUMNS_MIN: u16 = 120;

/// Render the whole dashboard.
pub fn view(model: &Model, f: &mut Frame) {
    let area = f.area();
    model.tab_hits.borrow_mut().clear();
    model.stem_hits.borrow_mut().clear();
    model.bar_hits.borrow_mut().clear();
    if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
        too_small(f, area);
        return;
    }
    let bar_h = u16::from(has_bar(model));
    let [title, body, bar, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(bar_h),
        Constraint::Length(1),
    ])
    .areas(area);
    title_bar(model, f, title);
    let (main, pane) = split_body(model, body);
    match model.view {
        ViewKind::Table => table(model, f, main),
        ViewKind::Detail => crate::detail::render(model, f, main),
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
    if bar_h > 0 {
        crate::bar::render(model, f, bar);
    }
    status_bar(model, f, status);
    crate::modals::toasts(model, f, area);
    if model.help {
        help(model, f, area);
    }
    crate::modals::render(model, f, area);
}

/// The body split: the current view and, with the split layout under
/// Table/Graph/Detail, the log pane (bottom 40 %).
fn split_body(model: &Model, body: Rect) -> (Rect, Option<Rect>) {
    let split = model.log_pane.visible
        && matches!(
            model.view,
            ViewKind::Table | ViewKind::Graph | ViewKind::Detail
        );
    if split {
        let [a, b] =
            Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
        (a, Some(b))
    } else {
        (body, None)
    }
}

/// The screen areas of the current view and of the split log pane for a
/// terminal of [`Model::size`] (what [`view`] draws into; the reducer uses
/// it for scrolling and the mouse wheel). `None` below [`MIN_SIZE`].
pub fn body_areas(model: &Model) -> Option<(Rect, Option<Rect>)> {
    let (w, h) = model.size;
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 {
        return None;
    }
    let bar = u16::from(has_bar(model));
    Some(split_body(model, Rect::new(0, 1, w, h - 2 - bar)))
}

/// The Detail view's scroll bounds for [`Model::size`]: `(content rows,
/// visible rows)`; `None` when Detail is not showing a stem.
pub fn detail_extent(model: &Model) -> Option<(usize, usize)> {
    let (c, h) = detail_content(model)?;
    Some((c.lines.len(), h))
}

/// The Detail content of the selected stem as drawn at [`Model::size`],
/// and the visible rows.
pub fn detail_content(model: &Model) -> Option<(crate::detail::Content, usize)> {
    let name = model.selected.as_deref()?;
    let (main, _) = body_areas(model)?;
    let inner = crate::detail::inner(main);
    Some((
        crate::detail::content(model, name, inner.width),
        usize::from(inner.height),
    ))
}

/// Whether the current view has the action bar (Table, Graph, Detail).
pub fn has_bar(model: &Model) -> bool {
    matches!(
        model.view,
        ViewKind::Table | ViewKind::Graph | ViewKind::Detail
    )
}

/// The status bar's unhealthy counter glyph: `↯` (`U` in ASCII); the
/// state's own glyph is the failed one (`✗`), counted apart.
pub fn unhealthy_marker(ascii: bool) -> &'static str {
    if ascii { "U" } else { "↯" }
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
    // The Logs view's title carries the stem strip (the split pane follows
    // the table / graph selection, which is its own picker).
    let strip = if model.view == ViewKind::Logs {
        let left = format!(" {title} · {} ", model.log_pane.status_text());
        let current = (!model.log_pane.merged)
            .then_some(model.selected.as_deref())
            .flatten();
        stem_strip(model, area, str_width(&left), current, "m merged")
    } else {
        None
    };
    let style = crate::logs::PaneStyle {
        title,
        ascii: model.ascii,
        accent: accent(model),
        strip,
    };
    model.log_pane.render(f, area, &style);
}

/// Display width of `s` in columns.
pub fn str_width(s: &str) -> u16 {
    Span::raw(s).width() as u16
}

/// The stem strip on the right of a Detail / Logs title (the stem picker):
/// ` a  [b]  c  d · [ ]/←→ switch `, the current stem bracketed, every
/// visible stem in table order. When it does not fit next to the title
/// (`left` columns) the hint goes first, then the stems furthest from the
/// current one (`…` marks the cut sides). Each stem's columns are recorded
/// in [`Model::stem_hits`] for mouse clicks. `None` with fewer than two
/// stems (nothing to switch to) or no room.
pub fn stem_strip(
    model: &Model,
    area: Rect,
    left: u16,
    current: Option<&str>,
    extra: &str,
) -> Option<Line<'static>> {
    let names: Vec<String> = model.visible().iter().map(|s| s.name.clone()).collect();
    if names.len() < 2 || area.width < 4 {
        return None;
    }
    let a = model.ascii;
    // Inside the corners, one border cell between the title and the strip.
    let max = area.width.saturating_sub(2 + left + 1);
    let cur = current.and_then(|c| names.iter().position(|n| n == c));
    let label = |i: usize| {
        if Some(i) == cur {
            format!("[{}]", names[i])
        } else {
            names[i].clone()
        }
    };
    let ell = if a { "..." } else { "…" };
    let arrows = if a { "<-/->" } else { "←/→" };
    let hint = if extra.is_empty() {
        format!("[ ] {arrows} switch")
    } else {
        format!("[ ] {arrows} switch · {extra}")
    };
    const GAP: u16 = 2;
    let width = |lo: usize, hi: usize, with_hint: bool| -> u16 {
        let items: u16 = (lo..=hi).map(|i| str_width(&label(i))).sum();
        let mut w = 2 + items + GAP * (hi - lo) as u16;
        if lo > 0 {
            w += str_width(ell) + GAP;
        }
        if hi + 1 < names.len() {
            w += str_width(ell) + GAP;
        }
        if with_hint {
            w += 3 + str_width(&hint);
        }
        w
    };
    let last = names.len() - 1;
    let (lo, hi, with_hint) = if width(0, last, true) <= max {
        (0, last, true)
    } else if width(0, last, false) <= max {
        (0, last, false)
    } else {
        // Grow a window around the current stem, right first.
        let c = cur.unwrap_or(0);
        if width(c, c, false) > max {
            return None;
        }
        let (mut lo, mut hi) = (c, c);
        loop {
            let mut grew = false;
            if hi < last && width(lo, hi + 1, false) <= max {
                hi += 1;
                grew = true;
            }
            if lo > 0 && width(lo - 1, hi, false) <= max {
                lo -= 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        (lo, hi, false)
    };
    let total = width(lo, hi, with_hint);
    let bold = Style::default()
        .fg(accent(model))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut x = (area.x + area.width).saturating_sub(1 + total);
    let mut spans = vec![Span::raw(" ")];
    x += 1;
    let mut hits = Vec::new();
    let push = |spans: &mut Vec<Span<'static>>, x: &mut u16, text: String, style: Style| {
        *x += str_width(&text);
        spans.push(Span::styled(text, style));
    };
    let gap = || " ".repeat(usize::from(GAP));
    if lo > 0 {
        push(&mut spans, &mut x, ell.to_string(), dim);
        push(&mut spans, &mut x, gap(), Style::default());
    }
    for (i, name) in names.iter().enumerate().take(hi + 1).skip(lo) {
        if i > lo {
            push(&mut spans, &mut x, gap(), Style::default());
        }
        let start = x;
        let style = if Some(i) == cur {
            bold
        } else {
            Style::default()
        };
        push(&mut spans, &mut x, label(i), style);
        hits.push(StemHit {
            stem: name.clone(),
            row: area.y,
            start,
            end: x,
        });
    }
    if hi < last {
        push(&mut spans, &mut x, gap(), Style::default());
        push(&mut spans, &mut x, ell.to_string(), dim);
    }
    if with_hint {
        push(&mut spans, &mut x, " · ".to_string(), dim);
        push(&mut spans, &mut x, hint, dim);
    }
    spans.push(Span::raw(" "));
    model.stem_hits.borrow_mut().extend(hits);
    Some(Line::from(spans).right_aligned())
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
        " {} · profile {} · daemon pid {} · {}{} {}{} {}{} {}{} {}{} {}{} {}{}",
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
        unhealthy_marker(model.ascii),
        s.unhealthy,
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
            (ViewKind::Detail, _) => {
                "j/k row · Enter act · [ ] stem · Esc back · ? help · q quit ".to_string()
            }
            (ViewKind::Logs, _) => {
                "Space pause · j/k scroll · [ ] stem · ? help · q quit ".to_string()
            }
            (ViewKind::Events, _) => "j/k scroll · Enter logs · ? help · q quit ".to_string(),
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

/// `http:18090,admin:18091` (`wide`) or `18090,18091`; `-` without ports.
pub fn ports_text(s: &StemStatus, wide: bool) -> String {
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
/// included): fixed columns, `STEM` up to its longest name, `REASON` the
/// rest; from [`EXTRA_COLUMNS_MIN`] also SCRIPTS, VARIANT and WATCH.
pub fn column_widths(model: &Model, width: u16) -> Vec<u16> {
    let wide = width >= 100;
    let extra = width >= EXTRA_COLUMNS_MIN;
    let mark = paused_marker(model.ascii).chars().count() + 1;
    let longest = model
        .stems
        .iter()
        .map(|s| s.name.chars().count() + if model.watch_paused(&s.name) { mark } else { 0 })
        .max()
        .unwrap_or(4)
        .max(4) as u16;
    let (stem_max, ty, st, pid, ports, up, rs, cpu, mem) = if extra {
        (20, 8, 12, 7, 12, 7, 8, 6, 6)
    } else if wide {
        (20, 8, 12, 7, 14, 7, 8, 10, 10)
    } else {
        (12, 7, 10, 6, 6, 6, 8, 5, 5)
    };
    let stem = longest.min(stem_max);
    let mut rest = vec![pid, ports, up, rs, cpu, mem];
    if extra {
        let variant = model
            .stems
            .iter()
            .filter_map(|s| s.variant.as_ref().map(|v| v.chars().count() as u16))
            .max()
            .unwrap_or(0)
            .clamp(7, 12);
        rest.extend([7, variant, 5]);
    }
    let columns = 4 + rest.len() as u16;
    let avail = width.saturating_sub(2 + columns - 1);
    let fixed = stem + ty + st + rest.iter().sum::<u16>();
    let reason = avail.saturating_sub(fixed);
    let mut out = vec![stem, ty, st, reason];
    out.extend(rest);
    out
}

fn table(model: &Model, f: &mut Frame, area: Rect) {
    let widths = column_widths(model, area.width);
    let wide = area.width >= 100;
    let extra = widths.len() > COLUMNS.len();
    let mut headers: Vec<&str> = COLUMNS.to_vec();
    if extra {
        headers.extend(EXTRA_COLUMNS);
    }
    let header = Row::new(headers.into_iter().map(Cell::from)).style(
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
            let mut cells = vec![
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
            ];
            if extra {
                let scripts = model
                    .custom_scripts(&s.name)
                    .map_or_else(|| "-".to_string(), |n| n.to_string());
                let variant = s.variant.clone().unwrap_or_else(|| "-".into());
                let watch = match &s.watch {
                    None => "-".to_string(),
                    Some(w) if w.paused => paused_marker(a).to_string(),
                    Some(_) => "on".to_string(),
                };
                cells.push(Cell::from(scripts));
                cells.push(Cell::from(truncate(&variant, usize::from(widths[11]), a)));
                cells.push(Cell::from(watch));
            }
            Row::new(cells)
        })
        .collect();
    let empty = rows.is_empty();
    let t = Table::new(rows, widths.iter().map(|w| Constraint::Length(*w)))
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
    ("j/k ↓/↑", "move · detail: rows (scroll)"),
    ("PgUp PgDn", "detail: scroll a page"),
    ("g / G", "first / last · top / end"),
    ("[ / ]", "prev / next stem (wraps)"),
    ("h/l ←/→", "graph cols · detail stem"),
    ("f e + -", "graph: focus labels zoom"),
    ("Enter", "detail · run/switch/pause row"),
    ("Tab S-Tab", "next / previous view"),
    ("1-5", "go to a view (header #)"),
    ("/", "filter stems by name"),
    ("O", "cycle the sort order"),
    ("Ctrl-L", "split: logs below"),
    ("click", "row/tab/strip/bar (mouse)"),
    ("wheel", "scroll / move (mouse)"),
    ("s", "start the stem"),
    ("x / X", "stop / with dependants"),
    ("r / R", "restart / rebuild"),
    ("p", "pause/resume watchdog"),
    ("v", "switch variant (picker)"),
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
    ("[ ] h/l ←/→", "previous / next stem (wraps; leaves m)"),
    ("j / k, g / G", "move · top / bottom (follows)"),
    ("PgUp / PgDn", "move a page"),
    ("wheel", "scroll (up leaves follow; mouse = true)"),
    ("Enter", "expand the fields of a structured line"),
    ("V, y", "visual range · copy line or range"),
    ("w / t", "wrap long lines / timestamps (UTC)"),
    ("Tab, Ctrl-L", "next view · split pane elsewhere"),
    ("1-5, click", "go to a view (header number or tab)"),
    ("click", "a stem in the title strip (mouse = true)"),
    ("q", "quit"),
];

/// Key help of the Events view.
pub const EVENTS_HELP: &[(&str, &str)] = &[
    ("j / k, g / G", "move · first / newest (follows)"),
    ("PgUp / PgDn", "move a page"),
    ("wheel", "scroll (mouse = true)"),
    ("/", "filter by stem, kind, state or reason"),
    ("Enter", "the stem's logs at the event time"),
    ("Esc", "clear the filter"),
    ("Tab, 1-5", "next view · go to a view"),
    ("[ / ]", "previous / next stem (selection)"),
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
