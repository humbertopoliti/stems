//! The action dispatcher (30): what the dashboard asks the daemon to do on
//! the user's behalf, and the state of the script menu and the command
//! palette.
//!
//! Every [`Action`] is one RPC ([`Action::method`], [`Action::params`]) on
//! the TUI's client, whose requests carry `actor: tui:<user>` (FR-AI-4);
//! the MCP server (31) calls the same RPCs with its own actor.

use serde_json::{Map, Value, json};
use stems_api::{CatalogScript, Method};
use stems_config::ScriptArg;
use stems_core::scriptargs::ScriptKind;

use crate::fuzzy;
use crate::model::{Model, ViewKind};

/// An action on the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `start {stems: [stem]}` (`s`).
    Start(String),
    /// `stop {stems: [stem], cascade}` (`x`, `X`).
    Stop {
        /// The stem.
        stem: String,
        /// Stop running dependants first.
        cascade: bool,
    },
    /// `restart {stems: [stem], build}` (`r`, `R`).
    Restart {
        /// The stem.
        stem: String,
        /// Run `build` first (rebuild + restart).
        build: bool,
    },
    /// `watch_pause` / `watch_resume {stems: [stem]}` (`p`).
    Watch {
        /// The stem.
        stem: String,
        /// Pause (`false`: resume).
        pause: bool,
    },
    /// `reset {stems: [stem]}` after the typed confirmation (`S`).
    Reset(String),
    /// `up` with the session's profile (`u`).
    UpAll {
        /// Profile.
        profile: Option<String>,
    },
    /// `down {all: true}` (`d`, attach mode).
    DownAll,
    /// `run_script {stem, name, args, wait: false}` (script menu).
    RunScript {
        /// Owning stem (`None`: workspace script).
        stem: Option<String>,
        /// Script name.
        script: String,
        /// Validated arguments.
        args: Map<String, Value>,
    },
    /// `config_apply {yes: true}` (the config-changed toast's `a`, 33).
    ConfigApply,
}

/// The log "stem" of workspace scripts' output (the daemon's
/// `WORKSPACE_LOG_STEM`).
pub const WORKSPACE_LOG_STEM: &str = "_workspace";

/// Timeout of an action RPC (`up` and `restart {build}` can be long).
pub const ACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

impl Action {
    /// The RPC.
    pub fn method(&self) -> Method {
        match self {
            Action::Start(_) => Method::START,
            Action::Stop { .. } => Method::STOP,
            Action::Restart { .. } => Method::RESTART,
            Action::Watch { pause: true, .. } => Method::WATCH_PAUSE,
            Action::Watch { pause: false, .. } => Method::WATCH_RESUME,
            Action::Reset(_) => Method::RESET,
            Action::UpAll { .. } => Method::UP,
            Action::DownAll => Method::DOWN,
            Action::RunScript { .. } => Method::RUN_SCRIPT,
            Action::ConfigApply => Method::CONFIG_APPLY,
        }
    }

    /// Its params.
    pub fn params(&self) -> Value {
        match self {
            Action::Start(s) => json!({ "stems": [s] }),
            Action::Stop { stem, cascade } => json!({ "stems": [stem], "cascade": cascade }),
            Action::Restart { stem, build } => json!({ "stems": [stem], "build": build }),
            Action::Watch { stem, .. } => json!({ "stems": [stem] }),
            Action::Reset(s) => json!({ "stems": [s] }),
            Action::UpAll { profile } => match profile {
                Some(p) => json!({ "profile": p }),
                None => json!({}),
            },
            Action::DownAll => json!({ "all": true }),
            Action::RunScript { stem, script, args } => json!({
                "stem": stem, "name": script, "args": args, "wait": false
            }),
            Action::ConfigApply => json!({ "yes": true }),
        }
    }

    /// What is being done (`restart shop-api`), for notices and errors.
    pub fn describe(&self) -> String {
        match self {
            Action::Start(s) => format!("start {s}"),
            Action::Stop {
                stem,
                cascade: false,
            } => format!("stop {stem}"),
            Action::Stop {
                stem,
                cascade: true,
            } => format!("stop {stem} (cascade)"),
            Action::Restart { stem, build: false } => format!("restart {stem}"),
            Action::Restart { stem, build: true } => format!("rebuild {stem}"),
            Action::Watch { stem, pause: true } => format!("pause watch {stem}"),
            Action::Watch { stem, pause: false } => format!("resume watch {stem}"),
            Action::Reset(s) => format!("reset {s}"),
            Action::UpAll { profile: None } => "up".into(),
            Action::UpAll { profile: Some(p) } => format!("up (profile {p})"),
            Action::DownAll => "down --all".into(),
            Action::RunScript { stem, script, .. } => match stem {
                Some(s) => format!("run {script} ({s})"),
                None => format!("run {script} (workspace)"),
            },
            Action::ConfigApply => "config apply".into(),
        }
    }

    /// The success toast of a result.
    pub fn done(&self, v: &Value) -> String {
        let list = |k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|s| !s.is_empty())
        };
        match self {
            Action::Start(s) => format!("started {s}"),
            Action::Stop { .. } => match list("stopped") {
                Some(l) => format!("stopped {l}"),
                None => "nothing to stop".into(),
            },
            Action::Restart { stem, build: false } => format!("restarted {stem}"),
            Action::Restart { stem, build: true } => format!("rebuilt and restarted {stem}"),
            Action::Watch { stem, pause: true } => format!("watch paused: {stem}"),
            Action::Watch { stem, pause: false } => format!("watch resumed: {stem}"),
            Action::Reset(s) => format!("reset {s}"),
            Action::UpAll { .. } => match list("ready") {
                Some(l) => format!("up: {l}"),
                None => "up: nothing to start".into(),
            },
            Action::DownAll => "down --all: stopping everything".into(),
            Action::RunScript { script, .. } => format!("{script} started"),
            Action::ConfigApply => {
                let n = v
                    .get("applied")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                format!(
                    "config applied ({n} change{})",
                    if n == 1 { "" } else { "s" }
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Script menu
// ---------------------------------------------------------------------------

/// One runnable script (from `script_catalog`).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuEntry {
    /// Owning stem (`None`: workspace script).
    pub stem: Option<String>,
    /// Script name.
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// Lifecycle or custom.
    pub kind: ScriptKind,
    /// Declared arguments (a form opens when not empty).
    pub args: Vec<ScriptArg>,
}

impl From<&CatalogScript> for MenuEntry {
    fn from(c: &CatalogScript) -> Self {
        Self {
            stem: c.entry.stem.clone(),
            name: c.entry.name.clone(),
            description: c.entry.description.clone(),
            kind: c.entry.kind,
            args: c.entry.args.clone(),
        }
    }
}

/// The script menu (`:`): the selected stem's scripts, then the
/// workspace's; typing filters (fuzzy), `↑/↓` move, `Enter` runs or opens
/// the form.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScriptMenu {
    /// The stem whose scripts are listed.
    pub stem: Option<String>,
    /// Filter text.
    pub filter: String,
    /// Selected position among [`ScriptMenu::visible`].
    pub selected: usize,
}

impl ScriptMenu {
    /// The entries of the menu from the catalogue: the stem's (lifecycle
    /// first, then custom, each by name), then the workspace's.
    pub fn entries<'a>(&self, catalog: &'a [MenuEntry]) -> Vec<&'a MenuEntry> {
        let mut own: Vec<&MenuEntry> = catalog
            .iter()
            .filter(|e| e.stem.is_some() && e.stem == self.stem)
            .collect();
        own.sort_by_key(|e| (e.kind != ScriptKind::Lifecycle, e.name.clone()));
        let mut ws: Vec<&MenuEntry> = catalog.iter().filter(|e| e.stem.is_none()).collect();
        ws.sort_by_key(|e| e.name.clone());
        own.extend(ws);
        own
    }

    /// The entries shown (filtered, best match first when filtering).
    pub fn visible<'a>(&self, catalog: &'a [MenuEntry]) -> Vec<&'a MenuEntry> {
        let all = self.entries(catalog);
        if self.filter.trim().is_empty() {
            return all;
        }
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        fuzzy::rank(&self.filter, &names)
            .into_iter()
            .map(|i| all[i])
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------

/// What a palette item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaletteTarget {
    /// Select `stem`, then press `key` (the same path as the key itself,
    /// confirmations included).
    StemKey {
        /// The stem.
        stem: String,
        /// The action key (`r`, `x`, ...).
        key: char,
    },
    /// Run (or open the form of) a script.
    Script {
        /// Owning stem.
        stem: Option<String>,
        /// Script name.
        script: String,
    },
    /// Select `stem` (if any) and switch to a view.
    View {
        /// The stem.
        stem: Option<String>,
        /// The view.
        view: ViewKind,
    },
    /// Press a key that needs no stem (`u`, `d`, `?`).
    Key(char),
}

/// One palette item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteItem {
    /// What is shown and matched (`restart shop-api`).
    pub label: String,
    /// What it does.
    pub target: PaletteTarget,
}

/// The command palette (`Ctrl-P`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    /// Query.
    pub query: String,
    /// Selected position among the matches.
    pub selected: usize,
}

/// The per-stem actions of the palette: (label, `{}` = the stem; key).
pub const STEM_ACTIONS: &[(&str, char)] = &[
    ("restart {}", 'r'),
    ("start {}", 's'),
    ("stop {}", 'x'),
    ("stop {} (cascade)", 'X'),
    ("rebuild {}", 'R'),
    ("pause watch {}", 'p'),
    ("open {} in $EDITOR", 'o'),
    ("reset {}", 'S'),
    ("scripts of {}", ':'),
];

/// Every palette item for the model: per-stem actions, logs, scripts
/// (from the catalogue, once loaded), views and global actions.
pub fn palette_items(model: &Model) -> Vec<PaletteItem> {
    let mut out = Vec::new();
    for s in &model.stems {
        for (label, key) in STEM_ACTIONS {
            let label = match *key {
                'p' if s.watch.as_ref().is_some_and(|w| w.paused) => "resume watch {}",
                _ => label,
            };
            out.push(PaletteItem {
                label: label.replace("{}", &s.name),
                target: PaletteTarget::StemKey {
                    stem: s.name.clone(),
                    key: *key,
                },
            });
        }
        out.push(PaletteItem {
            label: format!("view logs {}", s.name),
            target: PaletteTarget::View {
                stem: Some(s.name.clone()),
                view: ViewKind::Logs,
            },
        });
        out.push(PaletteItem {
            label: format!("view detail {}", s.name),
            target: PaletteTarget::View {
                stem: Some(s.name.clone()),
                view: ViewKind::Detail,
            },
        });
    }
    for e in model.catalog.as_deref().unwrap_or_default() {
        out.push(PaletteItem {
            label: match &e.stem {
                Some(s) => format!("run {} ({s})", e.name),
                None => format!("run {} (workspace)", e.name),
            },
            target: PaletteTarget::Script {
                stem: e.stem.clone(),
                script: e.name.clone(),
            },
        });
    }
    for v in ViewKind::ALL {
        out.push(PaletteItem {
            label: format!("view {}", v.title().to_lowercase()),
            target: PaletteTarget::View {
                stem: None,
                view: v,
            },
        });
    }
    out.push(PaletteItem {
        label: "up all".into(),
        target: PaletteTarget::Key('u'),
    });
    out.push(PaletteItem {
        label: "down all".into(),
        target: PaletteTarget::Key('d'),
    });
    out.push(PaletteItem {
        label: "help".into(),
        target: PaletteTarget::Key('?'),
    });
    out
}

/// The palette's matches for its query, best first.
pub fn palette_matches(model: &Model, p: &Palette) -> Vec<PaletteItem> {
    let items = palette_items(model);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    fuzzy::rank(&p.query, &labels)
        .into_iter()
        .map(|i| items[i].clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_and_params() {
        let a = Action::Stop {
            stem: "b".into(),
            cascade: true,
        };
        assert_eq!(a.method(), Method::STOP);
        assert_eq!(a.params(), json!({"stems": ["b"], "cascade": true}));
        let a = Action::Restart {
            stem: "b".into(),
            build: true,
        };
        assert_eq!(a.params(), json!({"stems": ["b"], "build": true}));
        assert_eq!(a.describe(), "rebuild b");
        assert_eq!(
            Action::Watch {
                stem: "api".into(),
                pause: false
            }
            .method(),
            Method::WATCH_RESUME
        );
        let mut args = Map::new();
        args.insert("email".into(), json!("a@b.c"));
        let a = Action::RunScript {
            stem: Some("api".into()),
            script: "create-test-user".into(),
            args,
        };
        assert_eq!(
            a.params(),
            json!({"stem": "api", "name": "create-test-user", "args": {"email": "a@b.c"}, "wait": false})
        );
        assert_eq!(Action::ConfigApply.params(), json!({"yes": true}));
        assert_eq!(
            Action::UpAll {
                profile: Some("web".into())
            }
            .params(),
            json!({"profile": "web"})
        );
        assert_eq!(
            Action::Stop {
                stem: "b".into(),
                cascade: true
            }
            .done(&json!({"stopped": ["d", "b"]})),
            "stopped d, b"
        );
    }
}
