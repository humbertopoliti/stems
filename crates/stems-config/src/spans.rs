//! Source-span index: a second parse of every config file with `marked-yaml`
//! mapping each [`ConfigPath`] to where it was (last) written.

use std::collections::HashMap;
use std::path::Path;

use marked_yaml::Node;

use crate::diagnostic::Span;
use crate::path::ConfigPath;

/// `ConfigPath -> Span` for every mapping key and sequence item seen.
/// When several files define the same path, the last merged one wins, which
/// is the file the effective value came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpanIndex {
    map: HashMap<ConfigPath, Span>,
}

impl SpanIndex {
    /// Exact lookup.
    pub fn get(&self, path: &ConfigPath) -> Option<&Span> {
        self.map.get(path)
    }

    /// Lookup of `path`, falling back to its closest indexed ancestor.
    pub fn locate(&self, path: &ConfigPath) -> Option<&Span> {
        let mut p = path.clone();
        loop {
            if let Some(s) = self.map.get(&p) {
                return Some(s);
            }
            if p.is_root() {
                return None;
            }
            p = p.parent();
        }
    }

    /// The deepest path whose span starts at or before `line:col` in `file`
    /// on the same line (used to recover a path from a YAML error location).
    pub fn path_at(&self, file: &Path, line: usize, col: usize) -> Option<ConfigPath> {
        self.map
            .iter()
            .filter(|(_, s)| s.file == file && s.line == line && s.col <= col)
            .max_by_key(|(p, s)| (s.col, p.segments().len()))
            .map(|(p, _)| p.clone())
    }

    /// Add all entries of `other`, overriding existing paths.
    pub fn extend(&mut self, other: SpanIndex) {
        self.map.extend(other.map);
    }

    /// Number of indexed paths.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Index one file's text. Parse failures are ignored (the typed parse
    /// reports them); an empty document indexes nothing.
    pub fn index_file(&mut self, file: &Path, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let Ok(node) = marked_yaml::parse_yaml(0, text) else {
            return;
        };
        self.walk(file, &node, &ConfigPath::root());
    }

    fn insert(&mut self, file: &Path, path: ConfigPath, span: &marked_yaml::Span) {
        if let Some(m) = span.start() {
            self.map.insert(
                path,
                Span {
                    file: file.to_path_buf(),
                    line: m.line(),
                    col: m.column(),
                },
            );
        }
    }

    fn walk(&mut self, file: &Path, node: &Node, path: &ConfigPath) {
        match node {
            Node::Scalar(_) => {}
            Node::Mapping(m) => {
                for (k, v) in m.iter() {
                    let p = path.key(k.as_str());
                    self.insert(file, p.clone(), k.span());
                    self.walk(file, v, &p);
                }
            }
            Node::Sequence(s) => {
                for (i, v) in s.iter().enumerate() {
                    let p = path.index(i);
                    self.insert(file, p.clone(), v.span());
                    self.walk(file, v, &p);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_keys_and_items() {
        let text = "stems:\n  api:\n    depends_on:\n      - db\n      - { stem: cache }\n";
        let mut idx = SpanIndex::default();
        let f = Path::new("/ws/stems.yaml");
        idx.index_file(f, text);
        let p: ConfigPath = "stems.api.depends_on[1].stem".parse().unwrap();
        let s = idx.get(&p).unwrap();
        assert_eq!((s.line, s.col), (5, 11));
        let s = idx.get(&"stems.api".parse().unwrap()).unwrap();
        assert_eq!((s.line, s.col), (2, 3));
        let deep: ConfigPath = "stems.api.depends_on[0].nope".parse().unwrap();
        assert_eq!(idx.locate(&deep).unwrap().line, 4);
        assert_eq!(idx.path_at(f, 5, 12), Some(p));
    }

    #[test]
    fn later_file_wins_and_bad_yaml_is_ignored() {
        let mut idx = SpanIndex::default();
        idx.index_file(Path::new("a.yaml"), "name: a\n");
        idx.index_file(Path::new("b.yaml"), "\n\nname: b\n");
        idx.index_file(Path::new("c.yaml"), "name: [unclosed\n");
        idx.index_file(Path::new("d.yaml"), "");
        let s = idx.get(&"name".parse().unwrap()).unwrap();
        assert_eq!(s.file, Path::new("b.yaml"));
        assert_eq!(s.line, 3);
    }
}
