//! MCP prompts (FR-AI-1, P1):
//!
//! * `diagnose_stem {stem}` — one user message holding the stem's status,
//!   health history, restart info, its last 20 error-level log lines and
//!   its recent events, followed by instructions for finding the cause.
//! * `bring_up_and_report {profile?}` — instructions to bring the
//!   environment up with the tools and report, with the current status.
//!
//! Prompts gather what they can: without a daemon they say so and still
//! return the instructions.

use rmcp::model::{GetPromptResult, Prompt, PromptArgument, PromptMessage, Role};
use serde_json::{Map, Value, json};
use stems_api::{
    EventsParams, EventsResult, HealthParams, HealthResult, Method, QueryLogsParams,
    QueryLogsResult, StatusParams, StatusResult,
};
use stems_core::{Error, ErrorCode};

use crate::backend::Backend;
use crate::resources::log_line;

/// Error lines included by `diagnose_stem`.
pub const ERROR_LINES: usize = 20;
/// Events included by `diagnose_stem`.
pub const EVENTS: usize = 20;

/// `prompts/list`.
pub fn list() -> Vec<Prompt> {
    vec![
        Prompt::new(
            "diagnose_stem",
            Some(
                "Diagnose why a stem is unhealthy or failing: its status, health history, restarts, recent error logs and events, with instructions",
            ),
            Some(vec![
                PromptArgument::new("stem")
                    .with_description("The stem to diagnose")
                    .with_required(true),
            ]),
        ),
        Prompt::new(
            "bring_up_and_report",
            Some("Bring the environment up (optionally one profile) and report every stem's state"),
            Some(vec![
                PromptArgument::new("profile")
                    .with_description("Profile to bring up (default: the workspace default)")
                    .with_required(false),
            ]),
        ),
    ]
}

fn arg(args: &Option<Map<String, Value>>, name: &str) -> Option<String> {
    args.as_ref()
        .and_then(|m| m.get(name))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn pretty(v: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// `prompts/get`.
pub async fn get(
    b: &Backend,
    actor: &str,
    name: &str,
    args: &Option<Map<String, Value>>,
) -> Result<GetPromptResult, Error> {
    match name {
        "diagnose_stem" => {
            let stem = arg(args, "stem").ok_or_else(|| {
                Error::usage(
                    "`diagnose_stem` needs the `stem` argument",
                    "pass {\"stem\": \"<name>\"}",
                )
            })?;
            let text = diagnose(b, actor, &stem).await?;
            Ok(
                GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                    .with_description(format!("Diagnose stem {stem}")),
            )
        }
        "bring_up_and_report" => {
            let profile = arg(args, "profile");
            let text = bring_up(b, actor, profile.as_deref()).await;
            Ok(
                GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                    .with_description("Bring the environment up and report"),
            )
        }
        other => Err(Error::usage(
            format!("unknown prompt `{other}`"),
            "prompts: diagnose_stem, bring_up_and_report (see prompts/list)",
        )),
    }
}

async fn diagnose(b: &Backend, actor: &str, stem: &str) -> Result<String, Error> {
    let resolved = b.load_config()?;
    let Some(cfg) = resolved.workspace.stem(stem) else {
        return Err(
            Error::new(ErrorCode::UnknownStem, format!("no stem named `{stem}`")).with_hint(
                format!(
                    "stems in this workspace: {}",
                    resolved
                        .workspace
                        .stems
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        );
    };
    let mut out = format!(
        "Diagnose the stem `{stem}` ({} stem{}) of the stems workspace `{}`.\n\n",
        cfg.kind(),
        cfg.description
            .as_deref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default(),
        resolved.workspace.name
    );
    let c = match b.connect_existing(actor).await {
        Ok(c) => c,
        Err(e) => {
            out.push_str(&format!(
                "The stems daemon is not running ({}), so there is no live state. The stem is not running.\n\n",
                e.message
            ));
            out.push_str(&instructions(stem));
            return Ok(out);
        }
    };
    let status: Result<StatusResult, Error> = c
        .call(
            Method::STATUS,
            StatusParams {
                stems: vec![stem.to_string()],
                verbose: false,
            },
        )
        .await;
    match status.ok().and_then(|s| s.stems.into_iter().next()) {
        Some(s) => {
            out.push_str(&format!(
                "## Status\nstate: {}\nglyph: {}\nreason: {}\npid: {}\nuptime_s: {}\n",
                s.state,
                s.glyph.name(),
                s.reason.as_deref().unwrap_or("-"),
                s.pid.map_or("-".into(), |p| p.to_string()),
                s.uptime_s.map_or("-".into(), |u| u.to_string()),
            ));
            out.push_str(&format!(
                "restarts: {} (in the restart window: {})\n",
                s.restarts, s.restarts_in_window
            ));
            if let Some(e) = &s.error {
                out.push_str(&format!("last error: {}\n", pretty(e)));
            } else {
                out.push_str("last error: none\n");
            }
            if let Some(h) = &s.health {
                out.push_str(&format!("health: {}\n", pretty(h)));
            }
            out.push('\n');
        }
        None => out.push_str("## Status\nunavailable\n\n"),
    }
    let health: Result<HealthResult, Error> = c
        .call(
            Method::HEALTH,
            HealthParams {
                stems: vec![stem.to_string()],
                last: Some(10),
            },
        )
        .await;
    out.push_str("## Health history (last probes, oldest first)\n");
    match health.ok().and_then(|h| h.stems.into_iter().next()) {
        Some(h) if !h.results.is_empty() => {
            for r in &h.results {
                out.push_str(&format!(
                    "{} ok={} {} {}ms {}\n",
                    r.ts.format("%H:%M:%S%.3f"),
                    r.ok,
                    serde_json::to_value(r.outcome)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    r.latency_ms,
                    r.detail
                ));
            }
            out.push_str(&format!(
                "consecutive failures: {}, transitions in the last 60 s: {}\n",
                h.consecutive_failures, h.transitions_60s
            ));
        }
        _ => out.push_str("no probe results\n"),
    }
    out.push('\n');
    let mut q = QueryLogsParams::default();
    q.filter.stems = vec![stem.to_string()];
    q.filter.level = Some("error+".into());
    q.filter.tail = Some(ERROR_LINES);
    let logs: Result<QueryLogsResult, Error> = c.call(Method::QUERY_LOGS, &q).await;
    out.push_str(&format!("## Last {ERROR_LINES} error-level log lines\n"));
    match logs {
        Ok(l) if !l.records.is_empty() => {
            for r in &l.records {
                out.push_str(&log_line(r));
                out.push('\n');
            }
        }
        Ok(_) => out.push_str("none\n"),
        Err(e) => out.push_str(&format!("unavailable: {}\n", e.message)),
    }
    out.push('\n');
    let events: Result<EventsResult, Error> = c.call(Method::EVENTS, EventsParams::default()).await;
    out.push_str(&format!("## Recent events of {stem} (oldest first)\n"));
    match events {
        Ok(ev) => {
            let mine: Vec<_> = ev
                .events
                .iter()
                .filter(|e| e.stem.as_deref() == Some(stem))
                .collect();
            let start = mine.len().saturating_sub(EVENTS);
            if mine.is_empty() {
                out.push_str("none\n");
            }
            for e in &mine[start..] {
                out.push_str(&format!(
                    "#{} {} {}{}{} actor={}{}\n",
                    e.seq,
                    e.ts.format("%H:%M:%S%.3f"),
                    e.kind,
                    e.from
                        .as_deref()
                        .map(|f| format!(" {f}"))
                        .unwrap_or_default(),
                    e.to.as_deref()
                        .map(|t| format!(" -> {t}"))
                        .unwrap_or_default(),
                    e.actor,
                    e.reason
                        .as_deref()
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default(),
                ));
            }
        }
        Err(e) => out.push_str(&format!("unavailable: {}\n", e.message)),
    }
    out.push('\n');
    out.push_str(&instructions(stem));
    Ok(out)
}

fn instructions(stem: &str) -> String {
    format!(
        "## What to do\n\
1. From the status, health history, errors and events above, state the most likely cause of the problem with `{stem}` (process crash, failing health probe, dependency down, port conflict, bad config, ...).\n\
2. Confirm it with the tools: `get_logs` with {{\"stems\": [\"{stem}\"], \"level\": \"warn+\"}} (page with `next_cursor`), `get_health`, `get_graph` with {{\"focus\": \"{stem}\"}} for dependencies, `get_config` with {{\"stem\": \"{stem}\"}}.\n\
3. Propose a fix. Restarting (`restart`) is fine; destructive actions (`reset`, `down` with `all`/`volumes`, `up` with `fresh`) need the user's explicit consent and `confirm: true`.\n"
    )
}

async fn bring_up(b: &Backend, actor: &str, profile: Option<&str>) -> String {
    let args = match profile {
        Some(p) => json!({ "profile": p }),
        None => json!({}),
    };
    let mut out = format!(
        "Bring the stems environment up{} and report.\n\n",
        profile
            .map(|p| format!(" with the profile `{p}`"))
            .unwrap_or_default()
    );
    match b.connect_existing(actor).await {
        Ok(c) => match c
            .call::<StatusResult>(Method::STATUS, StatusParams::default())
            .await
        {
            Ok(st) => {
                out.push_str("## Current status\n");
                for s in &st.stems {
                    out.push_str(&format!(
                        "{}: {}{}\n",
                        s.name,
                        s.glyph.name(),
                        s.reason
                            .as_deref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    ));
                }
                out.push('\n');
            }
            Err(e) => out.push_str(&format!("Status unavailable: {}\n\n", e.message)),
        },
        Err(_) => out.push_str(
            "The daemon is not running yet; `up` starts it when the server runs with --auto-start (otherwise ask the user to run `stems up`).\n\n",
        ),
    }
    out.push_str(&format!(
        "## Steps\n\
1. Call `up` with {args} and wait for the result (progress notifications show each stem's state).\n\
2. Call `get_status`. For every stem that is not `healthy`, call `get_logs` with its name and `\"level\": \"warn+\"`, and `get_health`.\n\
3. Report a table: stem, state, ports, and for failures the cause in one sentence with the relevant log line. Do not run destructive tools (`reset`, `down` with `all`/`volumes`, `up` with `fresh`) without the user's consent.\n"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_list() {
        let p = list();
        assert_eq!(p[0].name, "diagnose_stem");
        assert_eq!(p[0].arguments.as_ref().unwrap()[0].required, Some(true));
        assert_eq!(p[1].name, "bring_up_and_report");
        assert!(instructions("api").contains("\"focus\": \"api\""));
    }
}
