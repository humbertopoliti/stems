//! Tool versions and `requires:` ranges.
//!
//! Versions are `MAJOR[.MINOR[.PATCH]]` (pre-release/build suffixes are
//! ignored). Ranges are space-separated comparators that must all hold:
//! `>=`, `>`, `<=`, `<`, `=` (or a bare version), `^`, `~`, `*`/`x`, e.g.
//! `">=20"`, `"^1.2"`, `"~3.11"`, `">=1 <2"`. Partial versions follow npm:
//! `>1.2` means `>=1.3.0`, `<=1.2` means `<1.3.0`, `=1.2` means `1.2.x`.

use std::fmt;
use std::str::FromStr;
use std::sync::LazyLock;

use regex::Regex;

/// A `MAJOR.MINOR.PATCH` version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// Major.
    pub major: u64,
    /// Minor.
    pub minor: u64,
    /// Patch.
    pub patch: u64,
}

impl Version {
    /// Constructor.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)(?:\.(\d+))?(?:\.(\d+))?").expect("valid version regex"));

/// The first `N[.N[.N]]` number in `text` (e.g. `v24.6.0`, `Python 3.13.7`,
/// `go version go1.22.1 darwin/amd64`).
pub fn find_version(text: &str) -> Option<Version> {
    parse_partial(VERSION_RE.captures(text)?.get(0)?.as_str()).map(|p| p.full())
}

/// A possibly partial version as written in a range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Partial {
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
}

impl Partial {
    fn full(self) -> Version {
        Version::new(self.major, self.minor.unwrap_or(0), self.patch.unwrap_or(0))
    }
    /// The first version after every version matching this partial one
    /// (`1.2` -> `1.3.0`, `1` -> `2.0.0`, `1.2.3` -> `1.2.4`).
    fn next(self) -> Version {
        match (self.minor, self.patch) {
            (None, _) => Version::new(self.major + 1, 0, 0),
            (Some(m), None) => Version::new(self.major, m + 1, 0),
            (Some(m), Some(p)) => Version::new(self.major, m, p + 1),
        }
    }
}

fn parse_partial(s: &str) -> Option<Partial> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    // Drop pre-release / build metadata.
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let num = |p: Option<&str>| -> Result<Option<u64>, ()> {
        match p {
            None => Ok(None),
            Some("x" | "X" | "*") => Ok(None),
            Some(n) => n.parse().map(Some).map_err(|_| ()),
        }
    };
    let major: u64 = parts.next()?.parse().ok()?;
    let minor = num(parts.next()).ok()?;
    let patch = if minor.is_some() {
        num(parts.next()).ok()?
    } else {
        None
    };
    if parts.next().is_some() {
        return None;
    }
    Some(Partial {
        major,
        minor,
        patch,
    })
}

/// One normalised bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    Ge(Version),
    Gt(Version),
    Lt(Version),
    Le(Version),
}

impl Bound {
    fn matches(self, v: Version) -> bool {
        match self {
            Bound::Ge(b) => v >= b,
            Bound::Gt(b) => v > b,
            Bound::Lt(b) => v < b,
            Bound::Le(b) => v <= b,
        }
    }
}

/// A parsed `requires:` range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionReq {
    text: String,
    bounds: Vec<Bound>,
}

/// A range that cannot be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid version range `{range}`: {reason}")]
pub struct VersionReqError {
    /// The range as written.
    pub range: String,
    /// What is wrong.
    pub reason: String,
}

impl VersionReq {
    /// True if `v` satisfies every comparator.
    pub fn matches(&self, v: Version) -> bool {
        self.bounds.iter().all(|b| b.matches(v))
    }

    /// The range as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

const OPS: [&str; 7] = [">=", "<=", ">", "<", "=", "^", "~"];

fn split_op(tok: &str) -> (&str, &str) {
    for op in OPS {
        if let Some(rest) = tok.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", tok)
}

impl FromStr for VersionReq {
    type Err = VersionReqError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason: String| VersionReqError {
            range: s.to_string(),
            reason,
        };
        // Tokenise, joining a bare operator with the following version
        // (`>= 3.9` == `>=3.9`); commas are accepted as separators.
        let mut toks: Vec<String> = Vec::new();
        let mut pending_op: Option<String> = None;
        for raw in s.split(|c: char| c.is_whitespace() || c == ',') {
            if raw.is_empty() {
                continue;
            }
            let (op, rest) = split_op(raw);
            if rest.is_empty() && !op.is_empty() {
                if pending_op.is_some() {
                    return Err(err(format!("two operators in a row near `{raw}`")));
                }
                pending_op = Some(op.to_string());
                continue;
            }
            match pending_op.take() {
                Some(op) => toks.push(format!("{op}{raw}")),
                None => toks.push(raw.to_string()),
            }
        }
        if let Some(op) = pending_op {
            return Err(err(format!("operator `{op}` without a version")));
        }
        if toks.is_empty() {
            return Err(err("empty range".into()));
        }
        let mut bounds = Vec::new();
        for tok in &toks {
            let (op, rest) = split_op(tok);
            if matches!(rest, "*" | "x" | "X") && op.is_empty() {
                continue;
            }
            let p = parse_partial(rest).ok_or_else(|| {
                err(format!(
                    "`{tok}` is not a comparator; expected e.g. `>=20`, `^1.2`, `~3.11`, `>=1 <2`"
                ))
            })?;
            let exact = p.minor.is_some() && p.patch.is_some();
            match op {
                ">=" => bounds.push(Bound::Ge(p.full())),
                ">" if exact => bounds.push(Bound::Gt(p.full())),
                ">" => bounds.push(Bound::Ge(p.next())),
                "<" => bounds.push(Bound::Lt(p.full())),
                "<=" if exact => bounds.push(Bound::Le(p.full())),
                "<=" => bounds.push(Bound::Lt(p.next())),
                "=" | "" => {
                    bounds.push(Bound::Ge(p.full()));
                    if exact {
                        bounds.push(Bound::Le(p.full()));
                    } else {
                        bounds.push(Bound::Lt(p.next()));
                    }
                }
                "~" => {
                    bounds.push(Bound::Ge(p.full()));
                    let upper = match p.minor {
                        None => Version::new(p.major + 1, 0, 0),
                        Some(m) => Version::new(p.major, m + 1, 0),
                    };
                    bounds.push(Bound::Lt(upper));
                }
                "^" => {
                    bounds.push(Bound::Ge(p.full()));
                    let upper = match (p.major, p.minor, p.patch) {
                        (0, None, _) => Version::new(1, 0, 0),
                        (0, Some(0), None) => Version::new(0, 1, 0),
                        (0, Some(0), Some(z)) => Version::new(0, 0, z + 1),
                        (0, Some(m), _) => Version::new(0, m + 1, 0),
                        (x, _, _) => Version::new(x + 1, 0, 0),
                    };
                    bounds.push(Bound::Lt(upper));
                }
                _ => unreachable!("split_op only returns known operators"),
            }
        }
        Ok(Self {
            text: s.trim().to_string(),
            bounds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        find_version(s).unwrap()
    }

    fn ok(range: &str, version: &str) -> bool {
        range.parse::<VersionReq>().unwrap().matches(v(version))
    }

    #[test]
    fn finds_versions_in_tool_output() {
        assert_eq!(v("v24.6.0"), Version::new(24, 6, 0));
        assert_eq!(v("Python 3.13.7"), Version::new(3, 13, 7));
        assert_eq!(
            v("go version go1.22.1 darwin/amd64"),
            Version::new(1, 22, 1)
        );
        assert_eq!(
            v("Docker version 27.0.3, build 7d4bcd8"),
            Version::new(27, 0, 3)
        );
        assert_eq!(
            v("cargo 1.96.1 (356927216 2026-06-26)"),
            Version::new(1, 96, 1)
        );
        assert_eq!(
            v("git version 2.50.1 (Apple Git-155)"),
            Version::new(2, 50, 1)
        );
        assert_eq!(v("9.12"), Version::new(9, 12, 0));
        assert_eq!(find_version("no digits here"), None);
    }

    #[test]
    fn comparators() {
        assert!(ok(">=3.9", "3.13.7"));
        assert!(ok(">= 3.9", "3.9.0"));
        assert!(!ok(">=3.9", "3.8.18"));
        assert!(!ok(">=99", "24.6.0"));
        assert!(ok(">20", "21.0.0"));
        assert!(!ok(">20", "20.9.9"));
        assert!(ok(">1.2.3", "1.2.4"));
        assert!(!ok(">1.2.3", "1.2.3"));
        assert!(ok("<2", "1.99.0"));
        assert!(!ok("<2", "2.0.0"));
        assert!(ok("<=1.2", "1.2.9"));
        assert!(!ok("<=1.2", "1.3.0"));
        assert!(ok("<=1.2.3", "1.2.3"));
        assert!(ok("=1.2", "1.2.7"));
        assert!(!ok("=1.2", "1.3.0"));
        assert!(ok("1.2.3", "1.2.3"));
        assert!(!ok("=1.2.3", "1.2.4"));
        assert!(ok("*", "0.0.1"));
    }

    #[test]
    fn caret_and_tilde() {
        assert!(ok("^1.2", "1.9.0"));
        assert!(!ok("^1.2", "2.0.0"));
        assert!(!ok("^1.2", "1.1.0"));
        assert!(ok("^0.2.3", "0.2.9"));
        assert!(!ok("^0.2.3", "0.3.0"));
        assert!(ok("^0.0.3", "0.0.3"));
        assert!(!ok("^0.0.3", "0.0.4"));
        assert!(ok("~3.11", "3.11.9"));
        assert!(!ok("~3.11", "3.12.0"));
        assert!(ok("~1", "1.9.9"));
        assert!(!ok("~1", "2.0.0"));
        assert!(ok("~1.2.3", "1.2.9"));
        assert!(!ok("~1.2.3", "1.2.2"));
    }

    #[test]
    fn ranges_intersect() {
        assert!(ok(">=1 <2", "1.5.0"));
        assert!(!ok(">=1 <2", "2.0.0"));
        assert!(!ok(">=1 <2", "0.9.0"));
        assert!(ok(">=1.2, <1.4", "1.3.1"));
        assert!(ok("1.2.x", "1.2.5"));
        assert!(!ok("1.2.x", "1.3.0"));
    }

    #[test]
    fn invalid_ranges() {
        for bad in ["", ">=", "abc", ">= >= 1", ">=1.2.3.4", "=>1", "1.x.3.4"] {
            assert!(bad.parse::<VersionReq>().is_err(), "{bad} should fail");
        }
        let e = "banana".parse::<VersionReq>().unwrap_err();
        assert!(e.to_string().contains("banana"));
    }
}
