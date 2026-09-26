//! `stems status [stems…]` (FR-HS-2; the full table, external probes and
//! `--watch` land in 13).
//!
//! `data: { stems: [ { name, type, state, glyph, reason, pid, pgid, ports:
//! [{name, port, auto}], uptime_s, started_at, restarts, health, error } ],
//! summary: { healthy, degraded, failed, stopped, unknown, starting } }`.
//! With `-v`/`--verbose` each running stem also has `env`: the resolved
//! environment its process was started with (for debugging substitution).

use serde_json::json;
use stems_api::{Method, StatusParams, StatusResult};
use stems_core::{Error, Errors};

use crate::cli::StatusArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Run `status`.
pub fn run(ctx: &Ctx, args: &StatusArgs) -> CommandOutput {
    if args.watch.is_some() {
        return CommandOutput::failed(
            Error::not_implemented("`stems status --watch`", "13")
                .with_details(json!({ "flag": "--watch", "deliverable": "13" })),
        );
    }
    block_on(async {
        let c = client::connect(ctx).await?;
        let st: StatusResult = c
            .call(
                Method::STATUS,
                StatusParams {
                    stems: args.stems.clone(),
                    verbose: ctx.global.verbose > 0,
                },
            )
            .await?;
        let data = serde_json::to_value(&st).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(table(&st)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

fn uptime(s: Option<u64>) -> String {
    match s {
        None => "-".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        Some(s) => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The plain status table (13 replaces it with the final layout).
pub fn table(st: &StatusResult) -> String {
    let mut rows = vec![[
        "".to_string(),
        "STEM".into(),
        "TYPE".into(),
        "STATE".into(),
        "PID".into(),
        "PORTS".into(),
        "UPTIME".into(),
    ]];
    for s in &st.stems {
        let ports = s
            .ports
            .iter()
            .map(|p| p.port.map_or_else(|| "auto".to_string(), |n| n.to_string()))
            .collect::<Vec<_>>()
            .join(",");
        rows.push([
            s.glyph.unicode().to_string(),
            s.name.clone(),
            s.kind.clone(),
            s.state.to_string(),
            s.pid.map_or_else(|| "-".into(), |p| p.to_string()),
            if ports.is_empty() { "-".into() } else { ports },
            uptime(s.uptime_s),
        ]);
    }
    let widths: Vec<usize> = (0..7)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for r in &rows {
        let line: Vec<String> = r
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    let m = &st.summary;
    out.push_str(&format!(
        "{} healthy, {} degraded, {} failed, {} starting, {} stopped, {} unknown\n",
        m.healthy, m.degraded, m.failed, m.starting, m.stopped, m.unknown
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::{PortStatus, StatusSummary, StemStatus};
    use stems_core::StemState;

    #[test]
    fn table_golden() {
        let stem = |name: &str, state: StemState, pid: Option<i32>, port: Option<u16>| StemStatus {
            name: name.into(),
            kind: "process".into(),
            state,
            glyph: state.glyph(false),
            reason: None,
            pid,
            pgid: pid,
            ports: vec![PortStatus {
                name: "http".into(),
                port,
                auto: port.is_none(),
            }],
            uptime_s: pid.map(|_| 75),
            started_at: None,
            restarts: 0,
            health: None,
            error: None,
            env: None,
        };
        let stems = vec![
            stem("echo-svc", StemState::Healthy, Some(4242), Some(18090)),
            stem("web", StemState::Stopped, None, None),
        ];
        let st = StatusResult {
            summary: StatusSummary::of(&stems),
            stems,
        };
        insta::assert_snapshot!("status-table", table(&st));
    }
}
