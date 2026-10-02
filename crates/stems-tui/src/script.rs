//! Headless key scripts: `wait:healthy;frame;j;Enter;frame;Tab;frame`.
//!
//! Tokens are separated by `;` (newlines work too, so a script file can hold
//! one token per line; `#` starts a comment line):
//!
//! | token | effect |
//! |---|---|
//! | `j`, `q`, `?` (one character) | that key |
//! | `Enter`, `Esc`, `Tab`, `BackTab`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Backspace`, `Space`, `Ctrl-C`, `Ctrl-L` | a named key |
//! | `/api<Enter>` (anything else) | typed character by character; `<Name>` inside is a named key |
//! | `wait:<state>` | poll until every stem is in `<state>` (a state or glyph name) |
//! | `wait:stem=<name>:<state>` | poll until that stem is in `<state>` |
//! | `wait:lines>=<n>` | wait until the log pane holds at least `n` lines (29) |
//! | `wait:log=<text>` | wait until a log pane line contains `<text>` (29) |
//! | `type:<text>` | the characters of `<text>` typed as keys, verbatim (spaces and `<`/`>` included) into the focused field (30) |
//! | `wait:event=<kind>[:<stem>]` | wait until an event of that kind (and stem) arrives after the last key (30) |
//! | `chaos:<path>` | `GET /__chaos/<path>` on the selected stem's first port (29; test workspaces) |
//! | `view:<name>` | switch to a view (`table`, `detail`, `graph`, `logs`, `events`, `scripts`) |
//! | `frame` | dump the current frame |
//! | `sleep:<ms>` | pause (scripts only; scenarios use waits) |

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::model::ViewKind;

/// One step of a headless script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// Press a key.
    Key(KeyEvent),
    /// Wait until stems reach a state.
    Wait(WaitFor),
    /// Switch view.
    View(ViewKind),
    /// Dump the frame.
    Frame,
    /// Pause.
    Sleep(u64),
    /// Call the selected stem's chaos endpoint (`/__chaos/<path>`).
    Chaos(String),
}

/// What a `wait:` token waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitFor {
    /// Every stem is in this state (or glyph).
    All(String),
    /// One stem is in this state (or glyph).
    Stem(String, String),
    /// The log pane holds at least this many lines (paused ones included).
    Lines(usize),
    /// A log pane line contains this text.
    Log(String),
    /// An event of this kind (and stem) arrived after the last key token.
    Event {
        /// Event kind (`script.finished`).
        kind: String,
        /// Only this stem's.
        stem: Option<String>,
    },
}

fn named(name: &str) -> Option<KeyEvent> {
    let k = |c| Some(KeyEvent::new(c, KeyModifiers::NONE));
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => k(KeyCode::Enter),
        "esc" | "escape" => k(KeyCode::Esc),
        "tab" => k(KeyCode::Tab),
        "backtab" | "shift-tab" => Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        "up" => k(KeyCode::Up),
        "down" => k(KeyCode::Down),
        "left" => k(KeyCode::Left),
        "right" => k(KeyCode::Right),
        "home" => k(KeyCode::Home),
        "end" => k(KeyCode::End),
        "pageup" | "pgup" => k(KeyCode::PageUp),
        "pagedown" | "pgdn" => k(KeyCode::PageDown),
        "backspace" => k(KeyCode::Backspace),
        "space" => k(KeyCode::Char(' ')),
        other => {
            let rest = other.strip_prefix("ctrl-")?;
            let mut cs = rest.chars();
            let c = cs.next()?;
            if cs.next().is_some() {
                return None;
            }
            Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
        }
    }
}

fn char_key(c: char) -> KeyEvent {
    let m = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    KeyEvent::new(KeyCode::Char(c), m)
}

fn typed(tok: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut rest = tok;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
            && let Some(k) = named(&rest[1..end])
        {
            out.push(Token::Key(k));
            rest = &rest[end + 1..];
            continue;
        }
        out.push(Token::Key(char_key(c)));
        rest = &rest[c.len_utf8()..];
    }
    if out.is_empty() {
        return Err(format!("empty token in {tok:?}"));
    }
    Ok(out)
}

/// Parse a script (see the module docs).
pub fn parse(script: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    for line in script.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        for raw in line.split(';') {
            let tok = raw.trim();
            if tok.is_empty() {
                continue;
            }
            if let Some(text) = raw.trim_start().strip_prefix("type:") {
                if text.is_empty() {
                    return Err(format!("`{tok}`: expected type:<text>"));
                }
                out.extend(text.chars().map(|c| Token::Key(char_key(c))));
            } else if tok == "frame" {
                out.push(Token::Frame);
            } else if let Some(w) = tok.strip_prefix("wait:") {
                if let Some(n) = w.strip_prefix("lines>=") {
                    let n = n
                        .trim()
                        .parse()
                        .map_err(|_| format!("`{tok}`: expected wait:lines>=<n>"))?;
                    out.push(Token::Wait(WaitFor::Lines(n)));
                } else if let Some(text) = w.strip_prefix("log=") {
                    if text.is_empty() {
                        return Err(format!("`{tok}`: expected wait:log=<text>"));
                    }
                    out.push(Token::Wait(WaitFor::Log(text.into())));
                } else if let Some(spec) = w.strip_prefix("event=") {
                    let (kind, stem) = match spec.split_once(':') {
                        Some((k, st)) => (k, Some(st.to_string()).filter(|x| !x.is_empty())),
                        None => (spec, None),
                    };
                    if kind.is_empty() {
                        return Err(format!("`{tok}`: expected wait:event=<kind>[:<stem>]"));
                    }
                    out.push(Token::Wait(WaitFor::Event {
                        kind: kind.into(),
                        stem,
                    }));
                } else if let Some(spec) = w.strip_prefix("stem=") {
                    let (stem, state) = spec
                        .rsplit_once(':')
                        .ok_or_else(|| format!("`{tok}`: expected wait:stem=<name>:<state>"))?;
                    out.push(Token::Wait(WaitFor::Stem(stem.into(), state.into())));
                } else if w.is_empty() {
                    return Err(format!("`{tok}`: expected wait:<state>"));
                } else {
                    out.push(Token::Wait(WaitFor::All(w.into())));
                }
            } else if let Some(v) = tok.strip_prefix("view:") {
                let v = ViewKind::parse(v).ok_or_else(|| {
                    format!("`{tok}`: unknown view (table, detail, graph, logs, events)")
                })?;
                out.push(Token::View(v));
            } else if let Some(path) = tok.strip_prefix("chaos:") {
                if path.is_empty() {
                    return Err(format!("`{tok}`: expected chaos:<path>"));
                }
                out.push(Token::Chaos(path.trim_start_matches('/').into()));
            } else if let Some(ms) = tok.strip_prefix("sleep:") {
                let ms = ms
                    .trim_end_matches("ms")
                    .parse()
                    .map_err(|_| format!("`{tok}`: expected sleep:<ms>"))?;
                out.push(Token::Sleep(ms));
            } else if let Some(k) = named(tok) {
                out.push(Token::Key(k));
            } else {
                out.extend(typed(tok)?);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> Token {
        Token::Key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    #[test]
    fn parses_the_documented_tokens() {
        let t = parse("wait:healthy;frame;j;Enter;frame;Tab;view:detail;sleep:5;Ctrl-C").unwrap();
        assert_eq!(
            t,
            vec![
                Token::Wait(WaitFor::All("healthy".into())),
                Token::Frame,
                key(KeyCode::Char('j')),
                key(KeyCode::Enter),
                Token::Frame,
                key(KeyCode::Tab),
                Token::View(ViewKind::Detail),
                Token::Sleep(5),
                Token::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ]
        );
    }

    #[test]
    fn typed_text_with_named_keys() {
        let t = parse("/ab<Enter>").unwrap();
        assert_eq!(
            t,
            vec![
                key(KeyCode::Char('/')),
                key(KeyCode::Char('a')),
                key(KeyCode::Char('b')),
                key(KeyCode::Enter)
            ]
        );
    }

    #[test]
    fn stem_waits_and_errors() {
        assert_eq!(
            parse("wait:stem=echo-svc:failed").unwrap(),
            vec![Token::Wait(WaitFor::Stem(
                "echo-svc".into(),
                "failed".into()
            ))]
        );
        assert!(parse("wait:stem=x").is_err());
        assert!(parse("view:nope").is_err());
        assert!(parse("sleep:x").is_err());
        assert_eq!(parse("# comment\nframe\n\n").unwrap(), vec![Token::Frame]);
    }

    #[test]
    fn type_and_event_tokens() {
        assert_eq!(
            parse("type:rest a;wait:event=script.finished:shop-api;wait:event=watch.paused")
                .unwrap(),
            vec![
                key(KeyCode::Char('r')),
                key(KeyCode::Char('e')),
                key(KeyCode::Char('s')),
                key(KeyCode::Char('t')),
                key(KeyCode::Char(' ')),
                key(KeyCode::Char('a')),
                Token::Wait(WaitFor::Event {
                    kind: "script.finished".into(),
                    stem: Some("shop-api".into())
                }),
                Token::Wait(WaitFor::Event {
                    kind: "watch.paused".into(),
                    stem: None
                }),
            ]
        );
        // Named keys are not interpreted inside type:.
        assert_eq!(parse("type:<Enter>").unwrap().len(), 7);
        assert!(parse("type:").is_err());
        assert!(parse("wait:event=").is_err());
    }

    #[test]
    fn log_tokens() {
        assert_eq!(
            parse("chaos:logs?n=5&level=error;wait:lines>=5;wait:log=chaos log line 4").unwrap(),
            vec![
                Token::Chaos("logs?n=5&level=error".into()),
                Token::Wait(WaitFor::Lines(5)),
                Token::Wait(WaitFor::Log("chaos log line 4".into())),
            ]
        );
        assert!(parse("wait:lines>=x").is_err());
        assert!(parse("wait:log=").is_err());
        assert!(parse("chaos:").is_err());
    }
}
