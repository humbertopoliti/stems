//! `stems events [-f] [--since <seq>]` (FR-CL-4).
//!
//! JSON mode prints **NDJSON** (one `stems_api::Event` per line, no
//! envelope): see the NDJSON exception in [`crate::output`]. Without `-f` the
//! daemon's buffered events with `seq > since` (default 0: all of them) are
//! printed and the command exits; with `-f` the same replay is followed by
//! live events until the daemon closes the stream (after `daemon.stopped`)
//! or the command gets Ctrl-C / SIGTERM (exit 0 in every case). Human mode
//! prints one compact line per event ([`human_line`]).

use std::io::Write;

use futures::StreamExt;
use serde_json::{Value, json};
use stems_api::{Event, EventsParams, EventsResult, Method};
use stems_core::Errors;

use crate::cli::EventsArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode};

/// Run `events`. With `-f`, lines are written to `stdout` as they arrive.
pub fn run(ctx: &Ctx, args: &EventsArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let since = args.since.unwrap_or(0);
    let r = if args.follow {
        block_on(follow(ctx, since, mode, stdout))
    } else {
        block_on(list(ctx, since))
    };
    r.unwrap_or_else(CommandOutput::failed)
}

async fn list(ctx: &Ctx, since: u64) -> Result<CommandOutput, Errors> {
    let c = client::connect(ctx).await?;
    let res: EventsResult = c
        .call(
            Method::EVENTS,
            EventsParams {
                since_seq: Some(since),
                limit: None,
                ..EventsParams::default()
            },
        )
        .await?;
    let ndjson: String = res.events.iter().map(|e| ndjson_line(e) + "\n").collect();
    let human: String = res.events.iter().map(|e| human_line(e) + "\n").collect();
    let data = serde_json::to_value(&res.events).unwrap_or(Value::Null);
    Ok(CommandOutput::data(data)
        .with_ndjson(ndjson)
        .with_human(human))
}

async fn follow(
    ctx: &Ctx,
    since: u64,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    use tokio::signal::unix::{SignalKind, signal};
    let c = client::connect(ctx).await?;
    let mut stream = c.subscribe_events(Some(since)).await?;
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
    loop {
        tokio::select! {
            ev = stream.next() => {
                let Some(ev) = ev else { break };
                let line = match mode {
                    Mode::Json => ndjson_line(&ev),
                    Mode::Human => human_line(&ev),
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

/// One NDJSON line (compact JSON, no trailing newline).
pub fn ndjson_line(e: &Event) -> String {
    serde_json::to_string(e).unwrap_or_else(|_| "{}".to_string())
}

/// One human line: `<ts> #<seq> <kind> [<stem>] [<from> -> <to>] [(<reason>)] by <actor>`.
pub fn human_line(e: &Event) -> String {
    let mut s = format!(
        "{} #{} {}",
        e.ts.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        e.seq,
        e.kind.as_str()
    );
    if let Some(stem) = &e.stem {
        s.push(' ');
        s.push_str(stem);
    }
    if e.from.is_some() || e.to.is_some() {
        s.push_str(&format!(
            " {} -> {}",
            e.from.as_deref().unwrap_or("-"),
            e.to.as_deref().unwrap_or("-")
        ));
    }
    if let Some(r) = &e.reason {
        s.push_str(&format!(" ({r})"));
    }
    if e.kind.as_str() == "process.output"
        && let Some(t) = e.data.get("text").and_then(Value::as_str)
    {
        s.push_str(&format!(": {t}"));
    }
    s.push_str(&format!(" by {}", e.actor));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use stems_api::EventKind;

    fn ev(kind: EventKind) -> Event {
        Event {
            ts: chrono::Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap(),
            seq: 7,
            kind,
            stem: None,
            from: None,
            to: None,
            reason: None,
            actor: "daemon".into(),
            data: json!({}),
        }
    }

    #[test]
    fn human_lines() {
        insta::assert_snapshot!(human_line(&ev(EventKind::DAEMON_STARTED)), @"2026-09-26T12:00:00.000Z #7 daemon.started by daemon");
        let mut e = ev(EventKind::STEM_STATE);
        e.stem = Some("api".into());
        e.from = Some("starting".into());
        e.to = Some("healthy".into());
        e.reason = Some("health check passed".into());
        e.actor = "cli:alice".into();
        insta::assert_snapshot!(human_line(&e), @"2026-09-26T12:00:00.000Z #7 stem.state api starting -> healthy (health check passed) by cli:alice");
        let mut e = ev(EventKind::PROCESS_OUTPUT);
        e.data = json!({"text": "hello", "stream": "stdout"});
        insta::assert_snapshot!(human_line(&e), @"2026-09-26T12:00:00.000Z #7 process.output: hello by daemon");
    }

    #[test]
    fn ndjson_is_one_line() {
        let l = ndjson_line(&ev(EventKind::DAEMON_STOPPING));
        assert!(!l.contains('\n'));
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["kind"], "daemon.stopping");
        assert_eq!(v["seq"], 7);
    }
}
