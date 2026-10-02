//! The TUI state ([`Model`]), its inputs ([`Msg`]) and effects ([`Cmd`]).
//!
//! Elm-style: [`crate::update`] is a pure reducer `(&mut Model, Msg) ->
//! Vec<Cmd>`; [`crate::view`] renders a `&Model`; the runners
//! ([`crate::runner`]) execute the [`Cmd`]s against the daemon and feed the
//! results back as [`Msg`]s. The one exception to "view only reads":
//! [`Model::tab_hits`] and [`Model::stem_hits`], the header tabs' and the
//! stem strip's columns as last drawn, which the view records for mouse
//! hit-testing.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use crossterm::event::{KeyEvent, MouseEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stems_api::{
    DaemonStatus, Event, StatusResult, StatusSummary, StemStatus, StemWatchStatus, VariantChoice,
};
use stems_core::logs::LogRecord;

pub use crate::actions::{Action, MenuEntry, Palette, ScriptMenu};
pub use crate::events::EventsState;
pub use crate::form::ScriptForm;
use crate::graph::{GraphState, GraphStem};
pub use crate::logs::LogPane;
use crate::prefs::{Clipboard, Prefs};
pub use crate::scripts::ScriptsState;
pub use crate::toast::{Toast, ToastKind, Toasts};

/// Events kept in memory (detail and Events views).
pub const EVENT_RING: usize = 2000;

/// The dashboard's views.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewKind {
    /// The stem table.
    Table,
    /// The selected stem's detail.
    Detail,
    /// The dependency graph (deliverable 28).
    Graph,
    /// Logs (deliverable 29).
    Logs,
    /// Events (deliverable 29).
    Events,
    /// Every runnable script: the workspace's, then each stem's.
    Scripts,
}

impl ViewKind {
    /// Every view, in `Tab` order.
    pub const ALL: [ViewKind; 6] = [
        ViewKind::Graph,
        ViewKind::Table,
        ViewKind::Detail,
        ViewKind::Logs,
        ViewKind::Events,
        ViewKind::Scripts,
    ];

    /// Title-case name shown in the tab bar.
    pub fn title(self) -> &'static str {
        match self {
            ViewKind::Table => "Table",
            ViewKind::Detail => "Detail",
            ViewKind::Graph => "Graph",
            ViewKind::Logs => "Logs",
            ViewKind::Events => "Events",
            ViewKind::Scripts => "Scripts",
        }
    }

    /// The view's direct key (`1`-`6`, in `Tab` order).
    pub fn digit(self) -> char {
        let i = ViewKind::ALL.iter().position(|v| *v == self).unwrap_or(0);
        char::from(b'1' + i as u8)
    }

    /// The view of a direct key (`1`-`6`).
    pub fn from_digit(c: char) -> Option<ViewKind> {
        let i = c.to_digit(10)?.checked_sub(1)?;
        ViewKind::ALL.get(i as usize).copied()
    }

    /// Parse `table`, `detail`, ... (case-insensitive).
    pub fn parse(s: &str) -> Option<ViewKind> {
        ViewKind::ALL
            .into_iter()
            .find(|v| v.title().eq_ignore_ascii_case(s.trim()))
    }

    /// The next view (`Tab`), wrapping.
    pub fn next(self) -> ViewKind {
        let i = ViewKind::ALL.iter().position(|v| *v == self).unwrap_or(0);
        ViewKind::ALL[(i + 1) % ViewKind::ALL.len()]
    }

    /// The previous view (`Shift-Tab`), wrapping.
    pub fn prev(self) -> ViewKind {
        let i = ViewKind::ALL.iter().position(|v| *v == self).unwrap_or(0);
        ViewKind::ALL[(i + ViewKind::ALL.len() - 1) % ViewKind::ALL.len()]
    }
}

/// A modal dialog on top of the view: every key goes to it first
/// (handled in one place, `update::modal_key`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Modal {
    /// Nothing.
    #[default]
    None,
    /// "Stop everything? [y/N/d(etach)]" (attached `up`).
    QuitConfirm,
    /// `x` on a stem with running dependants (the daemon answered
    /// `HAS_DEPENDANTS`): `y`/`X` stop them too, `n` cancels.
    StopConfirm {
        /// The stem.
        stem: String,
        /// Its running dependants.
        dependants: Vec<String>,
    },
    /// `d`: "Stop every stem and the daemon? [y/N]".
    DownConfirm,
    /// `S`: type `reset` then `Enter`.
    ResetTyped {
        /// The stem.
        stem: String,
        /// What was typed.
        buffer: String,
    },
    /// `:`: the script menu.
    ScriptMenu(ScriptMenu),
    /// The argument form of a script.
    ScriptForm(ScriptForm),
    /// `Ctrl-P`: the command palette.
    Palette(Palette),
    /// `e` on an error toast.
    ErrorDetails {
        /// Title (the code).
        title: String,
        /// Message, hint, details.
        text: String,
    },
    /// `v` on the config-changed toast: the `config_diff` plan (`None`
    /// while loading).
    Plan(Option<Result<Vec<String>, String>>),
    /// `v` on a stem: pick one of its variants ([`Model::variants`];
    /// `Enter` asks [`Modal::SwitchConfirm`]).
    VariantPicker {
        /// The stem.
        stem: String,
        /// Selected position among its choices.
        selected: usize,
    },
    /// `r` on a stem with running hard dependants: "also restart N
    /// dependants (b, c, d)? [y/N]": `y` restarts with `cascade: true`,
    /// `n`/`Enter` only the stem (`cascade: false`), `Esc` cancels.
    RestartConfirm {
        /// The stem.
        stem: String,
        /// Its running hard dependants (transitive, nearest first).
        dependants: Vec<String>,
    },
    /// "Restart shop-api as docker? [y/N]": `y` sends `switch_variant`.
    SwitchConfirm {
        /// The stem.
        stem: String,
        /// Its active choice.
        from: String,
        /// The chosen variant (`local`: the base definition).
        variant: String,
        /// The stem type with it.
        kind: Option<String>,
        /// The stem runs (it restarts in its new form).
        running: bool,
    },
}

impl Modal {
    /// Whether a dialog is open.
    pub fn is_open(&self) -> bool {
        *self != Modal::None
    }
}

/// Table sort order (`s` cycles).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortKey {
    /// Declaration order (as `status` returns it).
    #[default]
    Declared,
    /// By name.
    Name,
    /// By state (failed first, then transitioning, healthy, stopped).
    State,
    /// Longest uptime first.
    Uptime,
}

impl SortKey {
    /// The next sort key.
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Declared => SortKey::Name,
            SortKey::Name => SortKey::State,
            SortKey::State => SortKey::Uptime,
            SortKey::Uptime => SortKey::Declared,
        }
    }

    /// Short label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Declared => "config",
            SortKey::Name => "name",
            SortKey::State => "state",
            SortKey::Uptime => "uptime",
        }
    }
}

/// How the TUI was opened; decides what quitting does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachMode {
    /// `stems attach`: quitting just leaves (unless the daemon is ours).
    Attach,
    /// Attached `stems up`: quitting asks "stop everything? [y/N/d(etach)]".
    Up,
}

/// How the TUI ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Leave the daemon and its stems running.
    Detach,
    /// Run `down --all` (stop every stem and the daemon).
    StopAll,
}

/// Data of the detail view for one stem.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DetailData {
    /// The stem shown.
    pub stem: String,
    /// `stem_config` result (`None` while loading).
    pub config: Option<Result<Value, String>>,
    /// Recent events of this stem (oldest first).
    pub events: Vec<Event>,
    /// Recent probe results from the `health` RPC (oldest first; `None`
    /// while loading, `Err` when the daemon has no such RPC or it failed).
    pub health: Option<Result<Vec<Value>, String>>,
    /// The stem's watchdogs from `watch_status` (rules, last trigger);
    /// `None` while loading or for a stem without `watch:` rules.
    pub watch: Option<Result<StemWatchStatus, String>>,
}

/// What the dashboard last saw of a script (its `script.*` events, any
/// actor): shown inline in the Detail view's Scripts section and in the
/// Scripts view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptActivity {
    /// `script.queued`: waiting for another run of the stem.
    Queued,
    /// `script.started`.
    Running,
    /// `script.finished`: `ok`, and `1.2s` or `exit 3` / `timed out`.
    Finished {
        /// Exit 0, in time.
        ok: bool,
        /// Duration (ok) or why it failed.
        text: String,
    },
}

/// Per-stem metric history, fed from each `status` result's `metrics`
/// (25): a sample is appended when its `ts` is new; the last
/// [`stems_api::metrics::SPARK_SAMPLES`] are kept.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MetricHistory {
    /// CPU percent samples, oldest first.
    pub cpu: Vec<f64>,
    /// Memory (bytes) samples, oldest first.
    pub mem: Vec<f64>,
    /// Time of the newest sample.
    pub last_ts: Option<chrono::DateTime<chrono::Utc>>,
}

impl MetricHistory {
    /// Append the latest `metrics` of each stem of `st` (25); stems without
    /// one keep their history.
    pub fn feed(all: &mut BTreeMap<String, MetricHistory>, st: &StatusResult) {
        let keep = stems_api::metrics::SPARK_SAMPLES;
        for s in &st.stems {
            let Some(m) = s.metrics else { continue };
            let h = all.entry(s.name.clone()).or_default();
            if h.last_ts.is_some_and(|t| t >= m.ts) {
                continue;
            }
            h.last_ts = Some(m.ts);
            h.cpu.push(m.cpu_pct);
            h.mem.push(m.rss_bytes as f64);
            for v in [&mut h.cpu, &mut h.mem] {
                let n = v.len();
                v.drain(..n.saturating_sub(keep));
            }
        }
    }
}

/// Everything the dashboard shows.
#[derive(Clone, Debug)]
pub struct Model {
    /// How the TUI was opened.
    pub mode: AttachMode,
    /// The daemon was started for this session (quitting asks then too).
    pub owns_daemon: bool,
    /// Workspace name (from `daemon_status`).
    pub workspace: String,
    /// Active profile, if any.
    pub profile: Option<String>,
    /// Daemon pid.
    pub daemon_pid: Option<u32>,
    /// Stems as of the last `status` (event-patched in between).
    pub stems: Vec<StemStatus>,
    /// Counts of the last `status`.
    pub summary: StatusSummary,
    /// A `status` has arrived.
    pub loaded: bool,
    /// Selected stem (by name, stable across refreshes and sorting).
    pub selected: Option<String>,
    /// Current view.
    pub view: ViewKind,
    /// No view was asked for (`--view`, `default_view`): the first `status`
    /// picks Graph for more than one stem, Table otherwise.
    pub auto_view: bool,
    /// Where `Esc` in the detail view goes back to.
    pub return_view: ViewKind,
    /// The user moved the selection (the graph otherwise starts on its
    /// first box).
    pub touched: bool,
    /// The graph view (28).
    pub graph: GraphState,
    /// Table filter (substring of the stem name, case-insensitive).
    pub filter: String,
    /// Typing into the filter (`/`).
    pub filter_editing: bool,
    /// Table sort.
    pub sort: SortKey,
    /// Help overlay (`?`).
    pub help: bool,
    /// Modal dialog.
    pub modal: Modal,
    /// Preferences.
    pub prefs: Prefs,
    /// ASCII glyphs.
    pub ascii: bool,
    /// Detail data of the selected stem.
    pub detail: Option<DetailData>,
    /// Rows the Detail view is scrolled down (`j`/`k`, `PgUp`/`PgDn`,
    /// `g`/`G`, the wheel); 0 again for another stem. Kept within the
    /// content by the reducer (and clamped again when drawn).
    pub detail_scroll: usize,
    /// The selected row of the Detail view's actionable rows (scripts,
    /// variants, the watchdog; `crate::detail::rows`); 0 again for
    /// another stem.
    pub detail_row: usize,
    /// Each stem's variant choices (`switch_variant {stem}`), loaded for
    /// the stems whose `status` has a `variant`.
    pub variants: BTreeMap<String, Result<Vec<VariantChoice>, String>>,
    /// Stems whose choices are being loaded.
    pub variants_pending: BTreeSet<String>,
    /// `r` waits for the dependency graph (loaded on demand) to know the
    /// stem's dependants.
    pub pending_restart: Option<String>,
    /// The last `script.*` state of each `(stem, script)`.
    pub script_activity: BTreeMap<(Option<String>, String), ScriptActivity>,
    /// Recent events (ring of [`EVENT_RING`]).
    pub events: VecDeque<Event>,
    /// The Events view (29).
    pub events_view: EventsState,
    /// The Scripts view.
    pub scripts_view: ScriptsState,
    /// The log pane of the Logs view and of the split layout (29).
    pub log_pane: LogPane,
    /// Metric history per stem (25).
    pub metrics: BTreeMap<String, MetricHistory>,
    /// Ticks so far.
    pub ticks: u64,
    /// A status refresh is in flight.
    pub refresh_pending: bool,
    /// Last notice or error, shown in the status bar.
    pub message: Option<String>,
    /// Terminal size (for mouse hit-testing).
    pub size: (u16, u16),
    /// Set when the TUI should end.
    pub exit: Option<Outcome>,
    /// Toasts (30), bottom-right.
    pub toasts: Toasts,
    /// The tick clock (ms): `refresh_ms` per [`Msg::Tick`]; toasts expire on it.
    pub clock_ms: u64,
    /// The script catalogue (`script_catalog`), once loaded.
    pub catalog: Option<Vec<MenuEntry>>,
    /// `$EDITOR` (the CLI sets it; `o` falls back to `vi`).
    pub editor: Option<String>,
    /// Script runs started from the dashboard, awaiting `script.finished`.
    pub script_runs: Vec<ScriptRun>,
    /// The split pane follows this log stem instead of the selection (a
    /// workspace script's `_workspace` output) until the selection moves.
    pub log_focus: Option<String>,
    /// The header's view tabs as last drawn: `(view, first column, end
    /// column exclusive)` on row 0. Written by [`crate::view::view`] (the
    /// only render-time state), read by the mouse handler.
    pub tab_hits: RefCell<Vec<(ViewKind, u16, u16)>>,
    /// The stem strip of the Detail / Logs title as last drawn: `(stem,
    /// row, first column, end column exclusive)`. Written by the view,
    /// read by the mouse handler (a click selects that stem).
    pub stem_hits: RefCell<Vec<StemHit>>,
    /// The action bar's segments as last drawn: `(key, row, first column,
    /// end column exclusive)`; a click presses that key.
    pub bar_hits: RefCell<Vec<(char, u16, u16, u16)>>,
}

/// A stem's cells in the Detail / Logs stem strip ([`Model::stem_hits`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StemHit {
    /// The stem.
    pub stem: String,
    /// Screen row.
    pub row: u16,
    /// First column.
    pub start: u16,
    /// End column (exclusive).
    pub end: u16,
}

/// A script run started from the dashboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptRun {
    /// Owning stem.
    pub stem: Option<String>,
    /// Script.
    pub script: String,
    /// Run id, once `run_script` answered.
    pub run_id: Option<String>,
}

impl Model {
    /// A fresh model: no data yet, the view from the preferences (or `view`).
    pub fn new(mode: AttachMode, prefs: Prefs, view: Option<ViewKind>) -> Self {
        let asked = view.or(prefs.default_view);
        let view = asked.unwrap_or(ViewKind::Table);
        Self {
            mode,
            owns_daemon: false,
            workspace: String::new(),
            profile: None,
            daemon_pid: None,
            stems: Vec::new(),
            summary: StatusSummary::default(),
            loaded: false,
            selected: None,
            view,
            auto_view: asked.is_none(),
            return_view: ViewKind::Table,
            touched: false,
            graph: GraphState::default(),
            filter: String::new(),
            filter_editing: false,
            sort: SortKey::Declared,
            help: false,
            modal: Modal::None,
            log_pane: LogPane {
                visible: prefs.split_logs,
                ..LogPane::default()
            },
            events_view: EventsState::default(),
            scripts_view: ScriptsState::default(),
            prefs,
            ascii: false,
            detail: None,
            detail_scroll: 0,
            detail_row: 0,
            variants: BTreeMap::new(),
            variants_pending: BTreeSet::new(),
            script_activity: BTreeMap::new(),
            pending_restart: None,
            events: VecDeque::new(),
            metrics: BTreeMap::new(),
            ticks: 0,
            refresh_pending: false,
            message: None,
            size: (80, 24),
            exit: None,
            toasts: Toasts::default(),
            clock_ms: 0,
            catalog: None,
            editor: None,
            script_runs: Vec::new(),
            log_focus: None,
            tab_hits: RefCell::new(Vec::new()),
            stem_hits: RefCell::new(Vec::new()),
            bar_hits: RefCell::new(Vec::new()),
        }
    }

    /// Stems shown in the table: filtered, then sorted.
    pub fn visible(&self) -> Vec<&StemStatus> {
        let f = self.filter.to_lowercase();
        let mut v: Vec<&StemStatus> = self
            .stems
            .iter()
            .filter(|s| f.is_empty() || s.name.to_lowercase().contains(&f))
            .collect();
        match self.sort {
            SortKey::Declared => {}
            SortKey::Name => v.sort_by(|a, b| a.name.cmp(&b.name)),
            SortKey::State => v.sort_by_key(|s| (state_rank(s), s.name.clone())),
            SortKey::Uptime => {
                v.sort_by(|a, b| b.uptime_s.cmp(&a.uptime_s).then(a.name.cmp(&b.name)))
            }
        }
        v
    }

    /// Index of the selected stem among [`Model::visible`].
    pub fn selected_index(&self) -> Option<usize> {
        let name = self.selected.as_deref()?;
        self.visible().iter().position(|s| s.name == name)
    }

    /// The selected stem's status.
    pub fn selected_stem(&self) -> Option<&StemStatus> {
        let name = self.selected.as_deref()?;
        self.stems.iter().find(|s| s.name == name)
    }

    /// Whether `stem`'s watchdog is paused (as of the last `status`).
    pub fn watch_paused(&self, stem: &str) -> bool {
        self.stems
            .iter()
            .find(|s| s.name == stem)
            .and_then(|s| s.watch.as_ref())
            .is_some_and(|w| w.paused)
    }

    /// The custom scripts of `stem` in the catalogue (`None` until it is
    /// loaded).
    pub fn custom_scripts(&self, stem: &str) -> Option<usize> {
        self.catalog.as_ref().map(|c| {
            c.iter()
                .filter(|e| {
                    e.stem.as_deref() == Some(stem)
                        && e.kind == stems_core::scriptargs::ScriptKind::Custom
                })
                .count()
        })
    }

    /// The workspace-level scripts in the catalogue (`None` until it is
    /// loaded).
    pub fn workspace_scripts(&self) -> Option<usize> {
        self.catalog
            .as_ref()
            .map(|c| c.iter().filter(|e| e.stem.is_none()).count())
    }

    /// `stem`'s variant choices, once loaded.
    pub fn variant_choices(&self, stem: &str) -> Option<&[VariantChoice]> {
        self.variants
            .get(stem)
            .and_then(|r| r.as_ref().ok())
            .map(Vec::as_slice)
    }

    /// Recent events of `stem`, oldest first, at most `n`.
    pub fn stem_events(&self, stem: &str, n: usize) -> Vec<&Event> {
        let all: Vec<&Event> = self
            .events
            .iter()
            .filter(|e| e.stem.as_deref() == Some(stem))
            .collect();
        all[all.len().saturating_sub(n)..].to_vec()
    }
}

fn state_rank(s: &StemStatus) -> u8 {
    use stems_core::Glyph;
    match s.glyph {
        Glyph::Failed => 0,
        Glyph::Degraded => 1,
        Glyph::Transitioning => 2,
        Glyph::Unknown => 3,
        Glyph::Healthy => 4,
        Glyph::Stopped => 5,
    }
}

/// An RPC result delivered back to the reducer.
#[derive(Clone, Debug, PartialEq)]
pub enum RpcResult {
    /// `daemon_status`.
    Daemon(Box<DaemonStatus>),
    /// `stem_config {stem}`.
    StemConfig {
        /// Stem asked for.
        stem: String,
        /// The config JSON or an error message.
        result: Result<Value, String>,
    },
    /// Probe history of a stem (`health {stems: [stem]}`).
    StemHealth {
        /// Stem asked for.
        stem: String,
        /// Probe records (`{ts, ok, outcome, latency_ms, detail}`, oldest
        /// first) or an error message.
        result: Result<Vec<Value>, String>,
    },
    /// Buffered events of a stem (`events`, filtered).
    StemEvents {
        /// Stem asked for.
        stem: String,
        /// Events (oldest first) or an error message.
        result: Result<Vec<Event>, String>,
    },
    /// The dependency graph: `stem_config` of every stem (`names`, sorted).
    Graph {
        /// Stems asked for.
        names: Vec<String>,
        /// Their types and edges, or an error message.
        result: Result<Vec<GraphStem>, String>,
    },
    /// The daemon's buffered events (`events`), for the Events view.
    Events(Result<Vec<Event>, String>),
    /// `script_catalog` (every script) -> [`Model::catalog`].
    Catalog(Result<Vec<MenuEntry>, String>),
    /// The result of an [`Action`].
    Action {
        /// What was done.
        action: Action,
        /// The RPC result (JSON) or its error.
        result: Result<Value, Box<stems_core::Error>>,
    },
    /// `$EDITOR` returned.
    Editor {
        /// The stem whose codebase was opened.
        stem: String,
        /// The directory, or why it failed.
        result: Result<String, String>,
    },
    /// `config_diff`, as lines for the plan modal.
    Plan(Result<Vec<String>, String>),
    /// A stem's variant choices (`switch_variant {stem}`).
    Variants {
        /// Stem asked for.
        stem: String,
        /// Its choices (`local` first) or an error message.
        result: Result<Vec<VariantChoice>, String>,
    },
    /// A stem's watchdogs (`watch_status {stems: [stem]}`).
    Watch {
        /// Stem asked for.
        stem: String,
        /// Its rules and state or an error message.
        result: Result<StemWatchStatus, String>,
    },
    /// A call failed.
    Failed {
        /// What was being done.
        what: String,
        /// The error message.
        message: String,
    },
}

/// A signal delivered to the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalKind {
    /// SIGINT (only arrives outside raw mode).
    Interrupt,
    /// SIGTERM.
    Terminate,
    /// SIGHUP (terminal closed).
    Hangup,
}

/// Inputs of the reducer.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    /// A key press.
    Key(KeyEvent),
    /// A mouse event (only with `mouse = true`).
    Mouse(MouseEvent),
    /// Periodic tick (`refresh_ms`).
    Tick,
    /// A daemon event.
    Event(Box<Event>),
    /// A `status` result.
    Status(Box<StatusResult>),
    /// Another RPC result.
    Rpc(RpcResult),
    /// The terminal was resized.
    Resize(u16, u16),
    /// A signal.
    Signal(SignalKind),
    /// The event stream ended (the daemon went away).
    Disconnected,
    /// Switch to a view (headless `view:<name>`).
    SetView(ViewKind),
    /// A log record of the log pane's subscription `generation`.
    Log {
        /// [`LogSubscription::generation`] it belongs to.
        generation: u64,
        /// The record.
        record: Box<LogRecord>,
    },
}

/// A `subscribe_logs` for the log pane ([`Cmd::SubscribeLogs`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogSubscription {
    /// Records are delivered as [`Msg::Log`] with this generation; starting
    /// a subscription ends the previous one.
    pub generation: u64,
    /// Stems (empty: every stem).
    pub stems: Vec<String>,
    /// Replay from this instant (RFC 3339).
    pub since: Option<String>,
    /// Replay the last `n` records.
    pub tail: Option<usize>,
}

/// Effects the runner performs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Call `status` -> [`Msg::Status`].
    RefreshStatus,
    /// Call `daemon_status` -> [`RpcResult::Daemon`].
    LoadDaemon,
    /// Call `stem_config` -> [`RpcResult::StemConfig`].
    LoadStemConfig(String),
    /// Call `events` and keep the stem's -> [`RpcResult::StemEvents`].
    LoadStemEvents(String),
    /// Call `health` for the stem -> [`RpcResult::StemHealth`].
    LoadStemHealth(String),
    /// Call `stem_config` for each stem -> [`RpcResult::Graph`].
    LoadGraph(Vec<String>),
    /// Call `events` -> [`RpcResult::Events`].
    LoadEvents,
    /// (Re)subscribe the log pane -> [`Msg::Log`]s.
    SubscribeLogs(LogSubscription),
    /// Put `text` on the clipboard (fire-and-forget; headless prints the
    /// OSC 52 sequence).
    Copy {
        /// The text.
        text: String,
        /// How.
        via: Clipboard,
    },
    /// Save `key = value` (TOML) in the preferences file.
    SavePref {
        /// `ui.toml`.
        path: PathBuf,
        /// Key.
        key: String,
        /// TOML value.
        value: String,
    },
    /// Run an [`Action`] -> [`RpcResult::Action`].
    Action(Action),
    /// Call `script_catalog` -> [`RpcResult::Catalog`].
    LoadCatalog,
    /// Call `config_diff` -> [`RpcResult::Plan`].
    LoadPlan,
    /// Call `switch_variant {stem}` (list only) -> [`RpcResult::Variants`].
    LoadVariants(String),
    /// Call `watch_status {stems: [stem]}` -> [`RpcResult::Watch`].
    LoadWatch(String),
    /// Open the stem's codebase in the editor (the terminal runner
    /// suspends the dashboard; headless runs it without terminal changes)
    /// -> [`RpcResult::Editor`].
    OpenEditor {
        /// The stem.
        stem: String,
        /// The editor command (`$EDITOR`; may hold arguments).
        editor: String,
    },
    /// End the TUI.
    Exit(Outcome),
}
