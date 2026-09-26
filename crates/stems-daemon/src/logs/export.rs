//! `stems logs --export` (FR-LG-5): a `.tar.gz` support bundle.
//!
//! Entries, in order:
//!
//! * `status.json` — the `status` result (or `{"error": …}` if unavailable);
//! * `events.ndjson` — the daemon's buffered events, one per line;
//! * `config.json` — the resolved workspace (`{"sources", "workspace"}`)
//!   passed through [`redact`];
//! * `logs/<stem>/<file>` — every stem's log files (`current.log`,
//!   `current.N.log`), one JSON record per line.
//!
//! With `since`, events and log records older than it are left out.

use std::io::Write;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::write::GzEncoder;
use regex::Regex;
use std::sync::Arc;

use serde_json::{Value, json};
use stems_api::{Event, ExportLogsParams, ExportLogsResult};
use stems_core::Error;

use super::{LogHub, list_log_files};

/// Env keys whose values are replaced by [`REDACTED`] in `config.json`.
pub const REDACT_KEY_PATTERN: &str = r"(?i)(secret|token|password|passwd|key)";
/// The replacement value.
pub const REDACTED: &str = "<redacted>";

static REDACT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(REDACT_KEY_PATTERN).expect("valid redaction regex"));

/// Redact the resolved config for sharing: in every environment map (`env`,
/// `local_env`, any `*_env`, `environment`; workspace and stems) values whose key matches [`REDACT_KEY_PATTERN`] or one of `extra`
/// (the hook for later secrets work) become [`REDACTED`].
pub fn redact(v: &mut Value, extra: &[Regex]) {
    let hit = |k: &str| REDACT_RE.is_match(k) || extra.iter().any(|r| r.is_match(k));
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                if is_env_key(k)
                    && let Value::Object(env) = child
                {
                    for (ek, ev) in env.iter_mut() {
                        if hit(ek) && !ev.is_null() {
                            *ev = Value::String(REDACTED.to_string());
                        }
                    }
                    continue;
                }
                redact(child, extra);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|c| redact(c, extra)),
        _ => {}
    }
}

/// `env`, `local_env`, `base_env`, `environment`: maps of environment variables.
fn is_env_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    k == "env" || k == "environment" || k.ends_with("_env")
}

/// Everything the bundle holds besides the log files (gathered by the
/// daemon on the async side).
pub struct BundleInputs {
    /// `status` result.
    pub status: Value,
    /// Buffered events.
    pub events: Vec<Event>,
    /// Resolved config, already redacted.
    pub config: Value,
    /// Cut-off.
    pub since: Option<DateTime<Utc>>,
}

/// Write the bundle to `path`; returns the entry names. Blocking.
pub fn write_bundle(
    hub: &LogHub,
    inputs: &BundleInputs,
    path: &Path,
) -> Result<Vec<String>, Error> {
    let io = |what: &str, e: std::io::Error| {
        Error::internal(format!("cannot write {} ({what}): {e}", path.display()))
            .with_hint("check that the output directory exists and is writable")
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| io("create dir", e))?;
    }
    let file = std::fs::File::create(path).map_err(|e| io("create", e))?;
    let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::default()));
    let mut entries = Vec::new();
    let now = Utc::now().timestamp().max(0) as u64;
    let mut add = |tar: &mut tar::Builder<GzEncoder<std::fs::File>>,
                   name: String,
                   data: &[u8]|
     -> Result<(), Error> {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(now);
        h.set_cksum();
        tar.append_data(&mut h, &name, data)
            .map_err(|e| io("append", e))?;
        entries.push(name);
        Ok(())
    };

    let pretty = |v: &Value| serde_json::to_vec_pretty(v).unwrap_or_default();
    add(&mut tar, "status.json".into(), &pretty(&inputs.status))?;
    let mut events = Vec::new();
    for ev in &inputs.events {
        if inputs.since.is_some_and(|s| ev.ts < s) {
            continue;
        }
        if let Ok(line) = serde_json::to_string(ev) {
            events.extend_from_slice(line.as_bytes());
            events.push(b'\n');
        }
    }
    add(&mut tar, "events.ndjson".into(), &events)?;
    add(&mut tar, "config.json".into(), &pretty(&inputs.config))?;
    for stem in hub.known_stems() {
        for file in list_log_files(&hub.stem_dir(&stem)) {
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let data = match inputs.since {
                None => std::fs::read(&file).map_err(|e| io("read log", e))?,
                Some(since) => {
                    let mut out = Vec::new();
                    super::read_records(&file, &mut |r| {
                        if r.ts >= since
                            && let Ok(line) = serde_json::to_string(&r)
                        {
                            out.extend_from_slice(line.as_bytes());
                            out.push(b'\n');
                        }
                    });
                    out
                }
            };
            add(&mut tar, format!("logs/{stem}/{name}"), &data)?;
        }
    }
    let gz = tar.into_inner().map_err(|e| io("finish tar", e))?;
    let mut file = gz.finish().map_err(|e| io("finish gzip", e))?;
    file.flush().map_err(|e| io("flush", e))?;
    Ok(entries)
}

/// `export_logs`: flush the sinks, gather the inputs and write the bundle
/// on a blocking thread.
pub async fn rpc_export(
    hub: &Arc<LogHub>,
    ws: Option<Arc<stems_config::Resolved>>,
    status: Value,
    events: Vec<Event>,
    p: ExportLogsParams,
) -> Result<ExportLogsResult, Error> {
    let since = match &p.since {
        None => None,
        Some(s) => Some(
            stems_core::logs::SinceSpec::parse(s)
                .map_err(|e| {
                    Error::usage(
                        format!("invalid --since: {e}"),
                        "use a duration like 10m or an RFC 3339 timestamp",
                    )
                })?
                .resolve(Utc::now()),
        ),
    };
    if !p.path.is_absolute() {
        return Err(Error::usage(
            format!("export path `{}` is not absolute", p.path.display()),
            "the CLI resolves -o against its working directory",
        ));
    }
    let mut config = match &ws {
        Some(r) => json!({ "sources": r.sources, "workspace": r.workspace }),
        None => Value::Null,
    };
    redact(&mut config, &[]);
    hub.flush_all().await;
    let inputs = BundleInputs {
        status,
        events,
        config,
        since,
    };
    let hub = hub.clone();
    let path = p.path.clone();
    let entries = tokio::task::spawn_blocking(move || write_bundle(&hub, &inputs, &path))
        .await
        .map_err(|e| Error::internal(format!("export failed: {e}")))??;
    let bytes = std::fs::metadata(&p.path).map(|m| m.len()).unwrap_or(0);
    Ok(ExportLogsResult {
        path: p.path,
        entries,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_env_values_by_key() {
        let mut v = json!({
            "env": {"API_TOKEN": "t", "PORT": "1"},
            "local_env": {"API_TOKEN": "t"},
            "stems": {"a": {"env": {"DB_PASSWORD": "p", "Secret_Thing": "s", "api_key": "k", "HOST": "h"},
                             "keep": 3}},
        });
        redact(&mut v, &[Regex::new("^HOST$").unwrap()]);
        assert_eq!(v["env"]["API_TOKEN"], REDACTED);
        assert_eq!(v["env"]["PORT"], "1");
        assert_eq!(v["local_env"]["API_TOKEN"], REDACTED);
        let a = &v["stems"]["a"]["env"];
        assert_eq!(a["DB_PASSWORD"], REDACTED);
        assert_eq!(a["Secret_Thing"], REDACTED);
        assert_eq!(a["api_key"], REDACTED);
        assert_eq!(a["HOST"], REDACTED);
        assert_eq!(v["stems"]["a"]["keep"], 3);
    }
}
