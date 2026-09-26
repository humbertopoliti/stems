//! `stems logs [stem…] [-f] [--since] [--until] [--grep] [--level]
//! [--script] [--tail] [--export -o FILE]` (FR-LG-1/2/4/5, `docs/logs.md`).
//!
//! JSON mode prints **NDJSON**: one `LogRecord` per line (`{ts, stem,
//! stream, tag, level, text, fields}`), no envelope — the same exception as
//! `stems events` (see [`crate::output`]); errors still use the envelope.
//! Human mode prints `HH:MM:SS.mmm <stem> | [tag] text` (local time), the
//! stem prefix coloured per stem unless `--no-color`/`STEMS_NO_COLOR` or
//! stdout is not a terminal. Several stems interleave by timestamp.
//!
//! Without `-f` the daemon's `query_logs` answers (at most 10 000 records,
//! the newest; `--tail` defaults to all). With `-f` the command subscribes
//! (`subscribe_logs`): `--since`/`--tail` history first (default: the last
//! 10 lines), then live lines until Ctrl-C / SIGTERM (exit 0) or the daemon
//! stops. `--export` asks the daemon to write a `.tar.gz` support bundle
//! (`status.json`, `events.ndjson`, redacted `config.json`, `logs/<stem>/*`).

use std::io::Write;
use std::path::PathBuf;

use futures::StreamExt;
use serde_json::{Value, json};
use stems_api::{
    ExportLogsParams, ExportLogsResult, LogFilter, LogRecord, Method, QueryLogsParams,
    QueryLogsResult, SubscribeLogsParams,
};
use stems_core::logs::{LevelFilter, SinceSpec};
use stems_core::{Error, Errors};

use crate::cli::LogsArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode};

/// Lines replayed by `-f` when neither `--since` nor `--tail` is given.
pub const FOLLOW_DEFAULT_TAIL: usize = 10;

/// Run `logs`. With `-f`, lines are written to `stdout` as they arrive.
pub fn run(ctx: &Ctx, args: &LogsArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let r = (|| {
        let filter = filter(args)?;
        if args.export {
            return block_on(export(ctx, args));
        }
        let color = use_color(ctx);
        if args.follow {
            block_on(follow(ctx, filter, mode, color, stdout))
        } else {
            block_on(list(ctx, filter, color))
        }
    })();
    r.unwrap_or_else(CommandOutput::failed)
}

/// Validate the flags locally (USAGE, exit 2) and build the RPC filter.
fn filter(args: &LogsArgs) -> Result<LogFilter, Errors> {
    let bad = |flag: &str, e: String| {
        Errors::from(
            Error::usage(
                format!("invalid --{flag}: {e}"),
                "times: 10m, 1h30m or 2026-09-26T10:00:00Z; levels: error, warn+, info+",
            )
            .with_details(json!({ "flag": flag })),
        )
    };
    for (flag, v) in [("since", &args.since), ("until", &args.until)] {
        if let Some(v) = v {
            SinceSpec::parse(v).map_err(|e| bad(flag, e.to_string()))?;
        }
    }
    if let Some(l) = &args.level {
        LevelFilter::parse(l).map_err(|e| bad("level", e.to_string()))?;
    }
    if args.output.is_some() && !args.export {
        return Err(Errors::from(Error::usage(
            "-o/--output is only used with --export",
            "run `stems logs --export -o bundle.tar.gz`",
        )));
    }
    Ok(LogFilter {
        stems: args.stems.clone(),
        since: args.since.clone(),
        until: args.until.clone(),
        grep: args.grep.clone(),
        level: args.level.clone(),
        script: args.script.clone(),
        tail: args.tail,
    })
}

fn use_color(ctx: &Ctx) -> bool {
    use std::io::IsTerminal;
    let env_off = ctx
        .env
        .get("STEMS_NO_COLOR")
        .is_some_and(|v| !matches!(v.as_str(), "" | "0" | "false" | "no" | "off" | "n" | "f"));
    !ctx.global.no_color && !env_off && std::io::stdout().is_terminal()
}

async fn list(ctx: &Ctx, filter: LogFilter, color: bool) -> Result<CommandOutput, Errors> {
    let c = client::connect(ctx).await?;
    let res: QueryLogsResult = c
        .call(
            Method::QUERY_LOGS,
            QueryLogsParams {
                filter,
                from_files: false,
            },
        )
        .await?;
    let ndjson: String = res.records.iter().map(|r| ndjson_line(r) + "\n").collect();
    let mut human = String::new();
    if res.truncated {
        human.push_str(&format!(
            "(showing the newest {} records; narrow with --since/--tail)\n",
            res.records.len()
        ));
    }
    let now = chrono::Local::now();
    for r in &res.records {
        human.push_str(&human_line(r, color, &now.timezone()));
        human.push('\n');
    }
    let data = json!({ "records": res.records, "truncated": res.truncated });
    Ok(CommandOutput::data(data)
        .with_ndjson(ndjson)
        .with_human(human))
}

async fn follow(
    ctx: &Ctx,
    mut filter: LogFilter,
    mode: Mode,
    color: bool,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    use tokio::signal::unix::{SignalKind, signal};
    if filter.since.is_none() && filter.tail.is_none() {
        filter.tail = Some(FOLLOW_DEFAULT_TAIL);
    }
    let c = client::connect(ctx).await?;
    let mut stream = c.subscribe_logs(SubscribeLogsParams { filter }).await?;
    let mut term = signal(SignalKind::terminate()).ok();
    let sigterm = async {
        match term.as_mut() {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending().await,
        }
    };
    tokio::pin!(sigterm);
    let tz = chrono::Local::now().timezone();
    loop {
        tokio::select! {
            rec = stream.next() => {
                let Some(rec) = rec else { break };
                let line = match mode {
                    Mode::Json => ndjson_line(&rec),
                    Mode::Human => human_line(&rec, color, &tz),
                };
                if writeln!(stdout, "{line}").and_then(|()| stdout.flush()).is_err() {
                    break; // reader went away (e.g. `| head`)
                }
            }
            _ = tokio::signal::ctrl_c() => break,
            () = &mut sigterm => break,
        }
    }
    Ok(CommandOutput::data(json!(null))
        .with_ndjson("")
        .with_human(""))
}

async fn export(ctx: &Ctx, args: &LogsArgs) -> Result<CommandOutput, Errors> {
    let path = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "stems-logs-{}.tar.gz",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    let path = ctx.cwd.join(path);
    let c = client::connect(ctx).await?;
    let res: ExportLogsResult = c
        .call(
            Method::EXPORT_LOGS,
            ExportLogsParams {
                path,
                since: args.since.clone(),
            },
        )
        .await?;
    let human = format!(
        "wrote {} ({} entries, {} bytes)\n",
        res.path.display(),
        res.entries.len(),
        res.bytes
    );
    let data = serde_json::to_value(&res).unwrap_or(Value::Null);
    Ok(CommandOutput::data(data).with_human(human))
}

/// One NDJSON line (compact JSON, no trailing newline).
pub fn ndjson_line(r: &LogRecord) -> String {
    serde_json::to_string(r).unwrap_or_else(|_| "{}".to_string())
}

/// ANSI colours cycled over stem names (stable per name).
const PALETTE: [&str; 6] = ["36", "33", "35", "32", "34", "91"];

fn stem_color(stem: &str) -> &'static str {
    let h = stem
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[(h as usize) % PALETTE.len()]
}

/// One human line: `HH:MM:SS.mmm <stem> | [tag] text` in `tz`.
pub fn human_line<Tz: chrono::TimeZone>(r: &LogRecord, color: bool, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let ts = r.ts.with_timezone(tz).format("%H:%M:%S%.3f");
    let prefix = if color {
        format!("\x1b[{}m{} |\x1b[0m", stem_color(&r.stem), r.stem)
    } else {
        format!("{} |", r.stem)
    };
    let tag = r
        .tag
        .as_deref()
        .map(|t| format!(" [{t}]"))
        .unwrap_or_default();
    format!("{ts} {prefix}{tag} {}", r.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use stems_core::logs::{Level, Stream};

    fn rec(stem: &str, secs: i64, tag: Option<&str>, text: &str) -> LogRecord {
        LogRecord {
            ts: Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap()
                + chrono::Duration::milliseconds(secs * 1000 + 123),
            stem: stem.into(),
            stream: if tag.is_some() {
                Stream::Script
            } else {
                Stream::Out
            },
            tag: tag.map(str::to_string),
            level: Some(Level::Info),
            text: text.into(),
            fields: None,
        }
    }

    #[test]
    fn human_lines_golden() {
        let recs = stems_core::logs::merge_by_ts(vec![
            vec![
                rec("shop-api", 0, None, "listening on 127.0.0.1:8080"),
                rec("shop-api", 2, Some("seed"), "seeded 3 users"),
            ],
            vec![rec("web", 1, None, "GET / 200")],
        ]);
        let text: Vec<String> = recs.iter().map(|r| human_line(r, false, &Utc)).collect();
        insta::assert_snapshot!("logs-human", text.join("\n"));
        let colored = human_line(&recs[0], true, &Utc);
        assert!(colored.contains("\x1b["), "{colored:?}");
        assert!(colored.contains("|\x1b[0m listening"), "{colored:?}");
    }

    #[test]
    fn stem_colours_are_stable() {
        assert_eq!(stem_color("shop-api"), stem_color("shop-api"));
    }

    #[test]
    fn ndjson_is_one_line_with_every_key() {
        let l = ndjson_line(&rec("a", 0, None, "x"));
        assert!(!l.contains('\n'));
        let v: Value = serde_json::from_str(&l).unwrap();
        for k in ["ts", "stem", "stream", "tag", "level", "text", "fields"] {
            assert!(v.get(k).is_some(), "{k} missing in {l}");
        }
    }
}
