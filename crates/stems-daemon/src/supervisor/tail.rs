//! The last output lines of a dying attempt, carried by exit-caused
//! failures (36, FR-LC-6): `START_FAILED` (an exit before the stem was
//! ready, or a policy-`never` crash) and `MAX_RESTARTS` (the tail of the
//! *last* attempt). The lines come from the daemon's per-stem log ring
//! ([`OutputSink::recent_output`]), limited to the attempt that just died
//! (`StemInfo::output_since`, taken before the process was started, so an
//! earlier attempt's lines are never shown).
//!
//! Shape, in `Error.details`:
//!
//! - `stderr_tail: [string]` — the last [`MAX_LINES`] stderr lines, oldest
//!   first; always present (possibly empty);
//! - `stdout_tail: [string]` — only when `stderr_tail` is empty and stdout
//!   had lines (many apps log to stdout); absent otherwise;
//! - `output_truncated: bool` — more lines existed than shown.
//!
//! Each line is cut at [`MAX_CHARS`] characters (`…`). Log text is not
//! redacted anywhere in stems today, so none is applied here either.

use std::time::Duration;

use serde_json::{Value, json};
use stems_core::Error;
use stems_core::logs::{LogRecord, Stream};

use super::Core;
use super::actor::StemCell;

/// Most lines in a tail.
pub const MAX_LINES: usize = 10;
/// Longest line kept (characters), the rest replaced by `…`.
pub const MAX_CHARS: usize = 500;
/// Longest wait for the dying process's pipes to be drained into the log
/// ring before the tail is read (a grandchild holding the pipes open, or a
/// container runtime, may never close them).
pub const SETTLE: Duration = Duration::from_millis(500);

/// Wait (at most [`SETTLE`]) until every line the unit wrote before it died
/// has reached the log ring, so the tail is complete.
pub(crate) async fn settle(core: &Core, stem: &str) {
    let Some(mut rx) = core.sink.output_drained(stem) else {
        return;
    };
    let _ = tokio::time::timeout(SETTLE, rx.wait_for(|done| *done)).await;
}

/// The tail details of the attempt that just died (the cell's
/// `output_since`), from the daemon's log ring. Empty when the stem never
/// started in this daemon run (the ring is in memory: after a crash
/// recovery the tail is empty and `output_truncated` is false).
pub(crate) fn details(core: &Core, cell: &StemCell) -> Value {
    let since = cell.info().output_since;
    let records = match since {
        Some(since) => core.sink.recent_output(&cell.name, since, MAX_LINES + 1),
        None => Vec::new(),
    };
    build(&records)
}

/// Add the tail of the attempt that just died to an exit-caused error.
pub(crate) fn with_tail(core: &Core, cell: &StemCell, mut e: Error) -> Error {
    merge(&mut e.details, details(core, cell));
    e
}

/// Merge `tail`'s fields into `details` (an object; anything else is
/// replaced).
pub(crate) fn merge(details: &mut Value, tail: Value) {
    match (details.as_object_mut(), tail) {
        (Some(d), Value::Object(t)) => d.extend(t),
        (None, t) => *details = t,
        _ => {}
    }
}

/// The details object from the records [`OutputSink::recent_output`]
/// returned for `MAX_LINES + 1` (one stream only: stderr if it had any
/// lines, else stdout), oldest first.
pub(crate) fn build(records: &[LogRecord]) -> Value {
    let stream = records.first().map(|r| r.stream);
    let truncated = records.len() > MAX_LINES;
    let skip = records.len().saturating_sub(MAX_LINES);
    let lines: Vec<String> = records.iter().skip(skip).map(|r| clip(&r.text)).collect();
    let mut v = json!({ "stderr_tail": [], "output_truncated": truncated });
    match stream {
        Some(Stream::Out) if !lines.is_empty() => v["stdout_tail"] = json!(lines),
        Some(_) => v["stderr_tail"] = json!(lines),
        None => {}
    }
    v
}

/// `text` cut to [`MAX_CHARS`] characters, ending in `…` when cut.
fn clip(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        let mut s: String = head.chars().take(MAX_CHARS - 1).collect();
        s.push('…');
        s
    } else {
        head
    }
}

/// The human rendering shared by `stems status` / `stems up`: `stderr |
/// <line>` (or `stdout |`) per line, at most `max` (the last ones), a
/// leading `… (earlier lines: stems logs <stem>)` when cut.
pub fn human_lines(details: &Value, stem: &str, max: usize) -> Vec<String> {
    let (label, lines) = match details.get("stderr_tail").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => ("stderr", a),
        _ => match details.get("stdout_tail").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => ("stdout", a),
            _ => return Vec::new(),
        },
    };
    let truncated = details
        .get("output_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let skip = lines.len().saturating_sub(max);
    let mut out = Vec::new();
    if truncated || skip > 0 {
        out.push(format!("{label} | … (earlier lines: stems logs {stem})"));
    }
    out.extend(
        lines
            .iter()
            .skip(skip)
            .map(|l| format!("{label} | {}", l.as_str().unwrap_or_default())),
    );
    out
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;

    fn rec(stream: Stream, text: &str) -> LogRecord {
        LogRecord {
            ts: DateTime::<Utc>::MIN_UTC,
            stem: "api".into(),
            stream,
            tag: None,
            level: None,
            text: text.into(),
            fields: None,
        }
    }

    #[test]
    fn empty_records_give_an_empty_stderr_tail() {
        assert_eq!(
            build(&[]),
            json!({ "stderr_tail": [], "output_truncated": false })
        );
    }

    #[test]
    fn stderr_lines_oldest_first_and_no_stdout_key() {
        let v = build(&[rec(Stream::Err, "a"), rec(Stream::Err, "b")]);
        assert_eq!(v["stderr_tail"], json!(["a", "b"]));
        assert_eq!(v["output_truncated"], json!(false));
        assert!(v.get("stdout_tail").is_none());
    }

    #[test]
    fn stdout_only_when_stderr_was_silent() {
        let v = build(&[rec(Stream::Out, "only-stdout")]);
        assert_eq!(v["stderr_tail"], json!([]));
        assert_eq!(v["stdout_tail"], json!(["only-stdout"]));
    }

    #[test]
    fn more_than_max_lines_is_truncated_to_the_newest() {
        let recs: Vec<LogRecord> = (0..=MAX_LINES)
            .map(|i| rec(Stream::Err, &format!("line {i}")))
            .collect();
        let v = build(&recs);
        let tail = v["stderr_tail"].as_array().unwrap();
        assert_eq!(tail.len(), MAX_LINES);
        assert_eq!(tail[0], "line 1");
        assert_eq!(tail[MAX_LINES - 1], format!("line {MAX_LINES}"));
        assert_eq!(v["output_truncated"], json!(true));
    }

    #[test]
    fn long_lines_are_clipped_with_an_ellipsis() {
        let long = "x".repeat(MAX_CHARS + 20);
        let v = build(&[rec(Stream::Err, &long)]);
        let got = v["stderr_tail"][0].as_str().unwrap();
        assert_eq!(got.chars().count(), MAX_CHARS);
        assert!(got.ends_with('…'));
        let exact = "y".repeat(MAX_CHARS);
        assert_eq!(clip(&exact), exact);
    }

    #[test]
    fn merge_adds_to_existing_details() {
        let mut d = json!({ "stem": "api", "exit_code": 3 });
        merge(&mut d, build(&[rec(Stream::Err, "boom")]));
        assert_eq!(d["exit_code"], 3);
        assert_eq!(d["stderr_tail"], json!(["boom"]));
        let mut n = Value::Null;
        merge(&mut n, json!({ "stderr_tail": [] }));
        assert_eq!(n, json!({ "stderr_tail": [] }));
    }

    #[test]
    fn human_lines_prefix_and_cut() {
        let d = json!({ "stderr_tail": ["a", "b", "c", "d"], "output_truncated": false });
        assert_eq!(
            human_lines(&d, "api", 3),
            vec![
                "stderr | … (earlier lines: stems logs api)",
                "stderr | b",
                "stderr | c",
                "stderr | d"
            ]
        );
        let d = json!({ "stderr_tail": [], "stdout_tail": ["x"], "output_truncated": true });
        assert_eq!(
            human_lines(&d, "api", 10),
            vec!["stdout | … (earlier lines: stems logs api)", "stdout | x"]
        );
        assert!(human_lines(&json!({ "stderr_tail": [] }), "api", 10).is_empty());
        assert!(human_lines(&json!({}), "api", 10).is_empty());
    }
}
