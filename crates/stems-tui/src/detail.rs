//! The Detail view: the selected stem in sections, top to bottom:
//!
//! * a header (state, reason, pid, uptime, restarts, variant, ports);
//! * **Scripts**: one selectable row per script of the stem (custom first,
//!   then lifecycle; name, kind, last run, args, description). `Enter` runs
//!   it (its form first when it declares `args`);
//! * **Variants** (stems with `variants`): `local` and each variant with its
//!   type, the active one marked. `Enter` switches (after a confirmation);
//! * **Watchdog** (stems with `watch:` rules): paused or on, the last
//!   trigger, then the rules (`paths → action · debounce`). `Enter` (or
//!   `p`) pauses / resumes it;
//! * Health (the probe summary and the last results), Recent events and
//!   the effective Config.
//!
//! `j`/`k` move over the selectable rows of every section (the view
//! scrolls to keep the row visible; past the first / last row they scroll);
//! `PageUp`/`PageDown`, `g`/`G` and the wheel scroll.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::Value;
use stems_api::StemStatus;
use stems_core::Glyph;
use stems_core::scriptargs::ScriptKind;

use crate::actions::MenuEntry;
use crate::graph::glyph_color;
use crate::model::{DetailData, Model, ScriptActivity};
use crate::view::{accent, config_lines, paused_marker, ports_text, str_width, truncate, uptime};

/// Events shown in the Recent events section.
pub const RECENT_EVENTS: usize = 10;

/// A selectable row of the Detail view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailRow {
    /// A script of the stem: `Enter` runs it (or opens its form).
    Script(String),
    /// A variant choice (`local` or a variant): `Enter` switches to it.
    Variant(String),
    /// The watchdog's state line: `Enter` pauses / resumes it.
    Watch,
}

/// The Detail view's lines for one stem and where its selectable rows are.
#[derive(Clone, Debug, Default)]
pub struct Content {
    /// Every line, top to bottom.
    pub lines: Vec<Line<'static>>,
    /// The selectable rows in order, with their line index.
    pub rows: Vec<(DetailRow, usize)>,
}

/// The stem's scripts in the order the Scripts section lists them: custom
/// ones by name, then lifecycle ones by name.
pub fn stem_scripts<'a>(catalog: &'a [MenuEntry], stem: &str) -> Vec<&'a MenuEntry> {
    let mut v: Vec<&MenuEntry> = catalog
        .iter()
        .filter(|e| e.stem.as_deref() == Some(stem))
        .collect();
    v.sort_by_key(|e| (e.kind != ScriptKind::Custom, e.name.clone()));
    v
}

/// The selectable rows of `stem`'s Detail (width does not matter).
pub fn rows(model: &Model, stem: &str) -> Vec<DetailRow> {
    content(model, stem, 80)
        .rows
        .into_iter()
        .map(|(r, _)| r)
        .collect()
}

/// `email*, role`: the arguments of a script (`*` = required).
fn args_summary(e: &MenuEntry) -> String {
    e.args
        .iter()
        .map(|a| format!("{}{}", a.name, if a.required { "*" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ")
}

fn activity_text(a: Option<&ScriptActivity>, ascii: bool) -> (String, Style) {
    let (ok, bad) = if ascii { ("OK", "X") } else { ("✓", "✗") };
    match a {
        None => (String::new(), Style::default()),
        Some(ScriptActivity::Queued) => (
            "queued".into(),
            Style::default().fg(ratatui::style::Color::Yellow),
        ),
        Some(ScriptActivity::Running) => (
            if ascii { "running..." } else { "running…" }.into(),
            Style::default().fg(ratatui::style::Color::Cyan),
        ),
        Some(ScriptActivity::Finished { ok: true, text }) => (
            format!("{ok} {text}"),
            Style::default().fg(ratatui::style::Color::Green),
        ),
        Some(ScriptActivity::Finished { ok: false, text }) => (
            format!("{bad} {text}"),
            Style::default().fg(ratatui::style::Color::Red),
        ),
    }
}

/// Build the Detail content of `name` for an inner width of `width`.
pub fn content(model: &Model, name: &str, width: u16) -> Content {
    let st = model.stems.iter().find(|s| s.name == name);
    let a = model.ascii;
    let d = model.detail.as_ref().filter(|d| d.stem == name);
    let w = usize::from(width.max(10));
    let head_style = Style::default()
        .fg(accent(model))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut c = Content::default();
    let heading = |c: &mut Content, title: &str, hint: &str| {
        let mut spans = vec![Span::styled(title.to_string(), head_style)];
        if !hint.is_empty() {
            spans.push(Span::styled(format!(" · {hint}"), dim));
        }
        c.lines.push(Line::from(spans));
    };
    // Selectable rows get the marker and, when selected, reverse video.
    let row_index = |c: &Content| c.rows.len();
    let marker = |selected: bool| match (selected, a) {
        (false, _) => "  ",
        (true, false) => "› ",
        (true, true) => "> ",
    };
    let sel = model.detail_row;

    // Header.
    if let Some(s) = st {
        c.lines.push(Line::from(vec![
            Span::styled(
                format!("{} {}", s.glyph.symbol(!a), s.state),
                Style::default().fg(glyph_color(s.glyph)),
            ),
            Span::raw(
                s.reason
                    .as_deref()
                    .map(|r| format!(" · {r}"))
                    .unwrap_or_default(),
            ),
        ]));
        let mut facts = format!(
            "pid {} · up {} · restarts {}",
            s.pid.map_or_else(|| "-".into(), |p| p.to_string()),
            uptime(s.uptime_s),
            s.restarts
        );
        if let Some(v) = &s.variant {
            facts.push_str(&format!(" · variant {v}"));
        }
        c.lines.push(Line::from(facts));
        c.lines
            .push(Line::from(format!("ports {}", ports_text(s, true))));
    } else {
        c.lines.push(Line::from("not in the last status"));
    }

    // Scripts.
    c.lines.push(Line::from(""));
    let scripts = model.catalog.as_deref().map(|cat| stem_scripts(cat, name));
    let hint = match &scripts {
        Some(v) if !v.is_empty() => "Enter run · : menu",
        _ => "",
    };
    heading(&mut c, "Scripts", hint);
    match &scripts {
        None => c
            .lines
            .push(Line::styled(if a { "loading..." } else { "loading…" }, dim)),
        Some(v) if v.is_empty() => c.lines.push(Line::styled("no scripts", dim)),
        Some(v) => {
            let nw = v
                .iter()
                .map(|e| e.name.chars().count())
                .max()
                .unwrap_or(4)
                .clamp(4, 24);
            for e in v {
                let i = row_index(&c);
                let selected = i == sel;
                let kind = match e.kind {
                    ScriptKind::Custom => "custom",
                    ScriptKind::Lifecycle => "lifecycle",
                };
                let act = model
                    .script_activity
                    .get(&(Some(name.to_string()), e.name.clone()));
                let (act_text, act_style) = activity_text(act, a);
                let args = args_summary(e);
                let mut spans = vec![
                    Span::raw(marker(selected).to_string()),
                    Span::raw(format!("{:<nw$} ", truncate(&e.name, nw, a))),
                    Span::styled(format!("{kind:<9} "), dim),
                ];
                if !act_text.is_empty() {
                    spans.push(Span::styled(format!("{act_text} "), act_style));
                }
                if !args.is_empty() {
                    spans.push(Span::raw(format!("({args}) ")));
                }
                if let Some(desc) = &e.description {
                    spans.push(Span::styled(desc.clone(), dim));
                }
                c.rows
                    .push((DetailRow::Script(e.name.clone()), c.lines.len()));
                c.lines.push(fit(spans, w, selected, a));
            }
        }
    }

    // Variants.
    if st.is_some_and(|s| s.variant.is_some()) {
        c.lines.push(Line::from(""));
        heading(&mut c, "Variants", "Enter switch · v picker");
        match model.variants.get(name) {
            None => c
                .lines
                .push(Line::styled(if a { "loading..." } else { "loading…" }, dim)),
            Some(Err(e)) => c.lines.push(Line::styled(format!("unavailable: {e}"), dim)),
            Some(Ok(v)) => {
                let nw = v
                    .iter()
                    .map(|x| x.name.chars().count())
                    .max()
                    .unwrap_or(5)
                    .clamp(5, 24);
                for x in v {
                    let i = row_index(&c);
                    let selected = i == sel;
                    let mark = match (x.active, a) {
                        (true, false) => "●",
                        (false, false) => "○",
                        (true, true) => "*",
                        (false, true) => " ",
                    };
                    let mut spans = vec![
                        Span::raw(marker(selected).to_string()),
                        Span::styled(format!("{mark} "), Style::default().fg(accent(model))),
                        Span::raw(format!("{:<nw$} ", truncate(&x.name, nw, a))),
                        Span::styled(format!("{:<8}", x.kind.as_deref().unwrap_or("-")), dim),
                    ];
                    if x.active {
                        spans.push(Span::styled(" active", Style::default().fg(accent(model))));
                    } else if x.name == "local" {
                        spans.push(Span::styled(" base definition", dim));
                    }
                    c.rows
                        .push((DetailRow::Variant(x.name.clone()), c.lines.len()));
                    c.lines.push(fit(spans, w, selected, a));
                }
            }
        }
    }

    // Watchdog.
    if let Some(ws) = st.and_then(|s| s.watch.as_ref()) {
        c.lines.push(Line::from(""));
        heading(&mut c, "Watchdog", "Enter/p pause·resume");
        let detail = d
            .and_then(|d| d.watch.as_ref())
            .and_then(|r| r.as_ref().ok());
        let i = row_index(&c);
        let selected = i == sel;
        let mut spans = vec![Span::raw(marker(selected).to_string())];
        if ws.paused {
            spans.push(Span::styled(
                format!("{} paused", paused_marker(a)),
                Style::default().fg(ratatui::style::Color::Yellow),
            ));
        } else {
            spans.push(Span::styled(
                "watch on",
                Style::default().fg(ratatui::style::Color::Green),
            ));
        }
        if let Some(dw) = detail {
            if !dw.active {
                spans.push(Span::styled(" · inactive (stem not running)", dim));
            }
            let last = dw
                .last_triggered
                .map_or_else(|| "never".to_string(), |t| t.format("%H:%M:%S").to_string());
            spans.push(Span::styled(format!(" · last trigger {last}"), dim));
            if dw.busy {
                spans.push(Span::styled(" · acting", dim));
            }
        }
        c.rows.push((DetailRow::Watch, c.lines.len()));
        c.lines.push(fit(spans, w, selected, a));
        let arrow = if a { "->" } else { "→" };
        match detail {
            Some(dw) => {
                for r in &dw.rules {
                    let mut t = format!(
                        "  {} {arrow} {} · debounce {}",
                        r.paths.join(", "),
                        r.action,
                        ms_text(r.debounce_ms)
                    );
                    if r.settle_ms > 0 {
                        t.push_str(&format!(" · settle {}", ms_text(r.settle_ms)));
                    }
                    c.lines.push(Line::from(truncate(&t, w, a)));
                }
            }
            None => c.lines.push(Line::styled(
                format!(
                    "  {} rule{}",
                    ws.rules,
                    if ws.rules == 1 { "" } else { "s" }
                ),
                dim,
            )),
        }
    }

    // Health.
    c.lines.push(Line::from(""));
    heading(&mut c, "Health", "");
    c.lines
        .extend(health_lines(model, st, d).into_iter().map(Line::from));

    // Recent events.
    c.lines.push(Line::from(""));
    heading(&mut c, "Recent events", "");
    let events = d.map(|d| d.events.as_slice()).unwrap_or_default();
    if events.is_empty() {
        c.lines.push(Line::styled("-", dim));
    }
    for e in events.iter().rev().take(RECENT_EVENTS).rev() {
        let mut s = format!("{} {}", e.ts.format("%H:%M:%S"), e.kind);
        if let (Some(from), Some(to)) = (&e.from, &e.to) {
            s.push_str(&format!(" {from}→{to}"));
        }
        c.lines.push(Line::from(truncate(&s, w, a)));
    }

    // Config.
    c.lines.push(Line::from(""));
    heading(&mut c, "Config", "");
    match d.and_then(|d| d.config.as_ref()) {
        None => c
            .lines
            .push(Line::from(if a { "loading..." } else { "loading…" })),
        Some(Err(e)) => c.lines.push(Line::from(format!("unavailable: {e}"))),
        Some(Ok(v)) => c.lines.extend(config_lines(v).into_iter().map(Line::from)),
    }
    c
}

/// `200ms`, `1s`, `1.5s`.
fn ms_text(ms: u64) -> String {
    if ms >= 1000 && ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// A row cut to `w` columns (`…`); reversed when selected.
fn fit(spans: Vec<Span<'static>>, w: usize, selected: bool, ascii: bool) -> Line<'static> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let sw = str_width(&s.content) as usize;
        if used + sw <= w {
            used += sw;
            out.push(s);
            continue;
        }
        let room = w.saturating_sub(used);
        if room > 0 {
            out.push(Span::styled(truncate(&s.content, room, ascii), s.style));
        }
        break;
    }
    let line = Line::from(out);
    if selected {
        line.style(Style::default().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

/// The health section: the probe summary from `status`, then the last
/// probe results from the `health` RPC (newest last, at most 3), or `n/a`.
pub fn health_lines(model: &Model, st: Option<&StemStatus>, d: Option<&DetailData>) -> Vec<String> {
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

/// The inner area of the Detail view's block.
pub fn inner(area: Rect) -> Rect {
    area.inner(Margin::new(1, 1))
}

/// Draw the Detail view of the selected stem into `area`.
pub fn render(model: &Model, f: &mut Frame, area: Rect) {
    let Some(name) = model.selected.as_deref() else {
        let block = Block::default().borders(Borders::ALL).title(" Detail ");
        f.render_widget(Paragraph::new(" no stem selected").block(block), area);
        return;
    };
    let st = model.selected_stem();
    let a = model.ascii;
    let inner = inner(area);
    let c = content(model, name, inner.width);
    let rows = c.lines.len();
    let h = usize::from(inner.height);
    let max = rows.saturating_sub(h);
    let off = model.detail_scroll.min(max);
    let mut title = match st {
        Some(s) => format!(" Detail: {name} · {} {} ", s.glyph.symbol(!a), s.state),
        None => format!(" Detail: {name} "),
    };
    if max > 0 {
        // Which rows show: `lines 11-30/45` (PgUp/PgDn scroll).
        title.push_str(&format!("· lines {}-{}/{rows} ", off + 1, off + h));
    }
    let strip = crate::view::stem_strip(model, area, str_width(&title), Some(name), "");
    let mut block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    if let Some(strip) = strip {
        block = block.title(strip);
    }
    f.render_widget(block, area);
    let scroll = (u16::try_from(off).unwrap_or(u16::MAX), 0);
    f.render_widget(Paragraph::new(c.lines).scroll(scroll), inner);
}
