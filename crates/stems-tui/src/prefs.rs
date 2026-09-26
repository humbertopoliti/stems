//! UI preferences: `~/.config/stems/ui.toml` (FR-UI-4).
//!
//! ```toml
//! theme = "dark"          # dark | light
//! mouse = false           # click selects rows
//! default_view = "table"  # table | detail | graph | logs | events
//! refresh_ms = 250        # tick interval (status refresh every ~1 s)
//! split_logs = false      # Ctrl-L: the log pane under Table/Graph/Detail (29; Ctrl-L saves it)
//! clipboard = "osc52"     # osc52 (terminal escape, works over ssh) | command (pbcopy/wl-copy/xclip)
//! ```
//!
//! The path is `$STEMS_UI_CONFIG` when set, else
//! `$XDG_CONFIG_HOME/stems/ui.toml`, else `$HOME/.config/stems/ui.toml`. A
//! missing file means defaults; an invalid one falls back to defaults with a
//! warning shown in the status bar.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::ViewKind;

/// Env var overriding the preferences path (tests).
pub const ENV_UI_CONFIG: &str = "STEMS_UI_CONFIG";

/// Colour theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Light text on a dark terminal.
    #[default]
    Dark,
    /// Dark text on a light terminal.
    Light,
}

/// How `y` copies to the clipboard (29).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Clipboard {
    /// Write an OSC 52 escape sequence to the terminal (works over ssh; the
    /// terminal must allow it).
    #[default]
    Osc52,
    /// Pipe into `pbcopy` (macOS), `wl-copy` (Wayland) or `xclip`.
    Command,
}

/// Parsed `ui.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// `dark` (default) or `light`.
    pub theme: Theme,
    /// Mouse clicks select table rows (default off).
    pub mouse: bool,
    /// The view the dashboard opens in (default: table).
    pub default_view: Option<ViewKind>,
    /// Tick interval in milliseconds (default 250, clamped to 50..=5000).
    pub refresh_ms: u64,
    /// Show the bottom log pane under Table/Graph/Detail (`Ctrl-L`, 29).
    pub split_logs: bool,
    /// How `y` copies (default OSC 52).
    pub clipboard: Clipboard,
    /// Where these preferences were read from (and where `Ctrl-L` saves
    /// `split_logs`); `None` for defaults without a path or an invalid file.
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            mouse: false,
            default_view: None,
            refresh_ms: 250,
            split_logs: false,
            clipboard: Clipboard::Osc52,
            path: None,
        }
    }
}

impl Prefs {
    /// Parse the TOML text of `ui.toml`.
    pub fn parse(text: &str) -> Result<Prefs, String> {
        let mut p: Prefs = toml::from_str(text).map_err(|e| e.to_string())?;
        p.refresh_ms = p.refresh_ms.clamp(50, 5000);
        Ok(p)
    }

    /// Where `ui.toml` lives for this environment (`env` looks variables up).
    pub fn path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let nonempty = |k: &str| env(k).filter(|v| !v.is_empty());
        if let Some(p) = nonempty(ENV_UI_CONFIG) {
            return Some(PathBuf::from(p));
        }
        if let Some(x) = nonempty("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(x).join("stems").join("ui.toml"));
        }
        nonempty("HOME").map(|h| PathBuf::from(h).join(".config/stems/ui.toml"))
    }

    /// Load the preferences: defaults when the file is missing; defaults plus
    /// a warning when it cannot be read or parsed.
    pub fn load(env: impl Fn(&str) -> Option<String>) -> (Prefs, Option<String>) {
        let Some(path) = Self::path(env) else {
            return (Prefs::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                Prefs {
                    path: Some(path),
                    ..Prefs::default()
                },
                None,
            ),
            Err(e) => (
                Prefs::default(),
                Some(format!("cannot read {}: {e}", path.display())),
            ),
            Ok(text) => match Self::parse(&text) {
                Ok(p) => (
                    Prefs {
                        path: Some(path),
                        ..p
                    },
                    None,
                ),
                Err(e) => (
                    Prefs::default(),
                    Some(format!(
                        "invalid {}: {}",
                        path.display(),
                        e.lines().next().unwrap_or("")
                    )),
                ),
            },
        }
    }
}

/// Set `key = value` (`value` is TOML text) in the file at `path`: the
/// first `key = ...` line is replaced (comments and other lines are kept),
/// else the line is appended; the file and its directory are created when
/// missing.
pub fn save_key(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let line = format!("{key} = {value}");
    let mut done = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            let is_key = t
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='));
            if is_key && !done {
                done = true;
                line.clone()
            } else {
                l.to_string()
            }
        })
        .collect();
    if !done {
        out.push(line);
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let mut body = out.join("\n");
    body.push('\n');
    std::fs::write(path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        assert_eq!(Prefs::parse("").unwrap(), Prefs::default());
        assert_eq!(Prefs::default().refresh_ms, 250);
    }

    #[test]
    fn parses_every_key() {
        let p = Prefs::parse(
            "theme = \"light\"\nmouse = true\ndefault_view = \"detail\"\nrefresh_ms = 10\nsplit_logs = true\n",
        )
        .unwrap();
        assert_eq!(p.theme, Theme::Light);
        assert!(p.mouse);
        assert_eq!(p.default_view, Some(ViewKind::Detail));
        assert_eq!(p.refresh_ms, 50, "clamped");
        assert!(p.split_logs);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Prefs::parse("theme = \"pink\"").is_err());
        assert!(Prefs::parse("default_view = \"nope\"").is_err());
    }

    #[test]
    fn path_precedence() {
        let env = |k: &str| match k {
            "STEMS_UI_CONFIG" => Some("/x/ui.toml".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        };
        assert_eq!(Prefs::path(env).unwrap(), PathBuf::from("/x/ui.toml"));
        let env = |k: &str| (k == "HOME").then(|| "/home/u".to_string());
        assert_eq!(
            Prefs::path(env).unwrap(),
            PathBuf::from("/home/u/.config/stems/ui.toml")
        );
    }

    #[test]
    fn load_missing_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("ui.toml");
        let fs = f.to_string_lossy().to_string();
        let env = |k: &str| (k == ENV_UI_CONFIG).then(|| fs.clone());
        let (p, warn) = Prefs::load(env);
        assert_eq!((p.path.as_deref(), warn), (Some(f.as_path()), None));
        assert_eq!(Prefs { path: None, ..p }, Prefs::default());
        std::fs::write(&f, "mouse = 3").unwrap();
        let (p, warn) = Prefs::load(env);
        assert_eq!(p, Prefs::default());
        assert!(warn.unwrap().contains("invalid"));
        std::fs::write(&f, "default_view = \"detail\"").unwrap();
        assert_eq!(Prefs::load(env).0.default_view, Some(ViewKind::Detail));
    }

    #[test]
    fn clipboard_values() {
        assert_eq!(Prefs::default().clipboard, Clipboard::Osc52);
        let p = Prefs::parse("clipboard = \"command\"").unwrap();
        assert_eq!(p.clipboard, Clipboard::Command);
        assert!(Prefs::parse("clipboard = \"x\"").is_err());
    }

    #[test]
    fn save_key_replaces_or_appends() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("sub/ui.toml");
        save_key(&f, "split_logs", "true").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "split_logs = true\n");
        std::fs::write(&f, "# mine\ntheme = \"light\"\nsplit_logs=true # x\n").unwrap();
        save_key(&f, "split_logs", "false").unwrap();
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "# mine\ntheme = \"light\"\nsplit_logs = false\n"
        );
        let p = Prefs::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(p.theme, Theme::Light);
        assert!(!p.split_logs);
    }
}
