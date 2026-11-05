//! `stems status [stems…] [--watch [<interval>]]` (FR-HS-2, FR-HS-3).
//!
//! JSON: `data: { stems: [ { name, type, state, glyph, reason, pid, pgid,
//! ports: [{name, port, auto}], uptime_s, started_at, restarts, health,
//! error } ], summary: { healthy, degraded, failed, unhealthy, stopped,
//! unknown, starting, disabled } }` (`unhealthy`: running, health check failing;
//! counted apart from `failed`). Disabled stems (46) come last with state
//! and glyph `disabled`, `enabled: false` and the reason (`enabled: false
//! in stems.local.yaml`); `--enabled` hides them (and their count). With `-v`/`--verbose` each running stem also has `env`:
//! the resolved environment its process was started with (for debugging
//! substitution).
//!
//! Human: the table `STEM TYPE STATUS REASON PID PORTS UPTIME RESTARTS`
//! ([`table`]) and a summary line; then, for each failed stem whose error
//! carries the last output lines of the attempt that died (36:
//! `details.stderr_tail` / `stdout_tail`), an `error <stem>: <message>`
//! block with those lines as `stderr | …`. `STATUS` is the glyph
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
//!
//! `--expect <healthy|stopped|failed|running> [--timeout D]` (49) checks
//! that every named (else every enabled) stem is in that state: `data` gets
//! `expect` and `mismatch: [{stem, state}]`; a mismatch is
//! `STATUS_MISMATCH` (exit 1). `--timeout` re-checks every 200 ms until the
//! deadline. Without a daemon every stem is `stopped` (`data = {stems:
//! [{name, state}], daemon: false, expect, mismatch}`), so `--expect
//! stopped` succeeds and any other state is `DAEMON_NOT_RUNNING` (exit 4).

use std::io::Write;
use std::time::Duration;

use serde_json::json;
use stems_api::client::Client;
use stems_api::{Method, StatusParams, StatusResult, StatusSummary, StemStatus};
use stems_core::{Error, ErrorCode, Errors, StemState};

use crate::cli::{ExpectState, StatusArgs};
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
    if let Some(want) = args.expect {
        let style = Style::detect(ctx, mode);
        return block_on(expect(ctx, args, params, want, &style))
            .unwrap_or_else(CommandOutput::failed);
    }
    if let Some(raw) = &args.watch {
        let every = match parse_interval(raw) {
            Ok(d) => d,
            Err(e) => return CommandOutput::failed(e),
        };
        return block_on(watch(ctx, params, every, args.enabled, mode, stdout))
            .unwrap_or_else(CommandOutput::failed);
    }
    let style = Style::detect(ctx, mode);
    let verbose = params.verbose;
    block_on(async {
        let c = client::connect(ctx).await?;
        let mut st: StatusResult = c.call(Method::STATUS, params).await?;
        if args.enabled {
            only_enabled(&mut st);
        }
        let data = serde_json::to_value(&st).unwrap_or_default();
        let mut human = table(&st, &style);
        if verbose {
            human.push_str(&warnings(&st));
        }
        Ok::<_, Errors>(CommandOutput::data(data).with_human(human))
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

/// Poll interval of `--expect --timeout`.
const EXPECT_POLL: Duration = Duration::from_millis(200);

/// The `--expect` name of a state.
fn expect_name(want: ExpectState) -> &'static str {
    match want {
        ExpectState::Healthy => "healthy",
        ExpectState::Stopped => "stopped",
        ExpectState::Failed => "failed",
        ExpectState::Running => "running",
    }
}

/// Is `state` what `--expect want` asks for?
pub fn expect_matches(want: ExpectState, state: StemState) -> bool {
    match want {
        ExpectState::Healthy => state == StemState::Healthy,
        ExpectState::Stopped => stems_daemon::supervisor::run::is_stopped(state),
        ExpectState::Failed => state == StemState::Failed,
        // Up or on its way up (every transitioning-or-up state).
        ExpectState::Running => matches!(
            state,
            StemState::Setup
                | StemState::Starting
                | StemState::Seeding
                | StemState::Healthy
                | StemState::Unhealthy
        ),
    }
}

/// `(stem, state)` of the checked stems not in `want`: the named ones (the
/// daemon already selected them), else every enabled stem.
fn mismatch(st: &StatusResult, named: bool, want: ExpectState) -> Vec<(String, StemState)> {
    st.stems
        .iter()
        .filter(|s| named || s.enabled)
        .filter(|s| !expect_matches(want, s.state))
        .map(|s| (s.name.clone(), s.state))
        .collect()
}

/// The stems `--expect` checks without a daemon: the named ones (each must
/// exist: `UNKNOWN_STEM`, like the daemon's `status`), else every enabled
/// stem.
fn no_daemon_names(ws: &stems_config::Workspace, named: &[String]) -> Result<Vec<String>, Errors> {
    if named.is_empty() {
        return Ok(ws.stems().map(|s| s.name.clone()).collect());
    }
    let unknown: Vec<Error> = named
        .iter()
        .filter(|n| ws.stem(n).is_none())
        .map(|n| {
            Error::new(
                ErrorCode::UnknownStem,
                format!("stem `{n}` does not exist in workspace `{}`", ws.name),
            )
            .with_hint(format!(
                "known stems: {}",
                ws.stems.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            .with_details(json!({ "stem": n }))
        })
        .collect();
    if unknown.is_empty() {
        Ok(named.to_vec())
    } else {
        Err(Errors(unknown))
    }
}

/// `status --expect` (49).
async fn expect(
    ctx: &Ctx,
    args: &StatusArgs,
    params: StatusParams,
    want: ExpectState,
    style: &Style,
) -> Result<CommandOutput, Errors> {
    let timeout = match &args.timeout {
        None => Duration::ZERO,
        Some(raw) => stems_core::logs::parse_duration(raw.trim()).map_err(|_| {
            Error::usage(
                format!("invalid --timeout `{raw}`"),
                "use a duration such as `30s`, `2m` or `500ms`",
            )
            .with_details(json!({ "flag": "--timeout", "value": raw }))
        })?,
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let named = !args.stems.is_empty();
    let mut conn = None;
    loop {
        let (mut data, human, wrong, no_daemon) = match refresh(ctx, &mut conn, &params).await {
            Ok(mut st) => {
                if args.enabled {
                    only_enabled(&mut st);
                }
                let wrong = mismatch(&st, named, want);
                let data = serde_json::to_value(&st).unwrap_or_default();
                (data, table(&st, style), wrong, None)
            }
            Err(e) if e.0.iter().any(|x| x.code == ErrorCode::DaemonNotRunning) => {
                // No daemon: every stem is stopped; named ones must exist.
                let ws = stems_config::load(ctx.load_options())
                    .map_err(Errors::from)?
                    .workspace;
                let names = no_daemon_names(&ws, &args.stems)?;
                let wrong: Vec<(String, StemState)> = names
                    .iter()
                    .filter(|_| !expect_matches(want, StemState::Stopped))
                    .map(|n| (n.clone(), StemState::Stopped))
                    .collect();
                let stems: Vec<_> = names
                    .iter()
                    .map(|n| json!({ "name": n, "state": StemState::Stopped }))
                    .collect();
                let human = format!(
                    "no daemon is running: every stem is stopped ({})\n",
                    names.join(", ")
                );
                (
                    json!({ "stems": stems, "daemon": false }),
                    human,
                    wrong,
                    Some(e),
                )
            }
            Err(e) => return Err(e),
        };
        let done = wrong.is_empty() || tokio::time::Instant::now() >= deadline;
        if !done {
            tokio::time::sleep_until(deadline.min(tokio::time::Instant::now() + EXPECT_POLL)).await;
            continue;
        }
        if !wrong.is_empty()
            && let Some(e) = no_daemon
        {
            return Err(e);
        }
        let name = expect_name(want);
        if let serde_json::Value::Object(m) = &mut data {
            m.insert("expect".into(), json!(name));
            m.insert(
                "mismatch".into(),
                json!(
                    wrong
                        .iter()
                        .map(|(n, s)| json!({ "stem": n, "state": s }))
                        .collect::<Vec<_>>()
                ),
            );
        }
        if wrong.is_empty() {
            let human = format!("{human}expect {name}: ok\n");
            return Ok(CommandOutput::data(data).with_human(human));
        }
        let list: Vec<String> = wrong.iter().map(|(n, s)| format!("{n} ({s})")).collect();
        let checked = data.get("stems").and_then(|s| s.as_array()).map_or(0, |a| {
            a.iter()
                .filter(|s| named || s.get("enabled") != Some(&json!(false)))
                .count()
        });
        let hint = if timeout.is_zero() {
            format!(
                "wait for it with `--timeout <duration>` (e.g. `stems status --expect {name} --timeout 30s`)"
            )
        } else {
            format!(
                "still not {name} after {}; see `stems status` and `stems logs`",
                args.timeout.as_deref().unwrap_or_default()
            )
        };
        let err = Error::new(
            ErrorCode::StatusMismatch,
            format!(
                "{} of {checked} stem(s) not {name}: {}",
                wrong.len(),
                list.join(", ")
            ),
        )
        .with_hint(hint)
        .with_details(json!({
            "expect": name,
            "mismatch": data.get("mismatch").cloned().unwrap_or_default(),
        }));
        let human = format!("{human}expect {name}: {}\n", list.join(", "));
        return Ok(CommandOutput::data(data)
            .with_human(human)
            .with_errors(Errors(vec![err])));
    }
}

/// `--enabled`: drop the disabled stems (46) and recount.
pub fn only_enabled(st: &mut StatusResult) {
    st.stems.retain(|s| s.enabled);
    st.summary = StatusSummary::of(&st.stems);
}

async fn watch(
    ctx: &Ctx,
    params: StatusParams,
    every: Duration,
    enabled_only: bool,
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
        let mut res = refresh(ctx, &mut conn, &params).await;
        if enabled_only && let Ok(st) = res.as_mut() {
            only_enabled(st);
        }
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
    out.push_str(&tails(st));
    out
}

/// The `error <stem>: <message>` blocks with the output tail of each failed
/// stem whose error has one (36).
fn tails(st: &StatusResult) -> String {
    let mut out = String::new();
    for s in &st.stems {
        let Some(e) = &s.error else { continue };
        let lines = stems_daemon::supervisor::tail::human_lines(&e.details, &s.name, usize::MAX);
        if lines.is_empty() {
            continue;
        }
        out.push_str(&format!("\nerror {}: {}\n", s.name, e.message));
        for l in lines {
            out.push_str(&format!("  {l}\n"));
        }
    }
    out
}

/// `status -v`: a `warning <stem>: <script> exited 3 (soft_fail)` block
/// with the output tail for each failed `soft_fail` script (41).
pub fn warnings(st: &StatusResult) -> String {
    let mut out = String::new();
    for s in &st.stems {
        for w in &s.warnings {
            out.push_str(&format!("\nwarning {}: {}\n", s.name, w.failure()));
            for l in &w.tail {
                out.push_str(&format!("  {} | {l}\n", w.script));
            }
        }
    }
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
    let mut full = join(true);
    // Disabled stems (46) only when there are some.
    let disabled = |mut s: String| {
        if m.disabled > 0 {
            s.push_str(&format!(", {} disabled", m.disabled));
        }
        s
    };
    full = disabled(full);
    match width {
        Some(w) if full.chars().count() > w && parts.iter().any(|(n, _)| *n > 0) => {
            disabled(join(false))
        }
        _ => full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::{PortStatus, StatusSummary, StemStatus};
    use stems_core::{Glyph, StemState};

    #[test]
    fn expect_running_includes_every_up_or_transitioning_state() {
        use StemState::*;
        for st in [Setup, Starting, Seeding, Healthy, Unhealthy] {
            assert!(expect_matches(ExpectState::Running, st), "{st}");
        }
        for st in [Stopped, Stopping, Failed, Disabled] {
            assert!(!expect_matches(ExpectState::Running, st), "{st}");
        }
    }

    #[test]
    fn expect_without_a_daemon_checks_names_against_the_config() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("stems.yaml"),
            "schema_version: 1\nname: t\nstems:\n  a: { type: process, command: x }\n  b: { type: process, command: x, enabled: false }\n",
        )
        .unwrap();
        let ws = stems_config::load(stems_config::LoadOptions {
            cwd: d.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap()
        .workspace;
        assert_eq!(no_daemon_names(&ws, &[]).unwrap(), ["a"]);
        assert_eq!(no_daemon_names(&ws, &["b".into()]).unwrap(), ["b"]);
        let e = no_daemon_names(&ws, &["a".into(), "nope".into()]).unwrap_err();
        assert_eq!(e.0.len(), 1);
        assert_eq!(e.0[0].code, ErrorCode::UnknownStem);
        assert_eq!(e.0[0].details["stem"], "nope");
    }

    #[test]
    fn table_golden() {
        let stem = |name: &str, state: StemState, pid: Option<i32>, port: Option<u16>| StemStatus {
            name: name.into(),
            kind: "process".into(),
            state,
            glyph: state.glyph(false),
            reason: None,
            enabled: true,
            degraded: false,
            warnings: Vec::new(),
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
            StemStatus::disabled(
                "search",
                "process",
                Some("enabled: ${env.WITH_SEARCH} → false".into()),
            ),
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
            enabled: true,
            degraded: false,
            warnings: Vec::new(),
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
    fn verbose_lists_soft_fail_warnings() {
        let mut st = one("api", "setup exited 3 (soft_fail)");
        let s = &mut st.stems[0];
        (s.state, s.glyph, s.degraded, s.restarts) = (StemState::Healthy, Glyph::Degraded, true, 0);
        s.warnings = vec![stems_api::ScriptWarning {
            script: "setup".into(),
            exit: Some(3),
            signal: None,
            timed_out: false,
            tail: vec!["checking poetry".into(), "poetry missing; skipping".into()],
            at: chrono::DateTime::UNIX_EPOCH,
        }];
        st.summary = StatusSummary::of(&st.stems);
        let mut out = table(&st, &Style::default());
        out.push_str(&warnings(&st));
        insta::assert_snapshot!(out, @r"
        STEM  TYPE     STATUS     REASON                      PID  PORTS  UPTIME  RESTARTS
        api   process  ! healthy  setup exited 3 (soft_fail)  -    -      -       0

        0 healthy, 1 degraded, 0 failed, 0 unhealthy, 0 starting, 0 stopped, 0 unknown

        warning api: setup exited 3 (soft_fail)
          setup | checking poetry
          setup | poetry missing; skipping
        ");
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
    fn failed_stem_with_an_output_tail_golden() {
        let mut st = one("api", "`api` exited with code 3 before it became ready");
        st.stems[0].error = Some(
            Error::new(
                stems_core::ErrorCode::StartFailed,
                "`api` exited with code 3 before it became ready",
            )
            .with_details(json!({
                "stem": "api",
                "exit_code": 3,
                "stderr_tail": ["Traceback (most recent call last):", "ValueError: boom"],
                "output_truncated": true
            })),
        );
        let mut quiet = st.stems[0].clone();
        quiet.name = "quiet".into();
        quiet.error = Some(
            Error::new(
                stems_core::ErrorCode::StartFailed,
                "`quiet` exited with code 3",
            )
            .with_details(json!({ "stderr_tail": [], "stdout_tail": ["only-stdout"] })),
        );
        let mut silent = st.stems[0].clone();
        silent.name = "silent".into();
        silent.error = Some(
            Error::new(
                stems_core::ErrorCode::StartFailed,
                "`silent` exited with code 3",
            )
            .with_details(json!({ "stderr_tail": [], "output_truncated": false })),
        );
        st.stems.push(quiet);
        st.stems.push(silent);
        st.summary = StatusSummary::of(&st.stems);
        insta::assert_snapshot!("status-table-tail", table(&st, &Style::default()));
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
