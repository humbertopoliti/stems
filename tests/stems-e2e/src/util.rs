//! Small pure helpers: command-line splitting, JSON matching, semver.

use serde_json::Value;
use serde_json_path::JsonPath;

/// Splits a command line the way a POSIX shell would for simple cases:
/// whitespace separates words, single quotes are literal, double quotes allow
/// `\"` and `\\` escapes, a backslash outside quotes escapes the next char.
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err(format!("unterminated ' in {line:?}")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => cur.push(c),
                            Some(c) => {
                                cur.push('\\');
                                cur.push(c);
                            }
                            None => return Err(format!("dangling \\ in {line:?}")),
                        },
                        Some(c) => cur.push(c),
                        None => return Err(format!("unterminated \" in {line:?}")),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => return Err(format!("dangling \\ in {line:?}")),
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    Ok(out)
}

/// Parses command stdout as JSON: a single document, or NDJSON (one document
/// per non-empty line) returned as an array, except that NDJSON ending with
/// an envelope (`stems up --json`: progress events, then the result) yields
/// that envelope. `None` if neither.
pub fn parse_json_output(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    let lines: Result<Vec<Value>, _> = trimmed
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect();
    let mut lines = lines.ok()?;
    // `stems up --json`: NDJSON progress events, then the envelope as the
    // last line. The envelope is the command's result.
    if lines.last().is_some_and(is_envelope) {
        return lines.pop();
    }
    Some(Value::Array(lines))
}

/// A stems JSON envelope (`ok` + `version` + `errors`).
pub fn is_envelope(v: &Value) -> bool {
    v.get("ok").is_some() && v.get("version").is_some() && v.get("errors").is_some()
}

/// Parses an expected value written in a step. Valid JSON is taken as is;
/// anything else is treated as a bare string.
pub fn parse_expected(raw: &str) -> Value {
    let raw = raw.trim();
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

/// Runs a JSONPath query (RFC 9535, via `serde_json_path`).
pub fn query<'a>(doc: &'a Value, path: &str) -> Result<Vec<&'a Value>, String> {
    let p = JsonPath::parse(path).map_err(|e| format!("invalid JSONPath {path:?}: {e}"))?;
    Ok(p.query(doc).all())
}

/// `actual` contains `expected` as a subset: objects recursively (every key
/// of `expected` present and matching), arrays element-wise by index-free
/// containment (every expected element matched by some actual element),
/// scalars by equality.
pub fn is_subset(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(e), Value::Object(a)) => e
            .iter()
            .all(|(k, ev)| a.get(k).is_some_and(|av| is_subset(ev, av))),
        (Value::Array(e), Value::Array(a)) => {
            e.iter().all(|ev| a.iter().any(|av| is_subset(ev, av)))
        }
        (e, a) => e == a,
    }
}

/// `contains` semantics for `Then the JSON at "<p>" contains <v>`:
/// string ⊇ substring, array has an element that is a superset of `expected`
/// (or, if `expected` is an array, every expected element is present),
/// object is a superset of `expected`.
pub fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::String(a), Value::String(e)) => a.contains(e.as_str()),
        (Value::Array(_), Value::Array(_)) => is_subset(expected, actual),
        (Value::Array(a), e) => a.iter().any(|av| is_subset(e, av)),
        (Value::Object(_), Value::Object(_)) => is_subset(expected, actual),
        (a, e) => a == e,
    }
}

/// Semantic Versioning 2.0.0 (core, optional pre-release and build).
pub fn is_semver(s: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$",
        )
        .expect("valid semver regex")
    })
    .is_match(s)
}

/// Truncates long text for failure messages.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}… ({} chars total)", s.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_args_handles_quotes() {
        assert_eq!(
            split_args(r#"stems run api 'a b' "c \"d\"" e\ f"#).unwrap(),
            vec!["stems", "run", "api", "a b", r#"c "d""#, "e f"]
        );
        assert_eq!(split_args("  a   b ").unwrap(), vec!["a", "b"]);
        assert_eq!(split_args("a ''").unwrap(), vec!["a", ""]);
        assert!(split_args("a 'b").is_err());
    }

    #[test]
    fn json_output_parsing() {
        assert_eq!(
            parse_json_output(r#"{"ok":true}"#),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            parse_json_output("{\"a\":1}\n\n{\"a\":2}\n"),
            Some(json!([{"a":1},{"a":2}]))
        );
        assert_eq!(
            parse_json_output(
                "{\"seq\":1}\n{\"ok\":true,\"data\":1,\"errors\":[],\"version\":\"0\"}\n"
            ),
            Some(serde_json::json!({"ok": true, "data": 1, "errors": [], "version": "0"}))
        );
        assert_eq!(parse_json_output("error: nope"), None);
        assert_eq!(parse_json_output("  "), None);
    }

    #[test]
    fn subset_and_contains() {
        let actual = json!({"kind": "stem.state", "stem": "api", "to": "healthy", "seq": 3});
        assert!(is_subset(
            &json!({"kind": "stem.state", "to": "healthy"}),
            &actual
        ));
        assert!(!is_subset(
            &json!({"kind": "stem.state", "to": "failed"}),
            &actual
        ));
        assert!(contains(&json!("hello world"), &json!("lo wo")));
        assert!(contains(&json!([1, {"a": 1, "b": 2}]), &json!({"a": 1})));
        assert!(contains(&json!(["a", "b", "c"]), &json!(["c", "a"])));
        assert!(!contains(&json!(["a"]), &json!("b")));
    }

    #[test]
    fn jsonpath_queries() {
        let doc = json!({"ok": false, "errors": [{"code": "CYCLE", "path": "stems.a"}]});
        assert_eq!(
            query(&doc, "$.errors[*].code").unwrap(),
            vec![&json!("CYCLE")]
        );
        assert!(query(&doc, "$.nope").unwrap().is_empty());
        assert!(query(&doc, "not a path").is_err());
    }

    #[test]
    fn semver() {
        for ok in ["0.1.0", "1.2.3-alpha.1", "10.0.0+build.5", "1.0.0-rc.1+x"] {
            assert!(is_semver(ok), "{ok}");
        }
        for bad in ["1.2", "01.2.3", "v1.2.3", "1.2.3-", ""] {
            assert!(!is_semver(bad), "{bad}");
        }
    }

    #[test]
    fn expected_values() {
        assert_eq!(parse_expected("2"), json!(2));
        assert_eq!(parse_expected("\"x\""), json!("x"));
        assert_eq!(parse_expected("bare words"), json!("bare words"));
    }
}
