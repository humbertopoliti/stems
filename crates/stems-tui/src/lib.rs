//! The stems terminal dashboard (deliverables 27 to 30, FR-UI-1..4).
//!
//! Elm-style architecture:
//!
//! * [`Model`]: everything shown (stems from `status`, selection, view,
//!   filter, sort, help, the [`Modal`] dialog, toasts, detail data, recent
//!   events, prefs);
//! * [`Msg`]: inputs (keys, mouse, ticks, daemon events, RPC results,
//!   resize, signals);
//! * [`update`]: the pure reducer `(&mut Model, Msg) -> Vec<Cmd>`;
//! * [`view`]: the pure renderer `(&Model, &mut Frame)`;
//! * [`runner`]: executes [`Cmd`]s against the daemon, in a terminal
//!   ([`run_terminal`]) or headless ([`run_headless`], frames as text).
//!
//! Frame goldens: [`assert_frame!`] renders a model at 80x24 and 120x40
//! and snapshots the text with `insta`. See `docs/tui.md`.

pub mod actions;
pub mod bar;
pub mod clipboard;
pub mod detail;
pub mod events;
pub mod form;
pub mod fuzzy;
pub mod graph;
pub mod logs;
pub mod modals;
pub mod model;
pub mod prefs;
pub mod runner;
pub mod script;
pub mod scripts;
pub mod terminal;
pub mod toast;
pub mod update;
pub mod view;

pub use actions::{Action, MenuEntry, Palette, PaletteItem, PaletteTarget, ScriptMenu};
pub use detail::DetailRow;
pub use events::EventsState;
pub use form::{FieldValue, FormAction, FormField, ScriptForm};
pub use graph::{GraphState, GraphStem, GraphWidget};
pub use logs::{LevelMode, LogRing, PaneAction, PaneStyle};
pub use model::{
    AttachMode, Cmd, DetailData, LogPane, LogSubscription, MetricHistory, Modal, Model, Msg,
    Outcome, RpcResult, ScriptActivity, ScriptRun, SignalKind, SortKey, ViewKind,
};
pub use prefs::{Clipboard, Prefs, Theme};
pub use runner::{
    ENV_PANIC_TEST, HeadlessOptions, HeadlessOutput, HeadlessRun, TerminalOptions, exec,
    run_headless, run_headless_output, run_terminal,
};
pub use script::{Token, WaitFor};
pub use scripts::ScriptsState;
pub use toast::{Toast, ToastKind, Toasts};
pub use update::{init, update};
pub use view::{render_text, view};

/// Env var: `0` forces the plain streaming attach.
pub const ENV_TUI: &str = "STEMS_TUI";
/// Env var: `1` opens the TUI even when stdout is not a terminal.
pub const ENV_TUI_FORCE: &str = "STEMS_TUI_FORCE";
/// Env var: a headless script for attached `stems up` (tests).
pub const ENV_TUI_SCRIPT: &str = "STEMS_TUI_SCRIPT";
/// Env var: headless frame size for attached `stems up` (`WxH`).
pub const ENV_TUI_SIZE: &str = "STEMS_TUI_SIZE";
/// Env var: headless frames directory for attached `stems up`.
pub const ENV_TUI_FRAMES_OUT: &str = "STEMS_TUI_FRAMES_OUT";

/// Parse `WxH` (e.g. `120x40`); at least 1x1, at most 1000x500.
pub fn parse_size(s: &str) -> Option<(u16, u16)> {
    let (w, h) = s.trim().split_once(['x', 'X'])?;
    let w: u16 = w.trim().parse().ok()?;
    let h: u16 = h.trim().parse().ok()?;
    ((1..=1000).contains(&w) && (1..=500).contains(&h)).then_some((w, h))
}

/// The frame delimiter printed before frame `n` in headless mode.
pub fn frame_header(n: usize) -> String {
    format!("--- frame {n} ---")
}

/// Snapshot a model's frame with `insta` at 80x24 and 120x40 (or one given
/// size): `assert_frame!(model, "table-minimal")` writes
/// `table-minimal-80x24` and `table-minimal-120x40`.
#[macro_export]
macro_rules! assert_frame {
    ($model:expr, $name:expr) => {{
        for (w, h) in [(80u16, 24u16), (120u16, 40u16)] {
            $crate::assert_frame!($model, $name, w, h);
        }
    }};
    ($model:expr, $name:expr, $w:expr, $h:expr) => {{
        let text = $crate::view::render_text(&$model, $w, $h);
        insta::assert_snapshot!(format!("{}-{}x{}", $name, $w, $h), text);
    }};
}

#[cfg(test)]
mod tests;
