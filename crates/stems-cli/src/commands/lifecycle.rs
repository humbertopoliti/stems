//! `stems up | down | start | stop | restart | attach` (deliverable 10,
//! FR-LC-1..4). Semantics: `docs/lifecycle.md`.
//!
//! **`up`** auto-starts the workspace daemon if needed, subscribes to its
//! events from the current seq, and calls the `up` RPC while printing
//! progress: human mode one line per `stem.state` / `stem.port_allocated`,
//! JSON mode every event as an NDJSON line. The result envelope follows as
//! the last line (compact in JSON mode): `data = UpResult { ok, requested,
//! ready, failed: [{stem, error}], skipped }`, exit 0 (all ready), 3 (some
//! ready, some failed) or 1 (nothing ready). With `--detach` the command
//! ends there. Attached (the default) it keeps streaming until SIGINT,
//! SIGTERM, SIGHUP or EOF on stdin (only when stdin is a terminal or a
//! pipe), then runs `down --all` with a deadline of the sum of `stop_grace`
//! + 5 s, waits for the daemon to exit and exits 0.
//!
//! **`attach`** streams the same way but never tears anything down: Ctrl-C
//! just exits (the TUI's "stop everything?" prompt comes with 27).

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use stems_api::client::{Client, EventStream};
use stems_api::{
    DaemonStatus, DownParams, DownResult, Event, EventKind, Method, RestartParams, StartParams,
    StopParams, UpParams, UpResult,
};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::{daemon_args, spawn_detached, wait_for_socket};

use crate::cli::{AttachArgs, DownArgs, RestartArgs, StartArgs, StopArgs, UpArgs};
use crate::client::{self, Target, block_on, connect_to};
use crate::commands::Ctx;
use crate::commands::daemon::WAIT;
use crate::output::{CommandOutput, Mode};

/// RPC timeout when the command has no `--timeout`.
const LONG: Duration = Duration::from_secs(3600);
/// Extra time on top of the teardown deadline for the reply and daemon exit.
const TEARDOWN_SLACK: Duration = Duration::from_secs(5);

fn parse_timeout(raw: Option<&str>) -> Result<Option<Duration>, Error> {
    raw.map(|s| {
        s.parse::<stems_config::Dur>()
            .map(stems_config::Dur::as_duration)
            .map_err(|e| Error::usage(e, "use e.g. `--timeout 90s`"))
    })
    .transpose()
}

fn ms(d: Option<Duration>) -> Option<u64> {
    d.map(|d| d.as_millis() as u64)
}

/// Sum of `stop_grace` over enabled stems (30 s if the config cannot load).
fn grace_sum(ctx: &Ctx) -> Duration {
    stems_config::load(ctx.load_options())
        .map(|r| {
            r.workspace
                .stems()
                .map(|s| s.stop_grace.as_duration())
                .sum()
        })
        .unwrap_or(Duration::from_secs(30))
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// Connect, starting the daemon (detached) if none runs. `true` = started here.
async fn connect_or_start(ctx: &Ctx, t: &Target) -> Result<(Client, bool), Error> {
    match connect_to(t, client::options(ctx)).await {
        Ok(c) => return Ok((c, false)),
        Err(e) if e.code != ErrorCode::DaemonNotRunning => return Err(e),
        Err(_) => {}
    }
    let exe = std::env::current_exe()
        .map_err(|e| Error::internal(format!("cannot locate the stems binary: {e}")))?;
    spawn_detached(
        &exe,
        &daemon_args(&t.home, Some(&t.workspace), false),
        &t.paths,
    )?;
    wait_for_socket(&t.paths, WAIT).await?;
    Ok((connect_to(t, client::options(ctx)).await?, true))
}

async fn subscribe_now(c: &Client) -> Result<EventStream, Error> {
    let st: DaemonStatus = c.call(Method::DAEMON_STATUS, json!({})).await?;
    c.subscribe_events(Some(st.last_seq)).await
}

/// One progress line for humans, or `None` for events not shown.
pub fn progress_line(e: &Event) -> Option<String> {
    let ts = e.ts.format("%H:%M:%S%.3f");
    let stem = e.stem.as_deref().unwrap_or("-");
    let k = e.kind.as_str();
    if k == EventKind::STEM_STATE {
        let mut s = format!(
            "{ts} {stem:<16} {} -> {}",
            e.from.as_deref().unwrap_or("-"),
            e.to.as_deref().unwrap_or("-")
        );
        if let Some(r) = &e.reason {
            s.push_str(&format!("  ({r})"));
        }
        Some(s)
    } else if k == EventKind::STEM_PORT_ALLOCATED {
        Some(format!(
            "{ts} {stem:<16} port {} = {}",
            e.data.get("name").and_then(Value::as_str).unwrap_or("?"),
            e.data.get("port").map(Value::to_string).unwrap_or_default()
        ))
    } else if k == EventKind::PROCESS_EXITED {
        Some(format!(
            "{ts} {stem:<16} process exited (code {}, signal {})",
            e.data.get("code").map(Value::to_string).unwrap_or_default(),
            e.data
                .get("signal")
                .map(Value::to_string)
                .unwrap_or_default()
        ))
    } else if k == EventKind::DAEMON_STOPPING {
        Some(format!("{ts} daemon stopping"))
    } else {
        None
    }
}

fn print_event(out: &mut dyn Write, mode: Mode, e: &Event) {
    let line = match mode {
        Mode::Json => Some(crate::commands::events::ndjson_line(e)),
        Mode::Human => progress_line(e),
    };
    if let Some(l) = line {
        let _ = writeln!(out, "{l}");
        let _ = out.flush();
    }
}

/// Exit code for an [`UpResult`]: 0 all ready, 3 some ready and some not, 1 none ready.
pub fn up_exit(r: &UpResult) -> i32 {
    if r.ok {
        0
    } else if !r.ready.is_empty() {
        stems_core::exit::PARTIAL
    } else {
        stems_core::exit::RUNTIME
    }
}

fn up_human(r: &UpResult) -> String {
    let mut s = format!(
        "up: {} ready, {} failed, {} skipped\n",
        r.ready.len(),
        r.failed.len(),
        r.skipped.len()
    );
    for f in &r.failed {
        s.push_str(&format!("  failed  {}: {}\n", f.stem, f.error.message));
    }
    for n in &r.skipped {
        s.push_str(&format!("  skipped {n}\n"));
    }
    s
}

/// Output for an [`UpResult`] (up/start/restart).
fn up_output(r: &UpResult) -> CommandOutput {
    let errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
    CommandOutput::data(to_value(r))
        .with_human(up_human(r))
        .with_errors(Errors(errors))
        .with_exit(up_exit(r))
}

fn down_output(r: &DownResult) -> CommandOutput {
    let errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
    let mut human = format!("stopped: {}\n", list(&r.stopped));
    if !r.skipped.is_empty() {
        human.push_str(&format!("not running: {}\n", list(&r.skipped)));
    }
    for f in &r.failed {
        human.push_str(&format!("failed: {}: {}\n", f.stem, f.error.message));
    }
    if r.daemon_stopping {
        human.push_str("daemon stopped\n");
    }
    CommandOutput::data(to_value(r))
        .with_human(human)
        .with_errors(Errors(errors))
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(", ")
    }
}

/// Wait (bounded) until the daemon's socket and lock are gone.
async fn wait_daemon_gone(t: &Target, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if !t.paths.socket.exists() && !t.paths.lock.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// --------------------------------------------------------------------------
// Signals and stdin EOF (attached mode)
// --------------------------------------------------------------------------

/// Resolves on SIGINT, SIGTERM, SIGHUP, or EOF on stdin when stdin is a
/// terminal or a pipe (never for `/dev/null` or a file).
struct Exit {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    eof: Option<tokio::sync::oneshot::Receiver<()>>,
}

fn stdin_is_interactive() -> bool {
    // SAFETY: isatty/fstat on fd 0 with a zeroed stat buffer.
    unsafe {
        if libc::isatty(0) == 1 {
            return true;
        }
        let mut st: libc::stat = std::mem::zeroed();
        if libc::fstat(0, &mut st) != 0 {
            return false;
        }
        let fmt = st.st_mode & libc::S_IFMT;
        fmt == libc::S_IFIFO || fmt == libc::S_IFSOCK
    }
}

impl Exit {
    fn install(watch_stdin: bool) -> Result<Self, Error> {
        use tokio::signal::unix::{SignalKind, signal};
        let sig = |k| {
            signal(k).map_err(|e| Error::internal(format!("cannot install signal handler: {e}")))
        };
        let eof = (watch_stdin && stdin_is_interactive()).then(|| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    // SAFETY: plain read(2) on fd 0 into a local buffer.
                    let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                    if n == 0
                        || (n < 0
                            && std::io::Error::last_os_error().kind()
                                != std::io::ErrorKind::Interrupted)
                    {
                        let _ = tx.send(());
                        return;
                    }
                }
            });
            rx
        });
        Ok(Self {
            int: sig(SignalKind::interrupt())?,
            term: sig(SignalKind::terminate())?,
            hup: sig(SignalKind::hangup())?,
            eof,
        })
    }

    async fn wait(&mut self) -> &'static str {
        let eof = async {
            match self.eof.as_mut() {
                Some(rx) => {
                    let _ = rx.await;
                }
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = self.int.recv() => "SIGINT",
            _ = self.term.recv() => "SIGTERM",
            _ = self.hup.recv() => "SIGHUP",
            () = eof => "stdin closed",
        }
    }
}

// --------------------------------------------------------------------------
// up
// --------------------------------------------------------------------------

/// `stems up`.
pub fn up(ctx: &Ctx, args: &UpArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    block_on(up_async(ctx, args, mode, stdout))
        .unwrap_or_else(|e| CommandOutput::failed(e).compact())
}

async fn up_async(
    ctx: &Ctx,
    args: &UpArgs,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    let timeout = parse_timeout(args.timeout.as_deref())?;
    if args.fresh || args.sync || args.force_overlays {
        let (flag, nn) = if args.fresh {
            ("--fresh", "16")
        } else if args.sync {
            ("--sync", "20")
        } else {
            ("--force-overlays", "18")
        };
        return Err(Error::not_implemented(&format!("`stems up {flag}`"), nn)
            .with_details(json!({ "flag": flag, "deliverable": nn }))
            .into());
    }
    let pass_env: BTreeMap<String, String> = args
        .pass_env
        .iter()
        .filter(|k| !k.is_empty())
        .filter_map(|k| ctx.env.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    let t = client::target(ctx)?;
    // Attached: signals from now on tear down; detached keeps default handling.
    let mut exit = if args.detach {
        None
    } else {
        Some(Exit::install(true)?)
    };
    let (c, auto_started) = connect_or_start(ctx, &t).await?;
    let mut events = subscribe_now(&c).await?;
    let params = UpParams {
        stems: args.stems.clone(),
        profile: args.profile.clone(),
        detach: args.detach,
        timeout_ms: ms(timeout),
        fail_fast: !args.no_fail_fast,
        max_parallel: Some(args.max_parallel),
        pass_env,
        daemon_auto_started: auto_started,
    };
    let rpc_timeout = timeout.map_or(LONG, |d| d + Duration::from_secs(10));
    let call = c.call_with_timeout::<UpResult>(Method::UP, &params, rpc_timeout);
    tokio::pin!(call);

    let mut interrupted = None;
    let result: Result<UpResult, Error> = loop {
        let sig = async {
            match exit.as_mut() {
                Some(x) => x.wait().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = &mut call => break r,
            Some(ev) = events.next() => print_event(stdout, mode, &ev),
            s = sig => { interrupted = Some(s); break Err(Error::internal("interrupted")); }
        }
    };

    if let Some(why) = interrupted {
        return Ok(teardown(ctx, &t, &c, why, mode, stdout).await);
    }
    let res = match result {
        Ok(r) => r,
        Err(e) => {
            if auto_started && !args.detach {
                let _ = c.shutdown().await;
                wait_daemon_gone(&t, WAIT).await;
            }
            return Err(e.into());
        }
    };
    // Flush the progress events up to `up.finished`.
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(ev) = events.next().await {
            print_event(stdout, mode, &ev);
            if ev.kind == EventKind::UP_FINISHED {
                break;
            }
        }
    })
    .await;
    let out = up_output(&res).compact();
    if args.detach {
        return Ok(out);
    }

    // Attached: print the summary now, then follow until told to exit.
    crate::output::render(
        &out,
        &crate::output::OutputOptions {
            mode,
            json_explicit: mode == Mode::Json,
            quiet: false,
            no_color: true,
        },
        stdout,
        &mut std::io::stderr(),
    );
    if mode == Mode::Human {
        let _ = writeln!(
            stdout,
            "attached: Ctrl-C stops every stem and the daemon (use `stems up --detach` to leave them running)"
        );
        let _ = stdout.flush();
    }
    let mut exit = exit
        .take()
        .expect("attached mode installs the exit watcher");
    loop {
        tokio::select! {
            ev = events.next() => match ev {
                Some(ev) => print_event(stdout, mode, &ev),
                None => {
                    // The daemon went away (`stems down --all` elsewhere).
                    return Ok(CommandOutput::data(json!({ "detached": "daemon stopped" }))
                        .with_human("daemon stopped\n")
                        .compact());
                }
            },
            why = exit.wait() => return Ok(teardown(ctx, &t, &c, why, mode, stdout).await),
        }
    }
}

/// `down --all` with the attached-mode deadline, then wait for the daemon.
async fn teardown(
    ctx: &Ctx,
    t: &Target,
    c: &Client,
    why: &str,
    mode: Mode,
    stdout: &mut dyn Write,
) -> CommandOutput {
    if mode == Mode::Human {
        let _ = writeln!(stdout, "{why}: stopping every stem and the daemon...");
        let _ = stdout.flush();
    }
    let deadline = grace_sum(ctx) + TEARDOWN_SLACK;
    let params = DownParams {
        all: true,
        ..DownParams::default()
    };
    // A fresh connection: the up call may still own the first one.
    let fresh = connect_to(t, client::options(ctx)).await;
    let r = match &fresh {
        Ok(fc) => {
            fc.call_with_timeout::<DownResult>(Method::DOWN, &params, deadline)
                .await
        }
        Err(e) => Err(e.clone()),
    };
    let _ = c;
    match r {
        Ok(res) => {
            wait_daemon_gone(t, WAIT).await;
            down_output(&res).compact().with_exit(0)
        }
        Err(e) => {
            // Last resort: ask the daemon to shut down (its exit path stops stems).
            if let Ok(fc) = &fresh {
                let _ = fc.shutdown().await;
            }
            wait_daemon_gone(t, WAIT).await;
            CommandOutput::failed(e).compact()
        }
    }
}

// --------------------------------------------------------------------------
// down / start / stop / restart
// --------------------------------------------------------------------------

/// `stems down`.
pub fn down(ctx: &Ctx, args: &DownArgs) -> CommandOutput {
    block_on(async {
        if args.volumes {
            return Err(Errors::from(
                Error::not_implemented("`stems down --volumes`", "14")
                    .with_details(json!({ "flag": "--volumes", "deliverable": "14" })),
            ));
        }
        let timeout = parse_timeout(args.timeout.as_deref())?;
        let t = client::target(ctx)?;
        let c = connect_to(&t, client::options(ctx)).await?;
        let params = DownParams {
            stems: args.stems.clone(),
            all: args.all,
            timeout_ms: ms(timeout),
        };
        let bound = grace_sum(ctx) + Duration::from_secs(10);
        let res: DownResult = c.call_with_timeout(Method::DOWN, &params, bound).await?;
        if res.daemon_stopping {
            wait_daemon_gone(&t, WAIT).await;
        }
        Ok(down_output(&res))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems start`.
pub fn start(ctx: &Ctx, args: &StartArgs) -> CommandOutput {
    block_on(async {
        let timeout = parse_timeout(args.timeout.as_deref())?;
        let c = client::connect(ctx).await?;
        let params = StartParams {
            stems: args.stems.clone(),
            no_deps: args.no_deps,
            timeout_ms: ms(timeout),
        };
        let bound = timeout.map_or(LONG, |d| d + Duration::from_secs(10));
        let res: UpResult = c.call_with_timeout(Method::START, &params, bound).await?;
        Ok::<_, Errors>(up_output(&res))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems stop`.
pub fn stop(ctx: &Ctx, args: &StopArgs) -> CommandOutput {
    block_on(async {
        let timeout = parse_timeout(args.timeout.as_deref())?;
        let c = client::connect(ctx).await?;
        let params = StopParams {
            stems: args.stems.clone(),
            cascade: args.cascade,
            timeout_ms: ms(timeout),
        };
        let bound = grace_sum(ctx) + Duration::from_secs(10);
        let res: DownResult = c.call_with_timeout(Method::STOP, &params, bound).await?;
        Ok::<_, Errors>(down_output(&res))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems restart`.
pub fn restart(ctx: &Ctx, args: &RestartArgs) -> CommandOutput {
    block_on(async {
        let timeout = parse_timeout(args.timeout.as_deref())?;
        let c = client::connect(ctx).await?;
        let params = RestartParams {
            stems: args.stems.clone(),
            no_deps: args.no_deps,
            build: args.build,
            timeout_ms: ms(timeout),
        };
        let bound = grace_sum(ctx) + timeout.unwrap_or(LONG);
        let res: UpResult = c.call_with_timeout(Method::RESTART, &params, bound).await?;
        Ok::<_, Errors>(up_output(&res))
    })
    .unwrap_or_else(CommandOutput::failed)
}

// --------------------------------------------------------------------------
// attach
// --------------------------------------------------------------------------

/// `stems attach`: follow the daemon; exiting never stops anything.
pub fn attach(ctx: &Ctx, args: &AttachArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    if args.headless || args.script.is_some() || args.frames_out.is_some() || args.view.is_some() {
        return CommandOutput::failed(
            Error::not_implemented("the TUI (`stems attach --view/--headless`)", "27")
                .with_details(json!({ "deliverable": "27" })),
        );
    }
    block_on(async {
        let c = client::connect(ctx).await?;
        let mut exit = Exit::install(false)?;
        let mut events = subscribe_now(&c).await?;
        if mode == Mode::Human {
            let st: stems_api::StatusResult = c
                .call(Method::STATUS, stems_api::StatusParams::default())
                .await?;
            let _ = write!(stdout, "{}", crate::commands::status::table(&st));
            let _ = writeln!(stdout, "attached (Ctrl-C detaches; stems keep running)");
            let _ = stdout.flush();
        }
        loop {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(ev) => print_event(stdout, mode, &ev),
                    None => break,
                },
                _ = exit.wait() => break,
            }
        }
        Ok::<_, Errors>(
            CommandOutput::data(json!({ "detached": true }))
                .with_human("")
                .compact(),
        )
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::StemFailure;

    #[test]
    fn up_exit_codes() {
        let mut r = UpResult {
            ok: true,
            ready: vec!["a".into()],
            ..UpResult::default()
        };
        assert_eq!(up_exit(&r), 0);
        r.ok = false;
        r.failed.push(StemFailure {
            stem: "b".into(),
            error: Error::new(ErrorCode::StartFailed, "x"),
        });
        assert_eq!(up_exit(&r), 3);
        r.ready.clear();
        assert_eq!(up_exit(&r), 1);
        let out = up_output(&r);
        assert_eq!(out.exit_code(), 1);
        assert_eq!(out.envelope()["data"]["failed"][0]["stem"], "b");
    }

    #[test]
    fn progress_lines() {
        use chrono::TimeZone;
        let e = Event {
            ts: chrono::Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 1).unwrap(),
            seq: 3,
            kind: EventKind::STEM_STATE,
            stem: Some("api".into()),
            from: Some("starting".into()),
            to: Some("healthy".into()),
            reason: Some("ready".into()),
            actor: "daemon".into(),
            data: json!({}),
        };
        insta::assert_snapshot!(progress_line(&e).unwrap(), @"12:00:01.000 api              starting -> healthy  (ready)");
        let mut e2 = e.clone();
        e2.kind = EventKind::UP_STARTED;
        assert!(progress_line(&e2).is_none());
    }
}
