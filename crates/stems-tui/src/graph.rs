//! The graph view (deliverable 28, FR-UI-1, FR-GR-4, FR-GR-6).
//!
//! The dependency graph is laid out by the same engine as `stems graph`
//! ([`stems_core::layout`], [`stems_core::render::render_grid`]) and drawn
//! cell by cell by [`GraphWidget`], so boxes can be styled: the selected box
//! in reverse video, glyphs and reason lines in their colour, soft edges
//! dimmed.
//!
//! * The layout is built from the daemon's `stem_config` of every stem
//!   ([`crate::Cmd::LoadGraph`]) and cached on the set of stem names: it is
//!   rebuilt only when that set changes ([`GraphState`]). Glyphs and reasons
//!   come from the live `status` at every frame.
//! * Keys: `h`/`l` (or Left/Right) move across the columns, `j`/`k` (or
//!   Down/Up) within one ([`step`]); `Enter` opens the detail view; `f`
//!   toggles focus mode (the selected stem and its neighbours); `e` toggles
//!   edge labels (`protocol`/`via`); `-` zooms out to compact one-row boxes,
//!   `+` back to full boxes; by default (auto) boxes are compact only when
//!   the full drawing does not fit. When the drawing is larger than the view
//!   it scrolls to keep the selected box visible ([`scroll`]).
//! * The last line is the glyph legend, with the active modes on the right.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use serde_json::Value;
use stems_config::{Condition, Dependency, Protocol, StemType};
use stems_core::Glyph;
use stems_core::layout::{Layout, NodeId, from_parts};
use stems_core::render::{Grid, RenderOptions, StatusFn, graph_reason, render_grid};

use crate::model::Model;

/// A stem as the graph needs it: from the daemon's `stem_config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphStem {
    /// Stem name.
    pub name: String,
    /// Stem type.
    pub kind: StemType,
    /// Its `depends_on` edges.
    pub depends_on: Vec<Dependency>,
}

impl GraphStem {
    /// Read `type` and `depends_on` from a `stem_config` result (lenient:
    /// a bare name is a hard `healthy` edge; unknown values are skipped).
    pub fn from_config(name: &str, v: &Value) -> Self {
        let kind = v
            .get("type")
            .and_then(|t| serde_json::from_value(t.clone()).ok())
            .unwrap_or(StemType::Process);
        let depends_on = v
            .get("depends_on")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(dependency).collect())
            .unwrap_or_default();
        Self {
            name: name.to_string(),
            kind,
            depends_on,
        }
    }
}

fn dependency(v: &Value) -> Option<Dependency> {
    if let Some(s) = v.as_str() {
        return Some(Dependency {
            stem: s.to_string(),
            condition: Condition::Healthy,
            soft: false,
            protocol: None,
            via: None,
        });
    }
    let parse = |k: &str| v.get(k).cloned().filter(|x| !x.is_null());
    Some(Dependency {
        stem: v.get("stem")?.as_str()?.to_string(),
        condition: parse("condition")
            .and_then(|c| serde_json::from_value(c).ok())
            .unwrap_or(Condition::Healthy),
        soft: v.get("soft").and_then(Value::as_bool).unwrap_or(false),
        protocol: parse("protocol").and_then(|p| serde_json::from_value::<Protocol>(p).ok()),
        via: v.get("via").and_then(Value::as_str).map(str::to_string),
    })
}

/// The graph view's state in the [`Model`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphState {
    /// Sorted stem names the cached layout was built (or requested) for.
    pub names: Vec<String>,
    /// The laid-out graph (`None` until the first load).
    pub layout: Option<Layout>,
    /// A [`crate::Cmd::LoadGraph`] is in flight.
    pub pending: bool,
    /// Why the last load failed.
    pub error: Option<String>,
    /// Focus mode: the centre stem and its neighbourhood's layout.
    pub focus: Option<(String, Layout)>,
    /// Show `protocol`/`via` edge labels (`e`).
    pub edge_labels: bool,
    /// Zoom: `None` = auto, `Some(true)` = compact (`-`), `Some(false)` =
    /// full boxes (`+`).
    pub compact: Option<bool>,
    /// Hard `depends_on` edges `(dependant, dependency)` of the last load
    /// (soft edges left out): the `r` confirmation's dependants.
    pub hard_edges: Vec<(String, String)>,
}

impl GraphState {
    /// The layout on screen: the focus sub-layout in focus mode.
    pub fn shown(&self) -> Option<&Layout> {
        match &self.focus {
            Some((_, l)) => Some(l),
            None => self.layout.as_ref(),
        }
    }

    /// Install a freshly loaded graph (keeps focus mode when its centre
    /// still exists).
    pub fn set(&mut self, names: Vec<String>, stems: &[GraphStem]) {
        let deps: Vec<(String, Dependency)> = stems
            .iter()
            .flat_map(|s| s.depends_on.iter().map(|d| (s.name.clone(), d.clone())))
            .collect();
        let l = from_parts(stems.iter().map(|s| (s.name.clone(), s.kind)), &deps);
        self.hard_edges = deps
            .iter()
            .filter(|(_, d)| !d.soft)
            .map(|(n, d)| (n.clone(), d.stem.clone()))
            .collect();
        self.focus = self
            .focus
            .take()
            .filter(|(c, _)| l.node(c).is_some())
            .map(|(c, _)| {
                let f = l.focus(&c);
                (c, f)
            });
        self.names = names;
        self.layout = Some(l);
        self.pending = false;
        self.error = None;
    }

    /// Every stem that depends on `stem` through hard edges, transitively,
    /// nearest first (each once).
    pub fn dependants(&self, stem: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        let mut frontier = vec![stem.to_string()];
        while i < frontier.len() {
            let cur = frontier[i].clone();
            i += 1;
            for (dependant, dep) in &self.hard_edges {
                if *dep == cur && dependant != stem && !out.contains(dependant) {
                    out.push(dependant.clone());
                    frontier.push(dependant.clone());
                }
            }
        }
        out
    }

    /// Toggle focus mode around `stem`.
    pub fn toggle_focus(&mut self, stem: Option<&str>) {
        if self.focus.take().is_some() {
            return;
        }
        if let (Some(l), Some(s)) = (&self.layout, stem)
            && l.node(s).is_some()
        {
            self.focus = Some((s.to_string(), l.focus(s)));
        }
    }
}

/// A selection move in the graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    /// One column left (towards dependants).
    Left,
    /// One column right (towards dependencies).
    Right,
    /// Up within the column.
    Up,
    /// Down within the column.
    Down,
}

/// The first box in display order (top of the leftmost column).
pub fn first(layout: &Layout) -> Option<NodeId> {
    layout.columns().flatten().next().copied()
}

/// Where `mv` takes the selection from `from` (`None` or a stem not in the
/// layout: the first box). Across columns it prefers the first connected
/// stem (a dependency to the right, a dependant to the left) in column
/// order, else the stem at the same position (clamped); at the edge of the
/// graph it stays.
pub fn step(layout: &Layout, from: Option<NodeId>, mv: Move) -> Option<NodeId> {
    let cols: Vec<&Vec<NodeId>> = layout.columns().collect();
    let pos = from.and_then(|n| {
        cols.iter()
            .enumerate()
            .find_map(|(c, col)| col.iter().position(|&x| x == n).map(|i| (c, i)))
    });
    let Some((c, i)) = pos else {
        return first(layout);
    };
    let here = cols[c][i];
    let target = match mv {
        Move::Up => return Some(cols[c][i.saturating_sub(1)]),
        Move::Down => return Some(cols[c][(i + 1).min(cols[c].len() - 1)]),
        Move::Right if c + 1 < cols.len() => c + 1,
        Move::Left if c > 0 => c - 1,
        _ => return Some(here),
    };
    let linked = |n: NodeId| {
        layout.edges.iter().any(|e| match mv {
            Move::Right => e.from == here && e.to == n,
            _ => e.to == here && e.from == n,
        })
    };
    let col = cols[target];
    Some(
        col.iter()
            .copied()
            .find(|&n| linked(n))
            .unwrap_or(col[i.min(col.len() - 1)]),
    )
}

/// Scroll offset along one axis: `0` when `content` fits `view`, else the
/// offset centring `[at, at + len)` as far as the content allows.
pub fn scroll(content: usize, view: usize, at: usize, len: usize) -> usize {
    if content <= view {
        return 0;
    }
    (at + len / 2).saturating_sub(view / 2).min(content - view)
}

/// Colour of a glyph in the dashboard.
pub fn glyph_color(g: Glyph) -> Color {
    match g {
        Glyph::Healthy => Color::Green,
        Glyph::Failed => Color::Red,
        Glyph::Degraded => Color::Yellow,
        Glyph::Stopped => Color::DarkGray,
        Glyph::Unknown => Color::Magenta,
        Glyph::Transitioning => Color::Cyan,
    }
}

/// Draws a [`Layout`] into a ratatui buffer (reusable, e.g. as a mini-map).
pub struct GraphWidget<'a> {
    layout: &'a Layout,
    status: Option<StatusFn<'a>>,
    selected: Option<NodeId>,
    unicode: bool,
    edge_labels: bool,
    compact: Option<bool>,
    paused: Vec<NodeId>,
}

impl<'a> GraphWidget<'a> {
    /// A config-only widget (every stem `·`), full boxes when they fit.
    pub fn new(layout: &'a Layout) -> Self {
        Self {
            layout,
            status: None,
            selected: None,
            unicode: true,
            edge_labels: false,
            compact: None,
            paused: Vec::new(),
        }
    }

    /// Stems whose watchdog is paused: `⏸` (`||`) on their box's top
    /// border (30).
    #[must_use]
    pub fn paused(mut self, nodes: Vec<NodeId>) -> Self {
        self.paused = nodes;
        self
    }

    /// Live glyphs and reasons.
    #[must_use]
    pub fn status(mut self, status: StatusFn<'a>) -> Self {
        self.status = Some(status);
        self
    }

    /// Highlight this stem (and keep it in view).
    #[must_use]
    pub fn selected(mut self, node: Option<NodeId>) -> Self {
        self.selected = node;
        self
    }

    /// Unicode box drawing and glyphs (`false`: ASCII).
    #[must_use]
    pub fn unicode(mut self, on: bool) -> Self {
        self.unicode = on;
        self
    }

    /// Show edge labels.
    #[must_use]
    pub fn edge_labels(mut self, on: bool) -> Self {
        self.edge_labels = on;
        self
    }

    /// Zoom: `None` auto (compact only when the full drawing does not fit
    /// `area`), `Some(true)` compact, `Some(false)` full.
    #[must_use]
    pub fn compact(mut self, zoom: Option<bool>) -> Self {
        self.compact = zoom;
        self
    }

    fn opts(&self, compact: bool) -> RenderOptions<'a> {
        RenderOptions {
            unicode: self.unicode,
            color: false,
            edge_labels: self.edge_labels,
            width: 0,
            status: self.status,
            compact,
        }
    }

    /// The grid drawn in `area` and whether it is compact.
    pub fn grid(&self, area: Rect) -> (Grid, bool) {
        let full = |c| render_grid(self.layout, &self.opts(c));
        match self.compact {
            Some(c) => (full(c), c),
            None => {
                let g = full(false);
                if g.width > usize::from(area.width) || g.height > usize::from(area.height) {
                    (full(true), true)
                } else {
                    (g, false)
                }
            }
        }
    }
}

impl Widget for GraphWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let (grid, _) = self.grid(area);
        let (vw, vh) = (usize::from(area.width), usize::from(area.height));
        let sel = self.selected.and_then(|n| grid.box_of(n));
        let ox = sel.map_or(0, |b| scroll(grid.width, vw, b.x, b.width));
        let oy = sel.map_or(0, |b| scroll(grid.height, vh, b.y, b.height));
        for (y, row) in grid.rows.iter().skip(oy).take(vh).enumerate() {
            for (x, c) in row.iter().skip(ox).take(vw).enumerate() {
                let mut style = Style::default();
                if let Some(g) = c.glyph {
                    style = style.fg(glyph_color(g));
                }
                if c.soft {
                    style = style.fg(Color::DarkGray);
                }
                if c.node.is_some() && c.node == self.selected {
                    style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
                }
                let cell = &mut buf[(area.x + x as u16, area.y + y as u16)];
                cell.set_char(c.ch);
                cell.set_style(style);
            }
        }
        // The paused marker on the top border, right-aligned.
        let mark = crate::view::paused_marker(!self.unicode);
        let mw = mark.chars().count();
        for b in self.paused.iter().filter_map(|n| grid.box_of(*n)) {
            if b.width < mw + 3 {
                continue;
            }
            let x0 = b.x + b.width - 1 - mw;
            for (i, ch) in mark.chars().enumerate() {
                let (gx, gy) = (x0 + i, b.y);
                if gx < ox || gy < oy || gx - ox >= vw || gy - oy >= vh {
                    continue;
                }
                let cell = &mut buf[(area.x + (gx - ox) as u16, area.y + (gy - oy) as u16)];
                cell.set_char(ch);
                cell.set_style(Style::default().fg(Color::Yellow));
            }
        }
    }
}

/// Glyph and graph reason of `stem` from the model's last `status`
/// (`·` for a stem the daemon did not report).
pub fn live(model: &Model, stem: &str) -> (Glyph, Option<String>) {
    match model.stems.iter().find(|s| s.name == stem) {
        Some(s) => (s.glyph, graph_reason(s.glyph, s.reason.as_deref())),
        None => (Glyph::Stopped, None),
    }
}

/// The legend line: glyphs on the left, active modes on the right.
pub fn legend(model: &Model, compact: bool, width: usize) -> String {
    let unicode = !model.ascii;
    let left = Glyph::legend(unicode);
    let g = &model.graph;
    let mut modes = Vec::new();
    if let Some((c, _)) = &g.focus {
        modes.push(format!("focus {c}"));
    }
    if g.edge_labels {
        modes.push("labels".to_string());
    }
    modes.push(
        match (g.compact, compact) {
            (None, false) => "zoom auto",
            (None, true) => "zoom auto (compact)",
            (Some(true), _) => "zoom compact",
            (Some(false), _) => "zoom full",
        }
        .to_string(),
    );
    let right = modes.join(" · ");
    let lw = left.chars().count() + 1;
    let rw = right.chars().count();
    if lw + 2 + rw <= width {
        format!(" {left}{}{right}", " ".repeat(width - lw - rw))
    } else {
        format!(" {left}")
    }
}

/// Draw the graph view into `area` (the body between title and status bar).
pub fn render(model: &Model, f: &mut ratatui::Frame, area: Rect) {
    use ratatui::widgets::Paragraph;
    if area.height < 2 {
        return;
    }
    let body = Rect::new(area.x, area.y, area.width, area.height - 1);
    let legend_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    let g = &model.graph;
    let note = |f: &mut ratatui::Frame, text: String| {
        let r = Rect::new(
            body.x + 2,
            body.y + 1.min(body.height - 1),
            body.width.saturating_sub(2),
            1,
        );
        f.render_widget(Paragraph::new(text), r);
    };
    let Some(layout) = g.shown() else {
        let text = match &g.error {
            Some(e) => format!("graph unavailable: {e}"),
            None => "loading…".to_string(),
        };
        note(f, text);
        return;
    };
    if layout.nodes.is_empty() {
        note(f, "no stems".to_string());
        return;
    }
    let status = |name: &str| live(model, name);
    let selected = model.selected.as_deref().and_then(|s| layout.node(s));
    let paused: Vec<NodeId> = model
        .stems
        .iter()
        .filter(|s| s.watch.as_ref().is_some_and(|w| w.paused))
        .filter_map(|s| layout.node(&s.name))
        .collect();
    let widget = GraphWidget::new(layout)
        .paused(paused)
        .status(&status)
        .selected(selected)
        .unicode(!model.ascii)
        .edge_labels(g.edge_labels)
        .compact(g.compact);
    let (_, compact) = widget.grid(body);
    f.render_widget(widget, body);
    f.render_widget(
        Paragraph::new(legend(model, compact, usize::from(area.width)))
            .style(Style::default().fg(Color::DarkGray)),
        legend_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(stem: &str) -> Dependency {
        Dependency {
            stem: stem.into(),
            condition: Condition::Healthy,
            soft: false,
            protocol: None,
            via: None,
        }
    }

    /// `name: dep, dep` specs to [`GraphStem`]s.
    pub(crate) fn stems(spec: &[&str]) -> Vec<GraphStem> {
        spec.iter()
            .map(|l| {
                let (name, deps) = l.split_once(':').unwrap();
                GraphStem {
                    name: name.trim().into(),
                    kind: StemType::Process,
                    depends_on: deps
                        .split(',')
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .map(dep)
                        .collect(),
                }
            })
            .collect()
    }

    fn lay(spec: &[&str]) -> Layout {
        let s = stems(spec);
        let mut st = GraphState::default();
        st.set(Vec::new(), &s);
        st.layout.unwrap()
    }

    fn name(l: &Layout, n: Option<NodeId>) -> &str {
        &l.nodes[n.unwrap()].name
    }

    #[test]
    fn config_parsing_is_lenient() {
        let v = serde_json::json!({"type": "docker", "depends_on": [
            "a",
            {"stem": "b", "condition": "seeded", "soft": true, "protocol": "http", "via": ":80"},
            {"stem": "c"},
            {"nope": 1}
        ]});
        let g = GraphStem::from_config("x", &v);
        assert_eq!(g.kind, StemType::Docker);
        assert_eq!(g.depends_on.len(), 3);
        assert_eq!(g.depends_on[1].condition, Condition::Seeded);
        assert!(g.depends_on[1].soft);
        assert_eq!(g.depends_on[1].protocol, Some(Protocol::Http));
        assert_eq!(g.depends_on[1].via.as_deref(), Some(":80"));
        assert_eq!(g.depends_on[2].condition, Condition::Healthy);
        let bare = GraphStem::from_config("y", &serde_json::json!({}));
        assert_eq!(bare.kind, StemType::Process);
        assert!(bare.depends_on.is_empty());
    }

    #[test]
    fn selection_moves_across_and_within_columns() {
        // Diamond: d -> b, c -> a (columns: [d], [b, c], [a]).
        let l = lay(&["a:", "b: a", "c: a", "d: b, c"]);
        let d = first(&l);
        assert_eq!(name(&l, d), "d");
        let b = step(&l, d, Move::Right);
        assert_eq!(name(&l, b), "b");
        let c = step(&l, b, Move::Down);
        assert_eq!(name(&l, c), "c");
        assert_eq!(name(&l, step(&l, c, Move::Down)), "c", "clamped");
        assert_eq!(name(&l, step(&l, c, Move::Up)), "b");
        assert_eq!(name(&l, step(&l, b, Move::Up)), "b", "clamped");
        let a = step(&l, c, Move::Right);
        assert_eq!(name(&l, a), "a");
        assert_eq!(name(&l, step(&l, a, Move::Right)), "a", "right edge");
        assert_eq!(name(&l, step(&l, a, Move::Left)), "b", "first dependant");
        assert_eq!(name(&l, step(&l, d, Move::Left)), "d", "left edge");
        assert_eq!(name(&l, step(&l, None, Move::Down)), "d", "no selection");
    }

    #[test]
    fn crossing_moves_prefer_linked_stems() {
        // web -> api -> db; worker -> db (columns: [web], [api, worker], [db]).
        let l = lay(&["web: api", "api: db", "worker: queue", "db:", "queue:"]);
        let worker = l.node("worker");
        assert_eq!(name(&l, step(&l, worker, Move::Right)), "queue");
        let queue = l.node("queue");
        assert_eq!(name(&l, step(&l, queue, Move::Left)), "worker");
        // Unlinked: same position, clamped.
        let lone = lay(&["a: b", "b:", "c:"]);
        let c = lone.node("c");
        assert!(step(&lone, c, Move::Left).is_some());
    }

    #[test]
    fn scroll_keeps_the_selection_in_view() {
        assert_eq!(scroll(50, 80, 40, 10), 0, "fits");
        assert_eq!(scroll(200, 80, 0, 10), 0);
        assert_eq!(scroll(200, 80, 100, 10), 65);
        assert_eq!(scroll(200, 80, 190, 10), 120, "clamped at the end");
    }

    #[test]
    fn focus_follows_reloads() {
        let s = stems(&["a:", "b: a", "c: b"]);
        let mut st = GraphState::default();
        st.set(vec!["a".into()], &s);
        st.toggle_focus(Some("c"));
        assert_eq!(st.shown().unwrap().nodes.len(), 2);
        st.set(vec!["a".into()], &s);
        assert_eq!(st.focus.as_ref().unwrap().0, "c");
        st.set(vec![], &stems(&["a:", "b: a"]));
        assert!(st.focus.is_none(), "centre gone");
        st.toggle_focus(Some("ghost"));
        assert!(st.focus.is_none());
        st.toggle_focus(Some("a"));
        st.toggle_focus(Some("a"));
        assert!(st.focus.is_none(), "toggles off");
    }

    fn widget_text(l: &Layout, w: u16, h: u16, zoom: Option<bool>, sel: Option<&str>) -> String {
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        GraphWidget::new(l)
            .selected(sel.and_then(|s| l.node(s)))
            .compact(zoom)
            .render(buf.area, &mut buf);
        crate::view::buffer_text(&buf)
    }

    fn twelve() -> Layout {
        lay(&[
            "gateway: auth, catalog, cart",
            "admin: auth, catalog",
            "auth: users",
            "catalog: search, db",
            "cart: db, cache",
            "search: index",
            "users: db",
            "index:",
            "db:",
            "cache:",
            "mailer: queue",
            "queue:",
        ])
    }

    #[test]
    fn widget_goldens() {
        let chain = lay(&["web: api", "api: db", "db:"]);
        let diamond = lay(&["top: left, right", "left: base", "right: base", "base:"]);
        let big = twelve();
        for (name, l) in [("chain", &chain), ("diamond", &diamond), ("twelve", &big)] {
            for (w, h) in [(40u16, 12u16), (80, 24), (120, 40)] {
                insta::assert_snapshot!(
                    format!("graph-widget-{name}-{w}x{h}"),
                    widget_text(l, w, h, None, None)
                );
            }
        }
    }

    #[test]
    fn zoom_compacts_only_when_needed() {
        let l = twelve();
        let area = Rect::new(0, 0, 200, 60);
        let (_, compact) = GraphWidget::new(&l).grid(area);
        assert!(!compact, "fits at 200x60");
        let (_, compact) = GraphWidget::new(&l).grid(Rect::new(0, 0, 40, 12));
        assert!(compact, "too big for 40x12");
        let (g, compact) = GraphWidget::new(&l).compact(Some(true)).grid(area);
        assert!(compact && g.boxes.iter().all(|b| b.height == 1));
        let (g, compact) = GraphWidget::new(&l)
            .compact(Some(false))
            .grid(Rect::new(0, 0, 40, 12));
        assert!(!compact && g.boxes.iter().all(|b| b.height == 3));
        // Forced full boxes in a small view scroll to the selection.
        let text = widget_text(&l, 40, 12, Some(false), Some("queue"));
        assert!(text.contains("queue"), "{text}");
        assert!(!text.contains("gateway"), "{text}");
    }

    #[test]
    fn selected_box_is_reversed() {
        let l = lay(&["web: api", "api:"]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 6));
        GraphWidget::new(&l)
            .selected(l.node("api"))
            .render(buf.area, &mut buf);
        let reversed = |x: u16, y: u16| buf[(x, y)].modifier.contains(Modifier::REVERSED);
        // web's box starts at column 0, api's after the channel.
        assert!(!reversed(0, 0));
        let api_x = (0..40).find(|&x| buf[(x, 1)].symbol() == "a").unwrap();
        assert!(reversed(api_x, 1));
    }
}
