//! The log pane (29): a ring of log records with a search index, and the
//! [`LogPane`] component that the Logs view, the split pane (`Ctrl-L`) and
//! 30's script output all use.
//!
//! The pane is fed with [`LogPane::push`] (the dashboard feeds it from
//! `subscribe_logs`), handles its own keys ([`LogPane::key`]) and renders
//! into any area ([`LogPane::render`]). It knows nothing about the daemon.
//!
//! Keys: `Space` pause/resume follow, `/` search (`n`/`N` next/previous
//! match), `L` level filter (`all → info+ → warn+ → error`), `s` script
//! lines, `j`/`k` move, `g`/`G` top/bottom (bottom follows again), `V`
//! visual range, `y` copy (the line or the range), `w` wrap, `t`
//! timestamps, `Enter` expands a structured line's fields, `Esc` leaves the
//! range / clears the search.

use std::collections::{BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::Value;
use stems_core::logs::{Level, LevelFilter, LogRecord, Stream};
use unicode_width::UnicodeWidthChar;

/// Lines kept per pane (the oldest are dropped).
pub const LOG_RING: usize = 20_000;

/// Records replayed when a pane subscribes (`tail`).
pub const LOG_REPLAY: usize = 2_000;

/// One line of the ring: the record, its id (monotonic per ring) and its
/// search key (lowercased text and fields).
#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    /// Monotonic id (never reused within a ring).
    pub id: u64,
    /// The record.
    pub record: LogRecord,
    /// Search key: `text` and the fields (`k=v`), lowercased.
    pub key: String,
}

/// A bounded ring of log lines, searchable by substring.
#[derive(Clone, Debug, PartialEq)]
pub struct LogRing {
    lines: VecDeque<LogLine>,
    next_id: u64,
    cap: usize,
}

impl Default for LogRing {
    fn default() -> Self {
        Self::with_capacity(LOG_RING)
    }
}

/// The fields of a structured record as `k=v` pairs (strings unquoted).
pub fn fields_text(rec: &LogRecord) -> String {
    rec.fields
        .as_ref()
        .map(|m| {
            m.iter()
                .map(|(k, v)| match v {
                    Value::String(s) => format!("{k}={s}"),
                    v => format!("{k}={v}"),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

impl LogRing {
    /// A ring keeping at most `cap` lines (at least 1).
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            next_id: 0,
            cap: cap.max(1),
        }
    }

    /// Append a record; returns its id. The oldest line goes when full.
    pub fn push(&mut self, record: LogRecord) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut key = record.text.to_lowercase();
        let f = fields_text(&record);
        if !f.is_empty() {
            key.push(' ');
            key.push_str(&f.to_lowercase());
        }
        self.lines.push_back(LogLine { id, record, key });
        while self.lines.len() > self.cap {
            self.lines.pop_front();
        }
        id
    }

    /// Lines held.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// No lines.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Drop every line (ids keep increasing).
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// The line at position `i` (0 = oldest held).
    pub fn at(&self, i: usize) -> Option<&LogLine> {
        self.lines.get(i)
    }

    /// The line with id `id`.
    pub fn get(&self, id: u64) -> Option<&LogLine> {
        let first = self.lines.front()?.id;
        self.lines
            .get(usize::try_from(id.checked_sub(first)?).ok()?)
    }

    /// Lines, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &LogLine> {
        self.lines.iter()
    }

    /// Ids of the lines containing `query` (case-insensitive), oldest first.
    pub fn search(&self, query: &str) -> Vec<u64> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.lines
            .iter()
            .filter(|l| l.key.contains(&q))
            .map(|l| l.id)
            .collect()
    }
}

/// The level filter `L` cycles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LevelMode {
    /// Every line (also those without a level).
    #[default]
    All,
    /// Info and above.
    Info,
    /// Warn and above.
    Warn,
    /// Errors only.
    Error,
}

impl LevelMode {
    /// The next mode: `all → info+ → warn+ → error → all`.
    pub fn next(self) -> LevelMode {
        match self {
            LevelMode::All => LevelMode::Info,
            LevelMode::Info => LevelMode::Warn,
            LevelMode::Warn => LevelMode::Error,
            LevelMode::Error => LevelMode::All,
        }
    }

    /// `all`, `info+`, `warn+`, `error`.
    pub fn label(self) -> &'static str {
        match self {
            LevelMode::All => "all",
            LevelMode::Info => "info+",
            LevelMode::Warn => "warn+",
            LevelMode::Error => "error",
        }
    }

    /// The equivalent `stems logs --level` filter (`None` for all).
    pub fn filter(self) -> Option<LevelFilter> {
        let (level, and_above) = match self {
            LevelMode::All => return None,
            LevelMode::Info => (Level::Info, true),
            LevelMode::Warn => (Level::Warn, true),
            LevelMode::Error => (Level::Error, true),
        };
        Some(LevelFilter { level, and_above })
    }

    /// Whether a line of `level` is shown (lines without a level only in
    /// `all`).
    pub fn matches(self, level: Option<Level>) -> bool {
        self.filter().is_none_or(|f| f.matches(level))
    }
}

/// What a pane is subscribed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTarget {
    /// One stem, or every stem (merged).
    pub stem: Option<String>,
    /// Replay from this instant (RFC 3339) instead of the last lines.
    pub since: Option<String>,
}

/// A pending jump from the Events view: open `stem`'s logs replayed from
/// `since`, positioned at `at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogJump {
    /// The stem.
    pub stem: String,
    /// Replay start (RFC 3339).
    pub since: String,
    /// The event time the selection goes to.
    pub at: DateTime<Utc>,
}

/// What a key did to the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneAction {
    /// Not a pane key.
    Ignored,
    /// Handled.
    Handled,
    /// Copy this text (`y`).
    Copy {
        /// The text.
        text: String,
        /// Lines copied.
        lines: usize,
    },
}

/// How [`LogPane::render`] draws.
#[derive(Clone, Debug)]
pub struct PaneStyle {
    /// Block title (e.g. `Logs: echo-svc`).
    pub title: String,
    /// ASCII glyphs.
    pub ascii: bool,
    /// Accent colour.
    pub accent: Color,
    /// A right-aligned second title (the Logs view's stem strip).
    pub strip: Option<Line<'static>>,
}

/// The log pane: a ring plus follow/pause, search, filters, selection.
#[derive(Clone, Debug, PartialEq)]
pub struct LogPane {
    /// Shown under Table/Graph/Detail (split layout, `Ctrl-L`).
    pub visible: bool,
    /// Every stem, with stem prefixes (`m` in the Logs view).
    pub merged: bool,
    /// The lines.
    pub ring: LogRing,
    /// Records received while paused (shown on resume).
    pub pending: VecDeque<LogRecord>,
    /// Paused (`Space`): new records wait in `pending`.
    pub paused: bool,
    /// The selection follows the newest line.
    pub follow: bool,
    /// Selected line id (when not following).
    pub selected: Option<u64>,
    /// Start of the visual range (`V`).
    pub visual: Option<u64>,
    /// Level filter (`L`).
    pub level: LevelMode,
    /// Show script output lines (`s`).
    pub show_scripts: bool,
    /// Wrap long lines (`w`).
    pub wrap: bool,
    /// Show timestamps (`t`).
    pub timestamps: bool,
    /// Search text (`/`).
    pub search: String,
    /// Typing the search.
    pub search_editing: bool,
    /// Structured lines whose fields are expanded (`Enter`).
    pub expanded: BTreeSet<u64>,
    /// What the pane is subscribed to (the dashboard's bookkeeping).
    pub source: Option<LogTarget>,
    /// Subscription generation: records of older ones are ignored.
    pub generation: u64,
    /// A jump from the Events view (the dashboard's bookkeeping).
    pub jump: Option<LogJump>,
    /// Move the selection to the newest line at or before this instant as
    /// records arrive (a jump), until the user moves.
    pub anchor_at: Option<DateTime<Utc>>,
}

impl Default for LogPane {
    fn default() -> Self {
        Self {
            visible: false,
            merged: false,
            ring: LogRing::default(),
            pending: VecDeque::new(),
            paused: false,
            follow: true,
            selected: None,
            visual: None,
            level: LevelMode::All,
            show_scripts: true,
            wrap: false,
            timestamps: true,
            search: String::new(),
            search_editing: false,
            expanded: BTreeSet::new(),
            source: None,
            generation: 0,
            jump: None,
            anchor_at: None,
        }
    }
}

/// Stem prefix colours (stable per name, like `stems logs`).
const PALETTE: [Color; 6] = [
    Color::Cyan,
    Color::Yellow,
    Color::Magenta,
    Color::Green,
    Color::Blue,
    Color::LightRed,
];

/// The colour of a stem's prefix in merged logs.
pub fn stem_color(stem: &str) -> Color {
    let h = stem
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    PALETTE[(h as usize) % PALETTE.len()]
}

/// The colour of a level.
pub fn level_color(level: Option<Level>) -> Option<Color> {
    match level? {
        Level::Error => Some(Color::Red),
        Level::Warn => Some(Color::Yellow),
        Level::Debug | Level::Trace => Some(Color::DarkGray),
        Level::Info => None,
    }
}

fn is_char(k: &KeyEvent, c: char) -> bool {
    k.code == KeyCode::Char(c) && !k.modifiers.contains(KeyModifiers::CONTROL)
}

impl LogPane {
    /// An empty pane (following, every level, timestamps on).
    pub fn new() -> Self {
        Self::default()
    }

    /// Start over for a new source: empty ring, following, not paused.
    /// Returns the new generation.
    pub fn reset(&mut self, source: Option<LogTarget>) -> u64 {
        self.ring.clear();
        self.pending.clear();
        self.paused = false;
        self.follow = true;
        self.selected = None;
        self.visual = None;
        self.expanded.clear();
        self.anchor_at = None;
        self.source = source;
        self.generation += 1;
        self.generation
    }

    /// Add a record (to `pending` while paused).
    pub fn push(&mut self, record: LogRecord) {
        if self.paused {
            self.pending.push_back(record);
            while self.pending.len() > LOG_RING {
                self.pending.pop_front();
            }
            return;
        }
        let ts = record.ts;
        let id = self.ring.push(record);
        if let Some(at) = self.anchor_at
            && ts <= at + chrono::Duration::seconds(2)
        {
            self.follow = false;
            self.selected = Some(id);
        }
        if let Some(first) = self.ring.at(0).map(|l| l.id) {
            while self.expanded.first().is_some_and(|x| *x < first) {
                self.expanded.pop_first();
            }
        }
    }

    /// Lines held, including those waiting while paused.
    pub fn total(&self) -> usize {
        self.ring.len() + self.pending.len()
    }

    /// Whether any held line (or pending one) contains `text`.
    pub fn contains(&self, text: &str) -> bool {
        self.ring.iter().any(|l| l.record.text.contains(text))
            || self.pending.iter().any(|r| r.text.contains(text))
    }

    /// Whether a record passes the level and script filters.
    pub fn shows(&self, r: &LogRecord) -> bool {
        (self.show_scripts || r.stream != Stream::Script) && self.level.matches(r.level)
    }

    /// Ring positions of the shown lines, oldest first.
    pub fn visible(&self) -> Vec<usize> {
        self.ring
            .iter()
            .enumerate()
            .filter(|(_, l)| self.shows(&l.record))
            .map(|(i, _)| i)
            .collect()
    }

    fn id_at(&self, vis: &[usize], pos: usize) -> Option<u64> {
        vis.get(pos).and_then(|i| self.ring.at(*i)).map(|l| l.id)
    }

    /// Position (in `vis`) of the newest shown line with id <= `id`.
    fn pos_of(&self, vis: &[usize], id: u64) -> usize {
        let ids: Vec<u64> = vis
            .iter()
            .filter_map(|i| self.ring.at(*i))
            .map(|l| l.id)
            .collect();
        ids.partition_point(|x| *x <= id).saturating_sub(1)
    }

    /// Position of the cursor in `vis` (the newest line when following).
    pub fn cursor(&self, vis: &[usize]) -> Option<usize> {
        if vis.is_empty() {
            return None;
        }
        match (self.follow, self.selected) {
            (false, Some(id)) => Some(self.pos_of(vis, id)),
            _ => Some(vis.len() - 1),
        }
    }

    /// The selected line, if any.
    pub fn selected_line(&self) -> Option<&LogLine> {
        let vis = self.visible();
        let c = self.cursor(&vis)?;
        self.ring.at(vis[c])
    }

    fn select(&mut self, vis: &[usize], pos: usize) {
        self.anchor_at = None;
        if vis.is_empty() {
            return;
        }
        let pos = pos.min(vis.len() - 1);
        self.selected = self.id_at(vis, pos);
        // Back on the newest line: follow again (unless paused).
        self.follow = pos == vis.len() - 1 && !self.paused;
    }

    /// Resume after a pause: pending records join the ring, follow again.
    pub fn resume(&mut self) {
        self.paused = false;
        for r in std::mem::take(&mut self.pending) {
            self.push(r);
        }
        self.follow = true;
        self.selected = None;
    }

    /// Positions (in `vis`) of the lines matching the search.
    pub fn matches(&self, vis: &[usize]) -> Vec<usize> {
        let q = self.search.to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        vis.iter()
            .enumerate()
            .filter(|(_, i)| self.ring.at(**i).is_some_and(|l| l.key.contains(&q)))
            .map(|(p, _)| p)
            .collect()
    }

    fn jump_match(&mut self, forward: bool, inclusive: bool) {
        let vis = self.visible();
        let m = self.matches(&vis);
        if m.is_empty() {
            return;
        }
        let cur = self.cursor(&vis).unwrap_or(0);
        let next = if forward {
            m.iter()
                .find(|p| if inclusive { **p >= cur } else { **p > cur })
                .or(m.first())
        } else {
            m.iter()
                .rev()
                .find(|p| if inclusive { **p <= cur } else { **p < cur })
                .or(m.last())
        };
        if let Some(p) = next.copied() {
            self.select(&vis, p);
            self.follow = false;
        }
    }

    /// The text `y` copies: the visual range, else the selected line.
    pub fn copy_text(&self) -> Option<(String, usize)> {
        let vis = self.visible();
        let cur = self.cursor(&vis)?;
        let (a, b) = match self.visual {
            Some(id) => {
                let v = self.pos_of(&vis, id);
                (v.min(cur), v.max(cur))
            }
            None => (cur, cur),
        };
        let lines: Vec<&str> = vis[a..=b]
            .iter()
            .filter_map(|i| self.ring.at(*i))
            .map(|l| l.record.text.as_str())
            .collect();
        let n = lines.len();
        Some((lines.join("\n"), n))
    }

    /// Handle a key (see the module docs).
    pub fn key(&mut self, k: KeyEvent) -> PaneAction {
        if self.search_editing {
            match k.code {
                KeyCode::Enter => {
                    self.search_editing = false;
                    self.jump_match(false, true);
                }
                KeyCode::Esc => {
                    self.search_editing = false;
                    self.search.clear();
                }
                KeyCode::Backspace => {
                    self.search.pop();
                }
                KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search.push(c);
                }
                _ => {}
            }
            return PaneAction::Handled;
        }
        let vis = self.visible();
        let cur = self.cursor(&vis);
        match k.code {
            KeyCode::Char(' ') => {
                if self.paused {
                    self.resume();
                } else {
                    self.paused = true;
                }
            }
            KeyCode::Char('/') => {
                self.search_editing = true;
                self.search.clear();
            }
            _ if is_char(&k, 'n') => self.jump_match(true, false),
            _ if is_char(&k, 'N') => self.jump_match(false, false),
            _ if is_char(&k, 'L') => self.level = self.level.next(),
            _ if is_char(&k, 's') => self.show_scripts = !self.show_scripts,
            _ if is_char(&k, 'w') => self.wrap = !self.wrap,
            _ if is_char(&k, 't') => self.timestamps = !self.timestamps,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(c) = cur {
                    self.select(&vis, c + 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(c) = cur {
                    self.select(&vis, c.saturating_sub(1));
                    self.follow = false;
                }
            }
            KeyCode::PageDown => {
                if let Some(c) = cur {
                    self.select(&vis, c + 10);
                }
            }
            KeyCode::PageUp => {
                if let Some(c) = cur {
                    self.select(&vis, c.saturating_sub(10));
                    self.follow = false;
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.select(&vis, 0);
                self.follow = false;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.anchor_at = None;
                self.selected = None;
                self.follow = true;
            }
            _ if is_char(&k, 'V') => {
                self.visual = match self.visual {
                    Some(_) => None,
                    None => cur.and_then(|c| self.id_at(&vis, c)),
                };
            }
            _ if is_char(&k, 'y') => {
                let Some((text, lines)) = self.copy_text() else {
                    return PaneAction::Handled;
                };
                self.visual = None;
                return PaneAction::Copy { text, lines };
            }
            KeyCode::Enter => {
                if let Some(l) = self.selected_line()
                    && l.record.fields.as_ref().is_some_and(|f| !f.is_empty())
                {
                    let id = l.id;
                    if !self.expanded.remove(&id) {
                        self.expanded.insert(id);
                    }
                    // Keep the cursor on this line while it is expanded.
                    if self.follow {
                        self.selected = Some(id);
                    }
                }
            }
            KeyCode::Esc => {
                if self.visual.is_some() {
                    self.visual = None;
                } else if !self.search.is_empty() {
                    self.search.clear();
                } else {
                    return PaneAction::Ignored;
                }
            }
            _ => return PaneAction::Ignored,
        }
        PaneAction::Handled
    }

    /// Move the selected line by `delta` (the mouse wheel): up leaves
    /// follow mode like `k`; down past the newest line follows again.
    pub fn scroll(&mut self, delta: isize) {
        let vis = self.visible();
        let Some(c) = self.cursor(&vis) else {
            return;
        };
        if delta < 0 {
            self.select(&vis, c.saturating_sub(delta.unsigned_abs()));
            self.follow = false;
        } else {
            self.select(&vis, c + delta as usize);
        }
    }

    /// `following`, `paused (+N)` or `scrolled`, then the active filters.
    pub fn status_text(&self) -> String {
        let mut parts = vec![if self.paused {
            format!("paused (+{})", self.pending.len())
        } else if self.follow {
            "following".to_string()
        } else {
            "scrolled".to_string()
        }];
        if self.level != LevelMode::All {
            parts.push(format!("level {}", self.level.label()));
        }
        if !self.show_scripts {
            parts.push("no scripts".into());
        }
        if self.wrap {
            parts.push("wrap".into());
        }
        parts.join(" · ")
    }

    /// The spans of one line (without the gutter).
    fn line_spans(&self, l: &LogLine, ascii: bool, query: &str) -> Vec<Span<'static>> {
        let r = &l.record;
        let mut spans = Vec::new();
        if self.timestamps {
            spans.push(Span::styled(
                format!("{} ", r.ts.format("%H:%M:%S%.3f")),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if self.merged {
            spans.push(Span::styled(
                format!("{} {} ", r.stem, if ascii { "|" } else { "│" }),
                Style::default().fg(stem_color(&r.stem)),
            ));
        }
        if let Some(t) = &r.tag {
            spans.push(Span::styled(
                format!("[{t}] "),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let lc = level_color(r.level);
        let structured = r.fields.as_ref().is_some_and(|f| !f.is_empty());
        let text_style = if structured {
            let name = r.level.map_or("-", Level::as_str).to_uppercase();
            let mut st = Style::default().add_modifier(Modifier::BOLD);
            if let Some(c) = lc {
                st = st.fg(c);
            }
            spans.push(Span::styled(format!("{name:<5} "), st));
            Style::default()
        } else {
            lc.map_or_else(Style::default, |c| Style::default().fg(c))
        };
        spans.extend(highlight(&r.text, query, text_style));
        if structured {
            let open = self.expanded.contains(&l.id);
            let marker = match (open, ascii) {
                (true, false) => "▾",
                (false, false) => "▸",
                (true, true) => "v",
                (false, true) => ">",
            };
            spans.push(Span::styled(
                format!(" {marker} {}", fields_text(r)),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans
    }

    /// The rows of one shown line (its fields when expanded, wrapped when
    /// `wrap` is on).
    #[allow(clippy::too_many_arguments)]
    fn entry_rows(
        &self,
        l: &LogLine,
        cursor: bool,
        in_range: bool,
        matched: bool,
        style: &PaneStyle,
        query: &str,
        width: usize,
    ) -> Vec<Line<'static>> {
        let a = style.ascii;
        let mark = if cursor {
            if a { ">" } else { "›" }
        } else if in_range {
            if a { "|" } else { "│" }
        } else {
            " "
        };
        let mut spans = vec![Span::styled(
            format!("{mark}{} ", if matched { "*" } else { " " }),
            Style::default().fg(style.accent),
        )];
        spans.extend(self.line_spans(l, a, query));
        let mut line = Line::from(spans);
        if cursor {
            line = line.style(Style::default().add_modifier(Modifier::REVERSED));
        } else if in_range {
            line = line.style(Style::default().bg(Color::DarkGray));
        }
        let mut rows = if self.wrap {
            wrap_line(line, width, 3)
        } else {
            vec![line]
        };
        if self.expanded.contains(&l.id)
            && let Some(f) = &l.record.fields
        {
            for (k, v) in f {
                rows.push(Line::styled(
                    format!("      {k}: {v}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
        rows
    }

    /// Draw the pane (bordered, titled) into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect, style: &PaneStyle) {
        let title = format!(" {} · {} ", style.title, self.status_text());
        let mut block = Block::default().borders(Borders::ALL).title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if let Some(strip) = &style.strip {
            block = block.title(strip.clone());
        }
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let vis = self.visible();
        let bar = self.search_editing || !self.search.is_empty() || self.visual.is_some();
        let (body, bar_area) = if bar && inner.height >= 2 {
            (
                Rect::new(inner.x, inner.y, inner.width, inner.height - 1),
                Some(Rect::new(
                    inner.x,
                    inner.y + inner.height - 1,
                    inner.width,
                    1,
                )),
            )
        } else {
            (inner, None)
        };
        let matches = self.matches(&vis);
        let cur = self.cursor(&vis);
        if let Some(r) = bar_area {
            f.render_widget(Paragraph::new(self.bar_text(&matches, cur)), r);
        }
        let Some(cur) = cur else {
            let msg = if self.ring.is_empty() {
                " no log lines yet".to_string()
            } else {
                format!(
                    " no line matches (level {}{})",
                    self.level.label(),
                    if self.show_scripts {
                        ""
                    } else {
                        ", no scripts"
                    }
                )
            };
            f.render_widget(Paragraph::new(msg), body);
            return;
        };
        let query = self.search.to_lowercase();
        let range = self.visual.map(|id| {
            let v = self.pos_of(&vis, id);
            (v.min(cur), v.max(cur))
        });
        let h = usize::from(body.height);
        let w = usize::from(body.width);
        let rows_of = |p: usize| {
            let l = self.ring.at(vis[p]).expect("visible line");
            let in_range = range.is_some_and(|(a, b)| p >= a && p <= b);
            let matched = matches.binary_search(&p).is_ok();
            self.entry_rows(l, p == cur, in_range, matched, style, &query, w)
        };
        let mut cur_rows = rows_of(cur);
        cur_rows.truncate(h);
        let mut left = h - cur_rows.len();
        let mut below: Vec<Line> = Vec::new();
        let mut above: Vec<Vec<Line>> = Vec::new();
        // Centre the cursor when possible: half the room below it first,
        // then the lines above, then whatever room is left below.
        let mut down = cur + 1;
        let mut half = left / 2;
        while half > 0 && down < vis.len() {
            let r = rows_of(down);
            let take = r.len().min(half);
            below.extend(r.into_iter().take(take));
            half -= take;
            left -= take;
            down += 1;
        }
        let mut up = cur;
        while left > 0 && up > 0 {
            up -= 1;
            let mut r = rows_of(up);
            if r.len() > left {
                r.drain(..r.len() - left);
            }
            left -= r.len();
            above.push(r);
        }
        while left > 0 && down < vis.len() {
            let r = rows_of(down);
            let take = r.len().min(left);
            below.extend(r.into_iter().take(take));
            left -= take;
            down += 1;
        }
        let mut lines: Vec<Line> = above.into_iter().rev().flatten().collect();
        lines.extend(cur_rows);
        lines.extend(below);
        f.render_widget(Paragraph::new(lines), body);
    }

    fn bar_text(&self, matches: &[usize], cur: Option<usize>) -> String {
        let mut parts = Vec::new();
        if self.search_editing {
            parts.push(format!("/{}_", self.search));
        } else if !self.search.is_empty() {
            let n = matches.len();
            let mut s = format!(
                "/{} · {n} match{}",
                self.search,
                if n == 1 { "" } else { "es" }
            );
            if let Some(i) = cur.and_then(|c| matches.iter().position(|m| *m == c)) {
                s.push_str(&format!(" · {}/{n} (n/N)", i + 1));
            }
            parts.push(s);
        }
        if let Some((_, n)) = self.visual.and_then(|_| self.copy_text()) {
            parts.push(format!(
                "VISUAL {n} line{} · y copies · Esc cancels",
                if n == 1 { "" } else { "s" }
            ));
        }
        format!(" {}", parts.join(" · "))
    }
}

/// `text` as spans, the case-insensitive matches of `query` (lowercase)
/// highlighted.
fn highlight(text: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    let hl = base.bg(Color::Yellow).fg(Color::Black);
    let lower = text.to_lowercase();
    // Byte offsets only line up when lowercasing kept the lengths.
    if query.is_empty() || lower.len() != text.len() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut out = Vec::new();
    let mut at = 0;
    for (i, _) in lower.match_indices(query) {
        if i < at || !text.is_char_boundary(i) || !text.is_char_boundary(i + query.len()) {
            continue;
        }
        if i > at {
            out.push(Span::styled(text[at..i].to_string(), base));
        }
        out.push(Span::styled(text[i..i + query.len()].to_string(), hl));
        at = i + query.len();
    }
    if at < text.len() {
        out.push(Span::styled(text[at..].to_string(), base));
    }
    out
}

/// Cut a line into rows of `width` columns; rows after the first start
/// with `indent` spaces.
pub fn wrap_line(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let width = width.max(indent + 1);
    let style = line.style;
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut col = 0;
    for span in line.spans {
        let mut buf = String::new();
        for c in span.content.chars() {
            let cw = c.width().unwrap_or(0);
            if col + cw > width {
                if !buf.is_empty() {
                    rows.last_mut()
                        .expect("a row")
                        .push(Span::styled(std::mem::take(&mut buf), span.style));
                }
                rows.push(vec![Span::raw(" ".repeat(indent))]);
                col = indent;
            }
            buf.push(c);
            col += cw;
        }
        if !buf.is_empty() {
            rows.last_mut()
                .expect("a row")
                .push(Span::styled(buf, span.style));
        }
    }
    rows.into_iter()
        .map(|s| Line::from(s).style(style))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn rec(i: usize, level: Option<Level>, text: &str) -> LogRecord {
        LogRecord {
            ts: Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap()
                + chrono::Duration::milliseconds(i as i64 * 10),
            stem: "echo-svc".into(),
            stream: Stream::Out,
            tag: None,
            level,
            text: text.into(),
            fields: None,
        }
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn pane_with(n: usize) -> LogPane {
        let mut p = LogPane::new();
        for i in 0..n {
            let lvl = match i % 4 {
                0 => Some(Level::Error),
                1 => Some(Level::Warn),
                2 => Some(Level::Info),
                _ => None,
            };
            p.push(rec(i, lvl, &format!("line {i}")));
        }
        p
    }

    fn render(p: &LogPane, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        let style = PaneStyle {
            title: "Logs: echo-svc".into(),
            ascii: false,
            accent: Color::Cyan,
            strip: None,
        };
        term.draw(|f| p.render(f, f.area(), &style)).unwrap();
        crate::view::buffer_text(term.backend().buffer())
    }

    #[test]
    fn ring_is_bounded_and_ids_are_stable() {
        let mut r = LogRing::with_capacity(3);
        for i in 0..5 {
            r.push(rec(i, None, &format!("l{i}")));
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.at(0).unwrap().id, 2);
        assert_eq!(r.get(4).unwrap().record.text, "l4");
        assert!(r.get(1).is_none());
        assert_eq!(r.search("L3"), vec![3]);
        assert!(r.search("").is_empty());
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.push(rec(0, None, "x")), 5);
    }

    fn big_ring() -> LogRing {
        let mut r = LogRing::default();
        for i in 0..LOG_RING + 10 {
            let mut x = rec(
                i,
                Some(Level::Info),
                &format!("GET /api/items/{i} 200 in {}ms user=u{}", i % 97, i % 13),
            );
            if i % 1000 == 7 {
                x.text.push_str(" NEEDLE");
            }
            r.push(x);
        }
        r
    }

    #[test]
    fn search_over_the_full_ring_is_fast() {
        let r = big_ring();
        assert_eq!(r.len(), LOG_RING);
        let t = std::time::Instant::now();
        let hits = r.search("needle");
        let took = t.elapsed();
        assert_eq!(hits.len(), 20);
        // The budget is 50 ms (see `search_bench`); generous under load.
        assert!(took.as_millis() < 500, "search took {took:?}");
    }

    /// `cargo test -p stems-tui search_bench -- --ignored` (release for the
    /// real number): the 50 ms budget of plan/29.
    #[test]
    #[ignore = "benchmark"]
    fn search_bench() {
        let r = big_ring();
        let t = std::time::Instant::now();
        for _ in 0..10 {
            assert_eq!(r.search("needle").len(), 20);
        }
        let each = t.elapsed() / 10;
        eprintln!("search over {} lines: {each:?}", r.len());
        assert!(each.as_millis() < 50, "search took {each:?}");
    }

    #[test]
    fn level_filter_cycles() {
        let mut p = pane_with(8);
        assert_eq!(p.visible().len(), 8);
        let l = KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT);
        p.key(l);
        assert_eq!(p.level, LevelMode::Info);
        assert_eq!(p.visible().len(), 6, "no-level lines hidden");
        p.key(l);
        assert_eq!(p.visible().len(), 4);
        p.key(l);
        assert_eq!(p.visible().len(), 2);
        assert!(p.status_text().contains("level error"));
        p.key(l);
        assert_eq!(p.level, LevelMode::All);
        assert!(LevelMode::Warn.matches(Some(Level::Error)));
        assert!(!LevelMode::Warn.matches(None));
        assert!(LevelMode::All.matches(None));
    }

    #[test]
    fn script_lines_toggle() {
        let mut p = pane_with(2);
        let mut s = rec(3, None, "seeded");
        s.stream = Stream::Script;
        s.tag = Some("seed".into());
        p.push(s);
        assert_eq!(p.visible().len(), 3);
        p.key(key('s'));
        assert_eq!(p.visible().len(), 2);
        assert!(p.status_text().contains("no scripts"));
    }

    #[test]
    fn pause_holds_new_lines_until_resumed() {
        let mut p = pane_with(3);
        p.key(key(' '));
        assert!(p.paused);
        p.push(rec(9, None, "later"));
        assert_eq!(p.ring.len(), 3);
        assert_eq!(p.total(), 4);
        assert!(p.contains("later"));
        assert!(p.status_text().contains("paused (+1)"));
        p.key(key(' '));
        assert!(!p.paused && p.follow);
        assert_eq!(p.ring.len(), 4);
        assert_eq!(p.selected_line().unwrap().record.text, "later");
    }

    #[test]
    fn movement_follow_and_search() {
        let mut p = pane_with(10);
        assert_eq!(p.selected_line().unwrap().record.text, "line 9");
        p.key(key('k'));
        assert!(!p.follow);
        assert_eq!(p.selected_line().unwrap().record.text, "line 8");
        p.push(rec(10, None, "line 10"));
        assert_eq!(p.selected_line().unwrap().record.text, "line 8", "stays");
        p.key(key('g'));
        assert_eq!(p.selected_line().unwrap().record.text, "line 0");
        p.key(key('G'));
        assert!(p.follow);
        assert_eq!(p.selected_line().unwrap().record.text, "line 10");
        // Search: /line 1<Enter> goes to the newest match at or before.
        p.key(key('/'));
        for c in "LINE 1".chars() {
            p.key(key(c));
        }
        p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!p.search_editing);
        assert_eq!(p.selected_line().unwrap().record.text, "line 10");
        p.key(key('n'));
        assert_eq!(p.selected_line().unwrap().record.text, "line 1", "wraps");
        p.key(key('N'));
        assert_eq!(p.selected_line().unwrap().record.text, "line 10");
        p.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(p.search.is_empty());
        assert_eq!(
            p.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            PaneAction::Ignored
        );
    }

    #[test]
    fn copy_line_and_visual_range() {
        let mut p = pane_with(5);
        assert_eq!(
            p.key(key('y')),
            PaneAction::Copy {
                text: "line 4".into(),
                lines: 1
            }
        );
        p.key(key('k'));
        p.key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
        p.key(key('k'));
        p.key(key('k'));
        assert_eq!(
            p.key(key('y')),
            PaneAction::Copy {
                text: "line 1\nline 2\nline 3".into(),
                lines: 3
            }
        );
        assert!(p.visual.is_none());
    }

    #[test]
    fn jump_anchor_selects_the_line_at_the_event_time() {
        let mut p = LogPane::new();
        p.anchor_at = Some(rec(5, None, "").ts);
        p.follow = false;
        for i in 0..20 {
            p.push(rec(i * 100, None, &format!("l{i}")));
        }
        // ts of l0 is 0 ms, l1 1000 ms, ...; at = 50 ms (+2 s slack) → l2.
        assert_eq!(p.selected_line().unwrap().record.text, "l2");
        assert!(!p.follow);
    }

    fn structured() -> LogRecord {
        let mut r = rec(1, Some(Level::Warn), "slow request");
        r.fields = Some(
            json!({"request_id": "a1b2", "ms": 812})
                .as_object()
                .unwrap()
                .clone(),
        );
        r
    }

    #[test]
    fn structured_line_expands_on_enter() {
        let mut p = pane_with(2);
        p.push(structured());
        let text = render(&p, 70, 8);
        insta::assert_snapshot!("logpane-structured-collapsed", text);
        p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = render(&p, 70, 8);
        insta::assert_snapshot!("logpane-structured-expanded", text);
        assert!(text.contains("request_id: \"a1b2\""), "{text}");
    }

    #[test]
    fn render_search_level_merged_wrap() {
        let mut p = pane_with(12);
        p.merged = true;
        p.key(key('/'));
        for c in "line 1".chars() {
            p.key(key(c));
        }
        p.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        insta::assert_snapshot!("logpane-search-merged", render(&p, 60, 10));
        p.key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        p.key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        p.key(key('t'));
        insta::assert_snapshot!("logpane-warn-no-ts", render(&p, 60, 10));
        let mut p = LogPane::new();
        p.push(rec(0, None, &"x".repeat(80)));
        p.key(key('w'));
        let text = render(&p, 40, 6);
        insta::assert_snapshot!("logpane-wrap", text);
        let mut p = LogPane::new();
        assert!(render(&p, 40, 5).contains("no log lines yet"));
        p.push(rec(0, None, "plain"));
        p.key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert!(render(&p, 40, 5).contains("no line matches"));
    }

    #[test]
    fn wrap_line_cuts_by_width() {
        let rows = wrap_line(Line::from("abcdefghij"), 4, 1);
        let text: Vec<String> = rows.iter().map(|l| l.to_string()).collect();
        assert_eq!(text, vec!["abcd", " efg", " hij"]);
    }

    #[test]
    fn highlight_splits_matches() {
        let s = highlight("Line 1 and line 10", "line 1", Style::default());
        let parts: Vec<&str> = s.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(parts, vec!["Line 1", " and ", "line 1", "0"]);
    }
}
