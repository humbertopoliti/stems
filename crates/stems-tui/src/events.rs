//! The Events view (29): the daemon's event stream as a table (TIME KIND
//! STEM FROM→TO REASON ACTOR), newest at the bottom, filtered with `/`
//! (stem, kind, states or reason); `Enter` jumps to the event's stem logs
//! at the event time.

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, HighlightSpacing, Paragraph, Row, Table, TableState};
use stems_api::Event;

use crate::view::truncate;

/// Event table headers.
pub const EVENT_COLUMNS: [&str; 6] = ["TIME", "KIND", "STEM", "FROM→TO", "REASON", "ACTOR"];

/// State of the Events view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventsState {
    /// Filter text (`/`): substring of the stem, kind, states or reason.
    pub filter: String,
    /// Typing the filter.
    pub editing: bool,
    /// Selected event (`seq`); `None` follows the newest.
    pub selected: Option<u64>,
    /// The buffered events were requested (once per session).
    pub loaded: bool,
}

/// `from→to` of an event (`-` when neither).
pub fn transition(e: &Event, ascii: bool) -> String {
    let arrow = if ascii { "->" } else { "→" };
    match (&e.from, &e.to) {
        (Some(f), Some(t)) => format!("{f}{arrow}{t}"),
        (None, Some(t)) => format!("{arrow}{t}"),
        (Some(f), None) => format!("{f}{arrow}"),
        (None, None) => "-".into(),
    }
}

impl EventsState {
    /// Whether `e` passes the filter.
    pub fn shows(&self, e: &Event) -> bool {
        let f = self.filter.to_lowercase();
        if f.is_empty() {
            return true;
        }
        [
            Some(e.kind.to_string()),
            e.stem.clone(),
            e.from.clone(),
            e.to.clone(),
            e.reason.clone(),
        ]
        .into_iter()
        .flatten()
        .any(|s| s.to_lowercase().contains(&f))
    }

    /// The shown events, oldest first.
    pub fn rows<'a>(&self, events: &'a VecDeque<Event>) -> Vec<&'a Event> {
        events.iter().filter(|e| self.shows(e)).collect()
    }

    /// Position of the selection among `rows` (the newest by default).
    pub fn cursor(&self, rows: &[&Event]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        match self.selected {
            None => Some(rows.len() - 1),
            Some(seq) => Some(
                rows.partition_point(|e| e.seq <= seq)
                    .saturating_sub(1)
                    .min(rows.len() - 1),
            ),
        }
    }

    /// Move the selection by `delta` rows (clamped); the last row follows.
    pub fn step(&mut self, events: &VecDeque<Event>, delta: isize) {
        let rows = self.rows(events);
        let Some(cur) = self.cursor(&rows) else {
            return;
        };
        let last = rows.len() as isize - 1;
        let next = (cur as isize).saturating_add(delta).clamp(0, last) as usize;
        self.selected = if next as isize == last {
            None
        } else {
            Some(rows[next].seq)
        };
    }

    /// The selected event.
    pub fn selected_event<'a>(&self, events: &'a VecDeque<Event>) -> Option<&'a Event> {
        let rows = self.rows(events);
        let c = self.cursor(&rows)?;
        Some(rows[c])
    }
}

/// Column widths for a table `width` wide.
pub fn event_widths(width: u16) -> [u16; 6] {
    let (time, kind, stem, tr, actor) = if width >= 100 {
        (8, 20, 16, 23, 14)
    } else {
        (8, 15, 10, 19, 8)
    };
    let fixed = time + kind + stem + tr + actor;
    let reason = width.saturating_sub(2 + 5 + fixed);
    [time, kind, stem, tr, reason, actor]
}

/// Draw the Events view.
pub fn render(
    state: &EventsState,
    events: &VecDeque<Event>,
    ascii: bool,
    accent: Color,
    f: &mut Frame,
    area: Rect,
) {
    let mut title = format!(" Events · {} ", events.len());
    if state.editing || !state.filter.is_empty() {
        title = format!(
            " Events · /{}{} ",
            state.filter,
            if state.editing { "_" } else { "" }
        );
    }
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = state.rows(events);
    let widths = event_widths(inner.width);
    let header = Row::new(EVENT_COLUMNS.map(Cell::from))
        .style(Style::default().fg(accent).add_modifier(Modifier::BOLD));
    let body: Vec<Row> = rows
        .iter()
        .map(|e| {
            let t = |s: &str, i: usize| Cell::from(truncate(s, usize::from(widths[i]), ascii));
            let reason = e
                .reason
                .as_deref()
                .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "-".into());
            let color = match e.to.as_deref() {
                Some("failed" | "unhealthy") => Some(Color::Red),
                Some("healthy" | "running") => Some(Color::Green),
                _ => None,
            };
            let tr = truncate(&transition(e, ascii), usize::from(widths[3]), ascii);
            Row::new(vec![
                Cell::from(e.ts.format("%H:%M:%S").to_string()),
                t(&e.kind.to_string(), 1),
                t(e.stem.as_deref().unwrap_or("-"), 2),
                Cell::from(Span::styled(
                    tr,
                    color.map_or_else(Style::default, |c| Style::default().fg(c)),
                )),
                t(&reason, 4),
                t(&e.actor, 5),
            ])
        })
        .collect();
    let empty = body.is_empty();
    let table = Table::new(body, widths.map(Constraint::Length))
        .header(header)
        .column_spacing(1)
        .highlight_symbol(if ascii { "> " } else { "› " })
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut ts = TableState::default().with_selected(state.cursor(&rows));
    f.render_stateful_widget(table, inner, &mut ts);
    if empty && inner.height > 1 {
        let msg = if state.filter.is_empty() {
            "no events yet".to_string()
        } else {
            format!("no event matches /{}", state.filter)
        };
        let r = Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(2), 1);
        f.render_widget(Paragraph::new(msg), r);
    }
}
