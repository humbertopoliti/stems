//! `tests/features/PENDING.txt`: scenarios that are expected to fail.
//!
//! This replaces `@ignore`: pending scenarios are always *run*. A failure is
//! reported as `PENDING (expected)` and does not fail the run; a *pass* fails
//! the run with "remove from PENDING.txt", so the list can only shrink.
//! A `LEAK:` from the After hook is never excused by this list.
//!
//! Format: one entry per line, `#` starts a comment.
//! - `tests/features/harness/smoke.feature` — every scenario in the file;
//! - `tests/features/config/` — every feature file under the directory;
//! - `tests/features/config/show.feature:12` — the scenario at line 12.
//!
//! Paths are relative to the repository root.

/// One parsed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub line: Option<usize>,
    /// Line number in PENDING.txt, for diagnostics.
    pub source_line: usize,
}

impl Entry {
    /// Does this entry cover the scenario at `path:line`?
    pub fn matches(&self, path: &str, line: usize) -> bool {
        let path_ok = if self.path.ends_with('/') {
            path.starts_with(&self.path)
        } else {
            path == self.path
        };
        path_ok && self.line.is_none_or(|l| l == line)
    }
}

/// Parses PENDING.txt contents.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let line = raw.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                return None;
            }
            let (path, num) = match line.rsplit_once(':') {
                Some((p, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                    (p.to_owned(), n.parse().ok())
                }
                _ => (line.to_owned(), None),
            };
            Some(Entry {
                path: path.trim_start_matches("./").to_owned(),
                line: num,
                source_line: i + 1,
            })
        })
        .collect()
}

/// Is `path:line` pending under any entry?
pub fn is_pending(entries: &[Entry], path: &str, line: usize) -> bool {
    entries.iter().any(|e| e.matches(path, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# comment
tests/features/harness/smoke.feature   # 07 adds --version --json
tests/features/config/
./tests/features/x.feature:12

";

    #[test]
    fn parses_entries() {
        let e = parse(SAMPLE);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].path, "tests/features/harness/smoke.feature");
        assert_eq!(e[0].line, None);
        assert_eq!(
            e[2],
            Entry {
                path: "tests/features/x.feature".into(),
                line: Some(12),
                source_line: 4
            }
        );
    }

    #[test]
    fn matching() {
        let e = parse(SAMPLE);
        assert!(is_pending(&e, "tests/features/harness/smoke.feature", 3));
        assert!(is_pending(&e, "tests/features/config/load.feature", 9));
        assert!(is_pending(&e, "tests/features/x.feature", 12));
        assert!(!is_pending(&e, "tests/features/x.feature", 13));
        assert!(!is_pending(
            &e,
            "tests/features/harness/leak-hook.feature",
            3
        ));
    }
}
