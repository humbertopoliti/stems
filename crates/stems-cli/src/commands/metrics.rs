//! `stems metrics [stem…] [--json] [--watch [<interval>]] [--history 5m]
//! [--sort cpu|mem] [--disk]` (deliverable 25, FR-MT-1..4, `docs/metrics.md`).
//!
//! JSON: `data = MetricsResult { interval_ms, stems: [{name, type, state,
//! latest: {ts, cpu_pct, rss_bytes, children, uptime_s, restarts} | null,
//! history?: [sample], open_ports, limits: [{metric, limit, for_s,
//! crossed}], disk?: {codebase_build_bytes, dirs, volumes_bytes}}],
//! totals: {cpu_pct, rss_bytes, children, disk_bytes?}}`. `--sort` orders
//! the stems highest first (JSON too); `--history` adds the samples of that
//! window; `--disk` measures build outputs and volumes (slow, cached 60 s).
//!
//! Human: `STEM CPU% MEM CHILDREN UPTIME RESTARTS CPU-SPARK MEM-SPARK`
//! (plus `DISK` with `--disk`) and a `TOTAL` row. Sparklines show the last
//! 30 samples (ASCII with `--no-color`, `STEMS_ASCII=1` or a non-UTF-8
//! locale). `--watch` redraws like `status --watch`.

use std::io::Write;
use std::time::Duration;

use serde_json::json;
use stems_api::client::Client;
use stems_api::metrics::SPARK_SAMPLES;
use stems_api::{Method, MetricsParams, MetricsResult, MetricsSort, StemMetrics};
use stems_core::metrics::{SparkStyle, sparkline_with};
use stems_core::{Error, Errors};

use crate::cli::{MetricsArgs, MetricsSort as SortArg};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::status::{Style, parse_interval};
use crate::output::{CommandOutput, Mode};

/// The RPC params for `args` (`human`: also the last 30 samples, for the
/// sparklines).
pub fn params(args: &MetricsArgs, human: bool) -> Result<MetricsParams, Error> {
    let history_ms = match &args.history {
        Some(raw) => Some(
            stems_core::logs::parse_duration(raw)
                .map_err(|_| {
                    Error::usage(
                        format!("invalid --history window `{raw}`"),
                        "use a duration such as `30s`, `5m` or `1h`",
                    )
                    .with_details(json!({ "flag": "--history", "value": raw }))
                })?
                .as_millis() as u64,
        ),
        None => None,
    };
    Ok(MetricsParams {
        stems: args.stems.clone(),
        history_ms,
        last: (human && history_ms.is_none()).then_some(SPARK_SAMPLES),
        sort: args.sort.map(|s| match s {
            SortArg::Cpu => MetricsSort::Cpu,
            SortArg::Mem => MetricsSort::Mem,
        }),
        disk: args.disk,
    })
}

/// Run `metrics`.
pub fn run(ctx: &Ctx, args: &MetricsArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let p = match params(args, mode == Mode::Human) {
        Ok(p) => p,
        Err(e) => return CommandOutput::failed(e),
    };
    if let Some(raw) = &args.watch {
        let every = match parse_interval(raw) {
            Ok(d) => d,
            Err(e) => return CommandOutput::failed(e),
        };
        return block_on(watch(ctx, p, every, mode, stdout)).unwrap_or_else(CommandOutput::failed);
    }
    let style = Style::detect(ctx, mode);
    block_on(async {
        let c = client::connect(ctx).await?;
        let r: MetricsResult = c.call(Method::METRICS, &p).await?;
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(table(&r, style.ascii)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

async fn watch(
    ctx: &Ctx,
    p: MetricsParams,
    every: Duration,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    use std::io::IsTerminal;
    use tokio::signal::unix::{SignalKind, signal};
    let tty = std::io::stdout().is_terminal();
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut term = signal(SignalKind::terminate()).ok();
    let mut conn: Option<Client> = None;
    let mut first = true;
    loop {
        let res = refresh(ctx, &mut conn, &p).await;
        let frame = match (&res, mode) {
            (Err(e), _) if first => return Err(e.clone()),
            (Ok(r), Mode::Json) => serde_json::to_string(r).unwrap_or_default() + "\n",
            (Err(e), Mode::Json) => {
                serde_json::to_string(&json!({ "errors": e })).unwrap_or_default() + "\n"
            }
            (Ok(r), Mode::Human) => table(r, Style::detect(ctx, mode).ascii),
            (Err(e), Mode::Human) => crate::output::human_errors(e),
        };
        let prefix = match mode {
            Mode::Human if tty => "\x1b[H\x1b[2J",
            Mode::Human if !first => "\n",
            _ => "",
        };
        first = false;
        if write!(stdout, "{prefix}{frame}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = recv(&mut int) => break,
            () = recv(&mut term) => break,
        }
    }
    Ok(CommandOutput::data(json!(null))
        .with_ndjson("")
        .with_human(""))
}

async fn refresh(
    ctx: &Ctx,
    conn: &mut Option<Client>,
    p: &MetricsParams,
) -> Result<MetricsResult, Errors> {
    let c = match conn {
        Some(c) => c,
        None => conn.insert(client::connect(ctx).await?),
    };
    let r = c.call(Method::METRICS, p).await.map_err(Errors::from);
    if r.is_err() {
        *conn = None;
    }
    r
}

async fn recv(s: &mut Option<tokio::signal::unix::Signal>) {
    match s {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

/// `1.5GB`, `120.3MB`, `512KB`, `900B` (1KB = 1024 B).
pub fn bytes(n: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TB", 1 << 40),
        ("GB", 1 << 30),
        ("MB", 1 << 20),
        ("KB", 1 << 10),
    ];
    for (u, m) in UNITS {
        if n >= m {
            return format!("{:.1}{u}", n as f64 / m as f64);
        }
    }
    format!("{n}B")
}

fn uptime(s: u64) -> String {
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// CPU and memory sparklines of a stem's samples (the last 30).
fn sparks(s: &StemMetrics, ascii: bool) -> (String, String) {
    let hist = s.history.as_deref().unwrap_or_default();
    let hist = &hist[hist.len().saturating_sub(SPARK_SAMPLES)..];
    if hist.is_empty() {
        return ("-".into(), "-".into());
    }
    let style = if ascii {
        SparkStyle::Ascii
    } else {
        SparkStyle::Unicode
    };
    let cpu: Vec<f64> = hist.iter().map(|x| x.cpu_pct).collect();
    let mem: Vec<f64> = hist.iter().map(|x| x.rss_bytes as f64).collect();
    let top = cpu.iter().copied().fold(100.0, f64::max);
    let w = hist.len();
    (
        sparkline_with(&cpu, w, style, Some((0.0, top))),
        sparkline_with(&mem, w, style, None),
    )
}

/// The human table with a `TOTAL` row.
pub fn table(r: &MetricsResult, ascii: bool) -> String {
    let disk = r.stems.iter().any(|s| s.disk.is_some());
    let mut header = vec![
        "STEM",
        "CPU%",
        "MEM",
        "CHILDREN",
        "UPTIME",
        "RESTARTS",
        "CPU-SPARK",
        "MEM-SPARK",
    ];
    if disk {
        header.push("DISK");
    }
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|h| (*h).to_string()).collect()];
    for s in &r.stems {
        let (cpu_spark, mem_spark) = sparks(s, ascii);
        let mut row = match &s.latest {
            Some(l) => vec![
                s.name.clone(),
                format!("{:.1}", l.cpu_pct),
                bytes(l.rss_bytes),
                l.children.to_string(),
                uptime(l.uptime_s),
                l.restarts.to_string(),
            ],
            None => vec![
                s.name.clone(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
            ],
        };
        row.push(cpu_spark);
        row.push(mem_spark);
        if disk {
            row.push(s.disk.as_ref().map_or_else(
                || "-".into(),
                |d| bytes(d.codebase_build_bytes + d.volumes_bytes.unwrap_or(0)),
            ));
        }
        rows.push(row);
    }
    let t = &r.totals;
    let mut total = vec![
        "TOTAL".to_string(),
        format!("{:.1}", t.cpu_pct),
        bytes(t.rss_bytes),
        t.children.to_string(),
        "-".into(),
        "-".into(),
        "-".into(),
        "-".into(),
    ];
    if disk {
        total.push(bytes(t.disk_bytes.unwrap_or(0)));
    }
    rows.push(total);
    let widths: Vec<usize> = (0..header.len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i].saturating_sub(c.chars().count());
                format!("{c}{}", " ".repeat(pad))
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use stems_api::{DiskUsage, MetricsTotals};
    use stems_core::StemState;
    use stems_core::metrics::Sample;

    fn sample(i: u32, cpu: f64, mb: u64) -> Sample {
        Sample {
            ts: Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, i).unwrap(),
            cpu_pct: cpu,
            rss_bytes: mb << 20,
            children: 1,
            uptime_s: 3725,
            restarts: 2,
        }
    }

    fn result(disk: bool) -> MetricsResult {
        let hist: Vec<Sample> = (0..8)
            .map(|i| sample(i, f64::from(i) * 15.0, 20 + u64::from(i) * 10))
            .collect();
        let mut stems = vec![
            StemMetrics {
                name: "shop-api".into(),
                kind: "process".into(),
                state: StemState::Healthy,
                latest: hist.last().cloned(),
                history: Some(hist),
                open_ports: vec![18090],
                limits: vec![],
                disk: disk.then(|| DiskUsage {
                    codebase_build_bytes: 3 << 20,
                    ..Default::default()
                }),
            },
            StemMetrics {
                name: "worker".into(),
                kind: "process".into(),
                state: StemState::Stopped,
                latest: None,
                history: Some(vec![]),
                open_ports: vec![],
                limits: vec![],
                disk: disk.then(DiskUsage::default),
            },
        ];
        stems[0].latest.as_mut().unwrap().children = 3;
        let totals = MetricsTotals::of(&stems);
        MetricsResult {
            interval_ms: 2000,
            stems,
            totals,
        }
    }

    #[test]
    fn human_table_golden() {
        insta::assert_snapshot!(table(&result(false), false), @r"
        STEM      CPU%   MEM     CHILDREN  UPTIME  RESTARTS  CPU-SPARK  MEM-SPARK
        shop-api  105.0  90.0MB  3         1h02m   2         ▁▂▃▄▅▆▇█   ▃▃▄▅▆▆▇█
        worker    -      -       -         -       -         -          -
        TOTAL     105.0  90.0MB  3         -       -         -          -
        ");
        insta::assert_snapshot!(table(&result(true), true), @r"
        STEM      CPU%   MEM     CHILDREN  UPTIME  RESTARTS  CPU-SPARK  MEM-SPARK  DISK
        shop-api  105.0  90.0MB  3         1h02m   2         _.,-~=+#   ,,-~==+#   3.0MB
        worker    -      -       -         -       -         -          -          0B
        TOTAL     105.0  90.0MB  3         -       -         -          -          3.0MB
        ");
    }

    #[test]
    fn byte_units() {
        assert_eq!(bytes(900), "900B");
        assert_eq!(bytes(1536), "1.5KB");
        assert_eq!(bytes(150 << 20), "150.0MB");
        assert_eq!(bytes(3 << 30), "3.0GB");
    }

    #[test]
    fn params_from_args() {
        let args = MetricsArgs {
            stems: vec!["a".into()],
            watch: None,
            history: Some("5m".into()),
            sort: Some(SortArg::Mem),
            disk: true,
        };
        let p = params(&args, true).unwrap();
        assert_eq!(p.history_ms, Some(300_000));
        assert_eq!(p.last, None);
        assert_eq!(p.sort, Some(MetricsSort::Mem));
        let args = MetricsArgs {
            history: None,
            ..args
        };
        assert_eq!(params(&args, true).unwrap().last, Some(30));
        assert_eq!(params(&args, false).unwrap().last, None);
        let bad = MetricsArgs {
            history: Some("soon".into()),
            ..args
        };
        let e = params(&bad, false).unwrap_err();
        assert_eq!(e.code, stems_core::ErrorCode::Usage);
    }
}
