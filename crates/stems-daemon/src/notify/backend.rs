//! Notification backends: shelling out, never a crate dependency.
//!
//! * macOS: `terminal-notifier` (if installed: `-group` replaces the
//!   previous notification) else `osascript` `display notification`; the
//!   AppleScript takes its strings as `argv`, so nothing is quoted into it.
//! * Linux: `notify-send --app-name stems -p` (`-r <id>` replaces; without
//!   `-p`/`-r` on older libnotify).
//! * Any OS: `notifications.command`, run with `/bin/sh -c` in the workspace
//!   root with `STEMS_NOTIFY_*` in its environment and the JSON payload on
//!   stdin.
//!
//! Tools are resolved on `PATH` (never an absolute path), so a test
//! harness can put a fake first. Every run is bounded by
//! [`super::BACKEND_TIMEOUT`]; a non-zero exit or a timeout is one
//! `notification.skipped {reason: "failed"}`, never retried for that batch.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{BACKEND_TIMEOUT, Notification};

/// What the notifier remembers between notifications of one daemon run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReplaceState {
    /// `stems:<ws-hash>` (`terminal-notifier -group`).
    pub group: String,
    /// The id `notify-send -p` printed, for `-r`.
    pub notify_send_id: Option<u32>,
    /// A notification with `-group` was posted this run (terminal-notifier
    /// replaces it).
    pub posted: bool,
}

/// A posted notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    /// `terminal-notifier`, `osascript`, `notify-send` or `command`.
    pub backend: &'static str,
    /// It replaced the previous notification of this workspace.
    pub replaced: bool,
}

/// Something that posts a notification.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Backend name (for events).
    fn name(&self) -> &'static str;
    /// Post it.
    async fn send(&self, n: &Notification, replace: &mut ReplaceState) -> Result<Sent, String>;
}

/// An OS backend found on `PATH`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `terminal-notifier` (macOS).
    TerminalNotifier(PathBuf),
    /// `osascript` (macOS).
    Osascript(PathBuf),
    /// `notify-send` (Linux, libnotify).
    NotifySend(PathBuf),
}

/// The first directory of `path` holding an executable file `name`.
pub fn find_on_path(path: Option<&OsStr>, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = path?;
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

impl Backend {
    /// The backend for `os` (`std::env::consts::OS`) on `path`, in
    /// resolution order; `None` on other systems or when nothing is found.
    pub fn detect(path: Option<&OsStr>, os: &str) -> Option<Self> {
        match os {
            "macos" => find_on_path(path, "terminal-notifier")
                .map(Self::TerminalNotifier)
                .or_else(|| find_on_path(path, "osascript").map(Self::Osascript)),
            "linux" => find_on_path(path, "notify-send").map(Self::NotifySend),
            _ => None,
        }
    }

    /// Backend name (for events and `doctor`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::TerminalNotifier(_) => "terminal-notifier",
            Self::Osascript(_) => "osascript",
            Self::NotifySend(_) => "notify-send",
        }
    }

    /// Where it was found.
    pub fn path(&self) -> &Path {
        match self {
            Self::TerminalNotifier(p) | Self::Osascript(p) | Self::NotifySend(p) => p,
        }
    }
}

/// `osascript` arguments: the script reads title, body and subtitle from
/// `argv`, so quotes, backslashes and newlines in them are never
/// interpreted.
pub fn osascript_args(n: &Notification) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["-e".into(), "on run argv".into(), "-e".into()];
    match &n.subtitle {
        Some(_) => v.push(
            "display notification (item 2 of argv) with title (item 1 of argv) subtitle (item 3 of argv)"
                .into(),
        ),
        None => v.push("display notification (item 2 of argv) with title (item 1 of argv)".into()),
    }
    v.push("-e".into());
    v.push("end run".into());
    v.push(n.title.clone().into());
    v.push(n.body.clone().into());
    if let Some(s) = &n.subtitle {
        v.push(s.clone().into());
    }
    v
}

/// `terminal-notifier` arguments (`-group` replaces the previous one).
pub fn terminal_notifier_args(n: &Notification, group: &str) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        "-title".into(),
        n.title.clone().into(),
        "-message".into(),
        n.body.clone().into(),
    ];
    if let Some(s) = &n.subtitle {
        v.push("-subtitle".into());
        v.push(s.clone().into());
    }
    v.push("-group".into());
    v.push(group.into());
    v
}

/// `notify-send` arguments; `replace` is the id of the notification to
/// replace (`-r`), `print_id` asks for the new id (`-p`, libnotify ≥ 0.7.9).
pub fn notify_send_args(n: &Notification, replace: Option<u32>, print_id: bool) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["--app-name".into(), "stems".into()];
    if let Some(id) = replace {
        v.push("-r".into());
        v.push(id.to_string().into());
    }
    if print_id {
        v.push("-p".into());
    }
    v.push("--".into());
    v.push(n.title.clone().into());
    let body = match &n.subtitle {
        Some(s) => format!("{}\n{s}", n.body),
        None => n.body.clone(),
    };
    v.push(body.into());
    v
}

/// Run `program args` within [`BACKEND_TIMEOUT`]: stdin from `stdin` (else
/// null), stdout captured when `capture`, stderr captured for the error
/// message. Ok(stdout).
async fn run(
    program: &Path,
    args: &[OsString],
    cwd: Option<&Path>,
    env: &[(String, String)],
    stdin: Option<String>,
    capture: bool,
) -> Result<String, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A notifier that never reads stdin must not block us.
        let _ = pipe.write_all(text.as_bytes()).await;
        drop(pipe);
    }
    let out = tokio::time::timeout(BACKEND_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            format!(
                "{} did not finish within {}s",
                program.display(),
                BACKEND_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.lines().next().unwrap_or("").trim();
        let mut msg = format!("{} {}", program.display(), out.status);
        if !stderr.is_empty() {
            msg.push_str(": ");
            msg.extend(stderr.chars().take(200));
        }
        return Err(msg);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The OS notifier.
pub struct OsNotifier {
    backend: Backend,
}

impl OsNotifier {
    /// A notifier over a detected backend.
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl Notifier for OsNotifier {
    fn name(&self) -> &'static str {
        self.backend.name()
    }

    async fn send(&self, n: &Notification, replace: &mut ReplaceState) -> Result<Sent, String> {
        let backend = self.backend.name();
        match &self.backend {
            Backend::Osascript(p) => {
                run(p, &osascript_args(n), None, &[], None, false).await?;
                Ok(Sent {
                    backend,
                    replaced: false,
                })
            }
            Backend::TerminalNotifier(p) => {
                let replaced = replace.posted;
                run(
                    p,
                    &terminal_notifier_args(n, &replace.group),
                    None,
                    &[],
                    None,
                    false,
                )
                .await?;
                // Any later notification of this run replaces this one.
                replace.posted = true;
                Ok(Sent { backend, replaced })
            }
            Backend::NotifySend(p) => {
                let prev = replace.notify_send_id;
                let first = run(p, &notify_send_args(n, prev, true), None, &[], None, true).await;
                let out = match first {
                    Ok(out) => out,
                    // Older libnotify: no `-p`/`-r`; post without replacing.
                    Err(_) => run(p, &notify_send_args(n, None, false), None, &[], None, false)
                        .await
                        .map(|_| String::new())?,
                };
                let id = out.trim().parse::<u32>().ok();
                replace.notify_send_id = id.or(prev);
                replace.posted = true;
                Ok(Sent {
                    backend,
                    replaced: prev.is_some() && id.is_some(),
                })
            }
        }
    }
}

/// `notifications.command`: `/bin/sh -c <command>` in the workspace root.
pub struct CommandNotifier {
    command: String,
    cwd: PathBuf,
}

impl CommandNotifier {
    /// A notifier running `command` in `cwd`.
    pub fn new(command: &str, cwd: &Path) -> Self {
        Self {
            command: command.to_owned(),
            cwd: cwd.to_path_buf(),
        }
    }

    /// The `STEMS_NOTIFY_*` environment of a notification.
    pub fn env(n: &Notification) -> Vec<(String, String)> {
        let mut v = vec![
            ("STEMS_NOTIFY_TITLE".to_owned(), n.title.clone()),
            ("STEMS_NOTIFY_BODY".to_owned(), n.body.clone()),
            ("STEMS_NOTIFY_STEMS".to_owned(), n.stems.join(",")),
            ("STEMS_NOTIFY_CODES".to_owned(), n.codes.join(",")),
            ("STEMS_NOTIFY_WORKSPACE".to_owned(), n.workspace.clone()),
            ("STEMS_NOTIFY_FOLDED".to_owned(), n.folded.to_string()),
        ];
        if let Some(s) = &n.subtitle {
            v.push(("STEMS_NOTIFY_SUBTITLE".to_owned(), s.clone()));
        }
        v
    }
}

#[async_trait]
impl Notifier for CommandNotifier {
    fn name(&self) -> &'static str {
        "command"
    }

    async fn send(&self, n: &Notification, _replace: &mut ReplaceState) -> Result<Sent, String> {
        let payload = serde_json::to_string(n).unwrap_or_default();
        let args: Vec<OsString> = vec!["-c".into(), self.command.clone().into()];
        let cwd = self.cwd.is_dir().then_some(self.cwd.as_path());
        run(
            Path::new("/bin/sh"),
            &args,
            cwd,
            &Self::env(n),
            Some(payload),
            false,
        )
        .await?;
        Ok(Sent {
            backend: "command",
            replaced: false,
        })
    }
}
