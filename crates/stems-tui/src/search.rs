//! The vim-style search shared by Table, Graph, Detail, Events and
//! Scripts: `/` types a query, `Enter` keeps it and jumps to the first
//! match at or after the cursor, `n` / `N` go to the next / previous match
//! (wrapping), `Esc` clears it. One register ([`Model::search`]) serves
//! every view: switching views keeps the query and `n` cycles the matches
//! of the view on screen. Matching is a case-insensitive substring test
//! over each view's rows (what [`rows`] lists); the matched text is
//! highlighted with [`match_style`], the status bar shows [`Search::bar_text`].
//! The Logs view keeps its own search in the log pane ([`crate::logs`]).
//!
//! [`Model::search`]: crate::model::Model::search

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use stems_core::layout::Layout;

use crate::model::{Model, ViewKind};

/// The search register: the query and whether it is being typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    /// The query (case-insensitive substring).
    pub query: String,
    /// Typing the query (`/`): every key goes to it.
    pub editing: bool,
}

/// What a key did to the search while typing ([`Search::edit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// The query changed (or the key was ignored).
    Typing,
    /// `Enter`: the query is kept; jump to the first match.
    Done,
    /// `Esc`: the query was cleared.
    Cancelled,
}

impl Search {
    /// Whether the search shows (typing, or a kept query).
    pub fn active(&self) -> bool {
        self.editing || !self.query.is_empty()
    }

    /// `/`: start typing a new query.
    pub fn start(&mut self) {
        self.editing = true;
        self.query.clear();
    }

    /// Drop the query.
    pub fn clear(&mut self) {
        self.editing = false;
        self.query.clear();
    }

    /// A key while typing: printable characters (digits included) are
    /// typed, `Backspace` deletes, `Enter` keeps the query, `Esc` clears it;
    /// control combinations and other keys are ignored.
    pub fn edit(&mut self, k: KeyEvent) -> Edit {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => {
                self.editing = false;
                Edit::Done
            }
            KeyCode::Esc => {
                self.clear();
                Edit::Cancelled
            }
            KeyCode::Backspace => {
                self.query.pop();
                Edit::Typing
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                Edit::Typing
            }
            _ => Edit::Typing,
        }
    }

    /// The positions (among `rows`) of the rows matching the query.
    pub fn matches<I, S>(&self, rows: I) -> Vec<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        matches(&self.query, rows)
    }

    /// The status bar text: `/query_` while typing, `/query · 2/5 (n/N)`
    /// on a match, `/query · 5 matches` off one, `/query · no match`.
    pub fn bar_text(&self, matches: usize, current: Option<usize>) -> String {
        if self.editing {
            return format!("/{}_", self.query);
        }
        match (matches, current) {
            (0, _) => format!("/{} · no match", self.query),
            (n, Some(i)) => format!("/{} · {}/{n} (n/N)", self.query, i + 1),
            (n, None) => format!(
                "/{} · {n} match{}",
                self.query,
                if n == 1 { "" } else { "es" }
            ),
        }
    }
}

/// The positions (among `rows`) of the rows containing `query`
/// (case-insensitive); none for an empty query.
pub fn matches<I, S>(query: &str, rows: I) -> Vec<usize>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let q = query.to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    rows.into_iter()
        .enumerate()
        .filter(|(_, r)| r.as_ref().to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

/// The next match from `cursor` among `matches` (sorted positions):
/// forwards the first one after it (at it too when `inclusive`),
/// backwards the last one before it; wrapping. `None` without matches.
pub fn next_match(
    matches: &[usize],
    cursor: usize,
    forward: bool,
    inclusive: bool,
) -> Option<usize> {
    if forward {
        matches
            .iter()
            .find(|p| {
                if inclusive {
                    **p >= cursor
                } else {
                    **p > cursor
                }
            })
            .or(matches.first())
    } else {
        matches
            .iter()
            .rev()
            .find(|p| {
                if inclusive {
                    **p <= cursor
                } else {
                    **p < cursor
                }
            })
            .or(matches.last())
    }
    .copied()
}

/// The style of a matched substring on top of `base` (the log pane's).
pub fn match_style(base: Style) -> Style {
    base.bg(Color::Yellow).fg(Color::Black)
}

/// `text` as spans, every case-insensitive occurrence of `query`
/// highlighted ([`match_style`] over `base`); one plain span for an empty
/// query or when lowercasing changes the byte layout.
pub fn highlight(text: &str, query: &str, base: Style) -> Vec<Span<'static>> {
    let query = query.to_lowercase();
    let lower = text.to_lowercase();
    // Byte offsets only line up when lowercasing kept the lengths.
    if query.is_empty() || lower.len() != text.len() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let hl = match_style(base);
    let mut out = Vec::new();
    let mut at = 0;
    for (i, _) in lower.match_indices(query.as_str()) {
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

/// `line` with the occurrences of `query` inside each span highlighted
/// (a span is split around them; the line's own style is kept).
pub fn highlight_line(line: Line<'static>, query: &str) -> Line<'static> {
    if query.is_empty() {
        return line;
    }
    let style = line.style;
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .flat_map(|s| highlight(&s.content, query, s.style))
        .collect();
    Line::from(spans).style(style)
}

/// The plain text of `line`.
pub fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// Whether `view` uses the shared search (every view but Logs, whose pane
/// has its own).
pub fn applies(view: ViewKind) -> bool {
    view != ViewKind::Logs
}

/// The searchable text of each row of the view on screen, in the view's
/// order: Table the visible stems' cells, Graph the drawn boxes (name and
/// type), Detail the lines of the selected stem, Events the shown events'
/// columns, Scripts the listed scripts (name, description, owner).
pub fn rows(model: &Model) -> Vec<String> {
    match model.view {
        ViewKind::Table => model
            .visible()
            .iter()
            .map(|s| {
                format!(
                    "{} {} {} {} {} {}",
                    s.name,
                    s.kind,
                    s.state,
                    s.reason.as_deref().unwrap_or_default(),
                    crate::view::ports_text(s, true),
                    s.variant.as_deref().unwrap_or_default()
                )
            })
            .collect(),
        ViewKind::Graph => model.graph.shown().map(graph_rows).unwrap_or_default(),
        ViewKind::Detail => crate::view::detail_content(model)
            .map(|(c, _)| c.lines.iter().map(line_text).collect())
            .unwrap_or_default(),
        ViewKind::Events => {
            let ev = &model.events_view;
            ev.rows(&model.events)
                .into_iter()
                .map(|e| {
                    format!(
                        "{} {} {} {} {} {}",
                        e.ts.format("%H:%M:%S"),
                        e.kind,
                        e.stem.as_deref().unwrap_or("-"),
                        crate::events::transition(e, model.ascii),
                        e.reason.as_deref().unwrap_or("-"),
                        e.actor
                    )
                })
                .collect()
        }
        ViewKind::Scripts => crate::scripts::entries(model)
            .into_iter()
            .map(|e| {
                format!(
                    "{} {} {}",
                    e.name,
                    e.description.as_deref().unwrap_or_default(),
                    e.stem.as_deref().unwrap_or(crate::scripts::WORKSPACE_GROUP)
                )
            })
            .collect(),
        ViewKind::Logs => Vec::new(),
    }
}

/// The searchable text of each box of `layout` (name and type), in node
/// order.
pub fn graph_rows(layout: &Layout) -> Vec<String> {
    layout
        .nodes
        .iter()
        .map(|n| format!("{} {}", n.name, n.kind))
        .collect()
}

/// The row the view's cursor is on (among [`rows`]): the selection in
/// Table, Graph, Events and Scripts; in Detail the current match, else the
/// first visible line.
pub fn cursor(model: &Model) -> Option<usize> {
    match model.view {
        ViewKind::Table => model.selected_index(),
        ViewKind::Graph => {
            let l = model.graph.shown()?;
            model.selected.as_deref().and_then(|s| l.node(s))
        }
        ViewKind::Detail => Some(model.search_line.unwrap_or(model.detail_scroll)),
        ViewKind::Events => {
            let ev = &model.events_view;
            ev.cursor(&ev.rows(&model.events))
        }
        ViewKind::Scripts => {
            let n = crate::scripts::entries(model).len();
            (n > 0).then(|| model.scripts_view.selected.min(n - 1))
        }
        ViewKind::Logs => None,
    }
}

/// The matches of the current view and, when the cursor is on one, its
/// position among them (for the status bar).
pub fn state(model: &Model) -> (Vec<usize>, Option<usize>) {
    if !applies(model.view) || model.search.query.is_empty() {
        return (Vec::new(), None);
    }
    let m = model.search.matches(rows(model));
    let cur = cursor(model).and_then(|c| m.iter().position(|p| *p == c));
    (m, cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_case_insensitive_substrings() {
        assert_eq!(matches("API", ["shop-api", "web", "Api gw"]), vec![0, 2]);
        assert!(matches("", ["a"]).is_empty());
        assert!(matches("zz", ["a"]).is_empty());
    }

    #[test]
    fn next_match_wraps_both_ways() {
        let m = [1, 4, 7];
        assert_eq!(next_match(&m, 4, true, false), Some(7));
        assert_eq!(next_match(&m, 4, true, true), Some(4));
        assert_eq!(next_match(&m, 7, true, false), Some(1));
        assert_eq!(next_match(&m, 4, false, false), Some(1));
        assert_eq!(next_match(&m, 1, false, false), Some(7));
        assert_eq!(next_match(&m, 0, false, true), Some(7));
        assert_eq!(next_match(&[], 0, true, true), None);
    }

    #[test]
    fn bar_text_forms() {
        let mut s = Search {
            query: "api".into(),
            editing: true,
        };
        assert_eq!(s.bar_text(0, None), "/api_");
        s.editing = false;
        assert_eq!(s.bar_text(0, None), "/api · no match");
        assert_eq!(s.bar_text(1, None), "/api · 1 match");
        assert_eq!(s.bar_text(5, None), "/api · 5 matches");
        assert_eq!(s.bar_text(5, Some(1)), "/api · 2/5 (n/N)");
    }

    #[test]
    fn highlight_splits_every_occurrence() {
        let s = highlight("Api and api", "API", Style::default());
        let text: Vec<&str> = s.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, vec!["Api", " and ", "api"]);
        assert_eq!(s[0].style, match_style(Style::default()));
        assert_eq!(s[1].style, Style::default());
        let l = highlight_line(Line::from(vec![Span::raw("ab"), Span::raw("xab")]), "ab");
        assert_eq!(l.spans.len(), 3);
        assert_eq!(line_text(&l), "abxab");
    }
}
