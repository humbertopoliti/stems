//! Desktop notifications when stems fail for good (deliverable 34, FR-LC-6).
//!
//! A task subscribes to the event bus and collects every `stem.state` with
//! `to: failed` (the state every "gave up" path ends in: `MAX_RESTARTS`,
//! `START_TIMEOUT`, exit before ready, policy `never`, setup/script
//! failures). The pure [`Batcher`] coalesces them three ways: a *debounce*
//! (quiet time after a failure, capped at five debounces), an *early flush*
//! when the command that caused them returns (`up.finished`,
//! `cascade.finished`, `cascade.aborted`) and a per-workspace *cooldown*
//! after each notification (failures inside it wait for the next one and
//! are counted as `folded`). One batch is one notification, rendered by
//! [`render`] and posted by a [`Notifier`]: the user's
//! `notifications.command`, else the first OS backend found on `PATH`
//! (`terminal-notifier` / `osascript` on macOS, `notify-send` on Linux).
//! Every outcome is an event: `notification.sent` or `notification.skipped
//! {reason: disabled | no_backend | failed}`.
//!
//! Excluded: a failure while the user stops the stem (`from: stopping`),
//! anything while a `down` is in flight or the daemon shuts down.

pub mod backend;

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use stems_api::{DAEMON_ACTOR, Event, EventKind};
use stems_config::NotificationSettings;
use tokio::sync::broadcast;

use crate::events::EventDraft;
use crate::supervisor::Supervisor;

pub use backend::{Backend, CommandNotifier, Notifier, OsNotifier, ReplaceState, Sent};

/// The environment kill switch: `STEMS_NOTIFICATIONS=0|off|false` on the
/// daemon wins over the config (CI, scripts, the e2e harness).
pub const ENV_KILL_SWITCH: &str = "STEMS_NOTIFICATIONS";

/// The debounce cap: a batch flushes at most this many debounces after its
/// first failure, however many keep arriving.
pub const DEBOUNCE_CAP: u32 = 5;

/// Longest a backend command may run.
pub const BACKEND_TIMEOUT: Duration = Duration::from_secs(5);

/// A failure message is cut at about this many characters.
pub const MESSAGE_LIMIT: usize = 120;

/// The code reported for a `failed` transition that carries no error
/// (`restart.policy: never` after an exit: reason `exited with code N`).
pub const EXITED_CODE: &str = "EXITED";

/// One stem that failed for good.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Stem name.
    pub stem: String,
    /// Error code (`MAX_RESTARTS`, `START_TIMEOUT`, ..., [`EXITED_CODE`]).
    pub code: String,
    /// The error message (one line).
    pub message: String,
    /// What to do about it.
    pub hint: Option<String>,
}

impl Failure {
    /// The failure of a `stem.state` event with `to: failed`: `data.error`
    /// when present, else the reason with code [`EXITED_CODE`].
    pub fn from_event(e: &Event) -> Option<Self> {
        if e.kind != EventKind::STEM_STATE || e.to.as_deref() != Some("failed") {
            return None;
        }
        let stem = e.stem.clone()?;
        let err = e.data.get("error");
        let text = |k: &str| {
            err.and_then(|v| v.get(k))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let message = text("message")
            .or_else(|| e.reason.clone())
            .unwrap_or_else(|| "failed".to_owned());
        Some(Self {
            stem,
            code: text("code").unwrap_or_else(|| EXITED_CODE.to_owned()),
            message,
            hint: text("hint"),
        })
    }
}

/// The failures one notification covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch {
    /// The workspace they belong to.
    pub workspace: String,
    /// In arrival order (a stem may appear twice: a crash loop).
    pub failures: Vec<Failure>,
    /// How many of them arrived during the cooldown of the previous
    /// notification and were held back for this one.
    pub folded: usize,
}

struct Open {
    first: Instant,
    last: Instant,
    /// Set by an early flush: due now (or when the cooldown ends).
    early: bool,
    failures: Vec<Failure>,
    folded: usize,
}

#[derive(Default)]
struct WsState {
    open: Option<Open>,
    cooldown_until: Option<Instant>,
}

/// Pure coalescing of failures into batches, driven by an explicit clock.
pub struct Batcher {
    debounce: Duration,
    cooldown: Duration,
    ws: HashMap<String, WsState>,
}

impl Batcher {
    /// A batcher with these windows.
    pub fn new(debounce: Duration, cooldown: Duration) -> Self {
        Self {
            debounce,
            cooldown,
            ws: HashMap::new(),
        }
    }

    /// New windows (the config was reloaded). They apply from now on; a
    /// cooldown already running keeps its end.
    pub fn configure(&mut self, debounce: Duration, cooldown: Duration) {
        self.debounce = debounce;
        self.cooldown = cooldown;
    }

    /// A stem of `workspace` failed at `now`.
    pub fn fail(&mut self, now: Instant, workspace: &str, f: Failure) {
        let st = self.ws.entry(workspace.to_owned()).or_default();
        let in_cooldown = st.cooldown_until.is_some_and(|c| now < c);
        let open = st.open.get_or_insert_with(|| Open {
            first: now,
            last: now,
            early: false,
            failures: Vec::new(),
            folded: 0,
        });
        open.last = now;
        open.failures.push(f);
        if in_cooldown {
            open.folded += 1;
        }
    }

    /// The command that caused the failures returned: every open batch is
    /// due now (a running cooldown still holds it until it ends).
    pub fn flush_now(&mut self) {
        for st in self.ws.values_mut() {
            if let Some(o) = st.open.as_mut() {
                o.early = true;
            }
        }
    }

    fn due_at(&self, st: &WsState) -> Option<Instant> {
        let o = st.open.as_ref()?;
        let d = if o.early {
            o.last
        } else {
            (o.last + self.debounce).min(o.first + self.debounce * DEBOUNCE_CAP)
        };
        Some(st.cooldown_until.map_or(d, |c| d.max(c)))
    }

    /// When the next batch is due, if any is open.
    pub fn deadline(&self) -> Option<Instant> {
        self.ws.values().filter_map(|st| self.due_at(st)).min()
    }

    /// The batches due at `now`, each starting its workspace's cooldown.
    pub fn take_due(&mut self, now: Instant) -> Vec<Batch> {
        let mut out = Vec::new();
        let mut names: Vec<String> = self.ws.keys().cloned().collect();
        names.sort();
        for name in names {
            let due = self
                .ws
                .get(&name)
                .and_then(|st| self.due_at(st))
                .is_some_and(|d| d <= now);
            if !due {
                continue;
            }
            let st = self.ws.get_mut(&name).expect("present");
            let Some(o) = st.open.take() else { continue };
            st.cooldown_until = Some(now + self.cooldown);
            out.push(Batch {
                workspace: name,
                failures: o.failures,
                folded: o.folded,
            });
        }
        out
    }

    /// Failures waiting in open batches (tests, shutdown logging).
    pub fn pending(&self) -> usize {
        self.ws
            .values()
            .filter_map(|st| st.open.as_ref())
            .map(|o| o.failures.len())
            .sum()
    }
}

/// What gets posted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Notification {
    /// `stems · <workspace>`.
    pub title: String,
    /// What failed and why.
    pub body: String,
    /// Second line: the hint (one stem) or the per-code tally (several).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Distinct stems, in order of first failure.
    pub stems: Vec<String>,
    /// Distinct codes, in order of first appearance.
    pub codes: Vec<String>,
    /// The workspace name.
    pub workspace: String,
    /// Failures held back by the cooldown.
    pub folded: usize,
}

fn unique(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in items {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// Cut `s` at about [`MESSAGE_LIMIT`] characters (on a char boundary, with
/// an ellipsis), and drop a leading `` `<stem>` `` the message repeats.
fn short_message(stem: &str, s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix(&format!("`{stem}` "))
        .or_else(|| s.strip_prefix(&format!("{stem} ")))
        .or_else(|| s.strip_prefix(&format!("`{stem}`: ")))
        .unwrap_or(s);
    let s = s.lines().next().unwrap_or("").trim_end();
    if s.chars().count() <= MESSAGE_LIMIT {
        return s.to_owned();
    }
    let cut: String = s.chars().take(MESSAGE_LIMIT - 1).collect();
    format!("{}…", cut.trim_end())
}

/// The notification for a batch: title `stems · <workspace>`; body
/// `<stem> failed: <message>` (hint as the subtitle) for one stem, `<a>, <b>,
/// <c> failed` with a per-code tally for two or three, `<n> stems failed:
/// <a>, <b>, <c>, +<n-3> more` beyond. Failures held back by the cooldown
/// are listed like any other (`folded` only counts them). Never a command
/// line, env or probe detail.
pub fn render(batch: &Batch) -> Notification {
    let stems = unique(batch.failures.iter().map(|f| f.stem.clone()));
    let codes = unique(batch.failures.iter().map(|f| f.code.clone()));
    // The latest failure of each stem is the one described.
    let latest = |stem: &str| batch.failures.iter().rev().find(|f| f.stem == stem);
    let (body, subtitle) = match stems.len() {
        0 => (String::from("nothing failed"), None),
        1 => {
            let f = latest(&stems[0]).expect("one stem");
            (
                format!("{} failed: {}", f.stem, short_message(&f.stem, &f.message)),
                f.hint.clone().map(|h| short_message(&f.stem, &h)),
            )
        }
        n => {
            let mut tally: Vec<(String, usize)> = Vec::new();
            for s in &stems {
                let code = latest(s).map_or(EXITED_CODE, |f| f.code.as_str());
                match tally.iter_mut().find(|(c, _)| c == code) {
                    Some((_, k)) => *k += 1,
                    None => tally.push((code.to_owned(), 1)),
                }
            }
            let tally: Vec<String> = tally.iter().map(|(c, k)| format!("{k} × {c}")).collect();
            let body = if n <= 3 {
                format!("{} failed", stems.join(", "))
            } else {
                format!(
                    "{n} stems failed: {}, +{} more",
                    stems[..3].join(", "),
                    n - 3
                )
            };
            (body, Some(tally.join(", ")))
        }
    };
    Notification {
        title: format!("stems · {}", batch.workspace),
        body,
        subtitle,
        stems,
        codes,
        workspace: batch.workspace.clone(),
        folded: batch.folded,
    }
}

/// `STEMS_NOTIFICATIONS=0|off|false` (case-insensitive, trimmed).
pub fn env_disables(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| {
        let v = v.to_string_lossy();
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        )
    })
}

/// A stable replace/group id per workspace: `stems:<8 hex of sha256(root)>`,
/// so a later notification replaces the earlier one in Notification Centre
/// instead of stacking.
pub fn replace_id(root: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let hex: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("stems:{hex}")
}

// ---------------------------------------------------------------------------
// The task
// ---------------------------------------------------------------------------

/// Start the notifier task of a daemon's supervisor. It ends with the
/// supervisor (weak reference) or the event bus.
pub(crate) fn spawn(sup: &Arc<Supervisor>) {
    let rx = sup.events().subscribe();
    let disabled = env_disables(std::env::var_os(ENV_KILL_SWITCH).as_deref());
    tokio::spawn(run(Arc::downgrade(sup), rx, disabled));
}

struct Task {
    weak: Weak<Supervisor>,
    env_disabled: bool,
    batcher: Batcher,
    last_seq: u64,
    down_in_flight: bool,
    stopping: bool,
    no_backend_reported: bool,
    replace: ReplaceState,
}

impl Task {
    fn sup(&self) -> Option<Arc<Supervisor>> {
        self.weak.upgrade()
    }

    /// The applied workspace's name, root and settings (defaults without one).
    fn settings(&self) -> (String, std::path::PathBuf, NotificationSettings) {
        self.sup().and_then(|s| s.host().resolved()).map_or_else(
            || {
                (
                    "workspace".to_owned(),
                    std::path::PathBuf::from("/"),
                    NotificationSettings::default(),
                )
            },
            |r| {
                (
                    r.workspace.name.clone(),
                    r.workspace.root.clone(),
                    r.workspace.notifications.clone(),
                )
            },
        )
    }

    fn handle(&mut self, e: &Event, now: Instant) {
        self.last_seq = self.last_seq.max(e.seq);
        match &e.kind {
            k if *k == EventKind::DOWN_STARTED => self.down_in_flight = true,
            k if *k == EventKind::DOWN_FINISHED => self.down_in_flight = false,
            k if *k == EventKind::DAEMON_STOPPING => self.stopping = true,
            k if *k == EventKind::UP_FINISHED
                || *k == EventKind::CASCADE_FINISHED
                || *k == EventKind::CASCADE_ABORTED =>
            {
                self.batcher.flush_now();
            }
            k if *k == EventKind::STEM_STATE => {
                if e.from.as_deref() == Some("stopping") || self.down_in_flight || self.stopping {
                    return;
                }
                if let Some(f) = Failure::from_event(e) {
                    // The applied config's windows, so the deadline and the
                    // cooldown started by `take_due` are the configured ones.
                    let (ws, _, settings) = self.settings();
                    self.batcher.configure(
                        settings.debounce.as_duration(),
                        settings.cooldown.as_duration(),
                    );
                    tracing::debug!(stem = %f.stem, code = %f.code, "failure queued for notification");
                    self.batcher.fail(now, &ws, f);
                }
            }
            _ => {}
        }
    }

    fn emit(&self, d: EventDraft) {
        if let Some(s) = self.sup() {
            s.events().emit(d);
        }
    }

    async fn flush(&mut self, batch: Batch) {
        let (_, root, settings) = self.settings();
        let n = render(&batch);
        let skipped = |reason: &str, error: Option<String>| {
            let mut data = json!({ "reason": reason, "stems": n.stems, "codes": n.codes });
            if let Some(e) = error {
                data["error"] = json!(e);
            }
            EventDraft::new(EventKind::NOTIFICATION_SKIPPED, DAEMON_ACTOR).data(data)
        };
        if self.env_disabled || !settings.enabled {
            tracing::debug!(stems = ?n.stems, "notification skipped: disabled");
            self.emit(skipped("disabled", None));
            return;
        }
        let notifier: Box<dyn Notifier> = match settings.command.as_deref() {
            Some(cmd) => Box::new(CommandNotifier::new(cmd, &root)),
            None => {
                match Backend::detect(std::env::var_os("PATH").as_deref(), std::env::consts::OS) {
                    Some(b) => Box::new(OsNotifier::new(b)),
                    None => {
                        if !self.no_backend_reported {
                            self.no_backend_reported = true;
                            tracing::info!(
                                "no desktop notification backend found on PATH (terminal-notifier, osascript, notify-send); set notifications.command or install one"
                            );
                            self.emit(skipped("no_backend", None));
                        } else {
                            tracing::debug!(stems = ?n.stems, "notification skipped: no backend");
                        }
                        return;
                    }
                }
            }
        };
        if self.replace.group.is_empty() {
            self.replace.group = replace_id(&root);
        }
        match notifier.send(&n, &mut self.replace).await {
            Ok(sent) => {
                tracing::info!(backend = sent.backend, stems = ?n.stems, "notification sent");
                tracing::debug!(title = %n.title, body = %n.body, "notification");
                self.emit(
                    EventDraft::new(EventKind::NOTIFICATION_SENT, DAEMON_ACTOR).data(json!({
                        "stems": n.stems,
                        "codes": n.codes,
                        "backend": sent.backend,
                        "title": n.title,
                        "body": n.body,
                        "subtitle": n.subtitle,
                        "replaced": sent.replaced,
                        "folded": n.folded,
                    })),
                );
            }
            Err(e) => {
                tracing::warn!(backend = notifier.name(), error = %e, "notification failed");
                self.emit(skipped("failed", Some(e)));
            }
        }
    }
}

async fn run(weak: Weak<Supervisor>, mut rx: broadcast::Receiver<Event>, env_disabled: bool) {
    let d = NotificationSettings::default();
    let mut t = Task {
        weak,
        env_disabled,
        batcher: Batcher::new(d.debounce.as_duration(), d.cooldown.as_duration()),
        last_seq: 0,
        down_in_flight: false,
        stopping: false,
        no_backend_reported: false,
        replace: ReplaceState::default(),
    };
    loop {
        let deadline = t
            .batcher
            .deadline()
            .map(tokio::time::Instant::from_std)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(e) => t.handle(&e, Instant::now()),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // A lagged notifier must not lose a `failed`: re-read the
                    // ring from the last seen seq (as reload.rs does).
                    tracing::debug!(lagged = n, "notifier re-reading the event ring");
                    let Some(sup) = t.sup() else { return };
                    let (backlog, new_rx) = sup.events().subscribe_from(Some(t.last_seq));
                    drop(sup);
                    rx = new_rx;
                    let now = Instant::now();
                    for e in &backlog {
                        t.handle(e, now);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            () = tokio::time::sleep_until(deadline) => {
                if t.sup().is_none() {
                    return;
                }
                for batch in t.batcher.take_due(Instant::now()) {
                    t.flush(batch).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
