//! Toasts (30): short notices stacked bottom-right for async results and
//! errors. Each lives [`TOAST_TTL_MS`] on the model's tick clock
//! ([`crate::Model::clock_ms`], advanced by `refresh_ms` per tick, so tests
//! drive it with `Msg::Tick`); headless runs have no ticks, so toasts stay
//! until dismissed (`Esc`) and frames are deterministic. Errors carry their
//! code and hint; `e` opens the full details.

use serde_json::Value;

/// How long a toast stays (ms of the tick clock).
pub const TOAST_TTL_MS: u64 = 5000;
/// Toasts kept (older ones are dropped).
pub const TOAST_MAX: usize = 4;

/// What a toast reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Success (`✓`).
    Ok,
    /// Failure (`✗`); `e` expands the details.
    Error,
    /// Neutral notice.
    Info,
    /// The config changed on disk (33): `a` applies, `v` views the plan.
    ConfigChanged,
}

/// One toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
    /// Kind.
    pub kind: ToastKind,
    /// First line (without the glyph).
    pub text: String,
    /// Error code (`HAS_DEPENDANTS`).
    pub code: Option<String>,
    /// Hint line.
    pub hint: Option<String>,
    /// Full details for `e` (message, hint, details JSON).
    pub details: Option<String>,
    /// Birth on the tick clock (ms).
    pub born_ms: u64,
}

impl Toast {
    /// A success toast.
    pub fn ok(text: impl Into<String>, now: u64) -> Self {
        Self::new(ToastKind::Ok, text, now)
    }

    /// A neutral toast.
    pub fn info(text: impl Into<String>, now: u64) -> Self {
        Self::new(ToastKind::Info, text, now)
    }

    fn new(kind: ToastKind, text: impl Into<String>, now: u64) -> Self {
        Self {
            kind,
            text: text.into(),
            code: None,
            hint: None,
            details: None,
            born_ms: now,
        }
    }

    /// An error toast from a stems error: `CODE message`, the hint, and the
    /// details for `e`.
    pub fn error(what: &str, e: &stems_core::Error, now: u64) -> Self {
        let mut details = format!("{what}\n\n{}: {}", e.code.as_str(), e.message);
        if let Some(h) = &e.hint {
            details.push_str(&format!("\n\nhint: {h}"));
        }
        if !matches!(&e.details, Value::Object(m) if m.is_empty()) && !e.details.is_null() {
            let pretty = serde_json::to_string_pretty(&e.details).unwrap_or_default();
            details.push_str(&format!("\n\ndetails:\n{pretty}"));
        }
        Self {
            kind: ToastKind::Error,
            text: format!("{what}: {}", e.message),
            code: Some(e.code.as_str().to_string()),
            hint: e.hint.clone(),
            details: Some(details),
            born_ms: now,
        }
    }

    /// An error toast without a stems error (a local failure).
    pub fn failure(text: impl Into<String>, now: u64) -> Self {
        let text = text.into();
        Self {
            details: Some(text.clone()),
            ..Self::new(ToastKind::Error, text, now)
        }
    }

    /// The glyph shown before the text.
    pub fn glyph(&self, ascii: bool) -> &'static str {
        match (&self.kind, ascii) {
            (ToastKind::Ok, false) => "✓",
            (ToastKind::Ok, true) => "OK",
            (ToastKind::Error, false) => "✗",
            (ToastKind::Error, true) => "X",
            (ToastKind::Info | ToastKind::ConfigChanged, false) => "•",
            (ToastKind::Info | ToastKind::ConfigChanged, true) => "*",
        }
    }

    /// The text lines (glyph first; code, hint and key hints after).
    pub fn lines(&self, ascii: bool) -> Vec<String> {
        let mut out = Vec::new();
        match &self.code {
            Some(c) => out.push(format!("{} {c} {}", self.glyph(ascii), self.text)),
            None => out.push(format!("{} {}", self.glyph(ascii), self.text)),
        }
        if let Some(h) = &self.hint {
            out.push(format!("  hint: {h}"));
        }
        match self.kind {
            ToastKind::Error => out.push("  e details · Esc dismiss".into()),
            ToastKind::ConfigChanged => out.push("  a apply · v view · Esc dismiss".into()),
            _ => {}
        }
        out
    }
}

/// The toast stack of the model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Toasts {
    /// Oldest first.
    pub items: Vec<Toast>,
}

impl Toasts {
    /// Add a toast (dropping the oldest beyond [`TOAST_MAX`]). A new
    /// config-changed toast replaces an older one.
    pub fn push(&mut self, t: Toast) {
        if t.kind == ToastKind::ConfigChanged {
            self.items.retain(|x| x.kind != ToastKind::ConfigChanged);
        }
        self.items.push(t);
        let n = self.items.len();
        self.items.drain(..n.saturating_sub(TOAST_MAX));
    }

    /// Drop toasts older than [`TOAST_TTL_MS`] at `now`. A config-changed
    /// toast stays until it is acted on or dismissed.
    pub fn expire(&mut self, now: u64) {
        self.items.retain(|t| {
            t.kind == ToastKind::ConfigChanged || now.saturating_sub(t.born_ms) < TOAST_TTL_MS
        });
    }

    /// Dismiss every toast (`Esc`); returns whether there were any.
    pub fn dismiss(&mut self) -> bool {
        let any = !self.items.is_empty();
        self.items.clear();
        any
    }

    /// The newest error toast (for `e`).
    pub fn last_error(&self) -> Option<&Toast> {
        self.items.iter().rev().find(|t| t.kind == ToastKind::Error)
    }

    /// The pending config-changed toast (for `a` / `v`).
    pub fn config_changed(&self) -> Option<&Toast> {
        self.items
            .iter()
            .find(|t| t.kind == ToastKind::ConfigChanged)
    }

    /// Remove the config-changed toast.
    pub fn clear_config(&mut self) {
        self.items.retain(|t| t.kind != ToastKind::ConfigChanged);
    }

    /// No toast shown.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_core::{Error, ErrorCode};

    #[test]
    fn lifecycle_with_a_fake_clock() {
        let mut t = Toasts::default();
        t.push(Toast::ok("restarted a", 0));
        t.push(Toast::ok("restarted b", 3000));
        t.expire(4999);
        assert_eq!(t.items.len(), 2);
        t.expire(5000);
        assert_eq!(t.items.len(), 1, "the first one is 5 s old");
        assert_eq!(t.items[0].text, "restarted b");
        t.expire(8000);
        assert!(t.is_empty());
    }

    #[test]
    fn stack_is_bounded_and_esc_dismisses() {
        let mut t = Toasts::default();
        for i in 0..6 {
            t.push(Toast::info(format!("n{i}"), 0));
        }
        assert_eq!(t.items.len(), TOAST_MAX);
        assert_eq!(t.items[0].text, "n2");
        assert!(t.dismiss());
        assert!(!t.dismiss());
    }

    #[test]
    fn errors_carry_code_hint_and_details() {
        let e = Error::new(ErrorCode::HasDependants, "cannot stop a")
            .with_hint("stop them too")
            .with_details(serde_json::json!({"dependants": ["b"]}));
        let t = Toast::error("stop a", &e, 7);
        assert_eq!(
            t.lines(false),
            vec![
                "✗ HAS_DEPENDANTS stop a: cannot stop a".to_string(),
                "  hint: stop them too".to_string(),
                "  e details · Esc dismiss".to_string(),
            ]
        );
        let d = t.details.as_deref().unwrap();
        assert!(d.contains("\"dependants\""), "{d}");
        let mut s = Toasts::default();
        s.push(t);
        s.push(Toast::ok("x", 8));
        assert_eq!(
            s.last_error().unwrap().code.as_deref(),
            Some("HAS_DEPENDANTS")
        );
    }

    #[test]
    fn config_changed_toast_stays_and_is_replaced() {
        let mut t = Toasts::default();
        let c = |n: &str, at| Toast {
            kind: ToastKind::ConfigChanged,
            ..Toast::info(n, at)
        };
        t.push(c("1 stem", 0));
        t.push(c("2 stems", 10));
        assert_eq!(t.items.len(), 1);
        t.expire(60_000);
        assert_eq!(t.config_changed().unwrap().text, "2 stems");
        t.clear_config();
        assert!(t.is_empty());
    }
}
