//! The script runner (deliverable 16, `docs/scripts.md`): runs one
//! lifecycle, workspace or (17) custom script and reports how it ended.
//!
//! * **Source.** An inline `command:` runs as `<shell> -c <command> <name>
//!   <args…>`; a `file:` must exist inside the integration repo (checked
//!   again here, after `validate`: `SCRIPT_NOT_FOUND` /
//!   `SCRIPT_OUTSIDE_WORKSPACE`) and is executed directly when executable,
//!   else through `/bin/sh`.
//! * **Process.** Through the process [`Runtime`], so the script gets its own
//!   session / process group: a timeout or a cancellation kills the whole
//!   tree, and whatever the script left behind in its group is killed when it
//!   exits (scripts never leak daemons).
//! * **Environment.** The caller's environment (a stem's resolved env, see
//!   `supervisor::env`) plus `STEMS_STATE_DIR` (created), `STEMS_SCRIPT`,
//!   `STEMS_RUN_ID`, `STEMS_WORKSPACE`, then `RunContext::extra_env`.
//! * **Output.** Every line goes to the stem's log as `stream: script, tag:
//!   <name>` ([`OutputSink::script_writer`]); the last [`TAIL_LINES`] are
//!   returned for error details.
//! * **Record.** Events `script.started {script, actor}` and
//!   `script.finished {script, exit, signal, duration_ms, timed_out,
//!   cancelled}`, and a [`ScriptRun`] in `state.json`.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use stems_api::EventKind;
use stems_config::{Script, ScriptSource};
use stems_core::{Error, ErrorCode};
use stems_runtime::{ExitStatus, OutputEvent, ProcessSpec, Runtime, StartSpec};
use tokio_util::sync::CancellationToken;

use crate::events::{EventBus, EventDraft};
use crate::state::{ScriptRun, StateStore};
use crate::supervisor::OutputSink;

/// Lines of output kept for error details.
pub const TAIL_LINES: usize = 20;
/// Log "stem" that workspace scripts (`bootstrap`, `teardown`) write to.
pub const WORKSPACE_LOG_STEM: &str = "_workspace";
/// Grace between SIGTERM and SIGKILL when a script is killed.
const KILL_GRACE: Duration = Duration::from_millis(500);
/// How long output may keep flowing after the script's group is gone.
const DRAIN: Duration = Duration::from_secs(2);

/// A script to run and the name it is known by.
#[derive(Clone, Copy, Debug)]
pub struct ScriptRef<'a> {
    /// Name (`setup`, `seed`, `bootstrap`, a custom name…).
    pub name: &'a str,
    /// The resolved script.
    pub script: &'a Script,
}

/// How to run a script.
#[derive(Clone, Debug, Default)]
pub struct RunContext {
    /// Positional arguments (`$1…` of the script).
    pub args: Vec<String>,
    /// The base environment (for stem scripts: the stem's full env).
    pub env: BTreeMap<String, String>,
    /// Added last.
    pub extra_env: BTreeMap<String, String>,
    /// Working directory instead of the script's `cwd`.
    pub cwd_override: Option<PathBuf>,
    /// Shell for inline commands (default `/bin/sh`).
    pub shell: Option<String>,
    /// Who asked (`cli:<user>`, `daemon`, …).
    pub actor: String,
    /// Cancelling kills the script's process group.
    pub cancel: Option<CancellationToken>,
    /// Extra fields for the `script.started` / `script.finished` events'
    /// `data` (17: `run_id`, `attempt`).
    pub event_extra: serde_json::Map<String, serde_json::Value>,
    /// Health `command` probes (21): no `script.*` events, no run record,
    /// no log output (the caller logs the tail when it matters).
    pub quiet: bool,
}

/// How a script run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptResult {
    /// Stem (`None` for workspace scripts).
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
    /// Exit status of the script's leader process.
    pub exit: ExitStatus,
    /// Wall time.
    pub duration_ms: u64,
    /// The last [`TAIL_LINES`] lines of output.
    pub tail: Vec<String>,
    /// Killed because it ran longer than its `timeout`.
    pub timed_out: bool,
    /// Killed because it was cancelled (a stop arrived).
    pub cancelled: bool,
    /// The script's `timeout`.
    pub timeout: Option<Duration>,
}

impl ScriptResult {
    /// Exited 0, in time, not cancelled.
    pub fn success(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit.success()
    }

    fn how(&self) -> String {
        if self.timed_out {
            let t = self.timeout.map_or(0.0, |d| d.as_secs_f64());
            return format!("timed out after {t}s and was killed");
        }
        if self.cancelled {
            return "was cancelled".into();
        }
        match (self.exit.code, self.exit.signal) {
            (Some(c), _) => format!("exited with status {c}"),
            (None, Some(s)) => format!("was killed by signal {s}"),
            _ => "ended with an unknown status".into(),
        }
    }

    fn details(&self) -> serde_json::Value {
        json!({
            "stem": self.stem,
            "script": self.script,
            "exit": self.exit.code,
            "signal": self.exit.signal,
            "duration_ms": self.duration_ms,
            "timed_out": self.timed_out,
            "reason": if self.timed_out { "timeout" } else if self.cancelled { "cancelled" } else { "exit" },
            "tail": self.tail,
        })
    }

    fn logs_hint(&self) -> String {
        let stem = self.stem.as_deref().unwrap_or(WORKSPACE_LOG_STEM);
        format!("`stems logs {stem} --script {}`", self.script)
    }

    /// `SCRIPT_FAILED` (exit code, `reason` and last lines in `details`).
    pub fn error(&self) -> Error {
        let what = match &self.stem {
            Some(s) => format!("script `{}` of `{s}`", self.script),
            None => format!("workspace script `{}`", self.script),
        };
        Error::new(ErrorCode::ScriptFailed, format!("{what} {}", self.how()))
            .with_hint(format!(
                "see the last lines in details, or {}{}",
                self.logs_hint(),
                if self.timed_out {
                    "; raise its `timeout:` if it is just slow"
                } else {
                    ""
                }
            ))
            .with_details(self.details())
    }

    /// `SETUP_FAILED` for a failed `setup` (or workspace `bootstrap`).
    pub fn setup_error(&self) -> Error {
        let (msg, hint) = match &self.stem {
            Some(s) => (
                format!(
                    "setup of `{s}` failed ({}); the stem was not started",
                    self.how()
                ),
                format!(
                    "fix the script and run `stems up {s}` again; {} shows its output",
                    self.logs_hint()
                ),
            ),
            None => (
                format!(
                    "workspace `{}` failed ({}); no stem was started",
                    self.script,
                    self.how()
                ),
                format!(
                    "fix the script and run `stems up` again; {} shows its output",
                    self.logs_hint()
                ),
            ),
        };
        Error::new(ErrorCode::SetupFailed, msg)
            .with_hint(hint)
            .with_details(self.details())
    }
}

/// Runs scripts. One per supervisor.
pub struct ScriptRunner {
    runtime: Arc<dyn Runtime>,
    events: Arc<EventBus>,
    sink: Arc<dyn OutputSink>,
    state: Option<Arc<StateStore>>,
    data_dir: PathBuf,
    run_id: String,
}

/// `path` resolved for execution: it must be a file inside `ws_root`.
pub fn resolve_file(ws_root: &Path, path: &Path) -> Result<PathBuf, Error> {
    let root = ws_root
        .canonicalize()
        .unwrap_or_else(|_| ws_root.to_path_buf());
    let shown = path
        .strip_prefix(ws_root)
        .unwrap_or(path)
        .display()
        .to_string();
    let outside = || {
        Error::new(
            ErrorCode::ScriptOutsideWorkspace,
            format!(
                "script file `{}` is outside the integration repo {}",
                path.display(),
                ws_root.display()
            ),
        )
        .with_hint("scripts live in the integration repo (FR-WS-2): move the script under it and reference it relative to stems.yaml")
        .with_details(json!({ "file": path }))
    };
    if !path.starts_with(ws_root) {
        return Err(outside());
    }
    match path.canonicalize() {
        Ok(c) if !c.starts_with(&root) => Err(outside()),
        Ok(c) if c.is_file() => Ok(c),
        _ => Err(Error::new(
            ErrorCode::ScriptNotFound,
            format!("script file `{shown}` does not exist in the integration repo"),
        )
        .with_hint(format!(
            "create `{shown}` in {} or fix the `file:` path (it is relative to the directory of stems.yaml)",
            ws_root.display()
        ))
        .with_details(json!({ "file": path }))),
    }
}

/// The text a script's stamp hashes: the inline command, or the file's contents.
pub fn script_text(ws_root: &Path, script: &Script) -> Result<String, Error> {
    match &script.source {
        ScriptSource::Command(c) => Ok(c.clone()),
        ScriptSource::File(f) => {
            let p = resolve_file(ws_root, f)?;
            std::fs::read_to_string(&p).map_err(|e| {
                Error::new(
                    ErrorCode::ScriptNotFound,
                    format!("cannot read script file {}: {e}", p.display()),
                )
                .with_details(json!({ "file": p }))
            })
        }
    }
}

/// Merge `extra` into the object `data`.
fn extend(data: &mut serde_json::Value, extra: &serde_json::Map<String, serde_json::Value>) {
    if let Some(m) = data.as_object_mut() {
        m.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl ScriptRunner {
    /// A runner starting scripts through `runtime`; `data_dir` is the
    /// daemon's `<home>/<ws-hash>` directory (state dirs live below it).
    pub fn new(
        runtime: Arc<dyn Runtime>,
        events: Arc<EventBus>,
        sink: Arc<dyn OutputSink>,
        state: Option<Arc<StateStore>>,
        data_dir: PathBuf,
        run_id: String,
    ) -> Self {
        Self {
            runtime,
            events,
            sink,
            state,
            data_dir,
            run_id,
        }
    }

    /// `STEMS_STATE_DIR`: `<data dir>/stems/<stem>/` (workspace scripts:
    /// `<data dir>/workspace/`).
    pub fn state_dir(&self, stem: Option<&str>) -> PathBuf {
        match stem {
            Some(s) => self.data_dir.join("stems").join(s),
            None => self.data_dir.join("workspace"),
        }
    }

    fn spec(
        &self,
        ws_root: &Path,
        sref: ScriptRef<'_>,
        ctx: &RunContext,
        env: BTreeMap<String, String>,
        cwd: PathBuf,
    ) -> Result<ProcessSpec, Error> {
        let (command, args) = match &sref.script.source {
            ScriptSource::Command(c) => {
                let shell = ctx.shell.clone().unwrap_or_else(|| "/bin/sh".into());
                let mut args = vec!["-c".to_string(), c.clone(), sref.name.to_string()];
                args.extend(ctx.args.iter().cloned());
                (shell, args)
            }
            ScriptSource::File(f) => {
                let p = resolve_file(ws_root, f)?;
                let p = p.display().to_string();
                if is_executable(Path::new(&p)) {
                    (p, ctx.args.clone())
                } else {
                    let mut args = vec![p];
                    args.extend(ctx.args.iter().cloned());
                    ("/bin/sh".to_string(), args)
                }
            }
        };
        Ok(ProcessSpec {
            command,
            args,
            shell: false,
            cwd,
            env,
            clear_env: true,
        })
    }

    /// Run `sref` for `stem` (`None`: a workspace script) of the workspace
    /// rooted at `ws_root`. `Err` only when the script cannot be started
    /// (missing file, bad cwd, spawn failure); a non-zero exit, a timeout or
    /// a cancellation is an `Ok` result (see [`ScriptResult::success`]).
    pub async fn run(
        &self,
        ws_root: &Path,
        stem: Option<&str>,
        sref: ScriptRef<'_>,
        ctx: RunContext,
    ) -> Result<ScriptResult, Error> {
        let name = sref.name;
        let state_dir = self.state_dir(stem);
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| Error::internal(format!("cannot create {}: {e}", state_dir.display())))?;
        let mut env = ctx.env.clone();
        if stem.is_none() {
            env.remove("STEMS_STEM");
            env.remove("STEMS_CODEBASE");
        }
        env.insert("STEMS_STATE_DIR".into(), state_dir.display().to_string());
        env.insert("STEMS_SCRIPT".into(), name.to_string());
        env.insert("STEMS_RUN_ID".into(), self.run_id.clone());
        env.entry("STEMS_WORKSPACE".into())
            .or_insert_with(|| ws_root.display().to_string());
        env.extend(ctx.extra_env.clone());
        let cwd = ctx
            .cwd_override
            .clone()
            .unwrap_or_else(|| sref.script.cwd.clone());
        let spec = self.spec(ws_root, sref, &ctx, env, cwd.clone())?;
        let owner = stem.unwrap_or(WORKSPACE_LOG_STEM);
        let what = match stem {
            Some(s) => format!("script `{name}` of `{s}`"),
            None => format!("workspace script `{name}`"),
        };
        if !cwd.is_dir() {
            return Err(Error::new(
                ErrorCode::ScriptFailed,
                format!(
                    "cannot run {what}: its working directory {} does not exist",
                    cwd.display()
                ),
            )
            .with_hint("fix the script's `cwd:` (or the stem's codebase)")
            .with_details(json!({ "stem": stem, "script": name, "cwd": cwd })));
        }

        let started = Utc::now();
        let t0 = Instant::now();
        let mut data = json!({
            "script": name,
            "actor": ctx.actor,
            "args": ctx.args,
        });
        extend(&mut data, &ctx.event_extra);
        let mut ev = EventDraft::new(EventKind::SCRIPT_STARTED, &ctx.actor).data(data);
        if let Some(s) = stem {
            ev = ev.stem(s);
        }
        if !ctx.quiet {
            self.events.emit(ev);
        }

        let handle = match self.runtime.start(&StartSpec::Process(spec)).await {
            Ok(h) => h,
            Err(e) => {
                let err = Error::new(ErrorCode::ScriptFailed, format!("cannot start {what}: {e}"))
                    .with_hint("check the script's file (it must be readable) and `cwd`")
                    .with_details(json!({ "stem": stem, "script": name, "reason": "spawn" }));
                if !ctx.quiet {
                    self.finished(stem, name, &ctx, ExitStatus::UNKNOWN, t0, false, false);
                    self.record(stem, name, started, None, false);
                }
                return Err(err);
            }
        };

        // Output: log it (tagged) and keep the tail.
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let pump = self.runtime.output_stream(&handle).map(|mut stream| {
            let writer = if ctx.quiet {
                None
            } else {
                self.sink.script_writer(owner, name)
            };
            let tail = tail.clone();
            tokio::spawn(async move {
                while let Some(ev) = stream.recv_event().await {
                    let text = match ev {
                        OutputEvent::Line(l) => l.text,
                        OutputEvent::Dropped(n) => format!("[stems] dropped {n} lines"),
                    };
                    {
                        let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                        if t.len() == TAIL_LINES {
                            t.pop_front();
                        }
                        t.push_back(text.clone());
                    }
                    if let Some(w) = &writer {
                        w.line(text).await;
                    }
                }
            })
        });

        let timeout = sref.script.timeout.map(|d| d.as_duration());
        let cancel = ctx.cancel.clone().unwrap_or_default();
        let (mut timed_out, mut cancelled) = (false, false);
        let status = tokio::select! {
            s = self.runtime.wait(&handle) => s.unwrap_or(ExitStatus::UNKNOWN),
            () = async {
                match timeout {
                    Some(d) => tokio::time::sleep(d).await,
                    None => std::future::pending().await,
                }
            } => { timed_out = true; ExitStatus::UNKNOWN }
            () = cancel.cancelled() => { cancelled = true; ExitStatus::UNKNOWN }
        };
        // Kill the tree on timeout/cancel, and whatever a finished script
        // left running in its group.
        if let Err(e) = self.runtime.stop(&handle, KILL_GRACE).await {
            tracing::warn!(script = name, error = %e, "cleaning up a script's process group");
        }
        let status = if timed_out || cancelled {
            tokio::time::timeout(KILL_GRACE, self.runtime.wait(&handle))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(ExitStatus::UNKNOWN)
        } else {
            status
        };
        if let Some(p) = pump {
            let abort = p.abort_handle();
            if tokio::time::timeout(DRAIN, p).await.is_err() {
                abort.abort();
            }
        }
        self.runtime.release(&handle);
        if timed_out
            && !ctx.quiet
            && let Some(w) = self.sink.script_writer(owner, name)
        {
            let t = timeout.map_or(0.0, |d| d.as_secs_f64());
            w.line(format!(
                "[stems] script `{name}` timed out after {t}s; killed"
            ))
            .await;
        }

        if !ctx.quiet {
            self.finished(stem, name, &ctx, status, t0, timed_out, cancelled);
            self.record(stem, name, started, status.code, timed_out);
        }
        let tail = tail
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        Ok(ScriptResult {
            stem: stem.map(str::to_string),
            script: name.to_string(),
            exit: status,
            duration_ms: t0.elapsed().as_millis() as u64,
            tail,
            timed_out,
            cancelled,
            timeout,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn finished(
        &self,
        stem: Option<&str>,
        name: &str,
        ctx: &RunContext,
        status: ExitStatus,
        t0: Instant,
        timed_out: bool,
        cancelled: bool,
    ) {
        let mut data = json!({
            "script": name,
            "exit": status.code,
            "signal": status.signal,
            "duration_ms": t0.elapsed().as_millis() as u64,
            "timed_out": timed_out,
            "cancelled": cancelled,
            "ok": status.success() && !timed_out && !cancelled,
        });
        extend(&mut data, &ctx.event_extra);
        let mut ev = EventDraft::new(EventKind::SCRIPT_FINISHED, &ctx.actor).data(data);
        if let Some(s) = stem {
            ev = ev.stem(s);
        }
        self.events.emit(ev);
    }

    fn record(
        &self,
        stem: Option<&str>,
        name: &str,
        started: chrono::DateTime<Utc>,
        exit: Option<i32>,
        timed_out: bool,
    ) {
        if let Some(st) = &self.state {
            st.record_script_run(ScriptRun {
                stem: stem.map(str::to_string),
                script: name.to_string(),
                started,
                ended: Utc::now(),
                exit,
                timed_out,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(src: ScriptSource, cwd: &Path) -> Script {
        Script {
            source: src,
            description: None,
            args: Vec::new(),
            inputs: Vec::new(),
            requires: Vec::new(),
            timeout: None,
            retries: 0,
            concurrent: false,
            stamp_env: Vec::new(),
            cwd: cwd.to_path_buf(),
            cwd_set: false,
        }
    }

    #[test]
    fn file_resolution() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("ws");
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/ok.sh"), "echo ok").unwrap();
        std::fs::write(d.path().join("outside.sh"), "echo no").unwrap();
        assert!(resolve_file(&root, &root.join("scripts/ok.sh")).is_ok());
        assert_eq!(
            resolve_file(&root, &root.join("scripts/nope.sh"))
                .unwrap_err()
                .code,
            ErrorCode::ScriptNotFound
        );
        assert_eq!(
            resolve_file(&root, &d.path().join("outside.sh"))
                .unwrap_err()
                .code,
            ErrorCode::ScriptOutsideWorkspace
        );
        // A symlink inside the repo pointing out of it escapes too.
        std::os::unix::fs::symlink(d.path().join("outside.sh"), root.join("scripts/link.sh"))
            .unwrap();
        assert_eq!(
            resolve_file(&root, &root.join("scripts/link.sh"))
                .unwrap_err()
                .code,
            ErrorCode::ScriptOutsideWorkspace
        );
        let s = script(ScriptSource::File(root.join("scripts/ok.sh")), &root);
        assert_eq!(script_text(&root, &s).unwrap(), "echo ok");
        let s = script(ScriptSource::Command("echo hi".into()), &root);
        assert_eq!(script_text(&root, &s).unwrap(), "echo hi");
    }

    #[test]
    fn errors_carry_exit_and_tail() {
        let r = ScriptResult {
            stem: Some("api".into()),
            script: "setup".into(),
            exit: ExitStatus {
                code: Some(7),
                signal: None,
            },
            duration_ms: 12,
            tail: vec!["boom".into()],
            timed_out: false,
            cancelled: false,
            timeout: None,
        };
        assert!(!r.success());
        let e = r.setup_error();
        assert_eq!(e.code, ErrorCode::SetupFailed);
        assert_eq!(
            e.message,
            "setup of `api` failed (exited with status 7); the stem was not started"
        );
        let d = e.details;
        assert_eq!(d["exit"], 7);
        assert_eq!(d["tail"][0], "boom");
        let t = ScriptResult {
            timed_out: true,
            timeout: Some(Duration::from_secs(1)),
            exit: ExitStatus::UNKNOWN,
            ..r
        };
        let e = t.error();
        assert_eq!(e.code, ErrorCode::ScriptFailed);
        assert_eq!(e.details["reason"], "timeout");
        assert_eq!(
            e.message,
            "script `setup` of `api` timed out after 1s and was killed"
        );
    }

    #[tokio::test]
    async fn runs_with_env_cwd_and_tail() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("ws");
        let cb = d.path().join("code");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cb).unwrap();
        let runner = ScriptRunner::new(
            Arc::new(stems_runtime::ProcessRuntime::new()),
            Arc::new(EventBus::new(100)),
            Arc::new(crate::supervisor::NullSink),
            None,
            d.path().join("data"),
            "RUN1".into(),
        );
        let s = script(
            ScriptSource::Command(
                "pwd; echo \"$STEMS_STATE_DIR $STEMS_RUN_ID $STEMS_SCRIPT $1\"; exit 3".into(),
            ),
            &cb,
        );
        let mut env: BTreeMap<String, String> = std::env::vars().collect();
        env.insert("STEMS_STEM".into(), "api".into());
        let r = runner
            .run(
                &root,
                Some("api"),
                ScriptRef {
                    name: "setup",
                    script: &s,
                },
                RunContext {
                    args: vec!["arg1".into()],
                    env,
                    actor: "test".into(),
                    ..RunContext::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(r.exit.code, Some(3));
        let cb = cb.canonicalize().unwrap();
        assert_eq!(r.tail[0], cb.display().to_string());
        let sd = d.path().join("data/stems/api");
        assert!(sd.is_dir());
        assert_eq!(r.tail[1], format!("{} RUN1 setup arg1", sd.display()));
    }

    #[tokio::test]
    async fn timeout_kills_the_tree() {
        let d = tempfile::tempdir().unwrap();
        let runner = ScriptRunner::new(
            Arc::new(stems_runtime::ProcessRuntime::new()),
            Arc::new(EventBus::new(100)),
            Arc::new(crate::supervisor::NullSink),
            None,
            d.path().join("data"),
            "RUN1".into(),
        );
        let mut s = script(
            ScriptSource::Command("sleep 30 & echo $! > child.pid; wait".into()),
            d.path(),
        );
        s.timeout = Some("300ms".parse().unwrap());
        let t0 = Instant::now();
        let r = runner
            .run(
                d.path(),
                None,
                ScriptRef {
                    name: "slow",
                    script: &s,
                },
                RunContext {
                    env: std::env::vars().collect(),
                    actor: "test".into(),
                    ..RunContext::default()
                },
            )
            .await
            .unwrap();
        assert!(r.timed_out && !r.success());
        assert!(t0.elapsed() < Duration::from_secs(3));
        let pid: i32 = std::fs::read_to_string(d.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The background `sleep` died with its group (zombies count as dead).
        let pgid = stems_runtime::os::process_group(pid);
        let alive = || {
            pgid.is_some_and(|g| {
                stems_runtime::os::process_tree(g)
                    .iter()
                    .any(|p| p.pid == pid)
            })
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(), "background child survived the timeout");
    }
}
