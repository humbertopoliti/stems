//! MCP resources (FR-AI-1):
//!
//! | URI | content |
//! |---|---|
//! | `stems://workspace` | the resolved workspace config (JSON, secrets redacted) |
//! | `stems://graph` | the dependency graph (JSON, live glyphs when the daemon runs) |
//! | `stems://<stem>/config` | one stem's resolved config (JSON, redacted) |
//! | `stems://<stem>/logs?tail=N` | the stem's last N log lines (text; default 200, at most 500) |
//!
//! `resources/list` enumerates the workspace, the graph and every stem's
//! config and logs; `resources/templates/list` gives the two per-stem
//! templates.

use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
use serde_json::Value;
use stems_api::{LogRecord, Method, QueryLogsParams, QueryLogsResult};
use stems_core::{Error, ErrorCode};

use crate::backend::Backend;
use crate::logs::PAGE_MAX;
use crate::tools::{self, GetConfigArgs, GetGraphArgs};

/// Default `tail` of `stems://<stem>/logs`.
pub const DEFAULT_TAIL: usize = 200;

/// A parsed resource URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Uri {
    /// `stems://workspace`.
    Workspace,
    /// `stems://graph`.
    Graph,
    /// `stems://<stem>/config`.
    StemConfig(String),
    /// `stems://<stem>/logs?tail=N`.
    StemLogs(String, usize),
}

fn not_found(uri: &str) -> Error {
    Error::usage(
        format!("unknown resource `{uri}`"),
        "resources: stems://workspace, stems://graph, stems://<stem>/config, stems://<stem>/logs?tail=200 (see resources/list)",
    )
}

/// Parse a `stems://` URI.
pub fn parse(uri: &str) -> Result<Uri, Error> {
    let rest = uri.strip_prefix("stems://").ok_or_else(|| not_found(uri))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    match path {
        "workspace" => return Ok(Uri::Workspace),
        "graph" => return Ok(Uri::Graph),
        _ => {}
    }
    let (stem, what) = path.split_once('/').ok_or_else(|| not_found(uri))?;
    if stem.is_empty() {
        return Err(not_found(uri));
    }
    match what {
        "config" => Ok(Uri::StemConfig(stem.to_string())),
        "logs" => {
            let mut tail = DEFAULT_TAIL;
            for kv in query.split('&').filter(|s| !s.is_empty()) {
                if let Some(v) = kv.strip_prefix("tail=") {
                    tail = v.parse().map_err(|_| {
                        Error::usage(
                            format!("invalid tail `{v}` in `{uri}`"),
                            "use a number of lines, e.g. stems://api/logs?tail=50",
                        )
                    })?;
                }
            }
            Ok(Uri::StemLogs(stem.to_string(), tail.clamp(1, PAGE_MAX)))
        }
        _ => Err(not_found(uri)),
    }
}

/// `resources/list`: the workspace, the graph, and each stem's config and logs.
pub fn list(b: &Backend) -> Vec<Resource> {
    let mut out = vec![
        Resource::new("stems://workspace", "workspace")
            .with_description("The resolved workspace config (secrets redacted)")
            .with_mime_type("application/json"),
        Resource::new("stems://graph", "graph")
            .with_description("The dependency graph with live status")
            .with_mime_type("application/json"),
    ];
    if let Ok(r) = b.load_config() {
        for s in r.workspace.stems.values() {
            out.push(
                Resource::new(
                    format!("stems://{}/config", s.name),
                    format!("{} config", s.name),
                )
                .with_description(format!("Resolved config of stem {}", s.name))
                .with_mime_type("application/json"),
            );
            out.push(
                Resource::new(
                    format!("stems://{}/logs?tail={DEFAULT_TAIL}", s.name),
                    format!("{} logs", s.name),
                )
                .with_description(format!("Last {DEFAULT_TAIL} log lines of stem {}", s.name))
                .with_mime_type("text/plain"),
            );
        }
    }
    out
}

/// `resources/templates/list`.
pub fn templates() -> Vec<ResourceTemplate> {
    vec![
        ResourceTemplate::new("stems://{stem}/config", "stem config")
            .with_description("Resolved config of a stem (secrets redacted)")
            .with_mime_type("application/json"),
        ResourceTemplate::new("stems://{stem}/logs{?tail}", "stem logs")
            .with_description("The last `tail` log lines of a stem (default 200, at most 500)")
            .with_mime_type("text/plain"),
    ]
}

/// One log record as a text line: `<ts> <stream> <level> [<tag>] <text>`.
pub fn log_line(r: &LogRecord) -> String {
    let level = r
        .level
        .map(|l| serde_json::to_value(l).ok())
        .and_then(|v| v.and_then(|v| v.as_str().map(str::to_string)))
        .unwrap_or_else(|| "-".into());
    let stream = serde_json::to_value(r.stream)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let tag = r
        .tag
        .as_deref()
        .map(|t| format!(" [{t}]"))
        .unwrap_or_default();
    format!(
        "{} {stream} {level}{tag} {}",
        r.ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        r.text
    )
}

/// `resources/read`.
pub async fn read(b: &Backend, actor: &str, uri: &str) -> Result<ResourceContents, Error> {
    let json = |v: Value| {
        ResourceContents::text(serde_json::to_string_pretty(&v).unwrap_or_default(), uri)
            .with_mime_type("application/json")
    };
    match parse(uri)? {
        Uri::Workspace => Ok(json(tools::get_config(b, &GetConfigArgs::default())?)),
        Uri::Graph => Ok(json(
            tools::get_graph(b, actor, GetGraphArgs::default()).await?,
        )),
        Uri::StemConfig(stem) => Ok(json(tools::get_config(
            b,
            &GetConfigArgs {
                stem: Some(stem),
                effective: false,
            },
        )?)),
        Uri::StemLogs(stem, tail) => {
            let resolved = b.load_config()?;
            if resolved.workspace.stem(&stem).is_none() {
                return Err(
                    Error::new(ErrorCode::UnknownStem, format!("no stem named `{stem}`"))
                        .with_hint("see resources/list for the stems of this workspace"),
                );
            }
            let c = b.connect(actor).await?;
            let mut p = QueryLogsParams::default();
            p.filter.stems = vec![stem];
            p.filter.tail = Some(tail);
            let r: QueryLogsResult = c.call(Method::QUERY_LOGS, &p).await?;
            let mut text: String = r.records.iter().map(|x| log_line(x) + "\n").collect();
            if text.is_empty() {
                text = "(no log lines)\n".into();
            }
            Ok(ResourceContents::text(text, uri).with_mime_type("text/plain"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        assert_eq!(parse("stems://workspace").unwrap(), Uri::Workspace);
        assert_eq!(parse("stems://graph").unwrap(), Uri::Graph);
        assert_eq!(
            parse("stems://echo-svc/config").unwrap(),
            Uri::StemConfig("echo-svc".into())
        );
        assert_eq!(
            parse("stems://echo-svc/logs?tail=5").unwrap(),
            Uri::StemLogs("echo-svc".into(), 5)
        );
        assert_eq!(
            parse("stems://echo-svc/logs").unwrap(),
            Uri::StemLogs("echo-svc".into(), DEFAULT_TAIL)
        );
        assert_eq!(
            parse("stems://a/logs?tail=100000").unwrap(),
            Uri::StemLogs("a".into(), PAGE_MAX)
        );
        for bad in [
            "http://x",
            "stems://",
            "stems://a/nope",
            "stems:///config",
            "stems://a/logs?tail=x",
        ] {
            assert_eq!(parse(bad).unwrap_err().code, ErrorCode::Usage, "{bad}");
        }
    }
}
