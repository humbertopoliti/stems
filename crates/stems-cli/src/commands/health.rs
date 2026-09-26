//! `stems health [stem] [--last N]` (deliverable 21, `docs/health.md`): the
//! last health probe results per stem, from the daemon's ring of 50.
//!
//! JSON: `data = HealthResult { stems: [{name, type, state,
//! consecutive_failures, transitions_60s, results: [{ts, ok, outcome,
//! latency_ms, detail}]}] }` (results oldest first). Human: one row per
//! probe, `STEM TYPE OK LATENCY DETAIL TS`; a stem without results shows
//! one row with `-`.

use stems_api::{HealthParams, HealthResult, Method};
use stems_core::Errors;

use crate::cli::HealthArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::status::truncate;
use crate::output::CommandOutput;

/// Widest `DETAIL` cell.
const MAX_DETAIL: usize = 60;

/// The human table.
pub fn human(r: &HealthResult) -> String {
    let mut rows: Vec<[String; 6]> =
        vec![["STEM", "TYPE", "OK", "LATENCY", "DETAIL", "TS"].map(str::to_string)];
    for s in &r.stems {
        let kind = s.kind.clone().unwrap_or_else(|| "-".into());
        if s.results.is_empty() {
            rows.push([
                s.name.clone(),
                kind,
                "-".into(),
                "-".into(),
                format!("no probe yet ({})", s.state),
                "-".into(),
            ]);
            continue;
        }
        for p in &s.results {
            rows.push([
                s.name.clone(),
                kind.clone(),
                match p.outcome {
                    stems_api::ProbeOutcome::Ok => "yes".into(),
                    stems_api::ProbeOutcome::Fail => "no".into(),
                    stems_api::ProbeOutcome::Unknown => "?".into(),
                },
                format!("{}ms", p.latency_ms),
                truncate(&p.detail, MAX_DETAIL, false),
                p.ts.format("%H:%M:%S%.3f").to_string(),
            ]);
        }
    }
    let widths: Vec<usize> = (0..6)
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
    out
}

/// `stems health`.
pub fn run(ctx: &Ctx, args: &HealthArgs) -> CommandOutput {
    let p = HealthParams {
        stems: args.stem.iter().cloned().collect(),
        last: Some(
            args.last
                .unwrap_or(if args.stem.is_some() { 10 } else { 1 }),
        ),
    };
    block_on(async {
        let c = client::connect(ctx).await?;
        let r: HealthResult = c.call(Method::HEALTH, &p).await?;
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use stems_api::{ProbeOutcome, ProbeRecord, StemHealth};
    use stems_core::StemState;

    #[test]
    fn human_table() {
        let ts = DateTime::from_timestamp(3600, 5_000_000).unwrap();
        let rec = |ok: bool, outcome, ms, detail: &str| ProbeRecord {
            ts,
            ok,
            outcome,
            latency_ms: ms,
            detail: detail.into(),
        };
        let r = HealthResult {
            stems: vec![
                StemHealth {
                    name: "shop-api".into(),
                    kind: Some("http".into()),
                    state: StemState::Unhealthy,
                    consecutive_failures: 1,
                    transitions_60s: 1,
                    results: vec![
                        rec(true, ProbeOutcome::Ok, 3, "HTTP 200"),
                        rec(false, ProbeOutcome::Fail, 12, "HTTP 503 (want 2xx)"),
                    ],
                },
                StemHealth {
                    name: "hosted".into(),
                    kind: Some("http".into()),
                    state: StemState::Unknown,
                    consecutive_failures: 1,
                    transitions_60s: 0,
                    results: vec![rec(
                        false,
                        ProbeOutcome::Unknown,
                        1,
                        "cannot resolve host: nodename nor servname provided",
                    )],
                },
                StemHealth {
                    name: "db".into(),
                    kind: None,
                    state: StemState::Stopped,
                    consecutive_failures: 0,
                    transitions_60s: 0,
                    results: vec![],
                },
            ],
        };
        insta::assert_snapshot!(human(&r), @r"
        STEM      TYPE  OK   LATENCY  DETAIL                                               TS
        shop-api  http  yes  3ms      HTTP 200                                             01:00:00.005
        shop-api  http  no   12ms     HTTP 503 (want 2xx)                                  01:00:00.005
        hosted    http  ?    1ms      cannot resolve host: nodename nor servname provided  01:00:00.005
        db        -     -    -        no probe yet (stopped)                               -
        ");
    }
}
