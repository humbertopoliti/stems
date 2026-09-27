//! Rendering of the dialogs ([`Modal`]) and toasts (30). Pure functions of
//! the model, called by [`crate::view::view`] after the body.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use stems_config::ArgType;
use stems_core::scriptargs::ScriptKind;

use crate::actions::{MenuEntry, Palette, ScriptMenu, palette_matches};
use crate::form::{FieldValue, ScriptForm};
use crate::model::{Modal, Model};
use crate::toast::ToastKind;
use crate::view::{accent, centered, truncate};

/// Lines of the palette's match list.
pub const PALETTE_ROWS: usize = 10;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// A bordered, cleared box with `title` and `lines`.
fn boxed(f: &mut Frame, r: Rect, title: &str, lines: Vec<Line<'_>>) {
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(format!(" {title} "), bold())),
        ),
        r,
    );
}

/// Draw the open dialog, if any.
pub fn render(model: &Model, f: &mut Frame, area: Rect) {
    match &model.modal {
        Modal::None => {}
        Modal::QuitConfirm => quit(f, area),
        Modal::StopConfirm { stem, dependants } => stop_confirm(model, f, area, stem, dependants),
        Modal::DownConfirm => down_confirm(model, f, area),
        Modal::ResetTyped { stem, buffer } => reset_typed(model, f, area, stem, buffer),
        Modal::ScriptMenu(m) => script_menu(model, f, area, m),
        Modal::ScriptForm(form) => script_form(model, f, area, form),
        Modal::Palette(p) => palette(model, f, area, p),
        Modal::ErrorDetails { title, text } => error_details(f, area, title, text),
        Modal::Plan(p) => plan(f, area, p.as_ref()),
        Modal::RestartConfirm { stem, dependants } => {
            restart_confirm(model, f, area, stem, dependants)
        }
        Modal::VariantPicker { stem, selected } => variant_picker(model, f, area, stem, *selected),
        Modal::SwitchConfirm {
            stem,
            from,
            variant,
            kind,
            running,
        } => switch_confirm(
            model,
            f,
            area,
            stem,
            from,
            variant,
            kind.as_deref(),
            *running,
        ),
    }
}

fn restart_confirm(model: &Model, f: &mut Frame, area: Rect, stem: &str, dependants: &[String]) {
    let w = 56.min(area.width.saturating_sub(4)).max(24);
    let inner = usize::from(w) - 3;
    let n = dependants.len();
    let lines = vec![
        Line::styled(
            truncate(
                &format!(
                    " also restart {n} dependant{} ({})? [y/N]",
                    if n == 1 { "" } else { "s" },
                    dependants.join(", ")
                ),
                inner,
                model.ascii,
            ),
            bold(),
        ),
        Line::from(""),
        Line::from(" y    restart them too, in dependency order"),
        Line::from(truncate(
            &format!(" n    restart only {stem}"),
            inner,
            model.ascii,
        )),
        Line::from(" Esc  cancel"),
    ];
    boxed(f, centered(area, w, 7), &format!("Restart {stem}"), lines);
}

fn variant_picker(model: &Model, f: &mut Frame, area: Rect, stem: &str, selected: usize) {
    let w = 48.min(area.width.saturating_sub(4)).max(24);
    let inner = usize::from(w) - 3;
    let mut lines: Vec<Line> = Vec::new();
    match model.variants.get(stem) {
        None => lines.push(Line::from(" loading…")),
        Some(Err(e)) => lines.push(Line::styled(
            truncate(&format!(" {e}"), inner, model.ascii),
            Style::default().fg(Color::Red),
        )),
        Some(Ok(v)) => {
            let nw = v
                .iter()
                .map(|c| c.name.chars().count())
                .max()
                .unwrap_or(5)
                .clamp(5, 24);
            for (i, c) in v.iter().enumerate() {
                let marker = match (i == selected, model.ascii) {
                    (false, _) => "  ",
                    (true, false) => "› ",
                    (true, true) => "> ",
                };
                let mark = match (c.active, model.ascii) {
                    (true, false) => "●",
                    (false, false) => "○",
                    (true, true) => "*",
                    (false, true) => " ",
                };
                let mut text = format!(
                    " {marker}{mark} {:<nw$}  {:<8}",
                    c.name,
                    c.kind.as_deref().unwrap_or("-")
                );
                if c.active {
                    text.push_str(" active");
                }
                let text = truncate(&text, inner, model.ascii);
                lines.push(if i == selected {
                    Line::styled(text, Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    Line::from(text)
                });
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(" ↑/↓ move · Enter switch · Esc", dim()));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    boxed(
        f,
        centered(area, w, h),
        &format!("Variant of {stem}"),
        lines,
    );
}

#[allow(clippy::too_many_arguments)]
fn switch_confirm(
    model: &Model,
    f: &mut Frame,
    area: Rect,
    stem: &str,
    from: &str,
    variant: &str,
    kind: Option<&str>,
    running: bool,
) {
    let w = 52.min(area.width.saturating_sub(4)).max(24);
    let inner = usize::from(w) - 3;
    let question = if running {
        format!(" restart {stem} as {variant}? [y/N]")
    } else {
        format!(" switch {stem} to {variant}? [y/N]")
    };
    let arrow = if model.ascii { "->" } else { "▸" };
    let lines = vec![
        Line::styled(truncate(&question, inner, model.ascii), bold()),
        Line::from(truncate(
            &format!(" {from} {arrow} {variant} ({})", kind.unwrap_or("-")),
            inner,
            model.ascii,
        )),
        Line::from(""),
        Line::from(" y  write stems.local.yaml and apply"),
        Line::from(" n  cancel"),
    ];
    boxed(f, centered(area, w, 7), "Switch variant", lines);
}

fn quit(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(" Stop everything? [y/N/d(etach)]"),
        Line::from(""),
        Line::from(" y  stop every stem and the daemon"),
        Line::from(" d  detach: leave them running"),
        Line::from(" n  cancel"),
    ];
    boxed(f, centered(area, 38, 7), "Quit", lines);
}

fn stop_confirm(model: &Model, f: &mut Frame, area: Rect, stem: &str, dependants: &[String]) {
    let w = 50.min(area.width.saturating_sub(4)).max(20);
    let inner = usize::from(w) - 3;
    let deps = if dependants.is_empty() {
        "-".to_string()
    } else {
        dependants.join(", ")
    };
    let lines = vec![
        Line::from(format!(" Stop {stem}? Running stems depend on it:")),
        Line::styled(
            format!(" {}", truncate(&deps, inner, model.ascii)),
            bold().fg(Color::Yellow),
        ),
        Line::from(""),
        Line::from(" y / X  stop them too (cascade)"),
        Line::from(" n      cancel"),
    ];
    boxed(f, centered(area, w, 7), &format!("Stop {stem}"), lines);
}

fn down_confirm(model: &Model, f: &mut Frame, area: Rect) {
    let what = match model.mode {
        crate::model::AttachMode::Up => " y  down --all, then leave the dashboard",
        crate::model::AttachMode::Attach => " y  down --all (the dashboard closes)",
    };
    let lines = vec![
        Line::from(" Stop every stem and the daemon? [y/N]"),
        Line::from(""),
        Line::from(what),
        Line::from(" n  cancel"),
    ];
    boxed(f, centered(area, 46, 6), "Down", lines);
}

fn reset_typed(model: &Model, f: &mut Frame, area: Rect, stem: &str, buffer: &str) {
    let w = 52.min(area.width.saturating_sub(4)).max(20);
    let inner = usize::from(w) - 3;
    let lines = vec![
        Line::from(truncate(
            &format!(" Stop {stem}, run its reset script, clear its stamps."),
            inner,
            model.ascii,
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw(" Type "),
            Span::styled("reset", bold()),
            Span::raw(" to confirm: "),
            Span::styled(format!("{buffer}_"), bold().fg(accent(model))),
        ]),
        Line::from(""),
        Line::styled(" Enter confirm · Esc cancel", dim()),
    ];
    boxed(f, centered(area, w, 7), &format!("Reset {stem}"), lines);
}

/// One menu row: name column, then the description (or the kind).
fn menu_row(
    e: &MenuEntry,
    name_w: usize,
    width: usize,
    selected: bool,
    model: &Model,
) -> Line<'static> {
    let marker = if !selected {
        "  "
    } else if model.ascii {
        "> "
    } else {
        "› "
    };
    let desc = match (&e.description, e.kind) {
        (Some(d), _) => d.clone(),
        (None, ScriptKind::Lifecycle) => "(lifecycle)".into(),
        (None, ScriptKind::Custom) => String::new(),
    };
    let args = if e.args.is_empty() { "" } else { " …" };
    let name = truncate(&format!("{}{args}", e.name), name_w, model.ascii);
    let room = width.saturating_sub(3 + name_w + 2);
    let text = format!(
        " {marker}{name:<name_w$}  {}",
        truncate(&desc, room, model.ascii)
    );
    if selected {
        Line::styled(text, Style::default().add_modifier(Modifier::REVERSED))
    } else {
        Line::from(text)
    }
}

fn script_menu(model: &Model, f: &mut Frame, area: Rect, m: &ScriptMenu) {
    let w = 72.min(area.width.saturating_sub(4)).max(24);
    let inner_w = usize::from(w) - 2;
    let title = match &m.stem {
        Some(s) => format!("Scripts: {s}"),
        None => "Scripts".to_string(),
    };
    let mut body: Vec<Line> = Vec::new();
    let mut sel_row = 0usize;
    match model.catalog.as_deref() {
        None => body.push(Line::from(" loading…")),
        Some(cat) => {
            let vis = m.visible(cat);
            let name_w = vis
                .iter()
                .map(|e| e.name.chars().count() + if e.args.is_empty() { 0 } else { 2 })
                .max()
                .unwrap_or(4)
                .clamp(4, 24);
            if vis.is_empty() {
                body.push(Line::from(if m.filter.is_empty() {
                    " no scripts"
                } else {
                    " no script matches"
                }));
            }
            let sections = m.filter.trim().is_empty();
            let mut last: Option<Option<&str>> = None;
            for (i, e) in vis.iter().enumerate() {
                if sections && last != Some(e.stem.as_deref()) {
                    let head = match &e.stem {
                        Some(s) => format!(" {s}"),
                        None => " workspace".to_string(),
                    };
                    body.push(Line::styled(head, bold().fg(accent(model))));
                    last = Some(e.stem.as_deref());
                }
                if i == m.selected {
                    sel_row = body.len();
                }
                body.push(menu_row(e, name_w, inner_w, i == m.selected, model));
            }
        }
    }
    let max_body = usize::from(area.height.saturating_sub(6)).max(1);
    let skip = sel_row.saturating_sub(max_body.saturating_sub(1));
    let shown: Vec<Line> = body.into_iter().skip(skip).take(max_body).collect();
    let mut lines = vec![Line::from(vec![
        Span::styled(" filter: ", dim()),
        Span::raw(format!("{}_", m.filter)),
    ])];
    let n = shown.len();
    lines.extend(shown);
    lines.push(Line::styled(
        " ↑/↓ move · Enter run (… = form) · type to filter · Esc",
        dim(),
    ));
    let h = (n as u16 + 4).min(area.height.saturating_sub(2));
    boxed(f, centered(area, w, h), &title, lines);
}

fn field_value(model: &Model, form: &ScriptForm, i: usize) -> String {
    let fld = &form.fields[i];
    let focused = i == form.focus;
    match (&fld.value, fld.arg.kind) {
        (FieldValue::Text(s), _) => {
            if focused {
                format!("{s}_")
            } else if s.is_empty() {
                "-".into()
            } else {
                s.clone()
            }
        }
        (FieldValue::Choice(_), ArgType::Enum) => {
            let (l, r) = if model.ascii {
                ("<", ">")
            } else {
                ("‹", "›")
            };
            format!("{l} {} {r}", fld.display())
        }
        (FieldValue::Toggle(b), _) => match b {
            Some(true) => "[x] true".into(),
            Some(false) => "[ ] false".into(),
            None => "[-] not set".into(),
        },
        _ => fld.display(),
    }
}

fn script_form(model: &Model, f: &mut Frame, area: Rect, form: &ScriptForm) {
    let w = 64.min(area.width.saturating_sub(4)).max(24);
    let inner = usize::from(w) - 3;
    let title = match &form.stem {
        Some(s) => format!("Run {} ({s})", form.script),
        None => format!("Run {} (workspace)", form.script),
    };
    let name_w = form
        .fields
        .iter()
        .map(|f| f.arg.name.chars().count() + usize::from(f.arg.required))
        .max()
        .unwrap_or(4)
        .clamp(4, 20);
    let mut lines: Vec<Line> = Vec::new();
    for (i, fld) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let marker = match (focused, model.ascii) {
            (false, _) => "  ",
            (true, false) => "› ",
            (true, true) => "> ",
        };
        let name = format!(
            "{}{}",
            fld.arg.name,
            if fld.arg.required { "*" } else { "" }
        );
        let mut st = Style::default();
        if focused {
            st = st.fg(accent(model)).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker}{name:<name_w$}  "), st),
            Span::raw(truncate(
                &field_value(model, form, i),
                inner.saturating_sub(name_w + 5),
                model.ascii,
            )),
        ]));
        if let Some(e) = &fld.error {
            let g = if model.ascii { "X" } else { "✗" };
            lines.push(Line::styled(
                truncate(&format!("     {g} {e}"), inner, model.ascii),
                Style::default().fg(Color::Red),
            ));
        } else if let Some(d) = &fld.arg.description {
            lines.push(Line::styled(
                truncate(&format!("     {d}"), inner, model.ascii),
                dim(),
            ));
        }
    }
    if let Some(e) = &form.error {
        lines.push(Line::styled(
            truncate(&format!(" {e}"), inner, model.ascii),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        " Enter run · Tab next · ←/→ choose · Space toggle · Esc",
        dim(),
    ));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    boxed(f, centered(area, w, h), &title, lines);
}

fn palette(model: &Model, f: &mut Frame, area: Rect, p: &Palette) {
    let w = 60.min(area.width.saturating_sub(4)).max(24);
    let inner = usize::from(w) - 3;
    let matches = palette_matches(model, p);
    let mut lines = vec![Line::from(vec![
        Span::styled(" > ", bold().fg(accent(model))),
        Span::raw(format!("{}_", p.query)),
    ])];
    let rows = PALETTE_ROWS.min(usize::from(area.height.saturating_sub(6)).max(1));
    let skip = p.selected.saturating_sub(rows - 1);
    if matches.is_empty() {
        lines.push(Line::styled(" no match", dim()));
    }
    for (i, m) in matches.iter().enumerate().skip(skip).take(rows) {
        let marker = match (i == p.selected, model.ascii) {
            (false, _) => "  ",
            (true, false) => "› ",
            (true, true) => "> ",
        };
        let text = truncate(&format!(" {marker}{}", m.label), inner, model.ascii);
        lines.push(if i == p.selected {
            Line::styled(text, Style::default().add_modifier(Modifier::REVERSED))
        } else {
            Line::from(text)
        });
    }
    lines.push(Line::styled(" ↑/↓ move · Enter run · Esc close", dim()));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + 2.min(area.height.saturating_sub(h)),
        w,
        h,
    );
    boxed(f, r, "Command palette", lines);
}

fn error_details(f: &mut Frame, area: Rect, title: &str, text: &str) {
    let w = 72.min(area.width.saturating_sub(4)).max(24);
    let mut lines: Vec<Line> = text.lines().map(|l| Line::from(format!(" {l}"))).collect();
    lines.push(Line::from(""));
    lines.push(Line::styled(" Esc close", dim()));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(format!(" {title} "), bold().fg(Color::Red))),
        ),
        r,
    );
}

fn plan(f: &mut Frame, area: Rect, p: Option<&Result<Vec<String>, String>>) {
    let w = 72.min(area.width.saturating_sub(4)).max(24);
    let mut lines: Vec<Line> = match p {
        None => vec![Line::from(" loading…")],
        Some(Err(e)) => vec![Line::styled(
            format!(" {e}"),
            Style::default().fg(Color::Red),
        )],
        Some(Ok(l)) => l.iter().map(|x| Line::from(format!(" {x}"))).collect(),
    };
    lines.push(Line::from(""));
    lines.push(Line::styled(" a apply · Esc close", dim()));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    boxed(f, centered(area, w, h), "Config changes", lines);
}

/// The toasts, stacked bottom-right above the status bar and the action
/// bar (newest lowest).
pub fn toasts(model: &Model, f: &mut Frame, area: Rect) {
    if model.toasts.is_empty() || area.height < 4 {
        return;
    }
    let max_w = 60.min(area.width.saturating_sub(2));
    // Above the status bar and the action bar.
    let bars = 1 + u16::from(crate::view::has_bar(model));
    let mut bottom = area.y + area.height - bars;
    for t in model.toasts.items.iter().rev() {
        let lines = t.lines(model.ascii);
        let w = (lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16 + 4)
            .clamp(16, max_w.max(16))
            .min(area.width);
        let h = lines.len() as u16 + 2;
        if bottom < area.y + 1 + h {
            break;
        }
        let y = bottom - h;
        let r = Rect::new(area.x + area.width - w, y, w, h);
        let color = match t.kind {
            ToastKind::Ok => Color::Green,
            ToastKind::Error => Color::Red,
            ToastKind::Info => Color::Cyan,
            ToastKind::ConfigChanged => Color::Yellow,
        };
        let inner = usize::from(w) - 3;
        let body: Vec<Line> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let text = format!(" {}", truncate(l, inner, model.ascii));
                if i == 0 {
                    Line::styled(text, Style::default().fg(color))
                } else {
                    Line::styled(text, dim())
                }
            })
            .collect();
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(body).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color)),
            ),
            r,
        );
        bottom = y;
    }
}
