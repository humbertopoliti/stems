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
use crate::commands::orphans;
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
pub(crate) async fn connect_or_start(ctx: &Ctx, t: &Target) -> Result<(Client, bool), Error> {
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

pub(crate) async fn subscribe_now(c: &Client) -> Result<EventStream, Error> {
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
    } else if k == EventKind::DOCKER_PULL {
        // Image-level lines only ("Pulling from ...", "Digest: ...",
        // "Status: ..."); per-layer progress is left to `stems events`.
        let layer = e.data.get("layer").and_then(Value::as_str).unwrap_or("");
        let status = e.data.get("status").and_then(Value::as_str)?;
        layer
            .is_empty()
            .then(|| format!("{ts} {stem:<16} {status}"))
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
    if let Some(c) = &r.cascade {
        s.push_str(&cascade_human(c));
    }
    s
}

/// A cascading restart (FR-LC-9): one line per dependant.
fn cascade_human(c: &stems_api::CascadeReport) -> String {
    if c.aborted {
        return format!(
            "cascade from {}: aborted, {} did not become healthy; dependants not restarted\n",
            c.origins.join(", "),
            c.origins.join(", ")
        );
    }
    let restarted: usize = c.restarted.iter().map(Vec::len).sum();
    let mut s = format!(
        "cascade from {}: {} restarted, {} failed, {} skipped\n",
        c.origins.join(", "),
        restarted,
        c.failed.len(),
        c.skipped.len()
    );
    for (i, layer) in c.restarted.iter().enumerate() {
        for n in layer {
            s.push_str(&format!("  restarted {n} (layer {})\n", i + 1));
        }
    }
    for f in &c.failed {
        s.push_str(&format!("  failed    {}: {}\n", f.stem, f.error.message));
    }
    for n in &c.skipped {
        s.push_str(&format!("  skipped   {n}\n"));
    }
    s
}

/// Output for an [`UpResult`] (up/start/restart).
fn up_output(r: &UpResult) -> CommandOutput {
    let mut errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
    if let Some(c) = &r.cascade {
        errors.extend(c.failed.iter().map(|f| f.error.clone()));
    }
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
    if !r.volumes_removed.is_empty() {
        human.push_str(&format!("volumes removed: {}\n", list(&r.volumes_removed)));
    }
    if r.daemon_stopping {
        human.push_str("daemon stopped\n");
    }
    CommandOutput::data(to_value(r))
        .with_human(human)
        .with_errors(Errors(errors))
}

/// `down --volumes` is destructive (14): `--yes`, or a `y` on a terminal
/// (human mode only); otherwise `DESTRUCTIVE_NOT_CONFIRMED` (exit 2), like
/// `stems reset`.
fn confirm_volumes(args: &DownArgs, mode: Mode) -> Result<(), Error> {
    if !args.volumes || args.yes {
        return Ok(());
    }
    let what = if args.stems.is_empty() {
        "every docker stem".to_string()
    } else {
        args.stems.join(", ")
    };
    if mode == Mode::Human && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let mut err = std::io::stderr();
        let _ = write!(
            err,
            "`stems down --volumes` deletes the docker volumes (and their data) of {what}. Continue? [y/N] "
        );
        let _ = err.flush();
        let mut line = String::new();
        if std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line).is_ok()
            && matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
        {
            return Ok(());
        }
    }
    Err(Error::new(
        ErrorCode::DestructiveNotConfirmed,
        format!("`stems down --volumes` deletes the docker volumes (and their data) of {what}"),
    )
    .with_hint("rerun with `--yes` to confirm")
    .with_details(json!({ "command": "down", "flag": "--volumes", "stems": args.stems })))
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
    let pass_env: BTreeMap<String, String> = args
        .pass_env
        .iter()
        .filter(|k| !k.is_empty())
        .filter_map(|k| ctx.env.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    if args.profile.is_some() && !args.stems.is_empty() {
        return Err(Error::usage(
            "`--profile` cannot be combined with stem names",
            "name the stems to start, or pass --profile alone (names always win over profiles)",
        )
        .into());
    }
    // `--profile`, else `STEMS_PROFILE` (26); the daemon falls back to the
    // workspace default (`profile:`, `default_profile`, `default`).
    let profile = args.profile.clone().or_else(|| {
        ctx.env
            .get(stems_core::selection::ENV_PROFILE)
            .filter(|p| !p.is_empty())
            .cloned()
    });
    let t = client::target(ctx)?;
    // The fast doctor subset (19): Docker must be reachable if the selection
    // needs it, before anything (even the daemon) starts. Config errors
    // (profile ones included) are left to the `up` RPC.
    if let Ok(resolved) = stems_config::load(ctx.load_options())
        && let Ok(sel) = stems_core::Selection::resolve(
            &resolved.workspace,
            profile.as_deref(),
            &args.stems,
            stems_core::SelectOptions::default(),
        )
    {
        stems_daemon::doctor::preflight_up(
            &resolved,
            &sel.closure,
            ctx.env.get("DOCKER_HOST").cloned(),
        )
        .await?;
    }
    // Attached: signals from now on tear down; detached keeps default handling.
    // The orphan prompt reads stdin: watch it for EOF only after the prompt.
    let may_prompt = mode == Mode::Human
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
        && !(args.yes || args.adopt_orphans || args.kill_orphans || args.kill_foreign);
    // The TUI (27) reads the keyboard itself: no stdin EOF watcher then.
    let tui = tui_choice_up(ctx, args, mode)?;
    let mut exit = if args.detach {
        None
    } else {
        Some(Exit::install(!may_prompt && tui.is_plain())?)
    };
    let (c, auto_started) = connect_or_start(ctx, &t).await?;
    let orphans = match handle_orphans(ctx, args, mode, &t, &c).await {
        Ok(o) => o,
        Err(e) => {
            if auto_started {
                shutdown_if_idle(&t, &c).await;
            }
            return Err(e);
        }
    };
    if may_prompt && exit.is_some() && tui.is_plain() {
        exit = Some(Exit::install(true)?);
    }
    let mut events = subscribe_now(&c).await?;
    let params = UpParams {
        stems: args.stems.clone(),
        profile,
        detach: args.detach,
        timeout_ms: ms(timeout),
        fail_fast: !args.no_fail_fast,
        max_parallel: Some(args.max_parallel),
        pass_env,
        daemon_auto_started: auto_started,
        fresh: args.fresh,
        force_overlays: args.force_overlays,
        sync: args.sync,
        no_watch: args.no_watch,
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
    let mut out = up_output(&res).compact();
    if !orphans.is_empty()
        && let Value::Object(m) = &mut out.data
    {
        m.insert("orphans".into(), Value::Array(orphans));
    }
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
    if !tui.is_plain() {
        drop(events);
        let session = TuiSession {
            mode: stems_tui::AttachMode::Up,
            profile: args.profile.clone(),
            view: None,
            owns_daemon: auto_started,
        };
        return Ok(run_tui_session(ctx, &t, &c, tui, session, mode, stdout).await);
    }
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

/// The orphan scan of `up` (deliverable 11, FR-CR-4): report, prompt, kill
/// or adopt per [`crate::commands::orphans`]. `Err(ORPHANS_FOUND)` when
/// there are orphans and no consent to act (nothing is started then).
/// Returns the handled orphans (with their `action`) for `data.orphans`.
async fn handle_orphans(
    ctx: &Ctx,
    args: &UpArgs,
    mode: Mode,
    t: &Target,
    c: &Client,
) -> Result<Vec<Value>, Errors> {
    let mut found = match orphans::scan(ctx, t, &[c.info().pid as i32]) {
        Ok(f) => f,
        // Config errors are reported by the `up` RPC itself.
        Err(_) => return Ok(Vec::new()),
    };
    // Running containers of this workspace not in state (14/15), only when
    // the selection needs Docker (process-only `up` never talks to Docker).
    if args.profile.is_none() && orphans::selection_needs_docker(ctx, &args.stems) {
        found.extend(orphans::scan_containers(ctx, t, true).await);
    }
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let policy = orphans::Policy::new(
        args.yes,
        args.adopt_orphans,
        args.kill_orphans,
        args.kill_foreign,
        mode == Mode::Json,
    );
    if policy.report_only() {
        let listed: Vec<Value> = found
            .iter()
            .map(|o| {
                let mut v = serde_json::to_value(o).unwrap_or(Value::Null);
                if let Value::Object(m) = &mut v {
                    m.insert("action".into(), json!("none"));
                }
                v
            })
            .collect();
        return Err(orphans::found_error(
            &listed,
            "nothing was started; inspect them with `stems doctor --orphans`, then rerun with `--kill-orphans`/`--yes` (kill those that look like their stem's start command), `--adopt-orphans` (adopt them) or `--kill-foreign` (also kill the rest)",
        )
        .into());
    }
    Ok(orphans::resolve(
        &found,
        &policy,
        Some(c),
        ctx.env.get("DOCKER_HOST").cloned(),
    )
    .await)
}

/// Shut an auto-started daemon down again if it runs nothing (an `up` that
/// did not start anything must not leave a daemon behind).
pub(crate) async fn shutdown_if_idle(t: &Target, c: &Client) {
    let st: Result<stems_api::StatusResult, Error> = c
        .call(Method::STATUS, stems_api::StatusParams::default())
        .await;
    let idle = st.is_ok_and(|s| s.stems.iter().all(|x| !x.state.is_running()));
    if idle {
        let _ = c.shutdown().await;
        wait_daemon_gone(t, WAIT).await;
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
pub fn down(ctx: &Ctx, args: &DownArgs, mode: Mode) -> CommandOutput {
    block_on(async {
        confirm_volumes(args, mode).map_err(Errors::from)?;
        let timeout = parse_timeout(args.timeout.as_deref())?;
        let t = client::target(ctx)?;
        // No daemon but the state file lists stems (a crashed daemon): start
        // one, which adopts what is still alive, and tear it all down
        // (deliverable 11, FR-CR-3). `--volumes` without a daemon (the
        // stems are already down) also needs one, briefly (14).
        let (c, recovered, temporary) = match connect_to(&t, client::options(ctx)).await {
            Ok(c) => (c, false, false),
            Err(e) if e.code == ErrorCode::DaemonNotRunning && has_recorded_stems(&t) => {
                let (c, _) = connect_or_start(ctx, &t).await?;
                (c, true, false)
            }
            Err(e) if e.code == ErrorCode::DaemonNotRunning && args.volumes => {
                let (c, _) = connect_or_start(ctx, &t).await?;
                (c, false, true)
            }
            Err(e) => return Err(Errors::from(e)),
        };
        let params = DownParams {
            stems: args.stems.clone(),
            all: args.all || (recovered && args.stems.is_empty()),
            timeout_ms: ms(timeout),
            volumes: args.volumes,
        };
        let bound = grace_sum(ctx) + Duration::from_secs(10);
        let res: DownResult = c.call_with_timeout(Method::DOWN, &params, bound).await?;
        if temporary && !res.daemon_stopping {
            let _ = c.shutdown().await;
            wait_daemon_gone(&t, WAIT).await;
        }
        if res.daemon_stopping {
            wait_daemon_gone(&t, WAIT).await;
        }
        let mut out = down_output(&res);
        if recovered {
            if let Value::Object(m) = &mut out.data {
                m.insert("recovered".into(), json!(true));
            }
            if let Some(h) = &mut out.human {
                h.insert_str(0, "recovered the stems of a crashed daemon\n");
            }
        }
        Ok(out)
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// The state file lists stems (a previous daemon did not clean up).
fn has_recorded_stems(t: &Target) -> bool {
    stems_daemon::state::StateFile::peek(&t.paths.state).is_some_and(|f| !f.stems.is_empty())
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
            cascade: args.cascade_flag(),
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

/// `stems attach`: the dashboard (27), or the plain event stream
/// (`STEMS_TUI=0`, `--json`, stdout not a terminal); exiting never stops
/// anything.
pub fn attach(ctx: &Ctx, args: &AttachArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let tui = match tui_choice_attach(ctx, args) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    if !tui.is_plain() {
        return block_on(async {
            let t = client::target(ctx)?;
            let c = connect_to(&t, client::options(ctx)).await?;
            let session = TuiSession {
                mode: stems_tui::AttachMode::Attach,
                profile: None,
                view: args.view.map(|v| match v {
                    crate::cli::View::Table => stems_tui::ViewKind::Table,
                    crate::cli::View::Detail => stems_tui::ViewKind::Detail,
                    crate::cli::View::Graph => stems_tui::ViewKind::Graph,
                }),
                owns_daemon: false,
            };
            Ok::<_, Errors>(run_tui_session(ctx, &t, &c, tui, session, mode, stdout).await)
        })
        .unwrap_or_else(CommandOutput::failed);
    }
    block_on(async {
        let c = client::connect(ctx).await?;
        let mut exit = Exit::install(false)?;
        let mut events = subscribe_now(&c).await?;
        if mode == Mode::Human {
            let st: stems_api::StatusResult = c
                .call(Method::STATUS, stems_api::StatusParams::default())
                .await?;
            let style = crate::commands::status::Style::detect(ctx, mode);
            let _ = write!(stdout, "{}", crate::commands::status::table(&st, &style));
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

// --------------------------------------------------------------------------
// The TUI (deliverable 27)
// --------------------------------------------------------------------------

/// How `attach` / attached `up` present the daemon.
#[derive(Debug)]
pub enum TuiChoice {
    /// The plain event stream (`STEMS_TUI=0`, `--json`, no terminal).
    Plain,
    /// The dashboard in the terminal.
    Terminal,
    /// Replay a key script, frames as text.
    Headless(stems_tui::HeadlessOptions),
}

impl TuiChoice {
    fn is_plain(&self) -> bool {
        matches!(self, TuiChoice::Plain)
    }
}

/// What the TUI session is for.
pub struct TuiSession {
    mode: stems_tui::AttachMode,
    profile: Option<String>,
    view: Option<stems_tui::ViewKind>,
    owns_daemon: bool,
}

fn env_flag(ctx: &Ctx, k: &str) -> Option<bool> {
    ctx.env
        .get(k)
        .filter(|v| !v.is_empty())
        .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off" | "n" | "f"))
}

fn headless_options(
    script: Option<&str>,
    size: Option<&str>,
    frames_out: Option<std::path::PathBuf>,
) -> Result<stems_tui::HeadlessOptions, Error> {
    let text = match script {
        None => "frame".to_string(),
        Some(s) => match s.strip_prefix('@') {
            Some(path) => std::fs::read_to_string(path).map_err(|e| {
                Error::usage(
                    format!("cannot read the script {path}: {e}"),
                    "pass `--script @FILE` with a readable file, or the tokens inline",
                )
            })?,
            None => s.to_string(),
        },
    };
    let tokens = stems_tui::script::parse(&text).map_err(|e| {
        Error::usage(
            format!("invalid headless script: {e}"),
            "see docs/tui.md for the tokens (keys, wait:<state>, frame, view:<name>)",
        )
    })?;
    let mut opts = stems_tui::HeadlessOptions::new(tokens);
    if let Some(sz) = size {
        opts.size = stems_tui::parse_size(sz).ok_or_else(|| {
            Error::usage(format!("invalid size `{sz}`"), "use e.g. `--size 120x40`")
        })?;
    }
    opts.frames_out = frames_out;
    Ok(opts)
}

fn stdout_is_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// `stems attach`: `--headless`/`--script` replay; else the dashboard on a
/// terminal (or with `STEMS_TUI_FORCE=1`) unless `STEMS_TUI=0` or `--json`.
fn tui_choice_attach(ctx: &Ctx, args: &AttachArgs) -> Result<TuiChoice, Error> {
    if args.headless || args.script.is_some() {
        return Ok(TuiChoice::Headless(headless_options(
            args.script.as_deref(),
            args.size.as_deref(),
            args.frames_out.clone(),
        )?));
    }
    if env_flag(ctx, stems_tui::ENV_TUI) == Some(false) || ctx.global.json {
        return Ok(TuiChoice::Plain);
    }
    if env_flag(ctx, stems_tui::ENV_TUI_FORCE) == Some(true) || stdout_is_tty() {
        return Ok(TuiChoice::Terminal);
    }
    Ok(TuiChoice::Plain)
}

/// Attached `stems up`: `STEMS_TUI_SCRIPT` replays headless (tests); else
/// the dashboard on a terminal in human mode (or `STEMS_TUI_FORCE=1`)
/// unless `STEMS_TUI=0`; else the plain stream.
fn tui_choice_up(ctx: &Ctx, args: &UpArgs, mode: Mode) -> Result<TuiChoice, Error> {
    if args.detach {
        return Ok(TuiChoice::Plain);
    }
    if let Some(script) = ctx
        .env
        .get(stems_tui::ENV_TUI_SCRIPT)
        .filter(|s| !s.is_empty())
    {
        return Ok(TuiChoice::Headless(headless_options(
            Some(script),
            ctx.env.get(stems_tui::ENV_TUI_SIZE).map(String::as_str),
            ctx.env
                .get(stems_tui::ENV_TUI_FRAMES_OUT)
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from),
        )?));
    }
    if env_flag(ctx, stems_tui::ENV_TUI) == Some(false) {
        return Ok(TuiChoice::Plain);
    }
    if env_flag(ctx, stems_tui::ENV_TUI_FORCE) == Some(true)
        || (mode == Mode::Human && stdout_is_tty())
    {
        return Ok(TuiChoice::Terminal);
    }
    Ok(TuiChoice::Plain)
}

/// Run the TUI on `t`'s daemon, then act on how it ended: `StopAll` runs
/// the attached-mode teardown (`down --all`), `Detach` leaves everything.
async fn run_tui_session(
    ctx: &Ctx,
    t: &Target,
    c: &Client,
    choice: TuiChoice,
    session: TuiSession,
    mode: Mode,
    stdout: &mut dyn Write,
) -> CommandOutput {
    let mut opts = client::options(ctx);
    opts.actor = opts.actor.replacen("cli:", "tui:", 1);
    let tc = match connect_to(t, opts).await {
        Ok(c) => std::sync::Arc::new(c),
        Err(e) => return CommandOutput::failed(e).compact(),
    };
    let env = |k: &str| ctx.env.get(k).cloned();
    // Headless frames must not depend on the developer's ~/.config: only
    // STEMS_UI_CONFIG is honoured there.
    let headless = matches!(choice, TuiChoice::Headless(_));
    let (prefs, warning) = stems_tui::Prefs::load(|k: &str| {
        if headless && k != stems_tui::prefs::ENV_UI_CONFIG {
            None
        } else {
            env(k)
        }
    });
    let mut model = stems_tui::Model::new(session.mode, prefs, session.view);
    model.profile = session.profile;
    model.owns_daemon = session.owns_daemon && session.mode == stems_tui::AttachMode::Attach;
    model.message = warning;
    // `o` (30): $EDITOR, else $VISUAL (the TUI falls back to `vi`).
    model.editor = env("EDITOR")
        .filter(|e| !e.trim().is_empty())
        .or_else(|| env("VISUAL"));
    let mut frames: Option<Vec<String>> = None;
    let result = match choice {
        TuiChoice::Plain => Ok(stems_tui::Outcome::Detach),
        TuiChoice::Terminal => {
            model.ascii = stems_core::status::prefers_ascii(env);
            let panic_test = env_flag(ctx, stems_tui::ENV_PANIC_TEST) == Some(true);
            stems_tui::run_terminal(tc, model, stems_tui::TerminalOptions { panic_test }).await
        }
        TuiChoice::Headless(h) => {
            // Deterministic frames: ASCII only when asked for explicitly.
            model.ascii = env_flag(ctx, "STEMS_ASCII") == Some(true);
            let print = !(ctx.global.json && session.mode == stems_tui::AttachMode::Attach);
            let r = stems_tui::run_headless_output(tc, model, h, |out| {
                if print {
                    match out {
                        stems_tui::HeadlessOutput::Frame(n, text) => {
                            let _ = writeln!(stdout, "{}", stems_tui::frame_header(n));
                            let _ = write!(stdout, "{text}");
                        }
                        // The OSC 52 sequence of a copy (`y`, 29), on its own line.
                        stems_tui::HeadlessOutput::Raw(raw) => {
                            let _ = writeln!(stdout, "{raw}");
                        }
                    }
                    let _ = stdout.flush();
                }
            })
            .await;
            r.map(|run| {
                frames = Some(run.frames);
                run.outcome
            })
        }
    };
    match result {
        Ok(stems_tui::Outcome::StopAll) => teardown(ctx, t, c, "quit", mode, stdout).await,
        Ok(stems_tui::Outcome::Detach) => {
            let mut data = json!({ "detached": true });
            if let Some(f) = frames {
                data["frames"] = json!(f);
            }
            let human = if session.mode == stems_tui::AttachMode::Up {
                "detached: the stems keep running (`stems attach` reconnects, `stems down --all` stops them)\n"
            } else {
                ""
            };
            let out = CommandOutput::data(data).with_human(human).compact();
            if session.mode == stems_tui::AttachMode::Attach {
                // Frames were printed as they were taken; the envelope only
                // with an explicit --json.
                out.with_raw("")
            } else {
                out
            }
        }
        Err(e) => {
            if session.mode == stems_tui::AttachMode::Up {
                let _ = teardown(ctx, t, c, "the dashboard failed", mode, stdout).await;
            }
            CommandOutput::failed(e).compact()
        }
    }
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
    fn cascade_human_golden() {
        let mut r = UpResult {
            ok: false,
            requested: vec!["a".into()],
            ready: vec!["a".into()],
            ..UpResult::default()
        };
        r.cascade = Some(stems_api::CascadeReport {
            id: "01J".into(),
            origin: "a".into(),
            origins: vec!["a".into()],
            restarted: vec![vec!["b".into(), "c".into()], vec!["d".into()]],
            failed: vec![StemFailure {
                stem: "e".into(),
                error: Error::new(ErrorCode::StartFailed, "`e` exited with code 3"),
            }],
            skipped: vec!["f".into()],
            aborted: false,
        });
        insta::assert_snapshot!("restart-cascade", up_human(&r));
        assert_eq!(up_exit(&r), 3);
        assert_eq!(
            up_output(&r).envelope()["errors"][0]["code"],
            "START_FAILED"
        );
        let c = r.cascade.as_mut().unwrap();
        c.aborted = true;
        c.restarted.clear();
        insta::assert_snapshot!("restart-cascade-aborted", cascade_human(c));
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

        // docker.pull: image-level lines only.
        let mut pull = e.clone();
        pull.kind = EventKind::DOCKER_PULL;
        pull.data = json!({"stem": "api", "image": "ghcr.io/a/api:main", "layer": "", "status": "Pulling from a/api"});
        insta::assert_snapshot!(progress_line(&pull).unwrap(), @"12:00:01.000 api              Pulling from a/api");
        pull.data["layer"] = json!("3f2a1c");
        assert!(
            progress_line(&pull).is_none(),
            "per-layer progress is not shown"
        );
    }
}
