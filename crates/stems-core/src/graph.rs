//! The dependency graph: hard `depends_on` edges order starts; soft edges are
//! informational only (FR-GR-1..3).

use std::collections::{BTreeSet, VecDeque};

use petgraph::algo::tarjan_scc;
use petgraph::graph::{DiGraph, NodeIndex};
use serde_json::json;
use stems_config::{ConfigPath, Stem, Workspace};

use crate::error::{Error, ErrorCode};

/// A dependency cycle: stem names in edge order, the first one repeated at
/// the end is implied (`["a", "b"]` is `a -> b -> a`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cycle {
    /// Stems on the cycle, starting with the first declared one.
    pub stems: Vec<String>,
    /// Index into `stems.<first>.depends_on` of the edge that starts it.
    pub first_edge: usize,
}

impl Cycle {
    /// `a -> b -> a`.
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = self.stems.iter().map(String::as_str).collect();
        parts.push(&self.stems[0]);
        parts.join(" -> ")
    }

    /// The `CYCLE` error for this cycle (path at the first stem's
    /// `depends_on`; call [`Error::located`] for the span).
    pub fn to_error(&self) -> Error {
        let first = &self.stems[0];
        let last = self.stems.last().unwrap_or(first);
        let edge = if self.stems.len() == 1 {
            format!("{first} -> {first}")
        } else {
            format!("{last} -> {first}")
        };
        Error::new(
            ErrorCode::Cycle,
            format!("dependency cycle: {}", self.display()),
        )
        .with_path(ConfigPath::root().key("stems").key(first).key("depends_on"))
        .with_hint(format!(
            "remove the edge {edge} or mark it `soft: true` (soft edges do not order starts)"
        ))
        .with_details(json!({ "cycle": self.stems }))
    }
}

/// Stems of `ws` selected by `keep`, with their hard edges to other kept
/// stems, as `(names, adjacency)` in declaration order. Each adjacency entry
/// is `(target, index in depends_on)`.
struct Hard<'a> {
    names: Vec<&'a str>,
    adj: Vec<Vec<(usize, usize)>>,
}

impl<'a> Hard<'a> {
    fn new(ws: &'a Workspace, keep: impl Fn(&Stem) -> bool) -> Self {
        let stems: Vec<&Stem> = ws.stems.values().filter(|s| keep(s)).collect();
        let names: Vec<&str> = stems.iter().map(|s| s.name.as_str()).collect();
        let index = |n: &str| names.iter().position(|x| *x == n);
        let adj = stems
            .iter()
            .map(|s| {
                s.depends_on
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| !d.soft)
                    .filter_map(|(i, d)| index(&d.stem).map(|t| (t, i)))
                    .collect()
            })
            .collect();
        Self { names, adj }
    }

    /// Every cycle, one per strongly connected component, in declaration
    /// order of their first stem.
    fn cycles(&self) -> Vec<Cycle> {
        let mut g: DiGraph<(), ()> = DiGraph::new();
        let nodes: Vec<NodeIndex> = self.names.iter().map(|_| g.add_node(())).collect();
        for (from, edges) in self.adj.iter().enumerate() {
            for (to, _) in edges {
                g.add_edge(nodes[from], nodes[*to], ());
            }
        }
        let mut out: Vec<Cycle> = tarjan_scc(&g)
            .into_iter()
            .filter_map(|scc| {
                let members: BTreeSet<usize> = scc.iter().map(|n| n.index()).collect();
                let start = *members.iter().next()?;
                let self_loop = self.adj[start].iter().any(|(t, _)| *t == start);
                if members.len() == 1 && !self_loop {
                    return None;
                }
                Some(self.shortest_cycle(start, &members))
            })
            .collect();
        out.sort_by_key(|c| self.names.iter().position(|n| *n == c.stems[0]));
        out
    }

    /// Shortest cycle through `start` within `members` (BFS over edges in
    /// declaration order, so the result is deterministic).
    fn shortest_cycle(&self, start: usize, members: &BTreeSet<usize>) -> Cycle {
        // parent[n] = (previous node, edge index at previous node)
        let mut parent: Vec<Option<(usize, usize)>> = vec![None; self.names.len()];
        let mut queue = VecDeque::new();
        let mut closing = None;
        for &(t, i) in &self.adj[start] {
            if t == start {
                return Cycle {
                    stems: vec![self.names[start].to_string()],
                    first_edge: i,
                };
            }
            if members.contains(&t) && parent[t].is_none() {
                parent[t] = Some((start, i));
                queue.push_back(t);
            }
        }
        'bfs: while let Some(n) = queue.pop_front() {
            for &(t, i) in &self.adj[n] {
                if t == start {
                    closing = Some(n);
                    break 'bfs;
                }
                if members.contains(&t) && parent[t].is_none() && t != start {
                    parent[t] = Some((n, i));
                    queue.push_back(t);
                }
            }
        }
        let mut path = Vec::new();
        let mut cur = closing.expect("a strongly connected component has a cycle");
        let first_edge = loop {
            path.push(cur);
            let (prev, edge) = parent[cur].expect("BFS parent");
            if prev == start {
                break edge;
            }
            cur = prev;
        };
        path.push(start);
        path.reverse();
        Cycle {
            stems: path.iter().map(|&i| self.names[i].to_string()).collect(),
            first_edge,
        }
    }
}

/// Every hard-edge cycle among all stems (enabled or not). Edges to unknown
/// stems are ignored (they are `UNKNOWN_DEPENDENCY`).
pub fn cycles(ws: &Workspace) -> Vec<Cycle> {
    Hard::new(ws, |_| true).cycles()
}

/// Layers of enabled stems for parallel start: every stem's hard
/// dependencies are in earlier layers; names are sorted within a layer; soft
/// edges and edges to disabled or unknown stems are ignored. A hard cycle is
/// a `CYCLE` error.
pub fn start_order(ws: &Workspace) -> Result<Vec<Vec<String>>, Error> {
    let g = Hard::new(ws, |s| s.enabled);
    let n = g.names.len();
    let mut placed = vec![false; n];
    let mut layers = Vec::new();
    let mut remaining = n;
    while remaining > 0 {
        let mut layer: Vec<usize> = (0..n)
            .filter(|&i| !placed[i] && g.adj[i].iter().all(|(t, _)| placed[*t]))
            .collect();
        if layer.is_empty() {
            let cycle = g
                .cycles()
                .into_iter()
                .next()
                .ok_or_else(|| Error::internal("start order stuck without a cycle"))?;
            return Err(cycle.to_error());
        }
        for &i in &layer {
            placed[i] = true;
        }
        remaining -= layer.len();
        layer.sort_by_key(|&i| g.names[i]);
        layers.push(layer.into_iter().map(|i| g.names[i].to_string()).collect());
    }
    Ok(layers)
}

/// [`start_order`] reversed: dependants stop before their dependencies.
pub fn stop_order(ws: &Workspace) -> Result<Vec<Vec<String>>, Error> {
    let mut layers = start_order(ws)?;
    layers.reverse();
    Ok(layers)
}

/// Method syntax for the ordering functions: `ws.start_order()`.
pub trait WorkspaceGraph {
    /// See [`start_order`].
    fn start_order(&self) -> Result<Vec<Vec<String>>, Error>;
    /// See [`stop_order`].
    fn stop_order(&self) -> Result<Vec<Vec<String>>, Error>;
}

impl WorkspaceGraph for Workspace {
    fn start_order(&self) -> Result<Vec<Vec<String>>, Error> {
        start_order(self)
    }
    fn stop_order(&self) -> Result<Vec<Vec<String>>, Error> {
        stop_order(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::Path;

    use super::*;

    /// A workspace from `(name, [(dep, soft)])`, all process stems.
    pub(crate) fn ws(spec: &[(&str, &[(&str, bool)])]) -> Workspace {
        let mut yaml = String::from("stems:\n");
        for (name, deps) in spec {
            yaml.push_str(&format!("  {name}:\n    type: process\n    depends_on:\n"));
            if deps.is_empty() {
                yaml.pop();
                yaml.push_str(" []\n");
            }
            for (d, soft) in *deps {
                yaml.push_str(&format!("      - {{ stem: {d}, soft: {soft} }}\n"));
            }
        }
        load_str(&yaml)
    }

    pub(crate) fn load_str(yaml: &str) -> Workspace {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stems.yaml"), yaml).unwrap();
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir.path().to_path_buf()),
            cwd: Path::new("/").to_path_buf(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap_or_else(|e| panic!("{e}\n{yaml}"))
        .workspace
    }

    fn layers(w: &Workspace) -> Vec<Vec<String>> {
        start_order(w).unwrap()
    }

    fn l(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|x| x.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn diamond() {
        let w = ws(&[
            ("top", &[("left", false), ("right", false)]),
            ("right", &[("base", false)]),
            ("left", &[("base", false)]),
            ("base", &[]),
        ]);
        assert_eq!(layers(&w), l(&[&["base"], &["left", "right"], &["top"]]));
        let stop = w.stop_order().unwrap();
        assert_eq!(stop[0], ["top"]);
        assert_eq!(stop[2], ["base"]);
    }

    #[test]
    fn chain_and_independent_stems() {
        let w = ws(&[
            ("c", &[("b", false)]),
            ("b", &[("a", false)]),
            ("a", &[]),
            ("z", &[]),
        ]);
        assert_eq!(layers(&w), l(&[&["a", "z"], &["b"], &["c"]]));
        assert!(cycles(&w).is_empty());
    }

    #[test]
    fn soft_edge_cycle_is_allowed_and_ignored_for_ordering() {
        let w = ws(&[("api", &[("worker", false)]), ("worker", &[("api", true)])]);
        assert!(cycles(&w).is_empty());
        assert_eq!(layers(&w), l(&[&["worker"], &["api"]]));
    }

    #[test]
    fn hard_cycle_is_an_error_naming_the_cycle() {
        let w = ws(&[
            ("free", &[]),
            ("api", &[("free", false), ("worker", false)]),
            ("worker", &[("api", false)]),
        ]);
        let cs = cycles(&w);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].display(), "api -> worker -> api");
        assert_eq!(cs[0].first_edge, 1);
        let e = start_order(&w).unwrap_err();
        assert_eq!(e.code, ErrorCode::Cycle);
        assert!(e.message.contains("api -> worker -> api"));
        assert_eq!(e.path.unwrap().to_string(), "stems.api.depends_on");
        assert!(e.hint.unwrap().contains("worker -> api"));
    }

    #[test]
    fn longer_cycles_and_self_loops() {
        let w = ws(&[
            ("a", &[("b", false)]),
            ("b", &[("c", false)]),
            ("c", &[("a", false), ("b", false)]),
            ("s", &[("s", false)]),
        ]);
        let cs = cycles(&w);
        let shown: Vec<String> = cs.iter().map(Cycle::display).collect();
        assert_eq!(shown, ["a -> b -> c -> a", "s -> s"]);
    }

    #[test]
    fn disabled_and_unknown_targets_are_ignored_for_ordering() {
        let w = load_str(
            "stems:\n  a: { type: process, depends_on: [b, ghost] }\n  b: { type: process, enabled: false }\n",
        );
        assert_eq!(layers(&w), l(&[&["a"]]));
    }
}
