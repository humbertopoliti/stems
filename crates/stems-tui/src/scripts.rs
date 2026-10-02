//! The Scripts view: every runnable script of the catalogue
//! (`script_catalog`) in one place, grouped by what it belongs to: the
//! workspace's scripts first, then each stem's in table order (custom ones
//! by name, then lifecycle ones, as in the Detail view's Scripts section).
//!
//! Each row shows the name, the kind, the last run the dashboard saw
//! (`script.*` events, any actor), the arguments and the description.
//! `j`/`k` move, `Enter` runs the script (its form first when it declares
//! `args`), `/` filters (fuzzy, on the script and its owner). Moving onto a
//! stem's script selects that stem, so the split log pane and the other
//! views follow.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use stems_core::scriptargs::ScriptKind;

use crate::actions::MenuEntry;
use crate::detail::{activity_text, args_summary, fit, stem_scripts};
use crate::fuzzy;
use crate::graph::glyph_color;
use crate::model::Model;
use crate::view::{accent, truncate};

/// The group label of the workspace-level scripts.
pub const WORKSPACE_GROUP: &str = "workspace";

/// State of the Scripts view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptsState {
    /// Filter text (`/`): fuzzy on `<script> <owner>`.
    pub filter: String,
    /// Typing the filter.
    pub editing: bool,
    /// Selected position among [`entries`].
    pub selected: usize,
}

/// The owner shown for an entry: its stem, or [`WORKSPACE_GROUP`].
fn owner(e: &MenuEntry) -> &str {
    e.stem.as_deref().unwrap_or(WORKSPACE_GROUP)
}

/// The scripts listed, in order: the workspace's (by name), then each
/// stem's in table order (stems the last `status` does not know come last,
/// by name); only those matching the filter.
pub fn entries(model: &Model) -> Vec<&MenuEntry> {
    let Some(catalog) = model.catalog.as_deref() else {
        return Vec::new();
    };
    let mut out: Vec<&MenuEntry> = catalog.iter().filter(|e| e.stem.is_none()).collect();
    out.sort_by_key(|e| e.name.clone());
    let mut stems: Vec<&str> = model.stems.iter().map(|s| s.name.as_str()).collect();
    let mut unknown: Vec<&str> = catalog
        .iter()
        .filter_map(|e| e.stem.as_deref())
        .filter(|s| !stems.contains(s))
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    stems.extend(unknown);
    for stem in stems {
        out.extend(stem_scripts(catalog, stem));
    }
    let filter = model.scripts_view.filter.trim();
    if !filter.is_empty() {
        out.retain(|e| fuzzy::score(filter, &format!("{} {}", e.name, owner(e))).is_some());
    }
    out
}

/// The selected entry (the selection is kept within the list).
pub fn selected(model: &Model) -> Option<&MenuEntry> {
    let all = entries(model);
    let i = model.scripts_view.selected.min(all.len().checked_sub(1)?);
    Some(all[i])
}

/// The view's lines for an inner width of `width`, and the line of the
/// selected row.
fn content(model: &Model, width: u16) -> (Vec<Line<'static>>, Option<usize>) {
    let a = model.ascii;
    let w = usize::from(width.max(10));
    let dim = Style::default().add_modifier(Modifier::DIM);
    let head = Style::default()
        .fg(accent(model))
        .add_modifier(Modifier::BOLD);
    let all = entries(model);
    let sel = model.scripts_view.selected.min(all.len().saturating_sub(1));
    let nw = all
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(4)
        .clamp(4, 24);
    let mut lines = Vec::new();
    let mut selected_line = None;
    let mut group: Option<&str> = None;
    for (i, e) in all.iter().enumerate() {
        if group != Some(owner(e)) {
            if group.is_some() {
                lines.push(Line::from(""));
            }
            group = Some(owner(e));
            let mut spans = vec![Span::styled(owner(e).to_string(), head)];
            match &e.stem {
                None => spans.push(Span::styled(" · global", dim)),
                Some(name) => {
                    if let Some(s) = model.stems.iter().find(|s| &s.name == name) {
                        spans.push(Span::styled(
                            format!(" · {} {}", s.glyph.symbol(!a), s.state),
                            Style::default().fg(glyph_color(s.glyph)),
                        ));
                    }
                }
            }
            lines.push(Line::from(spans));
        }
        let is_sel = i == sel;
        let marker = match (is_sel, a) {
            (false, _) => "  ",
            (true, false) => "› ",
            (true, true) => "> ",
        };
        let kind = match e.kind {
            ScriptKind::Custom => "custom",
            ScriptKind::Lifecycle => "lifecycle",
        };
        let act = model.script_activity.get(&(e.stem.clone(), e.name.clone()));
        let (act_text, act_style) = activity_text(act, a);
        let args = args_summary(e);
        let mut spans = vec![
            Span::raw(marker.to_string()),
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
        if is_sel {
            selected_line = Some(lines.len());
        }
        lines.push(fit(spans, w, is_sel, a));
    }
    (lines, selected_line)
}

/// Draw the Scripts view into `area`.
pub fn render(model: &Model, f: &mut Frame, area: Rect) {
    let st = &model.scripts_view;
    let a = model.ascii;
    let title = if st.editing || !st.filter.is_empty() {
        format!(
            " Scripts · /{}{} ",
            st.filter,
            if st.editing { "_" } else { "" }
        )
    } else {
        format!(" Scripts · {} ", entries(model).len())
    };
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    f.render_widget(block, area);
    let inner = area.inner(Margin::new(1, 1));
    let (lines, selected_line) = content(model, inner.width);
    if lines.is_empty() {
        let msg = if model.catalog.is_none() {
            if a { "loading..." } else { "loading…" }.to_string()
        } else if !st.filter.is_empty() {
            format!("no script matches /{}", st.filter)
        } else {
            "no scripts (declare `scripts:` in stems.yaml)".to_string()
        };
        f.render_widget(Paragraph::new(format!(" {msg}")), inner);
        return;
    }
    // Keep the selected row in view, with its group heading when it fits.
    let h = usize::from(inner.height);
    let max = lines.len().saturating_sub(h);
    let off = selected_line
        .map_or(0, |l| (l + 1).saturating_sub(h.saturating_sub(1).max(1)))
        .min(max);
    let scroll = (u16::try_from(off).unwrap_or(u16::MAX), 0);
    f.render_widget(Paragraph::new(lines).scroll(scroll), inner);
}
