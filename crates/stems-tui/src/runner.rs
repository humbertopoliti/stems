//! Runners: execute [`Cmd`]s against the daemon and drive the reducer, in a
//! real terminal ([`run_terminal`]) or headless ([`run_headless`], frames as
//! text for tests and CI).

use std::collections::VecDeque;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use stems_api::client::Client;
use stems_api::{
    ConfigDiffResult, DaemonStatus, EventsResult, Method, ScriptCatalogResult, StatusParams,
    StatusResult, SwitchVariantResult, WatchStatusResult,
};
use stems_core::{Error, ErrorCode};
use tokio::sync::mpsc;

use crate::actions::{ACTION_TIMEOUT, MenuEntry};
use crate::model::{Cmd, LogSubscription, Model, Msg, Outcome, RpcResult, SignalKind};
use crate::prefs::Clipboard;
use crate::script::{Token, WaitFor};
use crate::update::{DETAIL_EVENTS, init, update};
use crate::view::{render_text, view};

/// Env var: panic after the first frame (terminal mode), to prove the
/// terminal is restored on panic.
pub const ENV_PANIC_TEST: &str = "STEMS_TUI_PANIC_TEST";

/// Run one command; `None` for [`Cmd::Exit`] and for the commands the
/// runners handle themselves ([`Cmd::SubscribeLogs`], [`Cmd::Copy`],
/// [`Cmd::SavePref`]).
pub async fn exec(client: &Client, cmd: &Cmd) -> Option<Msg> {
    let failed = |what: &str, e: Error| {
        Msg::Rpc(RpcResult::Failed {
            what: what.to_string(),
            message: e.message,
        })
    };
    Some(match cmd {
        Cmd::Exit(_)
        | Cmd::SubscribeLogs(_)
        | Cmd::Copy { .. }
        | Cmd::SavePref { .. }
        | Cmd::OpenEditor { .. } => {
            return None;
        }
        Cmd::Action(a) => Msg::Rpc(RpcResult::Action {
            action: a.clone(),
            result: client
                .call_with_timeout::<Value>(a.method(), a.params(), ACTION_TIMEOUT)
                .await
                .map_err(Box::new),
        }),
        Cmd::LoadCatalog => Msg::Rpc(RpcResult::Catalog(
            client
                .call::<ScriptCatalogResult>(Method::SCRIPT_CATALOG, json!({}))
                .await
                .map(|r| r.scripts.iter().map(MenuEntry::from).collect())
                .map_err(|e| e.message),
        )),
        Cmd::LoadPlan => Msg::Rpc(RpcResult::Plan(
            client
                .call::<ConfigDiffResult>(Method::CONFIG_DIFF, json!({}))
                .await
                .map(|d| plan_lines(&d))
                .map_err(|e| e.message),
        )),
        Cmd::LoadVariants(stem) => Msg::Rpc(RpcResult::Variants {
            stem: stem.clone(),
            result: client
                .call::<SwitchVariantResult>(Method::SWITCH_VARIANT, json!({ "stem": stem }))
                .await
                .map(|r| r.variants)
                .map_err(|e| e.message),
        }),
        Cmd::LoadWatch(stem) => Msg::Rpc(RpcResult::Watch {
            stem: stem.clone(),
            result: client
                .call::<WatchStatusResult>(Method::WATCH_STATUS, json!({ "stems": [stem] }))
                .await
                .map_err(|e| e.message)
                .and_then(|r| {
                    r.stems
                        .into_iter()
                        .find(|s| &s.name == stem)
                        .ok_or_else(|| format!("{stem} has no watch rules"))
                }),
        }),
        Cmd::LoadEvents => Msg::Rpc(RpcResult::Events(
            client
                .call::<EventsResult>(Method::EVENTS, json!({}))
                .await
                .map(|r| r.events)
                .map_err(|e| e.message),
        )),
        Cmd::RefreshStatus => {
            match client
                .call::<StatusResult>(Method::STATUS, StatusParams::default())
                .await
            {
                Ok(st) => Msg::Status(Box::new(st)),
                Err(e) => failed("status", e),
            }
        }
        Cmd::LoadDaemon => match client
            .call::<DaemonStatus>(Method::DAEMON_STATUS, json!({}))
            .await
        {
            Ok(d) => Msg::Rpc(RpcResult::Daemon(Box::new(d))),
            Err(e) => failed("daemon_status", e),
        },
        Cmd::LoadStemConfig(stem) => {
            let r = client
                .call::<Value>(Method::STEM_CONFIG, json!({ "stem": stem }))
                .await
                .map_err(|e| e.message);
            Msg::Rpc(RpcResult::StemConfig {
                stem: stem.clone(),
                result: r,
            })
        }
        Cmd::LoadStemHealth(stem) => {
            let r = client
                .call::<Value>(
                    Method::new("health"),
                    json!({ "stems": [stem], "last": DETAIL_EVENTS }),
                )
                .await
                .map(|v| {
                    v.pointer("/stems/0/results")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .map_err(|e| e.message);
            Msg::Rpc(RpcResult::StemHealth {
                stem: stem.clone(),
                result: r,
            })
        }
        Cmd::LoadGraph(names) => {
            let mut stems = Vec::with_capacity(names.len());
            let mut error = None;
            for n in names {
                match client
                    .call::<Value>(Method::STEM_CONFIG, json!({ "stem": n }))
                    .await
                {
                    Ok(v) => stems.push(crate::graph::GraphStem::from_config(n, &v)),
                    Err(e) => {
                        error = Some(format!("{n}: {}", e.message));
                        break;
                    }
                }
            }
            Msg::Rpc(RpcResult::Graph {
                names: names.clone(),
                result: error.map_or(Ok(stems), Err),
            })
        }
        Cmd::LoadStemEvents(stem) => {
            let r = client
                .call::<EventsResult>(Method::EVENTS, json!({}))
                .await
                .map(|r| {
                    let mine: Vec<_> = r
                        .events
                        .into_iter()
                        .filter(|e| e.stem.as_deref() == Some(stem.as_str()))
                        .collect();
                    let n = mine.len();
                    mine.into_iter()
                        .skip(n.saturating_sub(DETAIL_EVENTS))
                        .collect()
                })
                .map_err(|e| e.message);
            Msg::Rpc(RpcResult::StemEvents {
                stem: stem.clone(),
                result: r,
            })
        }
    })
}

/// The plan modal's lines for a `config_diff` result.
pub fn plan_lines(d: &ConfigDiffResult) -> Vec<String> {
    let mut out: Vec<String> = d
        .plan
        .affected()
        .map(|s| {
            let fields = if s.fields.is_empty() {
                String::new()
            } else {
                format!(" ({})", s.fields.join(", "))
            };
            let run = if s.running { "" } else { " · stopped" };
            format!("{:<16} {}{fields}{run}", s.name, s.action)
        })
        .collect();
    for w in &d.plan.workspace {
        out.push(format!("workspace        {w}"));
    }
    if out.is_empty() {
        out.push(if d.pending {
            "no stem changes".into()
        } else {
            "nothing pending".into()
        });
    }
    out
}

/// The codebase directory of `stem` (`stem_config`), else the workspace root.
async fn codebase_dir(client: &Client, stem: &str) -> Result<String, String> {
    let cfg = client
        .call::<Value>(Method::STEM_CONFIG, json!({ "stem": stem }))
        .await
        .map_err(|e| e.message)?;
    if let Some(p) = cfg.pointer("/codebase/path").and_then(Value::as_str) {
        return Ok(p.to_string());
    }
    let d = client
        .call::<DaemonStatus>(Method::DAEMON_STATUS, json!({}))
        .await
        .map_err(|e| e.message)?;
    d.info
        .workspace
        .map(|w| w.display().to_string())
        .ok_or_else(|| format!("{stem} has no codebase"))
}

/// Run `editor <dir>` through `sh -c` (so `EDITOR="code -w"` works); in
/// the terminal the caller has suspended the dashboard, headless runs it
/// with no terminal (stdio to /dev/null).
async fn run_editor(editor: &str, dir: &str, interactive: bool) -> Result<(), String> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(dir)
        .current_dir(dir);
    if !interactive {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }
    let st = cmd
        .status()
        .await
        .map_err(|e| format!("cannot run {editor}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{editor} exited with {st}"))
    }
}

/// `o` without a terminal: resolve the directory, run the editor, report.
pub async fn open_editor_headless(client: &Client, stem: &str, editor: &str) -> Msg {
    let result = match codebase_dir(client, stem).await {
        Ok(dir) => run_editor(editor, &dir, false).await.map(|()| dir),
        Err(e) => Err(e),
    };
    Msg::Rpc(RpcResult::Editor {
        stem: stem.to_string(),
        result,
    })
}

/// Subscribe to logs for the log pane and forward the records into `tx`
/// as [`Msg::Log`] from a task (abort it to end the subscription). The
/// subscription is acknowledged before this returns, so lines written
/// afterwards are delivered.
pub async fn subscribe_logs(
    client: &Client,
    sub: &LogSubscription,
    tx: mpsc::UnboundedSender<Msg>,
) -> Result<tokio::task::JoinHandle<()>, Error> {
    let params = stems_api::logs::SubscribeLogsParams {
        filter: stems_api::logs::LogFilter {
            stems: sub.stems.clone(),
            since: sub.since.clone(),
            tail: sub.tail,
            ..Default::default()
        },
    };
    let mut stream = client.subscribe_logs(params).await?;
    let generation = sub.generation;
    Ok(tokio::spawn(async move {
        while let Some(r) = stream.next().await {
            let m = Msg::Log {
                generation,
                record: Box::new(r),
            };
            if tx.send(m).is_err() {
                return;
            }
        }
    }))
}

/// Save a preference line; errors come back as a notice.
fn save_pref(path: &std::path::Path, key: &str, value: &str) -> Option<Msg> {
    crate::prefs::save_key(path, key, value)
        .err()
        .map(|message| {
            Msg::Rpc(RpcResult::Failed {
                what: "ui.toml".into(),
                message,
            })
        })
}

/// `GET /__chaos/<path>` on `port` (HTTP/1.0, 5 s bound); the status line.
pub async fn chaos_get(port: u16, path: &str) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let io = async {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| format!("connect 127.0.0.1:{port}: {e}"))?;
        let req = format!("GET /__chaos/{path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
        s.write_all(req.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        // A crash may reset the connection after the reply: keep what came.
        let _ = s.read_to_end(&mut buf).await;
        let text = String::from_utf8_lossy(&buf);
        Ok(text.lines().next().unwrap_or("").to_string())
    };
    tokio::time::timeout(Duration::from_secs(5), io)
        .await
        .map_err(|_| format!("chaos {path}: timed out"))?
}

/// Forward the daemon's events into `tx` (then [`Msg::Disconnected`]).
async fn forward_events(client: &Client, tx: mpsc::UnboundedSender<Msg>) -> Result<(), Error> {
    let mut events = client.subscribe_events(None).await?;
    tokio::spawn(async move {
        while let Some(e) = events.next().await {
            if tx.send(Msg::Event(Box::new(e))).is_err() {
                return;
            }
        }
        let _ = tx.send(Msg::Disconnected);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Headless
// ---------------------------------------------------------------------------

/// Options of [`run_headless`].
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// The script.
    pub tokens: Vec<Token>,
    /// Frame size (default 80x24).
    pub size: (u16, u16),
    /// Also write `frame-NNN.txt` files here.
    pub frames_out: Option<PathBuf>,
    /// Bound of each `wait:` token (default 30 s).
    pub wait_timeout: Duration,
}

impl HeadlessOptions {
    /// Defaults for `tokens`.
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            size: (80, 24),
            frames_out: None,
            wait_timeout: Duration::from_secs(30),
        }
    }
}

/// What a headless run produced.
#[derive(Clone, Debug)]
pub struct HeadlessRun {
    /// Every dumped frame, in order.
    pub frames: Vec<String>,
    /// How it ended (the script's end = detach).
    pub outcome: Outcome,
    /// The final model.
    pub model: Box<Model>,
}

/// Output of a headless run, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeadlessOutput<'a> {
    /// Frame `n` (from 1) as text.
    Frame(usize, &'a str),
    /// Raw terminal output, e.g. the OSC 52 sequence of a copy (`y`).
    Raw(&'a str),
}

struct Driver {
    client: Arc<Client>,
    model: Model,
    tx: mpsc::UnboundedSender<Msg>,
    logs: Option<tokio::task::JoinHandle<()>>,
    raw: Vec<String>,
    /// Highest event seq seen before the last key (`wait:event=`).
    mark: u64,
}

impl Driver {
    /// Run one command: the side effects here, RPCs through [`exec`].
    async fn exec(&mut self, cmd: &Cmd) -> Option<Msg> {
        match cmd {
            Cmd::SubscribeLogs(sub) => {
                if let Some(h) = self.logs.take() {
                    h.abort();
                }
                match subscribe_logs(&self.client, sub, self.tx.clone()).await {
                    Ok(h) => {
                        self.logs = Some(h);
                        None
                    }
                    Err(e) => Some(Msg::Rpc(RpcResult::Failed {
                        what: "logs".into(),
                        message: e.message,
                    })),
                }
            }
            // Headless never touches the real clipboard: OSC 52 goes to
            // stdout (tests assert it), the command fallback is skipped.
            Cmd::Copy { text, via } => {
                if *via == Clipboard::Osc52 {
                    self.raw.push(crate::clipboard::osc52(text));
                }
                None
            }
            Cmd::SavePref { path, key, value } => save_pref(path, key, value),
            Cmd::OpenEditor { stem, editor } => {
                Some(open_editor_headless(&self.client, stem, editor).await)
            }
            _ => exec(&self.client, cmd).await,
        }
    }

    async fn dispatch(&mut self, msg: Msg) {
        let mut q = VecDeque::from([msg]);
        while let Some(m) = q.pop_front() {
            for cmd in update(&mut self.model, m) {
                if let Some(next) = self.exec(&cmd).await {
                    q.push_back(next);
                }
            }
        }
    }

    async fn run_cmds(&mut self, cmds: Vec<Cmd>) {
        for cmd in cmds {
            if let Some(m) = self.exec(&cmd).await {
                self.dispatch(m).await;
            }
        }
    }

    async fn pump(&mut self, rx: &mut mpsc::UnboundedReceiver<Msg>) {
        while let Ok(m) = rx.try_recv() {
            self.dispatch(m).await;
        }
    }

    /// A fresh `status`, whatever is in flight.
    async fn refresh(&mut self) {
        self.model.refresh_pending = false;
        if let Some(m) = exec(&self.client, &Cmd::RefreshStatus).await {
            self.dispatch(m).await;
        }
    }

    /// `chaos:<path>` on the selected stem's first port.
    async fn chaos(&mut self, path: &str) -> Result<(), Error> {
        let stem = self.model.selected_stem();
        let port = stem.and_then(|s| s.ports.iter().find_map(|p| p.port));
        let Some(port) = port else {
            return Err(Error::usage(
                format!(
                    "headless script: chaos:{path} needs a selected stem with a port (selected: {})",
                    self.model.selected.as_deref().unwrap_or("none")
                ),
                "select a stem with a port first",
            ));
        };
        if let Err(e) = chaos_get(port, path).await {
            self.model.message = Some(e);
        }
        Ok(())
    }

    fn satisfied(&self, w: &WaitFor) -> bool {
        let is = |s: &stems_api::StemStatus, want: &str| {
            s.state.to_string() == want || s.glyph.name() == want
        };
        match w {
            WaitFor::All(state) => {
                self.model.loaded
                    && !self.model.stems.is_empty()
                    && self.model.stems.iter().all(|s| is(s, state))
            }
            WaitFor::Stem(name, state) => self
                .model
                .stems
                .iter()
                .any(|s| &s.name == name && is(s, state)),
            WaitFor::Lines(n) => self.model.log_pane.total() >= *n,
            WaitFor::Log(text) => self.model.log_pane.contains(text),
            WaitFor::Event { kind, stem } => self.model.events.iter().any(|e| {
                e.seq > self.mark
                    && e.kind == kind.as_str()
                    && stem.as_ref().is_none_or(|s| e.stem.as_ref() == Some(s))
            }),
        }
    }
}

/// Replay `opts.tokens` against the daemon, calling `on_frame(n, text)` for
/// every `frame` token (n from 1). Raw output (copies) is dropped; see
/// [`run_headless_output`].
pub async fn run_headless(
    client: Arc<Client>,
    model: Model,
    opts: HeadlessOptions,
    mut on_frame: impl FnMut(usize, &str),
) -> Result<HeadlessRun, Error> {
    run_headless_output(client, model, opts, |o| {
        if let HeadlessOutput::Frame(n, text) = o {
            on_frame(n, text);
        }
    })
    .await
}

/// [`run_headless`], also passing raw terminal output (the OSC 52
/// sequence of `y`) to `on_output`, in order with the frames.
pub async fn run_headless_output(
    client: Arc<Client>,
    mut model: Model,
    opts: HeadlessOptions,
    mut on_output: impl FnMut(HeadlessOutput<'_>),
) -> Result<HeadlessRun, Error> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    forward_events(&client, tx.clone()).await?;
    model.size = opts.size;
    let cmds = init(&mut model);
    let mut d = Driver {
        client,
        model,
        tx,
        logs: None,
        raw: Vec::new(),
        mark: 0,
    };
    d.run_cmds(cmds).await;
    if let Some(dir) = &opts.frames_out {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::internal(format!("cannot create {}: {e}", dir.display())))?;
    }
    let mut frames = Vec::new();
    for tok in &opts.tokens {
        d.pump(&mut rx).await;
        for r in std::mem::take(&mut d.raw) {
            on_output(HeadlessOutput::Raw(&r));
        }
        if d.model.exit.is_some() {
            break;
        }
        match tok {
            Token::Chaos(path) => {
                d.chaos(path).await?;
                d.pump(&mut rx).await;
            }
            Token::Key(k) => {
                d.mark = d.model.events.iter().map(|e| e.seq).max().unwrap_or(0);
                d.dispatch(Msg::Key(*k)).await;
            }
            Token::View(v) => d.dispatch(Msg::SetView(*v)).await,
            Token::Sleep(ms) => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
                d.pump(&mut rx).await;
            }
            Token::Frame => {
                d.refresh().await;
                d.pump(&mut rx).await;
                let text = render_text(&d.model, opts.size.0, opts.size.1);
                frames.push(text.clone());
                if let Some(dir) = &opts.frames_out {
                    let p = dir.join(format!("frame-{:03}.txt", frames.len()));
                    std::fs::write(&p, &text).map_err(|e| {
                        Error::internal(format!("cannot write {}: {e}", p.display()))
                    })?;
                }
                on_output(HeadlessOutput::Frame(frames.len(), &text));
            }
            Token::Wait(w) => {
                let deadline = Instant::now() + opts.wait_timeout;
                let mut last_refresh = Instant::now() - Duration::from_secs(1);
                loop {
                    d.pump(&mut rx).await;
                    if d.satisfied(w) || d.model.exit.is_some() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let secs = opts.wait_timeout.as_secs();
                        let msg = match w {
                            WaitFor::All(s) => {
                                format!("every stem did not reach `{s}` within {secs}s")
                            }
                            WaitFor::Stem(n, s) => {
                                format!("stem `{n}` did not reach `{s}` within {secs}s")
                            }
                            WaitFor::Lines(n) => format!(
                                "the log pane has {} lines, not {n}, after {secs}s",
                                d.model.log_pane.total()
                            ),
                            WaitFor::Log(t) => {
                                format!("no log line contains `{t}` after {secs}s")
                            }
                            WaitFor::Event { kind, stem } => format!(
                                "no `{kind}` event{} after the last key within {secs}s",
                                stem.as_ref()
                                    .map(|s| format!(" of `{s}`"))
                                    .unwrap_or_default()
                            ),
                        };
                        return Err(Error::new(
                            ErrorCode::HealthTimeout,
                            format!("headless script: {msg}"),
                        )
                        .with_details(json!({
                            "stems": d.model.stems.iter()
                                .map(|s| json!({"name": s.name, "state": s.state.to_string()}))
                                .collect::<Vec<_>>()
                        })));
                    }
                    if last_refresh.elapsed() >= Duration::from_millis(250) {
                        d.refresh().await;
                        last_refresh = Instant::now();
                        continue;
                    }
                    tokio::select! {
                        Some(m) = rx.recv() => d.dispatch(m).await,
                        () = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
            }
        }
        if d.model.exit.is_some() {
            break;
        }
    }
    for r in std::mem::take(&mut d.raw) {
        on_output(HeadlessOutput::Raw(&r));
    }
    if let Some(h) = d.logs.take() {
        h.abort();
    }
    let outcome = d.model.exit.unwrap_or(Outcome::Detach);
    Ok(HeadlessRun {
        frames,
        outcome,
        model: Box::new(d.model),
    })
}

// ---------------------------------------------------------------------------
// Real terminal
// ---------------------------------------------------------------------------

/// Options of [`run_terminal`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TerminalOptions {
    /// Panic after the first frame ([`ENV_PANIC_TEST`]).
    pub panic_test: bool,
}

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::internal(format!("terminal: {e}"))
}

fn spawn_signal(
    kind: tokio::signal::unix::SignalKind,
    msg: SignalKind,
    tx: mpsc::UnboundedSender<Msg>,
) {
    if let Ok(mut s) = tokio::signal::unix::signal(kind) {
        tokio::spawn(async move {
            while s.recv().await.is_some() {
                if tx.send(Msg::Signal(msg)).is_err() {
                    return;
                }
            }
        });
    }
}

/// Forward terminal input (keys, mouse, resize) into `tx` from a task;
/// nothing when not `interactive`.
fn spawn_input(tx: mpsc::UnboundedSender<Msg>, interactive: bool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !interactive {
            return;
        }
        use crossterm::event::{Event as CEvent, EventStream, KeyEventKind};
        let mut stream = EventStream::new();
        while let Some(Ok(ev)) = stream.next().await {
            let msg = match ev {
                CEvent::Key(k) if k.kind != KeyEventKind::Release => Msg::Key(k),
                CEvent::Mouse(m) => Msg::Mouse(m),
                CEvent::Resize(w, h) => Msg::Resize(w, h),
                _ => continue,
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
    })
}

/// Run the dashboard in the terminal until the user quits (or a signal /
/// the daemon ends it). The terminal is restored on every exit path.
pub async fn run_terminal(
    client: Arc<Client>,
    mut model: Model,
    opts: TerminalOptions,
) -> Result<Outcome, Error> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions as TermOpts, Viewport};

    let (tx, mut rx) = mpsc::unbounded_channel();
    forward_events(&client, tx.clone()).await?;
    let interactive = std::io::stdout().is_terminal() && std::io::stdin().is_terminal();
    let mouse = model.prefs.mouse && interactive;
    let mut guard = crate::terminal::TerminalGuard::enter(mouse, interactive).map_err(io_err)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut term = if interactive {
        Terminal::new(backend).map_err(io_err)?
    } else {
        // STEMS_TUI_FORCE on a pipe: no size to query.
        Terminal::with_options(
            backend,
            TermOpts {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .map_err(io_err)?
    };
    if let Ok(s) = term.size() {
        model.size = (s.width, s.height);
    }

    // Input (only from a real terminal: reading /dev/tty from a background
    // process group would stop us with SIGTTIN).
    let mut input = spawn_input(tx.clone(), interactive);
    use tokio::signal::unix::SignalKind as K;
    spawn_signal(K::terminate(), SignalKind::Terminate, tx.clone());
    spawn_signal(K::hangup(), SignalKind::Hangup, tx.clone());
    // Without keyboard input nobody could answer the quit prompt: SIGINT
    // then acts like SIGTERM.
    let on_int = if interactive {
        SignalKind::Interrupt
    } else {
        SignalKind::Terminate
    };
    spawn_signal(K::interrupt(), on_int, tx.clone());

    let mut log_task: Option<tokio::task::JoinHandle<()>> = None;
    // `o` is returned and run after the batch, with the dashboard suspended.
    let mut run = |cmds: Vec<Cmd>, client: &Arc<Client>, tx: &mpsc::UnboundedSender<Msg>| {
        let mut editor: Option<(String, String)> = None;
        for cmd in cmds {
            match &cmd {
                Cmd::Exit(_) => continue,
                Cmd::OpenEditor { stem, editor: e } => {
                    editor = Some((stem.clone(), e.clone()));
                    continue;
                }
                Cmd::SubscribeLogs(sub) => {
                    if let Some(h) = log_task.take() {
                        h.abort();
                    }
                    let (c, tx, sub) = (Arc::clone(client), tx.clone(), sub.clone());
                    log_task = Some(tokio::spawn(async move {
                        match subscribe_logs(&c, &sub, tx.clone()).await {
                            // Keep this task alive while the forwarder runs,
                            // so aborting it (a new subscription) ends both.
                            Ok(h) => {
                                struct AbortOnDrop(tokio::task::JoinHandle<()>);
                                impl Drop for AbortOnDrop {
                                    fn drop(&mut self) {
                                        self.0.abort();
                                    }
                                }
                                let mut g = AbortOnDrop(h);
                                let _ = (&mut g.0).await;
                            }
                            Err(e) => {
                                let _ = tx.send(Msg::Rpc(RpcResult::Failed {
                                    what: "logs".into(),
                                    message: e.message,
                                }));
                            }
                        }
                    }));
                    continue;
                }
                Cmd::Copy { text, via } => {
                    match via {
                        Clipboard::Osc52 => {
                            use std::io::Write;
                            let mut out = std::io::stdout();
                            let _ = out.write_all(crate::clipboard::osc52(text).as_bytes());
                            let _ = out.flush();
                        }
                        Clipboard::Command => crate::clipboard::copy_with_command(text.clone()),
                    }
                    continue;
                }
                Cmd::SavePref { path, key, value } => {
                    if let Some(m) = save_pref(path, key, value) {
                        let _ = tx.send(m);
                    }
                    continue;
                }
                _ => {}
            }
            let c = Arc::clone(client);
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Some(m) = exec(&c, &cmd).await {
                    let _ = tx.send(m);
                }
            });
        }
        editor
    };
    let _ = run(init(&mut model), &client, &tx);
    let mut tick = tokio::time::interval(Duration::from_millis(model.prefs.refresh_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut first = true;
    let outcome = loop {
        term.draw(|f| view(&model, f)).map_err(io_err)?;
        if first && opts.panic_test {
            panic!("{ENV_PANIC_TEST}=1: panicking after the first frame");
        }
        first = false;
        let msg = tokio::select! {
            m = rx.recv() => match m { Some(m) => m, None => Msg::Disconnected },
            _ = tick.tick() => Msg::Tick,
        };
        let cmds = update(&mut model, msg);
        let mut editor = run(cmds, &client, &tx);
        while let Some((stem, ed)) = editor.take() {
            // Suspend: stop reading keys, leave the alternate screen and raw
            // mode, run the editor on the real terminal, then come back.
            input.abort();
            let dir = codebase_dir(&client, &stem).await;
            drop(guard);
            let result = match dir {
                Ok(dir) => run_editor(&ed, &dir, interactive).await.map(|()| dir),
                Err(e) => Err(e),
            };
            guard = crate::terminal::TerminalGuard::enter(mouse, interactive).map_err(io_err)?;
            term.clear().map_err(io_err)?;
            input = spawn_input(tx.clone(), interactive);
            let cmds = update(&mut model, Msg::Rpc(RpcResult::Editor { stem, result }));
            editor = run(cmds, &client, &tx);
        }
        if let Some(o) = model.exit {
            break o;
        }
    };
    input.abort();
    if let Some(h) = log_task.take() {
        h.abort();
    }
    let _ = term.show_cursor();
    drop(term);
    drop(guard);
    Ok(outcome)
}
