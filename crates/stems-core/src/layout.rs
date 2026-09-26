//! Layered (Sugiyama-lite) layout of the dependency graph, shared by
//! `stems graph` ([`crate::render`]) and the TUI graph view.
//!
//! The layout is pure data: layers of nodes (layer 0 = stems with no
//! dependencies, i.e. what starts first), the edges, and per-layer
//! [`Slot`]s that also hold the dummy positions long edges pass through.
//! Within a layer, order is alphabetical first and then refined by a
//! deterministic barycenter crossing-reduction pass.
//!
//! Renderers draw **dependants on the left and dependencies on the right**
//! (column `0` is the highest layer), so every arrow `from -> to` points
//! right, the same way `graph LR` in Mermaid and `rankdir=LR` in DOT draw
//! `dependant -> dependency`. See [`Layout::column_of`].

use std::cmp::Ordering;
use std::collections::BTreeSet;

use serde::Serialize;
use stems_config::{Condition, Dependency, Protocol, StemType, Workspace};

use crate::error::Error;
use crate::graph::start_order;

/// Index into [`Layout::nodes`].
pub type NodeId = usize;

/// One piece of an edge between two adjacent layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Segment {
    /// Index into [`Layout::edges`].
    pub edge: usize,
    /// The lower layer.
    pub layer: usize,
    /// Slot in `layer + 1` (the dependant side).
    pub upper: Slot,
    /// Slot in `layer` (the dependency side).
    pub lower: Slot,
}

/// A stem in the layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Stem name.
    pub name: String,
    /// Layer (0 = no dependencies; the [`start_order`] layer for hard edges).
    pub layer: usize,
    /// Position within [`Layout::layers`]`[layer]` (top to bottom).
    pub index: usize,
    /// Stem type.
    pub kind: StemType,
}

/// A `depends_on` edge: `from` depends on `to`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Edge {
    /// The dependant.
    pub from: NodeId,
    /// The dependency.
    pub to: NodeId,
    /// When the edge is satisfied.
    pub condition: Condition,
    /// Soft edges do not order starts.
    pub soft: bool,
    /// Informational protocol (FR-GR-5).
    pub protocol: Option<Protocol>,
    /// Informational address (FR-GR-5).
    pub via: Option<String>,
}

/// One vertical position in a layer: a stem, or a point a long edge
/// (spanning more than one layer) passes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// A stem.
    Node(NodeId),
    /// Edge `Layout::edges[edge]` passes through `layer`.
    Dummy {
        /// Index into [`Layout::edges`].
        edge: usize,
        /// The layer this point is in.
        layer: usize,
    },
}

/// The layered graph. See the module docs for orientation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Layout {
    /// Stems per layer, top to bottom.
    pub layers: Vec<Vec<NodeId>>,
    /// Every stem, sorted by name (a [`NodeId`] is an index here).
    pub nodes: Vec<Node>,
    /// Every edge between laid-out stems, in declaration order.
    pub edges: Vec<Edge>,
    /// Per layer, top to bottom: the stems of [`Layout::layers`] plus the
    /// dummy points of long edges, in their crossing-reduced order.
    pub slots: Vec<Vec<Slot>>,
}

/// Lay out the enabled stems of `ws`. A hard cycle is the `CYCLE` error of
/// [`start_order`].
pub fn layout(ws: &Workspace) -> Result<Layout, Error> {
    start_order(ws)?;
    let mut stems: Vec<(String, StemType)> =
        ws.stems().map(|s| (s.name.clone(), s.kind())).collect();
    stems.sort_by(|a, b| a.0.cmp(&b.0));
    let id = |n: &str| stems.iter().position(|(s, _)| s == n);
    let mut edges = Vec::new();
    for s in ws.stems() {
        for d in &s.depends_on {
            let (Some(from), Some(to)) = (id(&s.name), id(&d.stem)) else {
                continue;
            };
            edges.push(Edge {
                from,
                to,
                condition: d.condition,
                soft: d.soft,
                protocol: d.protocol,
                via: d.via.clone(),
            });
        }
    }
    Ok(build(stems, edges))
}

/// Lay out stems known only by name, type and `depends_on` (e.g. from the
/// daemon's `stem_config`, in the TUI): `deps` are `(dependant, edge)`
/// pairs; edges naming an unknown stem are dropped. Unlike [`layout`] there
/// is no cycle check: a hard cycle (which a loaded workspace cannot have)
/// still lays out, with the cycle's back edge undrawn.
pub fn from_parts(
    stems: impl IntoIterator<Item = (String, StemType)>,
    deps: &[(String, Dependency)],
) -> Layout {
    let mut stems: Vec<(String, StemType)> = stems.into_iter().collect();
    stems.sort_by(|a, b| a.0.cmp(&b.0));
    stems.dedup_by(|a, b| a.0 == b.0);
    let id = |n: &str| stems.binary_search_by(|(s, _)| s.as_str().cmp(n)).ok();
    let edges = deps
        .iter()
        .filter_map(|(from, d)| {
            Some(Edge {
                from: id(from)?,
                to: id(&d.stem)?,
                condition: d.condition,
                soft: d.soft,
                protocol: d.protocol,
                via: d.via.clone(),
            })
        })
        .collect();
    build(stems, edges)
}

impl Layout {
    /// The node called `name`.
    pub fn node(&self, name: &str) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.name == name)
    }

    /// Display column of `layer`: renderers draw the highest layer (the
    /// dependants) in column 0 and layer 0 rightmost.
    pub fn column_of(&self, layer: usize) -> usize {
        self.layers.len() - 1 - layer
    }

    /// Layers in display order, left to right.
    pub fn columns(&self) -> impl Iterator<Item = &Vec<NodeId>> {
        self.layers.iter().rev()
    }

    /// `true` if the edge can be drawn left to right (its dependant is in a
    /// higher layer). Only soft edges that would close a cycle, or a soft
    /// self-edge, are not; renderers list them separately.
    pub fn is_forward(&self, edge: usize) -> bool {
        let e = &self.edges[edge];
        self.nodes[e.from].layer > self.nodes[e.to].layer
    }

    /// Direct neighbours of `node` (dependencies and dependants, either
    /// edge kind), sorted, without `node` itself.
    pub fn neighbours(&self, node: NodeId) -> Vec<NodeId> {
        let set: BTreeSet<NodeId> = self
            .edges
            .iter()
            .filter_map(|e| {
                if e.from == node {
                    Some(e.to)
                } else if e.to == node {
                    Some(e.from)
                } else {
                    None
                }
            })
            .filter(|&n| n != node)
            .collect();
        set.into_iter().collect()
    }

    /// The sub-layout of `stem` and its direct neighbours (with every edge
    /// among them), laid out afresh. Empty if `stem` is not in the layout.
    pub fn focus(&self, stem: &str) -> Layout {
        let Some(center) = self.node(stem) else {
            return Layout::default();
        };
        let mut keep = self.neighbours(center);
        keep.push(center);
        self.subset(&keep)
    }

    /// The sub-layout of the stems named in `names` (unknown names are
    /// ignored), with every edge among them, laid out afresh (`stems graph
    /// --profile`).
    pub fn restrict<S: AsRef<str>>(&self, names: &[S]) -> Layout {
        let keep: Vec<NodeId> = names.iter().filter_map(|n| self.node(n.as_ref())).collect();
        self.subset(&keep)
    }

    fn subset(&self, keep: &[NodeId]) -> Layout {
        let mut keep = keep.to_vec();
        keep.sort_by(|a, b| self.nodes[*a].name.cmp(&self.nodes[*b].name));
        keep.dedup();
        let stems = keep
            .iter()
            .map(|&n| (self.nodes[n].name.clone(), self.nodes[n].kind))
            .collect();
        let new_id = |old: NodeId| keep.iter().position(|&k| k == old);
        let edges = self
            .edges
            .iter()
            .filter_map(|e| {
                Some(Edge {
                    from: new_id(e.from)?,
                    to: new_id(e.to)?,
                    ..e.clone()
                })
            })
            .collect();
        build(stems, edges)
    }

    /// Every drawn edge piece between adjacent layers: `upper` is in layer
    /// `layer + 1` (display column `column_of(layer + 1)`), `lower` in
    /// `layer`. Long edges yield one segment per layer they span, through
    /// their [`Slot::Dummy`]s. Backwards edges yield none.
    pub fn segments(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        for (i, e) in self.edges.iter().enumerate() {
            let (hi, lo) = (self.nodes[e.from].layer, self.nodes[e.to].layer);
            let at = |l: usize| {
                if l == hi {
                    Slot::Node(e.from)
                } else if l == lo {
                    Slot::Node(e.to)
                } else {
                    Slot::Dummy { edge: i, layer: l }
                }
            };
            for l in lo..hi {
                out.push(Segment {
                    edge: i,
                    layer: l,
                    upper: at(l + 1),
                    lower: at(l),
                });
            }
        }
        out
    }

    /// Number of edge crossings between adjacent layers (dummy segments
    /// included). What the ordering pass minimises.
    pub fn crossings(&self) -> usize {
        let segs = segments(self);
        let mut pos = vec![0usize; segs.slot_ids.len()];
        for layer in &self.slots {
            for (i, s) in layer.iter().enumerate() {
                pos[segs.id(*s)] = i;
            }
        }
        (0..self.slots.len().saturating_sub(1))
            .map(|l| segs.crossings(l, &pos))
            .sum()
    }
}

/// Lay out `stems` (sorted by name; index = [`NodeId`]) with `edges`.
fn build(stems: Vec<(String, StemType)>, edges: Vec<Edge>) -> Layout {
    let n = stems.len();
    let layer = layering(n, &edges);
    let depth = layer.iter().map(|l| l + 1).max().unwrap_or(0);
    let mut out = Layout {
        layers: vec![Vec::new(); depth],
        nodes: stems
            .into_iter()
            .zip(&layer)
            .map(|((name, kind), &layer)| Node {
                name,
                layer,
                index: 0,
                kind,
            })
            .collect(),
        edges,
        slots: vec![Vec::new(); depth],
    };
    // Initial order: connected stems alphabetically, then isolated ones.
    let connected: Vec<bool> = (0..n)
        .map(|v| {
            out.edges
                .iter()
                .enumerate()
                .any(|(i, e)| (e.from == v || e.to == v) && out.is_forward(i))
        })
        .collect();
    let mut ids: Vec<NodeId> = (0..n).collect();
    ids.sort_by_key(|&v| (!connected[v], v));
    for v in ids {
        out.slots[out.nodes[v].layer].push(Slot::Node(v));
    }
    for (i, e) in out.edges.iter().enumerate() {
        let (hi, lo) = (out.nodes[e.from].layer, out.nodes[e.to].layer);
        for l in (lo + 1)..hi {
            out.slots[l].push(Slot::Dummy { edge: i, layer: l });
        }
    }
    order(&mut out);
    for (l, slots) in out.slots.iter().enumerate() {
        for s in slots {
            if let Slot::Node(v) = *s {
                out.nodes[v].index = out.layers[l].len();
                out.layers[l].push(v);
            }
        }
    }
    out
}

/// Longest-path layering over hard edges plus every soft edge that does
/// not close a cycle (taken in declaration order). With hard edges only this
/// is exactly [`start_order`]'s layering.
fn layering(n: usize, edges: &[Edge]) -> Vec<usize> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges.iter().filter(|e| !e.soft) {
        adj[e.from].push(e.to);
    }
    for e in edges.iter().filter(|e| e.soft) {
        if e.from != e.to && !reaches(&adj, e.to, e.from) {
            adj[e.from].push(e.to);
        }
    }
    let mut layer: Vec<Option<usize>> = vec![None; n];
    fn visit(v: usize, adj: &[Vec<usize>], layer: &mut [Option<usize>], depth: usize) -> usize {
        if let Some(l) = layer[v] {
            return l;
        }
        // Hard edges are acyclic (checked by start_order) and soft edges
        // were only added when acyclic; the guard is belt and braces.
        if depth > adj.len() {
            return 0;
        }
        let l = adj[v]
            .iter()
            .map(|&t| visit(t, adj, layer, depth + 1) + 1)
            .max()
            .unwrap_or(0);
        layer[v] = Some(l);
        l
    }
    (0..n).map(|v| visit(v, &adj, &mut layer, 0)).collect()
}

fn reaches(adj: &[Vec<usize>], from: usize, target: usize) -> bool {
    let mut seen = vec![false; adj.len()];
    let mut stack = vec![from];
    while let Some(v) = stack.pop() {
        if v == target {
            return true;
        }
        if !std::mem::replace(&mut seen[v], true) {
            stack.extend(adj[v].iter().copied());
        }
    }
    false
}

/// Segments between adjacent layers over dense slot ids.
struct Segments {
    /// Every slot, dense id = index.
    slot_ids: Vec<Slot>,
    /// `between[l]` = segments `(upper slot in l+1, lower slot in l)`.
    between: Vec<Vec<(usize, usize)>>,
}

impl Segments {
    fn id(&self, s: Slot) -> usize {
        self.slot_ids
            .binary_search(&s)
            .expect("slot belongs to the layout")
    }

    fn crossings(&self, l: usize, pos: &[usize]) -> usize {
        let segs = &self.between[l];
        let mut c = 0;
        for (i, &(a1, b1)) in segs.iter().enumerate() {
            for &(a2, b2) in &segs[i + 1..] {
                let up = pos[a1].cmp(&pos[a2]);
                let down = pos[b1].cmp(&pos[b2]);
                if up != Ordering::Equal && down != Ordering::Equal && up != down {
                    c += 1;
                }
            }
        }
        c
    }
}

fn segments(out: &Layout) -> Segments {
    let mut slot_ids: Vec<Slot> = out.slots.iter().flatten().copied().collect();
    slot_ids.sort();
    let mut s = Segments {
        slot_ids,
        between: vec![Vec::new(); out.slots.len().saturating_sub(1)],
    };
    for seg in out.segments() {
        let pair = (s.id(seg.upper), s.id(seg.lower));
        s.between[seg.layer].push(pair);
    }
    s
}

/// Crossing reduction: barycenter sweeps down and up, then adjacent
/// transpositions; keeps the best order seen. Integer arithmetic and stable
/// tie-breaking make it deterministic.
fn order(out: &mut Layout) {
    let depth = out.slots.len();
    if depth < 2 {
        return;
    }
    let segs = segments(out);
    let mut pos = vec![0usize; segs.slot_ids.len()];
    let mut layers: Vec<Vec<usize>> = out
        .slots
        .iter()
        .map(|l| l.iter().map(|s| segs.id(*s)).collect())
        .collect();
    let sync = |layers: &Vec<Vec<usize>>, pos: &mut Vec<usize>| {
        for l in layers {
            for (i, &s) in l.iter().enumerate() {
                pos[s] = i;
            }
        }
    };
    let total = |pos: &[usize]| -> usize { (0..depth - 1).map(|l| segs.crossings(l, pos)).sum() };
    sync(&layers, &mut pos);
    let mut best = (total(&pos), layers.clone());

    // Reorder layer `l` by the barycenter of its neighbours in layer `other`.
    let reorder = |layers: &mut Vec<Vec<usize>>, pos: &mut Vec<usize>, l: usize, other: usize| {
        let (lo_layer, upper_side) = if other > l { (l, true) } else { (other, false) };
        let mut sum = vec![0usize; pos.len()];
        let mut cnt = vec![0usize; pos.len()];
        for &(up, down) in &segs.between[lo_layer] {
            let (me, nb) = if upper_side { (down, up) } else { (up, down) };
            sum[me] += pos[nb];
            cnt[me] += 1;
        }
        let key = |s: usize| {
            if cnt[s] == 0 {
                (pos[s], 1)
            } else {
                (sum[s], cnt[s])
            }
        };
        layers[l].sort_by(|&a, &b| {
            let (sa, ca) = key(a);
            let (sb, cb) = key(b);
            (sa * cb).cmp(&(sb * ca)).then(pos[a].cmp(&pos[b]))
        });
        for (i, &s) in layers[l].iter().enumerate() {
            pos[s] = i;
        }
    };

    for _ in 0..8 {
        for l in (0..depth - 1).rev() {
            reorder(&mut layers, &mut pos, l, l + 1);
        }
        for l in 1..depth {
            reorder(&mut layers, &mut pos, l, l - 1);
        }
        let t = total(&pos);
        if t < best.0 {
            best = (t, layers.clone());
        }
        if t == 0 {
            break;
        }
    }

    // Transpose: swap adjacent slots while it strictly reduces crossings.
    layers = best.1;
    sync(&layers, &mut pos);
    let local = |pos: &[usize], l: usize| -> usize {
        let mut c = 0;
        if l > 0 {
            c += segs.crossings(l - 1, pos);
        }
        if l + 1 < depth {
            c += segs.crossings(l, pos);
        }
        c
    };
    for _ in 0..16 {
        let mut improved = false;
        #[allow(clippy::needless_range_loop)] // `local` reads other layers
        for l in 0..depth {
            for i in 0..layers[l].len().saturating_sub(1) {
                let before = local(&pos, l);
                let (a, b) = (layers[l][i], layers[l][i + 1]);
                pos.swap(a, b);
                if local(&pos, l) < before {
                    layers[l].swap(i, i + 1);
                    improved = true;
                } else {
                    pos.swap(a, b);
                }
            }
        }
        if !improved {
            break;
        }
    }

    out.slots = layers
        .into_iter()
        .map(|l| l.into_iter().map(|s| segs.slot_ids[s]).collect())
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::tests::{load_str, ws};

    fn names(l: &Layout) -> Vec<Vec<&str>> {
        l.layers
            .iter()
            .map(|v| v.iter().map(|&n| l.nodes[n].name.as_str()).collect())
            .collect()
    }

    #[test]
    fn layers_follow_start_order_for_hard_edges() {
        let w = ws(&[
            ("top", &[("left", false), ("right", false)]),
            ("right", &[("base", false)]),
            ("left", &[("base", false)]),
            ("base", &[]),
        ]);
        let l = layout(&w).unwrap();
        let order: Vec<Vec<String>> = start_order(&w).unwrap();
        let got: Vec<Vec<String>> = names(&l)
            .into_iter()
            .map(|v| {
                let mut v: Vec<String> = v.into_iter().map(String::from).collect();
                v.sort();
                v
            })
            .collect();
        assert_eq!(got, order);
        assert_eq!(l.crossings(), 0);
        for (li, layer) in l.layers.iter().enumerate() {
            for (i, &n) in layer.iter().enumerate() {
                assert_eq!((l.nodes[n].layer, l.nodes[n].index), (li, i));
            }
        }
    }

    #[test]
    fn long_edges_get_dummies() {
        let w = ws(&[
            ("a", &[("b", false), ("c", false)]),
            ("b", &[("c", false)]),
            ("c", &[]),
        ]);
        let l = layout(&w).unwrap();
        assert_eq!(names(&l), [vec!["c"], vec!["b"], vec!["a"]]);
        let a_c = l
            .edges
            .iter()
            .position(|e| l.nodes[e.from].name == "a" && l.nodes[e.to].name == "c")
            .unwrap();
        assert!(l.slots[1].contains(&Slot::Dummy {
            edge: a_c,
            layer: 1
        }));
    }

    #[test]
    fn soft_edges_shape_layers_unless_they_close_a_cycle() {
        let w = ws(&[
            ("api", &[("db", false)]),
            ("worker", &[("api", true)]),
            ("db", &[("worker", true)]),
        ]);
        let l = layout(&w).unwrap();
        // worker -> api is taken (worker above api); db -> worker would close
        // a cycle and is left as a backwards edge.
        assert_eq!(names(&l), [vec!["db"], vec!["api"], vec!["worker"]]);
        let back: Vec<usize> = (0..l.edges.len()).filter(|&e| !l.is_forward(e)).collect();
        assert_eq!(back.len(), 1);
        assert_eq!(l.nodes[l.edges[back[0]].from].name, "db");
    }

    #[test]
    fn cycle_is_an_error() {
        let w = ws(&[("a", &[("b", false)]), ("b", &[("a", false)])]);
        assert_eq!(layout(&w).unwrap_err().code, crate::ErrorCode::Cycle);
    }

    #[test]
    fn disabled_stems_are_left_out() {
        let w = load_str(
            "stems:\n  a: { type: process, depends_on: [b, c] }\n  b: { type: process, enabled: false }\n  c: { type: docker, image: x }\n",
        );
        let l = layout(&w).unwrap();
        assert_eq!(names(&l), [vec!["c"], vec!["a"]]);
        assert_eq!(l.edges.len(), 1);
        assert_eq!(l.nodes[l.node("c").unwrap()].kind, StemType::Docker);
    }

    /// Deterministic pseudo-random DAG: stem `sNN` depends on some lower ids.
    pub(crate) fn random_dag(n: usize, seed: u64) -> Workspace {
        let mut x = seed;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let names: Vec<String> = (0..n).map(|i| format!("s{i:02}")).collect();
        let mut deps: Vec<Vec<(String, bool)>> = vec![Vec::new(); n];
        for (i, d) in deps.iter_mut().enumerate().skip(1) {
            let k = (next() % 3) as usize;
            let mut chosen = BTreeSet::new();
            for _ in 0..k {
                chosen.insert((next() % i as u64) as usize);
            }
            *d = chosen
                .into_iter()
                .map(|j| (names[j].clone(), false))
                .collect();
        }
        let spec: Vec<(&str, Vec<(&str, bool)>)> = names
            .iter()
            .zip(&deps)
            .map(|(n, d)| {
                (
                    n.as_str(),
                    d.iter().map(|(s, b)| (s.as_str(), *b)).collect(),
                )
            })
            .collect();
        let spec: Vec<(&str, &[(&str, bool)])> =
            spec.iter().map(|(n, d)| (*n, d.as_slice())).collect();
        ws(&spec)
    }

    #[test]
    fn ordering_is_deterministic_and_reduces_crossings() {
        let w = random_dag(20, 0x5eed);
        let a = layout(&w).unwrap();
        let b = layout(&w).unwrap();
        assert_eq!(a, b);
        // Compare with the initial alphabetical order.
        let mut initial = a.clone();
        for slots in &mut initial.slots {
            slots.sort();
        }
        assert!(a.crossings() <= initial.crossings());
    }

    #[test]
    fn focus_keeps_only_neighbours() {
        let w = ws(&[
            ("web", &[("api", false)]),
            ("api", &[("db", false), ("cache", true)]),
            ("worker", &[("db", false)]),
            ("db", &[]),
            ("cache", &[]),
        ]);
        let l = layout(&w).unwrap();
        let f = l.focus("api");
        let mut got: Vec<&str> = f.nodes.iter().map(|n| n.name.as_str()).collect();
        got.sort();
        assert_eq!(got, ["api", "cache", "db", "web"]);
        assert_eq!(f.edges.len(), 3);
        assert!(
            f.edges
                .iter()
                .all(|e| f.nodes[e.from].name == "api" || f.nodes[e.to].name == "api")
        );
        assert_eq!(l.focus("ghost"), Layout::default());
        let leaf = l.focus("worker");
        assert_eq!(leaf.nodes.len(), 2);
        assert_eq!(leaf.layers.len(), 2);
    }
}
