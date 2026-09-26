//! [`ConfigPath`]: a dotted path into the config tree, e.g.
//! `stems.shop-api.depends_on[1].stem`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One segment of a [`ConfigPath`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Segment {
    /// A mapping key.
    Key(String),
    /// A sequence index.
    Index(usize),
}

/// A path into the (merged) config tree.
///
/// Display form: keys are joined with `.`, indices are written `[n]`. Keys
/// that contain `.`, `[`, `]` or `"` (or are empty) are written as `["key"]`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConfigPath(Vec<Segment>);

impl ConfigPath {
    /// The empty (root) path.
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// Returns a new path with `key` appended.
    #[must_use]
    pub fn key(&self, key: impl Into<String>) -> Self {
        let mut v = self.0.clone();
        v.push(Segment::Key(key.into()));
        Self(v)
    }

    /// Returns a new path with index `i` appended.
    #[must_use]
    pub fn index(&self, i: usize) -> Self {
        let mut v = self.0.clone();
        v.push(Segment::Index(i));
        Self(v)
    }

    /// A path from its segments.
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self(segments)
    }

    /// The segments of this path.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    /// True for the root path.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The parent path (root's parent is root).
    #[must_use]
    pub fn parent(&self) -> Self {
        let mut v = self.0.clone();
        v.pop();
        Self(v)
    }

    /// The mapping key at position `i`, if that segment is a key.
    pub fn key_at(&self, i: usize) -> Option<&str> {
        match self.0.get(i) {
            Some(Segment::Key(k)) => Some(k),
            _ => None,
        }
    }

    /// True if `self` starts with `prefix`.
    pub fn starts_with(&self, prefix: &ConfigPath) -> bool {
        self.0.starts_with(&prefix.0)
    }
}

fn needs_quoting(k: &str) -> bool {
    k.is_empty() || k.contains(['.', '[', ']', '"'])
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.0.iter().enumerate() {
            match seg {
                Segment::Key(k) if needs_quoting(k) => {
                    write!(f, "[\"{}\"]", k.replace('\\', "\\\\").replace('"', "\\\""))?;
                }
                Segment::Key(k) => {
                    if i > 0 {
                        f.write_str(".")?;
                    }
                    f.write_str(k)?;
                }
                Segment::Index(n) => write!(f, "[{n}]")?,
            }
        }
        Ok(())
    }
}

/// Error returned when a string is not a valid [`ConfigPath`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid config path `{input}`: {reason}")]
pub struct ParsePathError {
    input: String,
    reason: &'static str,
}

impl FromStr for ConfigPath {
    type Err = ParsePathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParsePathError {
            input: s.to_string(),
            reason,
        };
        let mut segs = Vec::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        let mut expect_key = true;
        while i < chars.len() {
            match chars[i] {
                '[' => {
                    i += 1;
                    if chars.get(i) == Some(&'"') {
                        i += 1;
                        let mut key = String::new();
                        loop {
                            match chars.get(i) {
                                None => return Err(err("unterminated quoted key")),
                                Some('\\') => {
                                    let c = chars.get(i + 1).ok_or_else(|| err("bad escape"))?;
                                    key.push(*c);
                                    i += 2;
                                }
                                Some('"') => {
                                    i += 1;
                                    break;
                                }
                                Some(c) => {
                                    key.push(*c);
                                    i += 1;
                                }
                            }
                        }
                        if chars.get(i) != Some(&']') {
                            return Err(err("expected `]` after quoted key"));
                        }
                        i += 1;
                        segs.push(Segment::Key(key));
                    } else {
                        let start = i;
                        while i < chars.len() && chars[i] != ']' {
                            i += 1;
                        }
                        if i >= chars.len() {
                            return Err(err("unterminated index"));
                        }
                        let n: String = chars[start..i].iter().collect();
                        let n = n.parse().map_err(|_| err("index is not a number"))?;
                        i += 1;
                        segs.push(Segment::Index(n));
                    }
                    expect_key = false;
                }
                '.' => {
                    if expect_key {
                        return Err(err("empty key"));
                    }
                    i += 1;
                    expect_key = true;
                    if i >= chars.len() {
                        return Err(err("trailing `.`"));
                    }
                }
                _ => {
                    if !expect_key {
                        return Err(err("expected `.` or `[` between segments"));
                    }
                    let start = i;
                    while i < chars.len() && chars[i] != '.' && chars[i] != '[' {
                        i += 1;
                    }
                    segs.push(Segment::Key(chars[start..i].iter().collect()));
                    expect_key = false;
                }
            }
        }
        Ok(Self(segs))
    }
}

impl Serialize for ConfigPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ConfigPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_parse_roundtrip() {
        let p = ConfigPath::root()
            .key("stems")
            .key("shop-api")
            .key("depends_on")
            .index(1)
            .key("stem");
        assert_eq!(p.to_string(), "stems.shop-api.depends_on[1].stem");
        assert_eq!(
            "stems.shop-api.depends_on[1].stem"
                .parse::<ConfigPath>()
                .unwrap(),
            p
        );
    }

    #[test]
    fn quoted_keys_roundtrip() {
        let p = ConfigPath::root().key("stems").key("a.b").key("x");
        assert_eq!(p.to_string(), "stems[\"a.b\"].x");
        assert_eq!(p.to_string().parse::<ConfigPath>().unwrap(), p);
        let q = ConfigPath::root().key("we\"ird");
        assert_eq!(q.to_string().parse::<ConfigPath>().unwrap(), q);
    }

    #[test]
    fn root_and_leading_index() {
        assert_eq!("".parse::<ConfigPath>().unwrap(), ConfigPath::root());
        assert_eq!(ConfigPath::root().index(0).to_string(), "[0]");
        assert_eq!(
            "[0][2]".parse::<ConfigPath>().unwrap(),
            ConfigPath::root().index(0).index(2)
        );
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["a..b", ".a", "a.", "a[x]", "a[1", "a[1]b", "[\"x"] {
            assert!(bad.parse::<ConfigPath>().is_err(), "{bad} should fail");
        }
    }
}
