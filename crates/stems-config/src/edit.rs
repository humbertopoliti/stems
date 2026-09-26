//! Comment-preserving edits of a YAML file by config path (`stems config
//! set/unset`, FR-CL-5). DECISIONS.md: "Config edits that must preserve
//! comments: targeted line-level editing of the YAML text, not a round-trip
//! through a YAML model".
//!
//! The editor understands block mappings line by line:
//!
//! - an existing key whose value is on its line (`port: 1  # note`) gets the
//!   new value in place; the key, its indentation and any trailing comment
//!   are kept;
//! - an existing key with a block value (nested lines) gets the new value
//!   on its line and the nested lines are removed;
//! - missing keys are created at the end of their parent block, nested one
//!   indentation step (the file's own step, else two spaces) per level;
//! - every other line (comments, blank lines, unrelated keys) is untouched.
//!
//! Where the path goes *through* a value that is not a block mapping (a
//! flow collection `{a: 1}`, a list, an index segment such as `ports[0]`),
//! that one value is edited as data and rewritten in flow style on its key
//! line (comments inside it, if any, are lost; the rest of the file is not
//! touched). A missing list can be seeded from `base` (the committed value)
//! so `ports[0].port` edits a copy of the committed ports.
//!
//! Values are YAML: a scalar (`false`, `18080`, `"0"`, `abc`) or a flow
//! collection (`[a, b]`, `{ stem: db }`) on one line.

use std::path::Path;

use serde_yaml_ng::{Mapping, Value};

use crate::path::{ConfigPath, Segment};

/// An edit that cannot be made (bad path, unparsable file or value).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct EditError(pub String);

fn err(msg: impl Into<String>) -> EditError {
    EditError(msg.into())
}

// ---------------------------------------------------------------------------
// Line model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Kind {
    Blank,
    Comment,
    /// `key:` with the byte offset just after the colon, the inline value
    /// range (without trailing spaces) and where a trailing comment starts.
    Key {
        key: String,
        colon_end: usize,
        value: Option<(usize, usize)>,
        comment: Option<usize>,
    },
    /// Sequence items, continuation lines, `---`, anything else.
    Other,
}

#[derive(Clone, Debug)]
struct Line {
    text: String,
    indent: usize,
    kind: Kind,
}

impl Line {
    fn parse(text: &str) -> Line {
        let indent = text.len() - text.trim_start_matches(' ').len();
        let body = &text[indent..];
        let kind = if body.trim().is_empty() {
            Kind::Blank
        } else if body.starts_with('#') {
            Kind::Comment
        } else {
            key_line(text, indent).unwrap_or(Kind::Other)
        };
        Line {
            text: text.to_string(),
            indent,
            kind,
        }
    }

    fn is_content(&self) -> bool {
        !matches!(self.kind, Kind::Blank | Kind::Comment)
    }

    fn key(&self) -> Option<&str> {
        match &self.kind {
            Kind::Key { key, .. } => Some(key),
            _ => None,
        }
    }

    fn inline_value(&self) -> Option<&str> {
        match &self.kind {
            Kind::Key {
                value: Some((a, b)),
                ..
            } => Some(&self.text[*a..*b]),
            _ => None,
        }
    }
}

/// End of a quoted scalar starting at `start` (byte index of the quote),
/// returning the index just past the closing quote.
fn quoted_end(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let q = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if q == b'"' && c == b'\\' {
            i += 2;
            continue;
        }
        if c == q {
            if q == b'\'' && bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Where a trailing comment starts in `s[from..]` (a `#` after whitespace,
/// outside quotes), as an absolute index.
fn comment_start(s: &str, from: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = from;
    // A quote opens a quoted scalar only at a value start or after a flow
    // indicator (`[`, `{`, `,`, `:`) and spaces.
    let mut may_open = true;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' | b'\'' if may_open => {
                i = quoted_end(s, i).unwrap_or(bytes.len());
                may_open = false;
                continue;
            }
            b'#' if i == from || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' => {
                return Some(i);
            }
            b'[' | b'{' | b',' | b':' => may_open = true,
            b' ' | b'\t' => {}
            _ => may_open = false,
        }
        i += 1;
    }
    None
}

fn key_line(text: &str, indent: usize) -> Option<Kind> {
    let body = &text[indent..];
    if body.starts_with("- ") || body == "-" || body.starts_with("---") || body.starts_with("...") {
        return None;
    }
    let (key, colon) = if body.starts_with('"') || body.starts_with('\'') {
        let end = quoted_end(text, indent)?;
        let key: String = serde_yaml_ng::from_str(&text[indent..end]).ok()?;
        if text.as_bytes().get(end) != Some(&b':') {
            return None;
        }
        (key, end)
    } else {
        let bytes = text.as_bytes();
        let mut i = indent;
        let mut found = None;
        while i < bytes.len() {
            match bytes[i] {
                b':' if matches!(bytes.get(i + 1), None | Some(b' ') | Some(b'\t')) => {
                    found = Some(i);
                    break;
                }
                b'#' if i > indent && bytes[i - 1] == b' ' => return None,
                b'[' | b'{' | b'"' | b'\'' if i == indent => return None,
                _ => {}
            }
            i += 1;
        }
        let i = found?;
        (text[indent..i].trim_end().to_string(), i)
    };
    let colon_end = colon + 1;
    let rest = &text[colon_end..];
    let start = colon_end + (rest.len() - rest.trim_start().len());
    let comment = comment_start(text, start);
    let end = comment.unwrap_or(text.len());
    let value_end = start + text[start..end].trim_end().len();
    let value = (value_end > start).then_some((start, value_end));
    Some(Kind::Key {
        key,
        colon_end,
        value,
        comment,
    })
}

struct Doc {
    lines: Vec<Line>,
    trailing_newline: bool,
}

impl Doc {
    fn parse(text: &str) -> Doc {
        Doc {
            lines: text.lines().map(Line::parse).collect(),
            trailing_newline: text.is_empty() || text.ends_with('\n'),
        }
    }

    fn render(&self) -> String {
        let mut s = self
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if self.trailing_newline && !s.is_empty() {
            s.push('\n');
        }
        s
    }

    /// The indentation step of the file (smallest positive difference
    /// between a key and its first nested key), default 2.
    fn unit(&self) -> usize {
        let mut best: Option<usize> = None;
        for (i, l) in self.lines.iter().enumerate() {
            if l.key().is_none() || l.inline_value().is_some() {
                continue;
            }
            if let Some(n) = self.lines[i + 1..].iter().find(|n| n.is_content())
                && n.indent > l.indent
            {
                let d = n.indent - l.indent;
                best = Some(best.map_or(d, |b| b.min(d)));
            }
        }
        best.unwrap_or(2)
    }

    /// Lines nested under key line `i`: `(i+1)..end`, without trailing
    /// blank/comment lines.
    fn block(&self, i: usize) -> std::ops::Range<usize> {
        let k = self.lines[i].indent;
        let mut end = i + 1;
        let mut last_content = i + 1;
        while end < self.lines.len() {
            let l = &self.lines[end];
            let same_level_item =
                l.indent == k && matches!(l.kind, Kind::Other) && l.text[k..].starts_with('-');
            if l.is_content() && l.indent <= k && !same_level_item {
                break;
            }
            end += 1;
            if l.is_content() {
                last_content = end;
            }
        }
        (i + 1)..last_content.max(i + 1)
    }

    /// Content lines of a range at the indentation of its first content line.
    fn children(&self, range: std::ops::Range<usize>) -> (Option<usize>, Vec<usize>) {
        let first = range.clone().find(|&j| self.lines[j].is_content());
        let Some(first) = first else {
            return (None, Vec::new());
        };
        let ind = self.lines[first].indent;
        let kids = range
            .filter(|&j| self.lines[j].is_content() && self.lines[j].indent == ind)
            .collect();
        (Some(ind), kids)
    }

    fn find_key(
        &self,
        range: std::ops::Range<usize>,
        key: &str,
    ) -> Result<Option<usize>, EditError> {
        let (_, kids) = self.children(range);
        if let Some(&j) = kids.first()
            && self.lines[j].key().is_none()
        {
            return Err(err(format!(
                "line {} is not a `key: value` mapping entry, cannot edit it by path",
                j + 1
            )));
        }
        Ok(kids.into_iter().find(|&j| self.lines[j].key() == Some(key)))
    }

    /// The value of key line `i` as data (inline or its block).
    fn value_of(&self, i: usize) -> Result<Value, EditError> {
        if let Some(v) = self.lines[i].inline_value() {
            return serde_yaml_ng::from_str(v)
                .map_err(|e| err(format!("line {}: cannot parse `{v}`: {e}", i + 1)));
        }
        let r = self.block(i);
        if r.is_empty() {
            return Ok(Value::Null);
        }
        let ind = self.children(r.clone()).0.unwrap_or(0);
        let text: String = self.lines[r]
            .iter()
            .map(|l| {
                let cut = l.text.len().min(ind).min(l.indent);
                format!("{}\n", &l.text[cut..])
            })
            .collect();
        serde_yaml_ng::from_str(&text)
            .map_err(|e| err(format!("cannot parse the value of line {}: {e}", i + 1)))
    }

    /// Put `flow` on key line `i` (keeping the comment) and drop its block.
    fn replace_value(&mut self, i: usize, flow: &str) {
        let block = self.block(i);
        let l = &self.lines[i];
        let Kind::Key {
            colon_end,
            value,
            comment,
            ..
        } = &l.kind
        else {
            return;
        };
        let text = match (value, comment) {
            (Some((a, b)), _) => format!("{}{flow}{}", &l.text[..*a], &l.text[*b..]),
            (None, Some(c)) => {
                format!(
                    "{} {flow}{}",
                    &l.text[..*colon_end],
                    &l.text[*colon_end..*c]
                ) + &l.text[*c..]
            }
            (None, None) => format!("{} {flow}", &l.text[..*colon_end]),
        };
        let indent = l.indent;
        self.lines[i] = Line::parse(&text);
        debug_assert_eq!(self.lines[i].indent, indent);
        // Keep full-line comments of the removed block only if they are
        // not nested content (they would describe values that are gone).
        self.lines.drain(block);
    }

    fn insert(&mut self, at: usize, texts: Vec<String>) {
        let new: Vec<Line> = texts.iter().map(|t| Line::parse(t)).collect();
        self.lines.splice(at..at, new);
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Parse a command-line value as YAML (scalar or flow collection).
pub fn parse_value(text: &str) -> Result<Value, EditError> {
    serde_yaml_ng::from_str(text).map_err(|e| err(format!("`{text}` is not a YAML value: {e}")))
}

const INDICATORS: &[char] = &[
    ',', '[', ']', '{', '}', '#', ':', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`', '\n',
    '\t',
];

fn plain_ok(s: &str) -> bool {
    !s.is_empty()
        && s.trim() == s
        && !s.contains(INDICATORS)
        && serde_yaml_ng::from_str::<Value>(s).ok() == Some(Value::String(s.to_string()))
}

/// One-line flow-style YAML for a value.
pub fn to_flow(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if plain_ok(s) => s.clone(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}")),
        Value::Sequence(items) => format!(
            "[{}]",
            items.iter().map(to_flow).collect::<Vec<_>>().join(", ")
        ),
        Value::Mapping(m) if m.is_empty() => "{}".into(),
        Value::Mapping(m) => format!(
            "{{ {} }}",
            m.iter()
                .map(|(k, v)| format!("{}: {}", to_flow(k), to_flow(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Tagged(t) => format!("{} {}", t.tag, to_flow(&t.value)),
    }
}

fn key_text(k: &str) -> String {
    if plain_ok(k) {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_else(|_| format!("{k:?}"))
    }
}

/// Set `rest` below `v` to `new` (creating mappings/list items as needed).
fn set_in(v: &mut Value, rest: &[Segment], new: Value) -> Result<(), EditError> {
    let Some((first, tail)) = rest.split_first() else {
        *v = new;
        return Ok(());
    };
    match first {
        Segment::Key(k) => {
            if v.is_null() {
                *v = Value::Mapping(Mapping::new());
            }
            let Value::Mapping(m) = v else {
                return Err(err(format!(
                    "cannot set key `{k}`: the value there is not a mapping"
                )));
            };
            let key = Value::String(k.clone());
            if !m.contains_key(&key) {
                m.insert(key.clone(), Value::Null);
            }
            set_in(m.get_mut(&key).expect("inserted"), tail, new)
        }
        Segment::Index(i) => {
            if v.is_null() {
                *v = Value::Sequence(Vec::new());
            }
            let Value::Sequence(s) = v else {
                return Err(err(format!(
                    "cannot set item [{i}]: the value there is not a list"
                )));
            };
            if *i == s.len() {
                s.push(Value::Null);
            }
            let len = s.len();
            let item = s.get_mut(*i).ok_or_else(|| {
                err(format!(
                    "index [{i}] is out of range (the list has {len} items)"
                ))
            })?;
            set_in(item, tail, new)
        }
    }
}

/// Remove `rest` below `v`; true if something was removed.
fn unset_in(v: &mut Value, rest: &[Segment]) -> bool {
    match rest {
        [] => false,
        [Segment::Key(k)] => v
            .as_mapping_mut()
            .is_some_and(|m| m.remove(Value::String(k.clone())).is_some()),
        [Segment::Index(i)] => match v.as_sequence_mut() {
            Some(s) if *i < s.len() => {
                s.remove(*i);
                true
            }
            _ => false,
        },
        [first, tail @ ..] => {
            let child = match first {
                Segment::Key(k) => v.get_mut(k.as_str()),
                Segment::Index(i) => v.get_mut(*i),
            };
            child.is_some_and(|c| unset_in(c, tail))
        }
    }
}

fn navigate<'v>(v: &'v Value, segs: &[Segment]) -> Option<&'v Value> {
    segs.iter().try_fold(v, |cur, s| match s {
        Segment::Key(k) => cur.get(k.as_str()),
        Segment::Index(i) => cur.get(*i),
    })
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// The value at `path` in `text` (`None` when absent).
pub fn get(text: &str, path: &ConfigPath) -> Result<Option<Value>, EditError> {
    let root: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_yaml_ng::from_str(text).map_err(|e| err(format!("cannot parse the file: {e}")))?
    };
    Ok(navigate(&root, path.segments()).cloned())
}

fn check_path(path: &ConfigPath) -> Result<(), EditError> {
    match path.segments().first() {
        None => Err(err("the path is empty")),
        Some(Segment::Index(_)) => {
            Err(err("the top level is a mapping: start the path with a key"))
        }
        Some(Segment::Key(_)) => Ok(()),
    }
}

/// Set `path` to the YAML `value_text` in `text`. `base(prefix)` supplies
/// the committed value of a list or mapping that has to be copied before
/// editing into it (index segments into a list the file does not have).
pub fn set(
    text: &str,
    path: &ConfigPath,
    value_text: &str,
    base: &dyn Fn(&ConfigPath) -> Option<Value>,
) -> Result<String, EditError> {
    check_path(path)?;
    let value = parse_value(value_text)?;
    let flow = if value_text.contains('\n') || value_text.trim().is_empty() {
        to_flow(&value)
    } else {
        value_text.trim().to_string()
    };
    let segs = path.segments();
    let mut doc = Doc::parse(text);
    let unit = doc.unit();
    let mut range = 0..doc.lines.len();
    let mut parent: Option<usize> = None;
    let mut depth = 0;
    loop {
        let Segment::Key(k) = &segs[depth] else {
            unreachable!("index segments are handled at their key line")
        };
        let prefix = ConfigPath::from_segments(segs[..=depth].to_vec());
        match doc.find_key(range.clone(), k)? {
            Some(i) if depth + 1 == segs.len() => {
                doc.replace_value(i, &flow);
                return Ok(doc.render());
            }
            Some(i) => {
                let next_is_index = matches!(segs[depth + 1], Segment::Index(_));
                if next_is_index || doc.lines[i].inline_value().is_some() {
                    let mut cur = doc.value_of(i)?;
                    if cur.is_null() {
                        cur = base(&prefix).unwrap_or(Value::Null);
                    }
                    set_in(&mut cur, &segs[depth + 1..], value)?;
                    doc.replace_value(i, &to_flow(&cur));
                    return Ok(doc.render());
                }
                parent = Some(i);
                range = doc.block(i);
                depth += 1;
            }
            None => {
                // Create segs[depth..] at the end of the parent block.
                let (child_indent, at) = match parent {
                    None => (0, last_content_end(&doc, range.clone(), doc.lines.len())),
                    Some(p) => {
                        let ind = doc
                            .children(range.clone())
                            .0
                            .unwrap_or(doc.lines[p].indent + unit);
                        (ind, last_content_end(&doc, range.clone(), p + 1))
                    }
                };
                let mut out = Vec::new();
                let mut ind = child_indent;
                let mut d = depth;
                loop {
                    let Segment::Key(k) = &segs[d] else {
                        unreachable!()
                    };
                    let pad = " ".repeat(ind);
                    if d + 1 == segs.len() {
                        out.push(format!("{pad}{}: {flow}", key_text(k)));
                        break;
                    }
                    if matches!(segs[d + 1], Segment::Index(_)) {
                        let p = ConfigPath::from_segments(segs[..=d].to_vec());
                        let mut cur = base(&p).unwrap_or(Value::Null);
                        set_in(&mut cur, &segs[d + 1..], value)?;
                        out.push(format!("{pad}{}: {}", key_text(k), to_flow(&cur)));
                        break;
                    }
                    out.push(format!("{pad}{}:", key_text(k)));
                    ind += unit;
                    d += 1;
                }
                doc.insert(at, out);
                if !doc.trailing_newline {
                    doc.trailing_newline = true;
                }
                return Ok(doc.render());
            }
        }
    }
}

/// Index just past the last content line of `range` (or `fallback` when
/// the range has none).
fn last_content_end(doc: &Doc, range: std::ops::Range<usize>, fallback: usize) -> usize {
    range
        .rev()
        .find(|&j| doc.lines[j].is_content())
        .map_or(fallback.min(doc.lines.len()), |j| j + 1)
}

/// Remove `path` from `text`. Parents left empty are removed too (an empty
/// `stems.web:` would otherwise *clear* the stem when merged). Returns the
/// new text and whether anything was removed.
pub fn unset(text: &str, path: &ConfigPath) -> Result<(String, bool), EditError> {
    check_path(path)?;
    let segs = path.segments();
    let mut doc = Doc::parse(text);
    let mut range = 0..doc.lines.len();
    let mut chain: Vec<usize> = Vec::new();
    let mut depth = 0;
    loop {
        let Segment::Key(k) = &segs[depth] else {
            unreachable!()
        };
        let Some(i) = doc.find_key(range.clone(), k)? else {
            return Ok((text.to_string(), false));
        };
        if depth + 1 == segs.len() {
            let block = doc.block(i);
            doc.lines.drain(i..block.end);
            break;
        }
        let next_is_index = matches!(segs[depth + 1], Segment::Index(_));
        if next_is_index || doc.lines[i].inline_value().is_some() {
            let mut cur = doc.value_of(i)?;
            if !unset_in(&mut cur, &segs[depth + 1..]) {
                return Ok((text.to_string(), false));
            }
            doc.replace_value(i, &to_flow(&cur));
            return Ok((doc.render(), true));
        }
        chain.push(i);
        range = doc.block(i);
        depth += 1;
    }
    // Prune parents that no longer have any content, innermost first.
    while let Some(p) = chain.pop() {
        if doc.lines[p].inline_value().is_some() || !doc.block(p).is_empty() {
            break;
        }
        doc.lines.remove(p);
    }
    Ok((doc.render(), true))
}

/// `-old` / `+new` lines between two texts (line-level LCS).
pub fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push(format!("-{}", a[i]));
            i += 1;
        } else {
            out.push(format!("+{}", b[j]));
            j += 1;
        }
    }
    out
}

/// Outcome of [`write_checked`] when the new text is not kept.
#[derive(Debug)]
pub enum WriteError<E> {
    /// Reading or writing the file failed.
    Io(std::io::Error),
    /// `check` rejected the new content; the original is restored.
    Rejected(E),
}

/// Write `new` to `file` (atomically), run `check` (which re-loads and
/// validates the workspace from disk), and restore the original content
/// (or remove the file if it did not exist) when `check` fails.
pub fn write_checked<E>(
    file: &Path,
    new: &str,
    check: impl FnOnce() -> Result<(), E>,
) -> Result<(), WriteError<E>> {
    let original = match std::fs::read_to_string(file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(WriteError::Io(e)),
    };
    write_atomic(file, new).map_err(WriteError::Io)?;
    if let Err(e) = check() {
        let restored = match &original {
            Some(t) => write_atomic(file, t),
            None => std::fs::remove_file(file),
        };
        restored.map_err(WriteError::Io)?;
        return Err(WriteError::Rejected(e));
    }
    Ok(())
}

fn write_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "stems.local.yaml".into());
    let tmp = file.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> ConfigPath {
        s.parse().unwrap()
    }

    fn no_base(_: &ConfigPath) -> Option<Value> {
        None
    }

    fn set_ok(text: &str, path: &str, v: &str) -> String {
        set(text, &p(path), v, &no_base).unwrap()
    }

    #[test]
    fn replaces_a_scalar_keeping_its_trailing_comment() {
        let text = "# local overrides\nstems:\n  echo-svc:\n    enabled: true   # keep me\n    env:\n      A: \"1\"\n";
        assert_eq!(
            set_ok(text, "stems.echo-svc.enabled", "false"),
            "# local overrides\nstems:\n  echo-svc:\n    enabled: false   # keep me\n    env:\n      A: \"1\"\n"
        );
        assert_eq!(
            set_ok(text, "stems.echo-svc.env.A", "'x # y'"),
            "# local overrides\nstems:\n  echo-svc:\n    enabled: true   # keep me\n    env:\n      A: 'x # y'\n"
        );
    }

    #[test]
    fn creates_nested_keys_at_the_end_of_the_parent_block() {
        let text = "# top comment\nstems:\n  # about echo\n  echo-svc:\n    env:\n      A: \"1\"\n\n# trailing comment for vars\nvars:\n  x: 1\n";
        assert_eq!(
            set_ok(text, "stems.echo-svc.enabled", "false"),
            "# top comment\nstems:\n  # about echo\n  echo-svc:\n    env:\n      A: \"1\"\n    enabled: false\n\n# trailing comment for vars\nvars:\n  x: 1\n"
        );
        assert_eq!(
            set_ok(text, "stems.web.ports", "[{ port: 1 }]"),
            "# top comment\nstems:\n  # about echo\n  echo-svc:\n    env:\n      A: \"1\"\n  web:\n    ports: [{ port: 1 }]\n\n# trailing comment for vars\nvars:\n  x: 1\n"
        );
        assert_eq!(
            set_ok(text, "profile", "backend"),
            format!("{text}profile: backend\n")
        );
    }

    #[test]
    fn creates_the_file_content_from_nothing_and_uses_the_file_indent() {
        assert_eq!(
            set_ok("", "stems.api.enabled", "false"),
            "stems:\n  api:\n    enabled: false\n"
        );
        let four = "stems:\n    api:\n        env:\n            A: 1\n";
        assert_eq!(
            set_ok(four, "stems.web.enabled", "false"),
            "stems:\n    api:\n        env:\n            A: 1\n    web:\n        enabled: false\n"
        );
        assert_eq!(
            set_ok("# only a comment\n", "profile", "x"),
            "# only a comment\nprofile: x\n"
        );
    }

    #[test]
    fn replaces_a_block_value_and_edits_flow_values_as_data() {
        let text =
            "stems:\n  api:\n    env:  # the env\n      A: 1\n      B: 2\n    enabled: true\n";
        assert_eq!(
            set_ok(text, "stems.api.env", "{ C: 3 }"),
            "stems:\n  api:\n    env: { C: 3 }  # the env\n    enabled: true\n"
        );
        let flow = "stems:\n  api: { enabled: true, env: { A: 1 } }  # inline\n";
        assert_eq!(
            set_ok(flow, "stems.api.env.B", "two words"),
            "stems:\n  api: { enabled: true, env: { A: 1, B: two words } }  # inline\n"
        );
    }

    #[test]
    fn index_segments_copy_the_committed_list() {
        let base = |q: &ConfigPath| {
            (q.to_string() == "stems.api.ports")
                .then(|| serde_yaml_ng::from_str("[{ name: http, port: 18090 }]").unwrap())
        };
        let text = "stems:\n  api:\n    enabled: true\n";
        assert_eq!(
            set(text, &p("stems.api.ports[0].port"), "18091", &base).unwrap(),
            "stems:\n  api:\n    enabled: true\n    ports: [{ name: http, port: 18091 }]\n"
        );
        let text = "stems:\n  api:\n    ports:\n      - { name: http, port: 1 }\n";
        assert_eq!(
            set(text, &p("stems.api.ports[0].port"), "2", &base).unwrap(),
            "stems:\n  api:\n    ports: [{ name: http, port: 2 }]\n"
        );
        assert!(set(text, &p("stems.api.ports[5].port"), "2", &base).is_err());
    }

    #[test]
    fn unset_removes_the_key_and_empty_parents() {
        let text = "# c1\nstems:\n  api:\n    enabled: false  # off\n  web:\n    env:\n      A: 1\n# c2\nprofile: x\n";
        let (t, removed) = unset(text, &p("stems.api.enabled")).unwrap();
        assert!(removed);
        assert_eq!(
            t,
            "# c1\nstems:\n  web:\n    env:\n      A: 1\n# c2\nprofile: x\n"
        );
        let (t, removed) = unset(text, &p("stems.web.env.A")).unwrap();
        assert!(removed);
        assert_eq!(
            t,
            "# c1\nstems:\n  api:\n    enabled: false  # off\n# c2\nprofile: x\n"
        );
        let (t, removed) = unset(text, &p("stems.nope.enabled")).unwrap();
        assert!(!removed);
        assert_eq!(t, text);
    }

    #[test]
    fn get_reads_values() {
        let text = "stems:\n  api:\n    enabled: false\n    ports: [{ port: 1 }]\n";
        assert_eq!(
            get(text, &p("stems.api.enabled")).unwrap(),
            Some(Value::Bool(false))
        );
        assert_eq!(
            get(text, &p("stems.api.ports[0].port")).unwrap(),
            Some(serde_yaml_ng::from_str("1").unwrap())
        );
        assert_eq!(get(text, &p("stems.web")).unwrap(), None);
        assert_eq!(get("", &p("a")).unwrap(), None);
    }

    #[test]
    fn values_render_as_flow() {
        let v: Value =
            serde_yaml_ng::from_str("{ a: [1, 'x y', 'true', 'a: b'], b: null }").unwrap();
        assert_eq!(to_flow(&v), r#"{ a: [1, x y, "true", "a: b"], b: null }"#);
        assert!(parse_value("[unclosed").is_err());
    }

    #[test]
    fn diff_shows_changed_lines() {
        assert_eq!(diff_lines("a\nb\nc\n", "a\nB\nc\nd\n"), ["-b", "+B", "+d"]);
    }

    #[test]
    fn write_checked_restores_on_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("stems.local.yaml");
        std::fs::write(&f, "# keep\n").unwrap();
        let r = write_checked(&f, "bad\n", || Err::<(), _>("nope"));
        assert!(matches!(r, Err(WriteError::Rejected("nope"))));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "# keep\n");
        let g = dir.path().join("new.yaml");
        let r = write_checked(&g, "x: 1\n", || Err::<(), _>("no"));
        assert!(r.is_err());
        assert!(!g.exists());
        write_checked(&g, "x: 1\n", || Ok::<(), ()>(())).unwrap();
        assert_eq!(std::fs::read_to_string(&g).unwrap(), "x: 1\n");
    }
}
