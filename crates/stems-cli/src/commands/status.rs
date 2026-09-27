//! `stems status [stems…] [--watch [<interval>]]` (FR-HS-2, FR-HS-3).
//!
//! JSON: `data: { stems: [ { name, type, state, glyph, reason, pid, pgid,
//! ports: [{name, port, auto}], uptime_s, started_at, restarts, health,
//! error } ], summary: { healthy, degraded, failed, unhealthy, stopped,
//! unknown, starting } }` (`unhealthy`: running, health check failing;
//! counted apart from `failed`). With `-v`/`--verbose` each running stem also has `env`:
//! the resolved environment its process was started with (for debugging
//! substitution).
//!
//! Human: the table `STEM TYPE STATUS REASON PID PORTS UPTIME RESTARTS`
//! ([`table`]) and a summary line. `STATUS` is the glyph
//! ([`stems_core::Glyph::cell`]) and the state. Glyphs fall back to ASCII
//! (`OK FAIL WARN - ? ..`) with `--no-color`/`STEMS_NO_COLOR`,
//! `STEMS_ASCII=1`, or a non-UTF-8 locale; colour only on a terminal. Long
//! stem names and reasons are cut with `…` to fit `COLUMNS` (or the
//! terminal width).
//!
//! `--watch [<interval>]` (default `1s`; a bare number is seconds, minimum
//! 100 ms) redraws until Ctrl-C / SIGTERM (exit 0): on a terminal the screen
//! is cleared before each frame, on a pipe frames are separated by a blank
//! line; in JSON mode each frame is one NDJSON line holding `data`. A failed
//! refresh (e.g. the daemon stopped) is shown in the frame and retried.

use std::io::Write;
use std::time::Duration;

use serde_json::json;
use stems_api::client::Client;
use stems_api::{Method, StatusParams, StatusResult, StatusSummary, StemStatus};
use stems_core::{Error, Errors};

use crate::cli::StatusArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode};

/// Shortest `--watch` interval.
const MIN_WATCH: Duration = Duration::from_millis(100);
/// Widest `STEM` cell before truncation.
const MAX_STEM: usize = 24;
/// Widest `REASON` cell before truncation.
const MAX_REASON: usize = 40;

/// How the human table is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// ASCII glyphs and `...` instead of `…`.
    pub ascii: bool,
    /// ANSI colour for the glyph.
    pub color: bool,
    /// Total width to fit in (`None`: only the per-column caps apply).
    pub width: Option<usize>,
}

impl Style {
    /// The style for this invocation: colour only on a terminal without
    /// `--no-color`; ASCII with `--no-color`, `STEMS_ASCII=1` or a
    /// non-UTF-8 locale; width from `COLUMNS`, else the terminal.
    pub fn detect(ctx: &Ctx, mode: Mode) -> Self {
        use std::io::IsTerminal;
        let tty = std::io::stdout().is_terminal();
        let no_color = ctx.global.no_color;
        let ascii = no_color || stems_core::status::prefers_ascii(|k| ctx.env.get(k).cloned());
        let width = ctx
            .env
            .get("COLUMNS")
            .and_then(|c| c.trim().parse::<usize>().ok())
            .filter(|w| *w > 0)
            .or_else(|| tty.then(terminal_width).flatten());
        Self {
            ascii,
            color: mode == Mode::Human && tty && !no_color,
            width,
        }
    }
}

/// Columns of the terminal on stdout, if it is one.
fn terminal_width() -> Option<usize> {
    // SAFETY: TIOCGWINSZ on fd 1 into a zeroed winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        (libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0)
            .then_some(usize::from(ws.ws_col))
    }
}

/// Run `status`.
pub fn run(ctx: &Ctx, args: &StatusArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let params = StatusParams {
        stems: args.stems.clone(),
        verbose: ctx.global.verbose > 0,
    };
    if let Some(raw) = &args.watch {
        let every = match parse_interval(raw) {
            Ok(d) => d,
            Err(e) => return CommandOutput::failed(e),
        };
        return block_on(watch(ctx, params, every, mode, stdout))
            .unwrap_or_else(CommandOutput::failed);
    }
    let style = Style::detect(ctx, mode);
    block_on(async {
        let c = client::connect(ctx).await?;
        let st: StatusResult = c.call(Method::STATUS, params).await?;
        let data = serde_json::to_value(&st).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(table(&st, &style)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `--watch` interval: a bare number of seconds (`0.5`) or a duration
/// (`500ms`, `2s`); at least [`MIN_WATCH`].
pub fn parse_interval(raw: &str) -> Result<Duration, Error> {
    let t = raw.trim();
    let d = match t.parse::<f64>() {
        Ok(n) => Duration::try_from_secs_f64(n).ok(),
        Err(_) => stems_core::logs::parse_duration(t).ok(),
    };
    d.map(|d| d.max(MIN_WATCH)).ok_or_else(|| {
        Error::usage(
            format!("invalid --watch interval `{raw}`"),
            "use seconds (`--watch 0.5`) or a duration (`--watch 2s`, `--watch 500ms`)",
        )
        .with_details(json!({ "flag": "--watch", "value": raw }))
    })
}

async fn watch(
    ctx: &Ctx,
    params: StatusParams,
    every: Duration,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    use std::io::IsTerminal;
    use tokio::signal::unix::{SignalKind, signal};
    let tty = std::io::stdout().is_terminal();
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut term = signal(SignalKind::terminate()).ok();
    let mut conn = None;
    let mut first = true;
    loop {
        let res = refresh(ctx, &mut conn, &params).await;
        let frame = match (&res, mode) {
            (Err(e), _) if first => return Err(e.clone()),
            (Ok(st), Mode::Json) => serde_json::to_string(st).unwrap_or_default() + "\n",
            (Err(e), Mode::Json) => {
                serde_json::to_string(&json!({ "errors": e })).unwrap_or_default() + "\n"
            }
            (Ok(st), Mode::Human) => table(st, &Style::detect(ctx, mode)),
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
            break; // reader went away (e.g. `| head`)
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

/// One `status` call, (re)connecting when needed; the connection is
/// dropped after a failure so the next refresh reconnects.
async fn refresh(
    ctx: &Ctx,
    conn: &mut Option<Client>,
    params: &StatusParams,
) -> Result<StatusResult, Errors> {
    let c = match conn {
        Some(c) => c,
        None => conn.insert(client::connect(ctx).await?),
    };
    let r = c.call(Method::STATUS, params).await.map_err(Errors::from);
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

fn uptime(s: Option<u64>) -> String {
    match s {
        None => "-".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        Some(s) => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `text` cut to at most `max` characters, ending in `…` (`...` in ASCII)
/// when cut.
pub fn truncate(text: &str, max: usize, ascii: bool) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let ell = if ascii { "..." } else { "…" };
    let keep = max.saturating_sub(ell.chars().count());
    if keep == 0 {
        return text.chars().take(max).collect();
    }
    let mut s: String = text.chars().take(keep).collect();
    s.push_str(ell);
    s
}

const HEADER: [&str; 8] = [
    "STEM", "TYPE", "STATUS", "REASON", "PID", "PORTS", "UPTIME", "RESTARTS",
];
const COL_STEM: usize = 0;
const COL_STATUS: usize = 2;
const COL_REASON: usize = 3;
const COL_PORTS: usize = 5;
const GAP: usize = 2;

fn cells(s: &StemStatus, style: &Style) -> [String; 8] {
    let ports = s
        .ports
        .iter()
        .map(|p| {
            let n = p.port.map_or_else(|| "auto".to_string(), |n| n.to_string());
            format!("{}:{n}", p.name)
        })
        .collect::<Vec<_>>()
        .join(",");
    let reason = s
        .reason
        .as_deref()
        .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|r| !r.is_empty());
    [
        s.name.clone(),
        s.kind.clone(),
        format!("{} {}", s.glyph.symbol(!style.ascii), s.state),
        reason.unwrap_or_else(|| "-".into()),
        s.pid.map_or_else(|| "-".into(), |p| p.to_string()),
        if ports.is_empty() { "-".into() } else { ports },
        uptime(s.uptime_s),
        s.restarts.to_string(),
    ]
}

/// The human status table plus a summary line.
///
/// Every cell is plain text for width computations; the glyph is coloured
/// only when printed. Caps: `STEM` [`MAX_STEM`], `REASON` [`MAX_REASON`];
/// with a `width`, `REASON`, then `STEM`, then `PORTS` shrink (to 10, 8 and
/// 8 columns at least) until the table fits.
pub fn table(st: &StatusResult, style: &Style) -> String {
    let mut rows: Vec<[String; 8]> = vec![HEADER.map(str::to_string)];
    rows.extend(st.stems.iter().map(|s| cells(s, style)));
    let natural = |i: usize| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
    let mut widths: Vec<usize> = (0..HEADER.len()).map(natural).collect();
    widths[COL_STEM] = widths[COL_STEM].min(MAX_STEM.max(HEADER[COL_STEM].len()));
    widths[COL_REASON] = widths[COL_REASON].min(MAX_REASON);
    if let Some(total) = style.width {
        for (col, min) in [(COL_REASON, 10), (COL_STEM, 8), (COL_PORTS, 8)] {
            let used: usize = widths.iter().sum::<usize>() + GAP * (widths.len() - 1);
            if used <= total {
                break;
            }
            let floor = min.max(HEADER[col].len()).min(widths[col]);
            widths[col] = widths[col].saturating_sub(used - total).max(floor);
        }
    }
    let mut out = String::new();
    for (r, row) in rows.iter().enumerate() {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            let w = widths[i];
            let text = truncate(cell, w, style.ascii);
            let pad = w.saturating_sub(text.chars().count());
            if i > 0 {
                line.push_str(&" ".repeat(GAP));
            }
            if i == COL_STATUS && r > 0 && style.color {
                let g = st.stems[r - 1].glyph;
                let sym = g.symbol(!style.ascii);
                line.push_str(&g.cell(style.ascii, true));
                line.push_str(text.strip_prefix(sym).unwrap_or(&text));
            } else {
                line.push_str(&text);
            }
            line.push_str(&" ".repeat(pad));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&summary_line(&st.summary, style.width));
    out.push('\n');
    out
}

/// `1 healthy, 0 degraded, 0 failed, 0 unhealthy, 0 starting, 0 stopped,
/// 0 unknown`; when that is wider than `width`, only the non-zero counts.
fn summary_line(m: &StatusSummary, width: Option<usize>) -> String {
    let parts = [
        (m.healthy, "healthy"),
        (m.degraded, "degraded"),
        (m.failed, "failed"),
        (m.unhealthy, "unhealthy"),
        (m.starting, "starting"),
        (m.stopped, "stopped"),
        (m.unknown, "unknown"),
    ];
    let join = |all: bool| {
        parts
            .iter()
            .filter(|(n, _)| all || *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let full = join(true);
    match width {
        Some(w) if full.chars().count() > w && parts.iter().any(|(n, _)| *n > 0) => join(false),
        _ => full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::{PortStatus, StatusSummary, StemStatus};
    use stems_core::{Glyph, StemState};

    #[test]
    fn table_golden() {
        let stem = |name: &str, state: StemState, pid: Option<i32>, port: Option<u16>| StemStatus {
            name: name.into(),
            kind: "process".into(),
            state,
            glyph: state.glyph(false),
            reason: None,
            degraded: false,
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
            restarts_in_window: 0,
            seeded: false,
            health: None,
            metrics: None,
            error: None,
            env: None,
            outputs: Default::default(),
            watch: None,
            variant: None,
            cascade: None,
        };
        let mut hosted = stem("hosted", StemState::Unknown, None, None);
        hosted.kind = "external".into();
        hosted.ports.clear();
        hosted.reason = Some("external: not managed by stems (no health probe yet)".into());
        let mut web = stem("web", StemState::Stopped, None, None);
        web.restarts = 2;
        let stems = vec![
            stem("echo-svc", StemState::Healthy, Some(4242), Some(18090)),
            web,
            hosted,
        ];
        let st = StatusResult {
            summary: StatusSummary::of(&stems),
            stems,
        };
        insta::assert_snapshot!("status-table", table(&st, &Style::default()));
        let ascii = Style {
            ascii: true,
            ..Style::default()
        };
        insta::assert_snapshot!("status-table-ascii", table(&st, &ascii));
    }

    fn one(name: &str, reason: &str) -> StatusResult {
        let stems = vec![StemStatus {
            name: name.into(),
            kind: "process".into(),
            state: StemState::Failed,
            glyph: Glyph::Failed,
            reason: Some(reason.into()),
            degraded: false,
            pid: None,
            pgid: None,
            ports: vec![],
            uptime_s: None,
            started_at: None,
            restarts: 7,
            restarts_in_window: 0,
            seeded: false,
            health: None,
            metrics: None,
            error: None,
            env: None,
            outputs: Default::default(),
            watch: None,
            variant: None,
            cascade: None,
        }];
        StatusResult {
            summary: StatusSummary::of(&stems),
            stems,
        }
    }

    #[test]
    fn long_names_and_reasons_are_truncated_to_fit() {
        let st = one(
            "a-really-quite-extraordinarily-long-stem-name",
            "`a-really-quite-extraordinarily-long-stem-name` exited with code 1 before it became ready",
        );
        // Per-column caps only.
        insta::assert_snapshot!("status-table-long", table(&st, &Style::default()));
        // Fitted to 72 columns: REASON shrinks first (to 10), then STEM.
        let narrow = Style {
            width: Some(72),
            ..Style::default()
        };
        let out = table(&st, &narrow);
        insta::assert_snapshot!("status-table-narrow", out);
        assert!(out.lines().all(|l| l.chars().count() <= 72), "{out}");
        let ascii = Style {
            ascii: true,
            width: Some(60),
            ..Style::default()
        };
        assert!(table(&st, &ascii).contains("..."));
        assert!(table(&st, &ascii).contains("FAIL failed"));
    }

    #[test]
    fn color_wraps_only_the_glyph() {
        let st = one("api", "boom");
        let style = Style {
            color: true,
            ..Style::default()
        };
        let out = table(&st, &style);
        assert!(out.contains("\x1b[31m✗\x1b[0m failed"), "{out}");
        let plain: String = out.replace("\x1b[31m", "").replace("\x1b[0m", "");
        assert_eq!(plain, table(&st, &Style::default()));
    }

    #[test]
    fn truncation_and_intervals() {
        assert_eq!(truncate("abcdef", 6, false), "abcdef");
        assert_eq!(truncate("abcdefg", 6, false), "abcde…");
        assert_eq!(truncate("abcdefg", 6, true), "abc...");
        assert_eq!(truncate("abcdefg", 2, true), "ab");
        assert_eq!(parse_interval("0.2").unwrap(), Duration::from_millis(200));
        assert_eq!(parse_interval("2s").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_interval("0").unwrap(), MIN_WATCH);
        assert_eq!(parse_interval("250ms").unwrap(), Duration::from_millis(250));
        assert!(parse_interval("soon").is_err());
        assert!(parse_interval("-1").is_err());
    }
}
