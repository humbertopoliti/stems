//! The action bar: one line above the status bar in Table, Graph and
//! Detail (also under the split log pane), always visible, naming what the
//! keys do to the selected stem:
//!
//! ```text
//!  ■ x stop  ↻ r restart  : scripts (3)  v variant local ▸ docker  p watch on  o editor  ? more
//! ```
//!
//! `▶ s start` replaces stop/restart when the stem is stopped or failed;
//! `v variant …` shows only for a stem with `variants` (the active choice,
//! then the next one), `p watch …` only for one with `watch:` rules.
//! `: scripts (3+2)` counts the stem's custom scripts, then the workspace's
//! (left out when there are none). Segments that do nothing now are dimmed
//! (no scripts at all, no stem selected). On a narrow terminal the bar is cut from the right with `…`.
//! With `mouse = true` a click on a segment presses its key
//! ([`Model::bar_hits`]).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use stems_core::StemState;

use crate::model::Model;
use crate::view::{accent, paused_marker, str_width};

/// One segment of the bar: the key, then what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The key it stands for (a click presses it).
    pub key: char,
    /// An icon before the key (`▶`, `■`, `↻`), may be empty.
    pub icon: &'static str,
    /// The label after the key (`stop`, `scripts (3)`).
    pub label: String,
    /// Whether the key does something now.
    pub enabled: bool,
}

impl Segment {
    fn new(key: char, icon: &'static str, label: impl Into<String>, enabled: bool) -> Self {
        Self {
            key,
            icon,
            label: label.into(),
            enabled,
        }
    }

    /// The segment as text: `■ x stop`.
    pub fn text(&self) -> String {
        if self.icon.is_empty() {
            format!("{} {}", self.key, self.label)
        } else {
            format!("{} {} {}", self.icon, self.key, self.label)
        }
    }
}

/// The bar's segments for the selected stem, in order.
pub fn segments(model: &Model) -> Vec<Segment> {
    let a = model.ascii;
    let icon = |u: &'static str| if a { "" } else { u };
    let Some(s) = model.selected_stem() else {
        // Workspace scripts need no stem.
        let scripts = match model.workspace_scripts() {
            Some(w) if w > 0 => Segment::new(':', "", format!("scripts ({w})"), true),
            _ => Segment::new(':', "", "scripts", false),
        };
        return vec![
            Segment::new('s', icon("▶"), "start", false),
            scripts,
            Segment::new('o', "", "editor", false),
            Segment::new('?', "", "more", true),
        ];
    };
    let mut out = Vec::new();
    if matches!(s.state, StemState::Stopped | StemState::Failed) {
        out.push(Segment::new('s', icon("▶"), "start", true));
    } else {
        out.push(Segment::new('x', icon("■"), "stop", true));
        out.push(Segment::new('r', icon("↻"), "restart", true));
    }
    // The stem's custom scripts, `+` the workspace's (the menu lists both).
    match (model.custom_scripts(&s.name), model.workspace_scripts()) {
        (Some(n), Some(w)) if w > 0 => {
            out.push(Segment::new(':', "", format!("scripts ({n}+{w})"), true));
        }
        (Some(n), _) => out.push(Segment::new(':', "", format!("scripts ({n})"), n > 0)),
        (None, _) => out.push(Segment::new(':', "", "scripts", true)),
    }
    if let Some(active) = &s.variant {
        let next = model.variant_choices(&s.name).and_then(|v| {
            let i = v.iter().position(|c| &c.name == active)?;
            let n = v.get((i + 1) % v.len())?;
            (n.name != *active).then(|| n.name.clone())
        });
        let label = match next {
            Some(n) => format!("variant {active} {} {n}", if a { ">" } else { "▸" }),
            None => format!("variant {active}"),
        };
        out.push(Segment::new('v', "", label, true));
    }
    if let Some(w) = &s.watch {
        let label = if w.paused {
            format!("watch {} paused", paused_marker(a))
        } else {
            "watch on".to_string()
        };
        out.push(Segment::new('p', "", label, true));
    }
    out.push(Segment::new('o', "", "editor", true));
    out.push(Segment::new('?', "", "more", true));
    out
}

/// The bar as text for `width` columns, as [`render`] draws it (tests).
pub fn text(model: &Model, width: u16) -> String {
    layout(model, width)
        .0
        .iter()
        .map(|s| s.content.to_string())
        .collect()
}

const GAP: &str = "  ";

/// The spans of the bar and each placed segment's key and columns
/// (relative to the bar's left edge).
fn layout(model: &Model, width: u16) -> (Vec<Span<'static>>, Vec<(char, u16, u16)>) {
    let ell = if model.ascii { "..." } else { "…" };
    let key_style = Style::default()
        .fg(accent(model))
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let segs = segments(model);
    let mut spans = vec![Span::raw(" ")];
    let mut x: u16 = 1;
    let mut hits = Vec::new();
    for (i, s) in segs.iter().enumerate() {
        let gap = if i == 0 { 0 } else { GAP.len() as u16 };
        let w = str_width(&s.text());
        let last = i + 1 == segs.len();
        // Keep room for the `…` unless this is the last segment.
        let need = gap + w + if last { 0 } else { 1 + str_width(ell) };
        if x + need > width {
            if x + 1 + str_width(ell) <= width {
                spans.push(Span::styled(format!(" {ell}"), dim));
            }
            break;
        }
        if gap > 0 {
            spans.push(Span::raw(GAP));
        }
        x += gap;
        let start = x;
        if s.enabled {
            if !s.icon.is_empty() {
                spans.push(Span::raw(format!("{} ", s.icon)));
            }
            spans.push(Span::styled(s.key.to_string(), key_style));
            spans.push(Span::raw(format!(" {}", s.label)));
        } else {
            spans.push(Span::styled(s.text(), dim));
        }
        x += w;
        hits.push((s.key, start, x));
    }
    (spans, hits)
}

/// Draw the bar into `area` (one row) and record its hit ranges.
pub fn render(model: &Model, f: &mut Frame, area: Rect) {
    let (spans, hits) = layout(model, area.width);
    *model.bar_hits.borrow_mut() = hits
        .into_iter()
        .map(|(k, a, b)| (k, area.y, area.x + a, area.x + b))
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
