//! `stems watch pause|resume [stem…]` and `stems watch status [--json]`
//! (deliverable 24, FR-WD-2, `docs/watchdogs.md`).
//!
//! JSON: `pause`/`resume` → `data = WatchPauseResult { stems, global_paused }`;
//! `status` → `data = WatchStatusResult { stems: [{name, rules: [{paths,
//! ignore, action, debounce_ms, settle_ms, root, dir}], paused, active,
//! last_triggered, pending, busy}], global_paused, disabled }`. Human
//! `status`: one row per rule, `STEM RULE PATHS ACTION DEBOUNCE STATE LAST`.

use stems_api::{Method, WatchPauseParams, WatchPauseResult, WatchStatusParams, WatchStatusResult};
use stems_core::Errors;

use crate::cli::{WatchCommand, WatchStemArgs};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Run `stems watch <sub>`.
pub fn run(ctx: &Ctx, cmd: &WatchCommand) -> CommandOutput {
    match cmd {
        WatchCommand::Pause(a) => pause(ctx, a, true),
        WatchCommand::Resume(a) => pause(ctx, a, false),
        WatchCommand::Status => status(ctx),
    }
}

/// The human line of a pause/resume.
pub fn pause_human(r: &WatchPauseResult, pause: bool) -> String {
    let verb = if pause { "paused" } else { "resumed" };
    if r.stems.is_empty() {
        format!("{verb} every watchdog\n")
    } else {
        let mut s = format!("{verb} the watchdogs of {}", r.stems.join(", "));
        if !pause && r.global_paused {
            s.push_str(" (still paused globally: run `stems watch resume`)");
        }
        s.push('\n');
        s
    }
}

fn pause(ctx: &Ctx, a: &WatchStemArgs, pause: bool) -> CommandOutput {
    let p = WatchPauseParams {
        stems: a.stems.clone(),
    };
    let method = if pause {
        Method::WATCH_PAUSE
    } else {
        Method::WATCH_RESUME
    };
    block_on(async {
        let c = client::connect(ctx).await?;
        let r: WatchPauseResult = c.call(method, &p).await?;
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(pause_human(&r, pause)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

fn ms(v: u64) -> String {
    if v >= 1000 && v.is_multiple_of(1000) {
        format!("{}s", v / 1000)
    } else {
        format!("{v}ms")
    }
}

/// The human table.
pub fn status_human(r: &WatchStatusResult) -> String {
    let mut rows: Vec<[String; 7]> = vec![
        [
            "STEM", "RULE", "PATHS", "ACTION", "DEBOUNCE", "STATE", "LAST",
        ]
        .map(str::to_string),
    ];
    for s in &r.stems {
        let state = if s.paused {
            "paused"
        } else if s.busy {
            "running"
        } else if s.pending {
            "pending"
        } else if s.active {
            "watching"
        } else {
            "off"
        };
        let last = s
            .last_triggered
            .map_or_else(|| "-".into(), |t| t.format("%H:%M:%S").to_string());
        for (i, rule) in s.rules.iter().enumerate() {
            let debounce = if rule.settle_ms > 0 {
                format!("{} (settle {})", ms(rule.debounce_ms), ms(rule.settle_ms))
            } else {
                ms(rule.debounce_ms)
            };
            rows.push([
                s.name.clone(),
                i.to_string(),
                rule.paths.join(","),
                rule.action.clone(),
                debounce,
                state.into(),
                last.clone(),
            ]);
        }
    }
    if rows.len() == 1 {
        return "no stem declares watch rules\n".into();
    }
    let widths: Vec<usize> = (0..7)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    if r.disabled {
        out.push_str("\nwatchdogs are off (`up --no-watch`); run `stems up` to turn them on\n");
    } else if r.global_paused {
        out.push_str("\nevery watchdog is paused; `stems watch resume` resumes them\n");
    }
    out
}

fn status(ctx: &Ctx) -> CommandOutput {
    block_on(async {
        let c = client::connect(ctx).await?;
        let r: WatchStatusResult = c
            .call(Method::WATCH_STATUS, &WatchStatusParams::default())
            .await?;
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(status_human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::{StemWatchStatus, WatchRuleStatus};

    fn rule(paths: &[&str], action: &str, debounce_ms: u64, settle_ms: u64) -> WatchRuleStatus {
        WatchRuleStatus {
            paths: paths.iter().map(|s| s.to_string()).collect(),
            ignore: vec![],
            action: action.into(),
            debounce_ms,
            settle_ms,
            root: "codebase".into(),
            dir: None,
        }
    }

    #[test]
    fn status_table() {
        let r = WatchStatusResult {
            stems: vec![
                StemWatchStatus {
                    name: "api".into(),
                    rules: vec![
                        rule(&["*.py"], "restart", 500, 0),
                        rule(&["Dockerfile"], "rebuild", 1000, 2000),
                    ],
                    paused: false,
                    active: true,
                    last_triggered: None,
                    pending: false,
                    busy: false,
                },
                StemWatchStatus {
                    name: "web".into(),
                    rules: vec![rule(&["src/**"], "script:lint", 200, 0)],
                    paused: true,
                    active: true,
                    last_triggered: None,
                    pending: false,
                    busy: false,
                },
            ],
            global_paused: false,
            disabled: false,
        };
        insta::assert_snapshot!(status_human(&r), @r"
        STEM  RULE  PATHS       ACTION       DEBOUNCE        STATE     LAST
        api   0     *.py        restart      500ms           watching  -
        api   1     Dockerfile  rebuild      1s (settle 2s)  watching  -
        web   0     src/**      script:lint  200ms           paused    -
        ");
        assert_eq!(
            status_human(&WatchStatusResult::default()),
            "no stem declares watch rules\n"
        );
    }

    #[test]
    fn pause_lines() {
        let all = WatchPauseResult::default();
        assert_eq!(pause_human(&all, true), "paused every watchdog\n");
        let some = WatchPauseResult {
            stems: vec!["api".into()],
            global_paused: true,
        };
        assert_eq!(
            pause_human(&some, false),
            "resumed the watchdogs of api (still paused globally: run `stems watch resume`)\n"
        );
    }
}
