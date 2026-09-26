//! Renderers for a [`Layout`]: terminal text (box drawing), Mermaid, DOT and
//! the JSON graph shape consumed by the TUI and the MCP server.
//!
//! Text layout: columns of boxes, dependants on the left, dependencies on
//! the right (see [`crate::layout`]). Between two columns is a routing
//! channel: each source box leaves from the middle of its right border
//! (`├`), turns onto its own vertical track, and enters each target from the
//! left with an arrowhead. Edges into the same target merge just before it.
//! Soft edges are dashed. When the drawing is wider than
//! [`RenderOptions::width`], columns wrap into bands stacked vertically;
//! edges leaving a band end in the name of their target.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};
use stems_config::{Condition, Protocol};

use crate::layout::{Edge, Layout, NodeId, Slot};
use crate::status::Glyph;

/// Live status lookup: stem name to glyph and optional reason (e.g.
/// `dep shop-api` for a degraded stem).
pub type StatusFn<'a> = &'a dyn Fn(&str) -> (Glyph, Option<String>);

/// Options shared by every renderer.
#[derive(Clone, Copy)]
pub struct RenderOptions<'a> {
    /// Box drawing and Unicode glyphs; `false` = ASCII (`+--+`, `OK`...).
    pub unicode: bool,
    /// ANSI colours on glyphs and reasons (text renderer only).
    pub color: bool,
    /// Show `protocol`/`via` edge labels (FR-GR-5).
    pub edge_labels: bool,
    /// Terminal width for the text renderer; `0` = unlimited.
    pub width: usize,
    /// Live status; `None` = config only, every stem `·` (stopped).
    pub status: Option<StatusFn<'a>>,
}

impl Default for RenderOptions<'_> {
    fn default() -> Self {
        Self {
            unicode: true,
            color: false,
            edge_labels: false,
            width: 0,
            status: None,
        }
    }
}

impl std::fmt::Debug for RenderOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderOptions")
            .field("unicode", &self.unicode)
            .field("color", &self.color)
            .field("edge_labels", &self.edge_labels)
            .field("width", &self.width)
            .field("status", &self.status.is_some())
            .finish()
    }
}

fn status_of(status: Option<StatusFn<'_>>, name: &str) -> (Glyph, Option<String>) {
    status.map_or((Glyph::Stopped, None), |f| f(name))
}

/// `condition` as written in config.
pub fn condition_name(c: Condition) -> &'static str {
    match c {
        Condition::Started => "started",
        Condition::Healthy => "healthy",
        Condition::Seeded => "seeded",
    }
}

/// `protocol` as written in config.
pub fn protocol_name(p: Protocol) -> &'static str {
    match p {
        Protocol::Http => "http",
        Protocol::Grpc => "grpc",
        Protocol::Amqp => "amqp",
        Protocol::Tcp => "tcp",
    }
}

/// The FR-GR-5 label of an edge: `protocol` and `via`, space separated.
pub fn edge_label(e: &Edge) -> Option<String> {
    let parts: Vec<&str> = e
        .protocol
        .map(protocol_name)
        .into_iter()
        .chain(e.via.as_deref())
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

// ---------------------------------------------------------------------------
// JSON

/// The JSON graph shape:
/// `{ nodes: [{name, type, status, glyph, reason}], edges: [{from, to, condition, soft, protocol, via}] }`.
/// Nodes in display order (column by column, top to bottom), edges in
/// declaration order; `status` is the glyph name (`healthy`, `degraded`,
/// ...), `stopped` without live status.
pub fn to_json(layout: &Layout, status: Option<StatusFn<'_>>) -> Value {
    let nodes: Vec<Value> = display_nodes(layout)
        .map(|n| {
            let node = &layout.nodes[n];
            let (glyph, reason) = status_of(status, &node.name);
            json!({
                "name": node.name,
                "type": node.kind.to_string(),
                "status": glyph.name(),
                "glyph": glyph.unicode(),
                "reason": reason,
            })
        })
        .collect();
    let edges: Vec<Value> = layout
        .edges
        .iter()
        .map(|e| {
            json!({
                "from": layout.nodes[e.from].name,
                "to": layout.nodes[e.to].name,
                "condition": condition_name(e.condition),
                "soft": e.soft,
                "protocol": e.protocol.map(protocol_name),
                "via": e.via,
            })
        })
        .collect();
    json!({ "nodes": nodes, "edges": edges })
}

fn display_nodes(layout: &Layout) -> impl Iterator<Item = NodeId> + '_ {
    layout.columns().flatten().copied()
}

// ---------------------------------------------------------------------------
// Mermaid

/// Mermaid flowchart: `graph LR`, one node line per stem, one line per edge
/// (`a -->|healthy| b`, soft `a -.->|started| b`).
pub fn render_mermaid(layout: &Layout, opts: &RenderOptions<'_>) -> String {
    let mut ids: BTreeMap<NodeId, String> = BTreeMap::new();
    let mut used = BTreeSet::new();
    for (i, n) in layout.nodes.iter().enumerate() {
        let mut id: String = n
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        if id.is_empty() || id.starts_with(|c: char| c.is_ascii_digit()) {
            id.insert_str(0, "n_");
        }
        if [
            "end",
            "graph",
            "subgraph",
            "style",
            "class",
            "classdef",
            "click",
            "linkstyle",
        ]
        .contains(&id.to_ascii_lowercase().as_str())
        {
            id.push('_');
        }
        let base = id.clone();
        let mut k = 2;
        while !used.insert(id.clone()) {
            id = format!("{base}_{k}");
            k += 1;
        }
        ids.insert(i, id);
    }
    let quote = |s: &str| s.replace('"', "#quot;");
    let mut out = String::from("graph LR\n");
    let mut classes: BTreeMap<Glyph, Vec<&str>> = BTreeMap::new();
    for n in display_nodes(layout) {
        let node = &layout.nodes[n];
        let label = match opts.status {
            Some(_) => {
                let (g, reason) = status_of(opts.status, &node.name);
                classes.entry(g).or_default().push(&ids[&n]);
                match reason {
                    Some(r) => format!("{} {}<br/>{}", node.name, g.unicode(), r),
                    None => format!("{} {}", node.name, g.unicode()),
                }
            }
            None => node.name.clone(),
        };
        let _ = writeln!(out, "  {}[\"{}\"]", ids[&n], quote(&label));
    }
    for e in &layout.edges {
        let mut label = condition_name(e.condition).to_string();
        if opts.edge_labels
            && let Some(l) = edge_label(e)
        {
            label = format!("\"{}\"", quote(&format!("{label} {l}")));
        }
        let arrow = if e.soft { "-.->" } else { "-->" };
        let _ = writeln!(out, "  {} {arrow}|{label}| {}", ids[&e.from], ids[&e.to]);
    }
    for (g, members) in &classes {
        let stroke = match g {
            Glyph::Healthy => "#2e7d32",
            Glyph::Failed => "#c62828",
            Glyph::Degraded => "#f9a825",
            Glyph::Stopped => "#9e9e9e",
            Glyph::Unknown => "#8e24aa",
            Glyph::Transitioning => "#00838f",
        };
        let _ = writeln!(
            out,
            "  classDef {} stroke:{stroke},stroke-width:2px",
            g.name()
        );
        let _ = writeln!(out, "  class {} {}", members.join(","), g.name());
    }
    out
}

// ---------------------------------------------------------------------------
// DOT

/// Graphviz: `digraph stems { rankdir=LR; ... }`, soft edges dashed.
pub fn render_dot(layout: &Layout, opts: &RenderOptions<'_>) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from("digraph stems {\n  rankdir=LR;\n");
    out.push_str("  node [shape=box, style=rounded, fontname=\"Helvetica\"];\n");
    out.push_str("  edge [fontname=\"Helvetica\", fontsize=10];\n");
    for n in display_nodes(layout) {
        let node = &layout.nodes[n];
        match opts.status {
            Some(_) => {
                let (g, reason) = status_of(opts.status, &node.name);
                let label = match reason {
                    Some(r) => format!("{} {}\n{}", node.name, g.unicode(), r),
                    None => format!("{} {}", node.name, g.unicode()),
                };
                let _ = writeln!(
                    out,
                    "  {} [label={}, color={}];",
                    q(&node.name),
                    q(&label).replace('\n', "\\n"),
                    g.color()
                );
            }
            None => {
                let _ = writeln!(out, "  {};", q(&node.name));
            }
        }
    }
    for e in &layout.edges {
        let mut label = condition_name(e.condition).to_string();
        if opts.edge_labels
            && let Some(l) = edge_label(e)
        {
            label = format!("{label} {l}");
        }
        let style = if e.soft { ", style=dashed" } else { "" };
        let _ = writeln!(
            out,
            "  {} -> {} [label={}{style}];",
            q(&layout.nodes[e.from].name),
            q(&layout.nodes[e.to].name),
            q(&label)
        );
    }
    out.push_str("}\n");
    out
}

// ---------------------------------------------------------------------------
// Text

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;
/// Gap (rows) between stacked slots.
const GAP: i64 = 1;
/// Longest reason line shown inside a box.
const REASON_MAX: usize = 28;

#[derive(Clone, Copy, Default)]
struct Cell {
    ch: Option<char>,
    mask: u8,
    solid: bool,
    color: Option<&'static str>,
}

struct Canvas {
    rows: Vec<Vec<Cell>>,
    unicode: bool,
}

impl Canvas {
    fn new(height: usize, width: usize, unicode: bool) -> Self {
        Self {
            rows: vec![vec![Cell::default(); width]; height],
            unicode,
        }
    }

    fn cell(&mut self, y: i64, x: usize) -> Option<&mut Cell> {
        let y = usize::try_from(y).ok()?;
        self.rows.get_mut(y)?.get_mut(x)
    }

    fn put(&mut self, y: i64, x: usize, ch: char, color: Option<&'static str>) {
        if let Some(c) = self.cell(y, x) {
            c.ch = Some(ch);
            c.color = color;
        }
    }

    fn text(&mut self, y: i64, x: usize, s: &str, color: Option<&'static str>) {
        for (i, ch) in s.chars().enumerate() {
            self.put(y, x + i, ch, color);
        }
    }

    fn line(&mut self, y: i64, x: usize, mask: u8, soft: bool) {
        if let Some(c) = self.cell(y, x) {
            c.mask |= mask;
            c.solid |= !soft;
        }
    }

    fn hline(&mut self, y: i64, x1: usize, x2: usize, soft: bool) {
        let (a, b) = (x1.min(x2), x1.max(x2));
        for x in a..=b {
            let mut m = 0;
            if x > a {
                m |= LEFT;
            }
            if x < b {
                m |= RIGHT;
            }
            self.line(y, x, m, soft);
        }
    }

    fn vline(&mut self, x: usize, y1: i64, y2: i64, soft: bool) {
        let (a, b) = (y1.min(y2), y1.max(y2));
        for y in a..=b {
            let mut m = 0;
            if y > a {
                m |= UP;
            }
            if y < b {
                m |= DOWN;
            }
            self.line(y, x, m, soft);
        }
    }

    fn line_char(&self, mask: u8, solid: bool) -> char {
        let h = mask & !(LEFT | RIGHT) == 0;
        let v = mask & !(UP | DOWN) == 0;
        if self.unicode {
            match mask {
                0 => ' ',
                _ if h => {
                    if solid {
                        '─'
                    } else {
                        '╌'
                    }
                }
                _ if v => {
                    if solid {
                        '│'
                    } else {
                        '╎'
                    }
                }
                m if m == DOWN | RIGHT => '╭',
                m if m == DOWN | LEFT => '╮',
                m if m == UP | RIGHT => '╰',
                m if m == UP | LEFT => '╯',
                m if m == LEFT | RIGHT | DOWN => '┬',
                m if m == LEFT | RIGHT | UP => '┴',
                m if m == UP | DOWN | RIGHT => '├',
                m if m == UP | DOWN | LEFT => '┤',
                _ => '┼',
            }
        } else {
            match mask {
                0 => ' ',
                _ if h => {
                    if solid {
                        '-'
                    } else {
                        '.'
                    }
                }
                _ if v => {
                    if solid {
                        '|'
                    } else {
                        ':'
                    }
                }
                _ => '+',
            }
        }
    }

    fn finish(&self, color: bool, out: &mut Vec<String>) {
        let lines: Vec<String> = self
            .rows
            .iter()
            .map(|row| {
                let mut s = String::new();
                let mut open: Option<&str> = None;
                for c in row {
                    let ch = c.ch.unwrap_or_else(|| self.line_char(c.mask, c.solid));
                    let want = if color { c.color } else { None };
                    if want != open {
                        if open.is_some() {
                            s.push_str("\x1b[0m");
                        }
                        if let Some(code) = want {
                            let _ = write!(s, "\x1b[{code}m");
                        }
                        open = want;
                    }
                    s.push(ch);
                }
                if open.is_some() {
                    s.push_str("\x1b[0m");
                }
                s.trim_end().to_string()
            })
            .collect();
        let first = lines.iter().position(|l| !l.is_empty());
        let last = lines.iter().rposition(|l| !l.is_empty());
        if let (Some(a), Some(b)) = (first, last) {
            out.extend(lines[a..=b].iter().cloned());
        }
    }
}

fn len(s: &str) -> usize {
    s.chars().count()
}

fn truncate(s: &str, max: usize) -> String {
    if len(s) <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Visual content of a stem box.
struct NodeView {
    glyph: Glyph,
    reason: Option<String>,
}

/// One edge piece in a routing channel.
#[derive(Clone)]
struct ChanSeg {
    edge: usize,
    target: Slot,
    yd: i64,
    soft: bool,
}

/// All segments leaving one slot into a channel; drawn via one track.
struct Group {
    src: Slot,
    ys: i64,
    segs: Vec<ChanSeg>,
    /// Track index, or `None` for a single straight line.
    track: Option<usize>,
}

impl Group {
    fn rows(&self) -> BTreeSet<i64> {
        self.segs.iter().map(|s| s.yd).collect()
    }
    fn needs_track(&self) -> bool {
        self.segs.iter().any(|s| s.yd != self.ys)
    }
    fn span(&self) -> (i64, i64) {
        let rows = self.rows();
        let lo = rows.first().copied().unwrap_or(self.ys).min(self.ys);
        let hi = rows.last().copied().unwrap_or(self.ys).max(self.ys);
        (lo, hi)
    }
}

struct Channel {
    groups: Vec<Group>,
    tracks: usize,
    /// Label per target row (real targets only).
    labels: BTreeMap<i64, String>,
    width: usize,
}

impl Channel {
    fn track_x(&self, t: usize) -> usize {
        2 + 2 * t
    }
    fn after_tracks(&self) -> usize {
        2 + 2 * self.tracks
    }
}

/// Crossing/overlap cost of a track order (`order[k]` = groups sharing
/// track `k`).
fn track_cost(groups: &[Group], order: &[Vec<usize>]) -> usize {
    const INF: usize = usize::MAX;
    let mut tx: Vec<Option<usize>> = vec![None; groups.len()];
    for (k, members) in order.iter().enumerate() {
        for &g in members {
            tx[g] = Some(2 + 2 * k);
        }
    }
    let mut cost = 0;
    for (a, ga) in groups.iter().enumerate() {
        let rows_a = ga.rows();
        // (row, x range, is_source)
        let mut hs: Vec<(i64, usize, usize, bool)> = Vec::new();
        match tx[a] {
            None => hs.push((ga.ys, 0, INF, false)),
            Some(t) => {
                hs.push((ga.ys, 0, t, true));
                for &r in &rows_a {
                    hs.push((r, t, INF, false));
                }
            }
        }
        for (b, gb) in groups.iter().enumerate() {
            let Some(tb) = tx[b] else { continue };
            if a == b {
                continue;
            }
            let (lo, hi) = gb.span();
            let rows_b = gb.rows();
            for &(r, x1, x2, is_src) in &hs {
                if !(x1 < tb && tb < x2 && lo <= r && r <= hi) {
                    continue;
                }
                let b_src = r == gb.ys;
                let b_tgt = rows_b.contains(&r);
                if !b_src && !b_tgt {
                    cost += 1; // plain crossing
                } else if (is_src && !rows_a.contains(&r) && b_tgt) || (!is_src && b_src && !b_tgt)
                {
                    // A source line running into another group's target
                    // line (or the reverse) would read as a connection.
                    cost += 100;
                }
            }
        }
    }
    cost
}

/// Two groups may share a vertical track when their spans are apart, or
/// meet only at a target row both lead to (they merge there).
fn can_share(a: &Group, b: &Group) -> bool {
    let (alo, ahi) = a.span();
    let (blo, bhi) = b.span();
    if ahi + 1 < blo || bhi + 1 < alo {
        return true;
    }
    let meet = if ahi == blo {
        ahi
    } else if bhi == alo {
        alo
    } else {
        return false;
    };
    a.rows().contains(&meet) && b.rows().contains(&meet) && a.ys != meet && b.ys != meet
}

/// Give every group that needs one a vertical track; returns the number of
/// tracks. Deterministic: exhaustive search for up to 6 tracks (first
/// minimum in lexicographic order), adjacent swaps beyond.
fn assign_tracks(groups: &mut [Group]) -> usize {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for g in (0..groups.len()).filter(|&g| groups[g].needs_track()) {
        match clusters
            .iter_mut()
            .find(|c| c.iter().all(|&m| can_share(&groups[m], &groups[g])))
        {
            Some(c) => c.push(g),
            None => clusters.push(vec![g]),
        }
    }
    clusters.sort_by_key(|c| (groups[c[0]].span(), groups[c[0]].ys));
    let mut best = clusters.clone();
    let mut best_cost = track_cost(groups, &best);
    if clusters.len() <= 6 {
        let mut idx: Vec<usize> = (0..clusters.len()).collect();
        loop {
            let perm: Vec<Vec<usize>> = idx.iter().map(|&k| clusters[k].clone()).collect();
            let c = track_cost(groups, &perm);
            if c < best_cost {
                best_cost = c;
                best = perm;
            }
            let Some(i) = (1..idx.len()).rev().find(|&i| idx[i - 1] < idx[i]) else {
                break;
            };
            let j = (i..idx.len())
                .rev()
                .find(|&j| idx[j] > idx[i - 1])
                .unwrap_or(i);
            idx.swap(i - 1, j);
            idx[i..].reverse();
        }
    } else {
        for _ in 0..64 {
            let mut improved = false;
            for i in 0..best.len() - 1 {
                best.swap(i, i + 1);
                let c = track_cost(groups, &best);
                if c < best_cost {
                    best_cost = c;
                    improved = true;
                } else {
                    best.swap(i, i + 1);
                }
            }
            if !improved {
                break;
            }
        }
    }
    for (k, members) in best.iter().enumerate() {
        for &g in members {
            groups[g].track = Some(k);
        }
    }
    best.len()
}

/// Everything the text renderer computes before painting.
struct Geometry {
    /// Slots per display column.
    cols: Vec<Vec<Slot>>,
    col_w: Vec<usize>,
    top: BTreeMap<Slot, i64>,
    height: BTreeMap<Slot, i64>,
    channels: Vec<Channel>,
    views: Vec<NodeView>,
}

impl Geometry {
    fn port(&self, s: Slot) -> i64 {
        self.top[&s] + if matches!(s, Slot::Node(_)) { 1 } else { 0 }
    }
    fn bottom(&self) -> i64 {
        self.top
            .iter()
            .map(|(s, y)| y + self.height[s])
            .max()
            .unwrap_or(0)
    }
}

fn geometry(layout: &Layout, opts: &RenderOptions<'_>) -> Geometry {
    let ncol = layout.layers.len();
    let cols: Vec<Vec<Slot>> = layout.slots.iter().rev().cloned().collect();
    let views: Vec<NodeView> = layout
        .nodes
        .iter()
        .map(|n| {
            let (glyph, reason) = status_of(opts.status, &n.name);
            NodeView { glyph, reason }
        })
        .collect();

    // Box sizes.
    let mut col_w = vec![0; ncol];
    let mut height = BTreeMap::new();
    for (c, slots) in cols.iter().enumerate() {
        let nodes: Vec<NodeId> = slots
            .iter()
            .filter_map(|s| match s {
                Slot::Node(n) => Some(*n),
                Slot::Dummy { .. } => None,
            })
            .collect();
        let name_w = nodes
            .iter()
            .map(|&n| len(&layout.nodes[n].name))
            .max()
            .unwrap_or(0);
        let glyph_w = nodes
            .iter()
            .map(|&n| len(views[n].glyph.symbol(opts.unicode)))
            .max()
            .unwrap_or(0);
        let reason_w = nodes
            .iter()
            .filter_map(|&n| views[n].reason.as_deref().map(len))
            .max()
            .unwrap_or(0)
            .min(REASON_MAX);
        col_w[c] = (name_w + 1 + glyph_w).max(reason_w) + 4;
        for s in slots {
            let h = match s {
                Slot::Node(n) => 3 + i64::from(views[*n].reason.is_some()),
                Slot::Dummy { .. } => 1,
            };
            height.insert(*s, h);
        }
    }

    // Neighbours across channels.
    let segs = layout.segments();
    let mut nbrs: BTreeMap<Slot, Vec<Slot>> = BTreeMap::new();
    for s in &segs {
        nbrs.entry(s.upper).or_default().push(s.lower);
        nbrs.entry(s.lower).or_default().push(s.upper);
    }
    let port_off = |s: &Slot| if matches!(s, Slot::Node(_)) { 1 } else { 0 };

    // Vertical placement: start stacked, then repeatedly move every slot
    // towards the mean port row of its neighbours (order-preserving
    // least-squares via pool-adjacent-violators).
    let mut top: BTreeMap<Slot, i64> = BTreeMap::new();
    for slots in &cols {
        let mut y = 0;
        for s in slots {
            top.insert(*s, y);
            y += height[s] + GAP;
        }
    }
    for pass in 0..24 {
        let order: Vec<usize> = if pass % 2 == 0 {
            (0..ncol).collect()
        } else {
            (0..ncol).rev().collect()
        };
        for c in order {
            let slots = &cols[c];
            let mut targets = Vec::with_capacity(slots.len());
            let mut offset = 0;
            for s in slots {
                let want_port = match nbrs.get(s) {
                    Some(ns) if !ns.is_empty() => {
                        let sum: i64 = ns.iter().map(|n| top[n] + port_off(n)).sum();
                        let cnt = ns.len() as i64;
                        (2 * sum + cnt).div_euclid(2 * cnt)
                    }
                    _ => top[s] + port_off(s),
                };
                targets.push(want_port - port_off(s) - offset);
                offset += height[s] + GAP;
            }
            let z = isotonic(&targets);
            let mut offset = 0;
            for (s, z) in slots.iter().zip(z) {
                top.insert(*s, z + offset);
                offset += height[s] + GAP;
            }
        }
    }
    let min = top.values().copied().min().unwrap_or(0);
    for y in top.values_mut() {
        *y -= min;
    }

    let mut geo = Geometry {
        cols,
        col_w,
        top,
        height,
        channels: Vec::new(),
        views,
    };

    // Routing channels.
    for c in 0..ncol.saturating_sub(1) {
        let lower_layer = ncol - 2 - c;
        let mut groups: BTreeMap<Slot, Group> = BTreeMap::new();
        for s in segs.iter().filter(|s| s.layer == lower_layer) {
            let g = groups.entry(s.upper).or_insert_with(|| Group {
                src: s.upper,
                ys: geo.port(s.upper),
                segs: Vec::new(),
                track: None,
            });
            g.segs.push(ChanSeg {
                edge: s.edge,
                target: s.lower,
                yd: geo.port(s.lower),
                soft: layout.edges[s.edge].soft,
            });
        }
        let mut groups: Vec<Group> = groups.into_values().collect();
        groups.sort_by_key(|g| g.ys);
        let tracks = assign_tracks(&mut groups);
        let mut labels: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        if opts.edge_labels {
            for g in &groups {
                for s in &g.segs {
                    if let (Slot::Node(_), Some(l)) = (s.target, edge_label(&layout.edges[s.edge]))
                    {
                        labels.entry(s.yd).or_default().insert(l);
                    }
                }
            }
        }
        let labels: BTreeMap<i64, String> = labels
            .into_iter()
            .map(|(r, ls)| (r, ls.into_iter().collect::<Vec<_>>().join(",")))
            .collect();
        let label_w = labels.values().map(|l| len(l)).max().unwrap_or(0);
        let after = 2 + 2 * tracks;
        let zone = if label_w > 0 { label_w + 3 } else { 0 };
        let width = (after + zone + 2).max(6);
        geo.channels.push(Channel {
            groups,
            tracks,
            labels,
            width,
        });
    }
    geo
}

/// Pool-adjacent-violators: the non-decreasing sequence closest (least
/// squares, floored means) to `t`.
fn isotonic(t: &[i64]) -> Vec<i64> {
    // blocks of (sum, count)
    let mut blocks: Vec<(i64, i64)> = Vec::new();
    for &v in t {
        blocks.push((v, 1));
        while blocks.len() > 1 {
            let (s2, c2) = blocks[blocks.len() - 1];
            let (s1, c1) = blocks[blocks.len() - 2];
            if s1 * c2 > s2 * c1 {
                blocks.pop();
                blocks.pop();
                blocks.push((s1 + s2, c1 + c2));
            } else {
                break;
            }
        }
    }
    let mut out = Vec::with_capacity(t.len());
    for (s, c) in blocks {
        let v = (2 * s + c).div_euclid(2 * c);
        out.extend(std::iter::repeat_n(v, c as usize));
    }
    out
}

/// Terminal rendering with box drawing (see module docs).
pub fn render_text(layout: &Layout, opts: &RenderOptions<'_>) -> String {
    let mut lines: Vec<String> = Vec::new();
    let ncol = layout.layers.len();
    if ncol > 0 {
        let geo = geometry(layout, opts);
        // Bands of columns that fit the width.
        let stub_w = |b: usize| -> usize {
            if b >= ncol {
                return 0;
            }
            let names = geo.cols[b]
                .iter()
                .map(|s| len(&final_target(layout, *s)))
                .max()
                .unwrap_or(0);
            geo.channels[b - 1].width + 1 + names
        };
        let span_w = |a: usize, b: usize| -> usize {
            (a..b).map(|c| geo.col_w[c]).sum::<usize>()
                + (a..b - 1).map(|c| geo.channels[c].width).sum::<usize>()
        };
        let mut bands = Vec::new();
        let mut a = 0;
        while a < ncol {
            let mut b = a + 1;
            while opts.width > 0 && b < ncol && span_w(a, b + 1) + stub_w(b + 1) <= opts.width {
                b += 1;
            }
            if opts.width == 0 {
                b = ncol;
            }
            bands.push((a, b));
            a = b;
        }
        for (i, &(a, b)) in bands.iter().enumerate() {
            if i > 0 {
                lines.push(String::new());
            }
            paint_band(layout, &geo, opts, a, b, &mut lines);
        }
    }
    // Soft edges that would point backwards are listed, not drawn.
    let back: Vec<&Edge> = (0..layout.edges.len())
        .filter(|&e| !layout.is_forward(e))
        .map(|e| &layout.edges[e])
        .collect();
    if !back.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("soft edges not drawn (they would close a cycle):".to_string());
        let arrow = if opts.unicode { "╌╌▶" } else { "..>" };
        for e in back {
            let mut l = format!(
                "  {} {arrow} {}",
                layout.nodes[e.from].name, layout.nodes[e.to].name
            );
            if opts.edge_labels
                && let Some(label) = edge_label(e)
            {
                let _ = write!(l, "  {label}");
            }
            lines.push(l);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The stem a slot leads to: itself, or a dummy's final target.
fn final_target(layout: &Layout, s: Slot) -> String {
    match s {
        Slot::Node(n) => layout.nodes[n].name.clone(),
        Slot::Dummy { edge: e, .. } => layout.nodes[layout.edges[e].to].name.clone(),
    }
}

fn paint_band(
    layout: &Layout,
    geo: &Geometry,
    opts: &RenderOptions<'_>,
    a: usize,
    b: usize,
    out: &mut Vec<String>,
) {
    let ncol = geo.cols.len();
    let stub = b < ncol;
    // x of each column in this band (and of the stub column).
    let mut xs = Vec::new();
    let mut x = 0;
    for c in a..b {
        xs.push(x);
        x += geo.col_w[c];
        if c + 1 < ncol && (c + 1 < b || stub) {
            x += geo.channels[c].width;
        }
    }
    let stub_x = x;
    let width = stub_x
        + if stub {
            1 + geo.cols[b]
                .iter()
                .map(|s| len(&final_target(layout, *s)))
                .max()
                .unwrap_or(0)
        } else {
            0
        };
    let height = usize::try_from(geo.bottom()).unwrap_or(0) + 1;
    let mut cv = Canvas::new(height, width, opts.unicode);
    let (h, v, tl, tr, bl, br, port, arrow) = if opts.unicode {
        ('─', '│', '┌', '┐', '└', '┘', '├', '▶')
    } else {
        ('-', '|', '+', '+', '+', '+', '+', '>')
    };

    for c in a..b {
        let x0 = xs[c - a];
        let w = geo.col_w[c];
        let has_out = c + 1 < ncol && (c + 1 < b || stub);
        let sources: BTreeSet<Slot> = if has_out {
            geo.channels[c].groups.iter().map(|g| g.src).collect()
        } else {
            BTreeSet::new()
        };
        for s in &geo.cols[c] {
            let y = geo.top[s];
            match *s {
                Slot::Dummy { edge: e, .. } => {
                    let soft = layout.edges[e].soft;
                    cv.hline(y, x0, x0 + w - 1, soft);
                    cv.line(y, x0, LEFT, soft);
                    cv.line(y, x0 + w - 1, RIGHT, soft);
                    if c == a && a > 0 {
                        cv.put(y, x0, if opts.unicode { '…' } else { '~' }, None);
                    }
                }
                Slot::Node(n) => {
                    let view = &geo.views[n];
                    let hgt = geo.height[s];
                    let inner = w - 2;
                    cv.put(y, x0, tl, None);
                    cv.put(y, x0 + w - 1, tr, None);
                    cv.put(y + hgt - 1, x0, bl, None);
                    cv.put(y + hgt - 1, x0 + w - 1, br, None);
                    for i in 1..=inner {
                        cv.put(y, x0 + i, h, None);
                        cv.put(y + hgt - 1, x0 + i, h, None);
                    }
                    for r in 1..hgt - 1 {
                        cv.put(y + r, x0, v, None);
                        cv.put(y + r, x0 + w - 1, v, None);
                        for i in 1..=inner {
                            cv.put(y + r, x0 + i, ' ', None);
                        }
                    }
                    let color = Some(view.glyph.ansi());
                    cv.text(y + 1, x0 + 2, &layout.nodes[n].name, None);
                    let sym = view.glyph.symbol(opts.unicode);
                    cv.text(y + 1, x0 + w - 2 - len(sym), sym, color);
                    if let Some(r) = &view.reason {
                        cv.text(y + 2, x0 + 2, &truncate(r, w - 4), color);
                    }
                    if sources.contains(s) {
                        cv.put(y + 1, x0 + w - 1, port, None);
                    }
                }
            }
        }
        if !has_out {
            continue;
        }
        // Channel to the right of column c.
        let ch = &geo.channels[c];
        let cs = x0 + w;
        let ce = cs + ch.width;
        let into_stub = c + 1 == b;
        for g in &ch.groups {
            for sg in &g.segs {
                let end = ce - 1;
                match g.track {
                    None => cv.hline(g.ys, cs, end, sg.soft),
                    Some(t) => {
                        let tx = cs + ch.track_x(t);
                        cv.hline(g.ys, cs, tx, sg.soft);
                        cv.vline(tx, g.ys, sg.yd, sg.soft);
                        cv.hline(sg.yd, tx, end, sg.soft);
                    }
                }
                cv.line(g.ys, cs, LEFT, sg.soft);
                if matches!(sg.target, Slot::Node(_)) || into_stub {
                    cv.put(sg.yd, end, arrow, None);
                } else {
                    cv.line(sg.yd, end, RIGHT, sg.soft);
                }
                if into_stub {
                    cv.text(sg.yd, ce + 1, &final_target(layout, sg.target), None);
                }
            }
        }
        for (&row, label) in &ch.labels {
            let lx = cs + ch.after_tracks() + 1;
            cv.put(row, lx, ' ', None);
            cv.text(row, lx + 1, label, None);
            cv.put(row, lx + 1 + len(label), ' ', None);
        }
    }
    cv.finish(opts.color, out);
}
