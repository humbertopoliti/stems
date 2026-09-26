//! `get_logs` pagination: at most [`PAGE_MAX`] records per call, oldest
//! first, with an opaque `next_cursor`.
//!
//! Log records carry no sequence number, only the daemon's receive time, so
//! a cursor is a position `(ts, skip)`: "records with `ts` greater or equal
//! to this instant, minus the first `skip` records stamped exactly `ts`"
//! (several lines can share a timestamp; the daemon's merge order for equal
//! timestamps is stable). The cursor also carries the filters of the first
//! call (`stems`, `level`, `grep`), so a follow-up call needs nothing else.
//! It is hex-encoded JSON behind a `c1_` prefix and must be treated as opaque.
//!
//! Which records a call returns:
//! * with `cursor`: the records after the cursor (`tail` / `since` ignored);
//! * with `since` or `tail`: that window, from its oldest record;
//! * with neither: the newest [`PAGE_MAX`] (or `limit`) records.
//!
//! `truncated` is true when the result does not hold every matching record
//! (more after this page, or older ones left out); `next_cursor` is always
//! returned once a record was seen, so an agent can poll for new lines.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use stems_api::LogRecord;
use stems_core::Error;

/// Most records one `get_logs` call returns.
pub const PAGE_MAX: usize = 500;

/// A decoded cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Position: records at or after this instant…
    pub ts: DateTime<Utc>,
    /// …minus this many records stamped exactly `ts` (already returned).
    pub skip: usize,
    /// Filter: stems.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stems: Vec<String>,
    /// Filter: level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// Filter: regex over the text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grep: Option<String>,
}

const PREFIX: &str = "c1_";

fn bad_cursor(why: &str) -> Error {
    Error::usage(
        format!("invalid log cursor ({why})"),
        "pass the `next_cursor` of a previous get_logs result unchanged, or omit `cursor` to start over",
    )
}

impl Cursor {
    /// The opaque string form.
    pub fn encode(&self) -> String {
        let raw = serde_json::to_vec(self).unwrap_or_default();
        let mut s = String::with_capacity(PREFIX.len() + raw.len() * 2);
        s.push_str(PREFIX);
        for b in raw {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Parse [`Self::encode`] output; `USAGE` otherwise.
    pub fn decode(s: &str) -> Result<Self, Error> {
        let hex = s
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| bad_cursor("unknown format"))?;
        if hex.len() % 2 != 0 {
            return Err(bad_cursor("odd length"));
        }
        let bytes: Result<Vec<u8>, _> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect();
        let bytes = bytes.map_err(|_| bad_cursor("not hex"))?;
        serde_json::from_slice(&bytes).map_err(|_| bad_cursor("corrupt"))
    }

    /// The `since` sent to the daemon (RFC 3339 with nanoseconds).
    pub fn since(&self) -> String {
        self.ts.to_rfc3339_opts(SecondsFormat::Nanos, true)
    }
}

/// One page cut out of the records a query returned.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// The records of this page, oldest first.
    pub records: Vec<LogRecord>,
    /// Position after the last record of this page (the input position when
    /// the page is empty).
    pub next: Option<(DateTime<Utc>, usize)>,
    /// Records after this page are already available.
    pub more: bool,
}

/// Cut the page after `after` out of `records` (sorted by `ts`).
pub fn paginate(
    records: Vec<LogRecord>,
    after: Option<(DateTime<Utc>, usize)>,
    limit: usize,
) -> Page {
    let limit = limit.clamp(1, PAGE_MAX);
    let mut start = 0;
    let mut skipped = 0;
    if let Some((ts, skip)) = after {
        while start < records.len() {
            let r = &records[start];
            if r.ts < ts {
                start += 1;
            } else if r.ts == ts && skipped < skip {
                skipped += 1;
                start += 1;
            } else {
                break;
            }
        }
    }
    let end = (start + limit).min(records.len());
    let more = end < records.len();
    let page: Vec<LogRecord> = records.into_iter().skip(start).take(end - start).collect();
    let next = match page.last() {
        Some(last) => {
            let mut n = page.iter().filter(|r| r.ts == last.ts).count();
            if let Some((ts, _)) = after
                && ts == last.ts
            {
                n += skipped;
            }
            Some((last.ts, n))
        }
        None => after,
    };
    Page {
        records: page,
        next,
        more,
    }
}

/// The JSON result of a page.
pub fn result_json(page: &Page, cursor: Option<&Cursor>, truncated: bool) -> serde_json::Value {
    let next_cursor = page.next.map(|(ts, skip)| {
        let mut c = cursor.cloned().unwrap_or(Cursor {
            ts,
            skip,
            stems: Vec::new(),
            level: None,
            grep: None,
        });
        c.ts = ts;
        c.skip = skip;
        c.encode()
    });
    json!({
        "records": page.records,
        "returned": page.records.len(),
        "truncated": truncated,
        "next_cursor": next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_core::logs::Stream;

    fn rec(ts_ms: i64, text: &str) -> LogRecord {
        LogRecord {
            ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
            stem: "a".into(),
            stream: Stream::Out,
            tag: None,
            level: None,
            text: text.into(),
            fields: None,
        }
    }

    #[test]
    fn cursor_roundtrip_and_errors() {
        let c = Cursor {
            ts: DateTime::from_timestamp_nanos(1_790_000_000_123_456_789),
            skip: 3,
            stems: vec!["echo-svc".into()],
            level: Some("error+".into()),
            grep: None,
        };
        let s = c.encode();
        assert!(s.starts_with("c1_"));
        assert_eq!(Cursor::decode(&s).unwrap(), c);
        assert!(c.since().contains(".123456789"));
        for bad in ["", "x", "c1_zz", "c1_abc", "c1_7b7d"] {
            assert_eq!(
                Cursor::decode(bad).unwrap_err().code,
                stems_core::ErrorCode::Usage,
                "{bad}"
            );
        }
    }

    /// Every record exactly once across pages, even when many share a ts.
    #[test]
    fn pages_cover_everything_once_with_equal_timestamps() {
        // 1200 records, 7 per millisecond.
        let all: Vec<LogRecord> = (0..1200).map(|i| rec(i / 7, &format!("l{i}"))).collect();
        let mut seen = Vec::new();
        let mut after = None;
        let mut calls = 0;
        loop {
            calls += 1;
            // The daemon returns records with ts >= cursor ts.
            let window: Vec<LogRecord> = all
                .iter()
                .filter(|r| after.is_none_or(|(ts, _)| r.ts >= ts))
                .cloned()
                .collect();
            let p = paginate(window, after, PAGE_MAX);
            seen.extend(p.records.iter().map(|r| r.text.clone()));
            after = p.next;
            if !p.more {
                break;
            }
        }
        assert_eq!(calls, 3);
        assert_eq!(seen.len(), 1200);
        let uniq: std::collections::BTreeSet<_> = seen.iter().collect();
        assert_eq!(uniq.len(), 1200);
        // Polling after the end returns nothing and keeps the position.
        let window: Vec<LogRecord> = all
            .iter()
            .filter(|r| r.ts >= after.unwrap().0)
            .cloned()
            .collect();
        let p = paginate(window, after, PAGE_MAX);
        assert!(p.records.is_empty());
        assert_eq!(p.next, after);
    }

    #[test]
    fn result_shape() {
        let p = paginate(vec![rec(1, "a"), rec(2, "b")], None, 1);
        assert!(p.more);
        let v = result_json(&p, None, true);
        assert_eq!(v["returned"], 1);
        assert_eq!(v["truncated"], true);
        let c = Cursor::decode(v["next_cursor"].as_str().unwrap()).unwrap();
        assert_eq!(c.skip, 1);
    }
}
