//! The built-in checks (ids in `docs/doctor.md`).

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use stems_config::{Codebase, PortRef, Requirement, ScriptSource, Stem, StemRuntime, StemType};
use stems_core::overlays::{ExistingFile, OverlayStatus, status_of};
use stems_core::tools::SystemTools;
use stems_core::{ErrorCode, validate::check_requirement};
use stems_runtime::os;
use stems_runtime::{ComposeOptions, ComposeRuntime, Orphan, OrphanScope};

use super::{
    Check, CheckResult, CheckStatus, DaemonProbe, DoctorCtx, MIN_FREE_BYTES, runtime_error,
};
use crate::lock::{self, LockState};
use crate::state::StateFile;

/// The registry for `cx`'s workspace.
pub(super) fn registry(cx: &DoctorCtx) -> Vec<Box<dyn Check>> {
    let mut v: Vec<Box<dyn Check>> = vec![Box::new(ConfigCheck), Box::new(DaemonCheck)];
    let Some(ws) = cx.workspace() else {
        v.push(Box::new(DiskCheck {
            id: "disk.home".into(),
            path: cx.input.home.clone(),
        }));
        return v;
    };
    if cx.needs_docker() {
        v.push(Box::new(DockerCheck));
    }
    if cx.needs_compose() {
        v.push(Box::new(ComposeCheck));
    }
    for (tool, req) in &ws.requires {
        v.push(Box::new(RequiresCheck {
            tool: tool.clone(),
            req: req.clone(),
        }));
    }
    for stem in ws.stems() {
        if stem.kind() == StemType::External {
            continue;
        }
        for (name, port) in declared_ports(stem, cx.state.as_ref()) {
            v.push(Box::new(PortCheck {
                stem: stem.name.clone(),
                name,
                port,
            }));
        }
    }
    for stem in ws.stems() {
        if let Some(cb) = &stem.codebase {
            v.push(Box::new(CodebaseCheck {
                stem: stem.name.clone(),
                codebase: cb.clone(),
                has_build: stem.scripts.contains_key("build"),
            }));
        }
    }
    for stem in ws.stems() {
        for (name, script) in &stem.scripts {
            if let ScriptSource::File(f) = &script.source {
                v.push(Box::new(ScriptCheck {
                    id: format!("scripts.{}.{name}", stem.name),
                    file: f.clone(),
                }));
            }
        }
    }
    for (name, script) in &ws.scripts {
        if let ScriptSource::File(f) = &script.source {
            v.push(Box::new(ScriptCheck {
                id: format!("scripts.workspace.{name}"),
                file: f.clone(),
            }));
        }
    }
    let ledger = cx
        .state
        .as_ref()
        .map(|s| s.overlays.clone())
        .unwrap_or_default();
    for stem in ledger.keys() {
        v.push(Box::new(OverlaysCheck { stem: stem.clone() }));
    }
    v.push(Box::new(OrphansCheck));
    v.push(Box::new(DiskCheck {
        id: "disk.home".into(),
        path: cx.input.home.clone(),
    }));
    let mut seen = BTreeSet::new();
    for stem in ws.stems() {
        if let Some(cb) = &stem.codebase
            && seen.insert(cb.path().to_path_buf())
        {
            v.push(Box::new(DiskCheck {
                id: format!("disk.codebase.{}", stem.name),
                path: cb.path().to_path_buf(),
            }));
        }
    }
    for stem in ws.stems() {
        if !stem.watch.is_empty() {
            v.push(Box::new(HotReloadCheck {
                stem: stem.name.clone(),
            }));
        }
    }
    v
}

/// Run blocking work off the async thread (so the check timeout can fire).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

// ---------------------------------------------------------------------------
// config, daemon
// ---------------------------------------------------------------------------

struct ConfigCheck;

#[async_trait]
impl Check for ConfigCheck {
    fn id(&self) -> String {
        "config".into()
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        if let Some(first) = cx.config_errors.first() {
            let n = cx.config_errors.len();
            let mut r = CheckResult::from_error("config", CheckStatus::Fail, first);
            if n > 1 {
                r.message = format!("{} (and {} more; see `stems validate`)", r.message, n - 1);
            }
            r.details = json!({ "errors": cx.config_errors });
            return vec![r];
        }
        let ws = cx.workspace().expect("no errors means a workspace");
        vec![
            CheckResult::ok(
                "config",
                format!(
                    "{} is valid ({} stem{})",
                    ws.config_file
                        .file_name()
                        .map_or("stems.yaml".into(), |n| n.to_string_lossy()),
                    ws.stems().count(),
                    if ws.stems().count() == 1 { "" } else { "s" }
                ),
            )
            .details(json!({ "workspace": ws.name, "config_file": ws.config_file })),
        ]
    }
}

struct DaemonCheck;

#[async_trait]
impl Check for DaemonCheck {
    fn id(&self) -> String {
        "daemon".into()
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let paths = &cx.input.paths;
        let r = match &cx.input.daemon {
            DaemonProbe::Running(st) => CheckResult::ok(
                "daemon",
                format!(
                    "running (pid {}, stems {}, up {}s)",
                    st.info.pid, st.info.version, st.info.uptime_s
                ),
            )
            .details(json!({
                "running": true,
                "pid": st.info.pid,
                "version": st.info.version,
                "api_version": st.info.api_version,
                "uptime_s": st.info.uptime_s,
                "workspace_name": st.workspace_name,
                "compatible": true,
            })),
            DaemonProbe::Incompatible(e) => {
                let mut r = CheckResult::from_error("daemon", CheckStatus::Fail, e);
                if let serde_json::Value::Object(m) = &mut r.details {
                    m.insert("compatible".into(), json!(false));
                }
                r
            }
            DaemonProbe::Unreachable(e) => CheckResult::from_error("daemon", CheckStatus::Fail, e),
            DaemonProbe::NotRunning => match lock::probe(paths) {
                LockState::Stale { pid } => CheckResult::fail(
                    "daemon",
                    format!("stale lock: the daemon (pid {pid}) exited without cleaning up"),
                )
                .hint("`stems doctor --fix` removes it (starting the daemon also reclaims it)")
                .code(ErrorCode::DaemonNotRunning)
                .fixable()
                .details(json!({
                    "running": false,
                    "lock_state": "stale",
                    "pid": pid,
                    "lock": paths.lock,
                    "socket": paths.socket,
                    "stale_socket": paths.socket.exists(),
                })),
                LockState::Held { pid, version } => CheckResult::fail(
                    "daemon",
                    format!(
                        "a daemon (pid {pid}, stems {version}) holds the lock but does not answer"
                    ),
                )
                .hint(format!(
                    "see its log {} or stop it with `stems daemon stop`",
                    paths.log_path().display()
                ))
                .code(ErrorCode::DaemonNotRunning)
                .details(json!({ "running": false, "lock_state": "held", "pid": pid })),
                LockState::Free if paths.socket.exists() => {
                    CheckResult::fail("daemon", "stale socket: no daemon listens on it")
                        .hint("`stems doctor --fix` removes it")
                        .code(ErrorCode::DaemonNotRunning)
                        .fixable()
                        .details(json!({
                            "running": false,
                            "lock_state": "free",
                            "socket": paths.socket,
                            "stale_socket": true,
                        }))
                }
                LockState::Free => {
                    CheckResult::ok("daemon", "not running (`stems up` starts it on demand)")
                        .details(json!({ "running": false, "lock_state": "free" }))
                }
            },
        };
        vec![r]
    }
}

// ---------------------------------------------------------------------------
// docker, compose
// ---------------------------------------------------------------------------

struct DockerCheck;

#[async_trait]
impl Check for DockerCheck {
    fn id(&self) -> String {
        "docker.reachable".into()
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let docker = match cx.docker().await {
            Ok(d) => d,
            Err(e) => {
                return vec![CheckResult::from_error(
                    "docker.reachable",
                    CheckStatus::Fail,
                    &e,
                )];
            }
        };
        let mut out = vec![
            CheckResult::ok(
                "docker.reachable",
                format!("reachable at {}", docker.host()),
            )
            .details(json!({ "host": docker.host() })),
        ];
        out.push(match docker.server_version().await {
            Ok(v) => CheckResult::ok("docker.version", format!("Docker Engine {v}"))
                .details(json!({ "version": v })),
            Err(e) => {
                CheckResult::from_error("docker.version", CheckStatus::Fail, &runtime_error(e))
            }
        });
        out
    }
}

struct ComposeCheck;

#[async_trait]
impl Check for ComposeCheck {
    fn id(&self) -> String {
        "compose.version".into()
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        // An unreachable Docker is `docker.reachable`'s failure.
        let Ok(docker) = cx.docker().await else {
            return Vec::new();
        };
        let ws_name = cx.workspace().map(|w| w.name.clone()).unwrap_or_default();
        let rt = ComposeRuntime::new(
            docker,
            ComposeOptions::new(ws_name, cx.input.paths.dir.join("compose")),
        );
        vec![match rt.ensure_version().await {
            Ok(v) => CheckResult::ok("compose.version", format!("docker compose {v}"))
                .details(json!({ "version": v.to_string() })),
            Err(e) => {
                CheckResult::from_error("compose.version", CheckStatus::Fail, &runtime_error(e))
            }
        }]
    }
}

// ---------------------------------------------------------------------------
// requires
// ---------------------------------------------------------------------------

struct RequiresCheck {
    tool: String,
    req: Requirement,
}

#[async_trait]
impl Check for RequiresCheck {
    fn id(&self) -> String {
        format!("requires.{}", self.tool)
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let (tool, req) = (self.tool.clone(), self.req.clone());
        let id = self.id();
        let range = req.version().to_string();
        let r = blocking(move || check_requirement(&tool, &req, &mut SystemTools::new())).await;
        vec![match r {
            Some(Ok(v)) => CheckResult::ok(&id, format!("{} {v} satisfies {range}", self.tool))
                .details(json!({ "tool": self.tool, "required": range, "found": v.to_string() })),
            Some(Err(e)) => CheckResult::from_error(&id, CheckStatus::Fail, &e),
            None => CheckResult::fail(&id, "the version probe panicked"),
        }]
    }
}

// ---------------------------------------------------------------------------
// ports
// ---------------------------------------------------------------------------

/// `(port name, host port)` of `stem`: fixed ports, plus `auto` ports
/// recorded in state.
fn declared_ports(stem: &Stem, state: Option<&StateFile>) -> Vec<(String, u16)> {
    let mut out: Vec<(String, u16)> = stem
        .ports
        .iter()
        .filter_map(|p| match p.port {
            PortRef::Fixed(n) => Some((p.name.clone(), n)),
            PortRef::Auto => None,
        })
        .collect();
    if let Some(r) = state.and_then(|f| f.stems.get(&stem.name)) {
        for p in r.ports.iter().filter(|p| p.auto) {
            if !out.iter().any(|(n, _)| *n == p.name) {
                out.push((p.name.clone(), p.port));
            }
        }
    }
    out
}

struct PortCheck {
    stem: String,
    name: String,
    port: u16,
}

#[async_trait]
impl Check for PortCheck {
    fn id(&self) -> String {
        format!("ports.{}.{}", self.stem, self.name)
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let id = self.id();
        let port = self.port;
        let listeners = blocking(move || {
            os::listeners_on_port(port)
                .into_iter()
                .map(|l| {
                    let cmd = os::command_line(l.pid).unwrap_or_else(|| l.command.clone());
                    (l.pid, os::process_group(l.pid), cmd)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if listeners.is_empty() {
            return vec![
                CheckResult::ok(&id, format!("port {port} is free"))
                    .details(json!({ "port": port })),
            ];
        }
        // Owned by a recorded stem (this one is fine, another is a clash).
        let owner = |pid: i32, pgid: Option<i32>| -> Option<String> {
            let st = cx.state.as_ref()?;
            st.stems
                .iter()
                .find(|(_, r)| {
                    r.is_alive()
                        && (r.pid == pid
                            || r.pgid == pid
                            || pgid.is_some_and(|g| g == r.pgid)
                            || (r.container_id.is_some() && r.ports.iter().any(|p| p.port == port)))
                })
                .map(|(n, _)| n.clone())
        };
        let (pid, pgid, command) = listeners[0].clone();
        match owner(pid, pgid) {
            Some(s) if s == self.stem => vec![
                CheckResult::ok(&id, format!("port {port} is in use by {s} (pid {pid})"))
                    .details(json!({ "port": port, "pid": pid, "stem": s })),
            ],
            Some(s) => vec![
                CheckResult::warn(
                    &id,
                    format!("port {port} is held by stem {s} (pid {pid}), not {}", self.stem),
                )
                .hint(format!("two stems use port {port}: change one in stems.local.yaml"))
                .code(ErrorCode::PortInUse)
                .details(json!({ "port": port, "pid": pid, "stem": s, "command": command })),
            ],
            None if cx.input.daemon.pid() == Some(pid) => vec![
                CheckResult::warn(&id, format!("port {port} is held by the stems daemon (pid {pid})"))
                    .code(ErrorCode::PortInUse)
                    .details(json!({ "port": port, "pid": pid, "command": command })),
            ],
            None => vec![
                CheckResult::warn(
                    &id,
                    format!("port {port} is held by pid {pid} ({command}), which stems did not start"),
                )
                .hint(format!(
                    "stop that process or change stems.{}.ports in stems.local.yaml; if it is a leftover of {}, `stems doctor --fix` (with --kill-foreign for anything else) kills it",
                    self.stem, self.stem
                ))
                .code(ErrorCode::PortInUse)
                .details(json!({
                    "port": port,
                    "pid": pid,
                    "pgid": pgid,
                    "command": command,
                    "listeners": listeners.iter().map(|(p, _, c)| json!({"pid": p, "command": c})).collect::<Vec<_>>(),
                })),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// codebases, scripts
// ---------------------------------------------------------------------------

/// `git -C dir <args>` stdout (trimmed), `None` if it fails. Killed when the
/// check times out.
async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

struct CodebaseCheck {
    stem: String,
    codebase: Codebase,
    has_build: bool,
}

#[async_trait]
impl Check for CodebaseCheck {
    fn id(&self) -> String {
        format!("codebase.{}", self.stem)
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let id = self.id();
        let path = self.codebase.path();
        let git_url = match &self.codebase {
            Codebase::Git { url, .. } => Some(url.clone()),
            Codebase::Local { .. } => None,
        };
        let details = json!({ "path": path, "git": git_url });
        if !path.is_dir() {
            return vec![match &git_url {
                Some(url) => CheckResult::warn(&id, format!("{} is not cloned yet", path.display()))
                    .hint(format!("`stems repos sync` (or `stems up`) clones {url}"))
                    .details(details),
                None => CheckResult::fail(&id, format!("codebase {} does not exist", path.display()))
                    .hint(format!(
                        "create it, or point stems.{}.codebase at the right directory (stems.local.yaml)",
                        self.stem
                    ))
                    .code(ErrorCode::CodebaseNotFound)
                    .details(details),
            }];
        }
        let top = git(path, &["rev-parse", "--show-toplevel"]).await;
        let Some(_top) = top else {
            return vec![match git_url {
                Some(_) => CheckResult::warn(
                    &id,
                    format!("{} exists but is not a git repository", path.display()),
                )
                .hint("move it aside so `stems repos sync` can clone it")
                .details(details),
                None => CheckResult::ok(
                    &id,
                    format!("{} exists (not a git repository)", path.display()),
                )
                .details(details),
            }];
        };
        let branch = git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap_or_else(|| "?".into());
        if self.has_build {
            let dirty = git(path, &["status", "--porcelain", "--", "."])
                .await
                .is_some_and(|s| !s.is_empty());
            if dirty {
                return vec![
                    CheckResult::warn(
                        &id,
                        format!(
                            "{} has uncommitted changes and a `build` script",
                            path.display()
                        ),
                    )
                    .hint("builds from a dirty tree are not reproducible; commit or stash first")
                    .details(json!({ "path": path, "branch": branch, "dirty": true })),
                ];
            }
        }
        vec![
            CheckResult::ok(&id, format!("{} (git, {branch})", path.display()))
                .details(json!({ "path": path, "branch": branch })),
        ]
    }
}

struct ScriptCheck {
    id: String,
    file: PathBuf,
}

#[async_trait]
impl Check for ScriptCheck {
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let d = json!({ "file": self.file });
        let r = match std::fs::metadata(&self.file) {
            Err(_) => {
                CheckResult::fail(&self.id, format!("{} does not exist", self.file.display()))
                    .hint("create the script or fix its `file:` path")
                    .code(ErrorCode::ScriptNotFound)
                    .details(d)
            }
            Ok(m) if !m.is_file() => {
                CheckResult::fail(&self.id, format!("{} is not a file", self.file.display()))
                    .code(ErrorCode::ScriptNotFound)
                    .details(d)
            }
            Ok(m) if m.permissions().mode() & 0o111 == 0 => CheckResult::warn(
                &self.id,
                format!("{} is not executable", self.file.display()),
            )
            .hint(format!("chmod +x {}", self.file.display()))
            .details(d),
            Ok(_) => {
                CheckResult::ok(&self.id, format!("{} exists", self.file.display())).details(d)
            }
        };
        vec![r]
    }
}

// ---------------------------------------------------------------------------
// overlays, orphans
// ---------------------------------------------------------------------------

/// Is `stem` recorded as running (and alive) in `state`?
pub(super) fn stem_running(state: Option<&StateFile>, stem: &str) -> bool {
    state
        .and_then(|s| s.stems.get(stem))
        .is_some_and(|r| r.is_alive())
}

struct OverlaysCheck {
    stem: String,
}

#[async_trait]
impl Check for OverlaysCheck {
    fn id(&self) -> String {
        format!("overlays.{}", self.stem)
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let id = self.id();
        let recs = cx
            .state
            .as_ref()
            .and_then(|s| s.overlays.get(&self.stem))
            .cloned()
            .unwrap_or_default();
        let running = stem_running(cx.state.as_ref(), &self.stem);
        let mut stale = Vec::new();
        let mut modified = Vec::new();
        let mut listed = Vec::new();
        for r in &recs {
            let status = match ExistingFile::probe(&r.dest) {
                Ok(x) => status_of(r, &x),
                Err(_) => OverlayStatus::Modified,
            };
            let is_stale = !running && !r.keep;
            listed.push(
                json!({ "dest": r.dest, "status": status, "keep": r.keep, "stale": is_stale }),
            );
            if is_stale {
                stale.push(r.dest.display().to_string());
            } else if status == OverlayStatus::Modified {
                modified.push(r.dest.display().to_string());
            }
        }
        let details = json!({ "running": running, "overlays": listed });
        let r = if !stale.is_empty() {
            CheckResult::warn(
                &id,
                format!(
                    "stale overlay{}: {} (recorded, but {} is not running)",
                    if stale.len() == 1 { "" } else { "s" },
                    stale.join(", "),
                    self.stem
                ),
            )
            .hint("`stems doctor --fix` removes unchanged files and forgets the records (modified files are left in place)")
            .fixable()
            .details(details)
        } else if !modified.is_empty() {
            CheckResult::warn(
                &id,
                format!("modified since stems wrote {}: {}", if modified.len() == 1 { "it" } else { "them" }, modified.join(", ")),
            )
            .hint("stems leaves modified overlays in place when the stem stops; delete them yourself if the change is not needed")
            .details(details)
        } else {
            CheckResult::ok(
                &id,
                format!(
                    "{} overlay{} recorded{}",
                    recs.len(),
                    if recs.len() == 1 { "" } else { "s" },
                    if running {
                        ", stem running"
                    } else {
                        " (keep: true)"
                    }
                ),
            )
            .details(details)
        };
        vec![r]
    }
}

/// Process orphans (11's scan) plus, when Docker is needed and reachable,
/// container orphans. The second value is set when Docker was not scanned.
pub(super) async fn scan_orphans(cx: &Arc<DoctorCtx>) -> (Vec<Orphan>, Option<String>) {
    let Some(ws) = cx.workspace() else {
        return (Vec::new(), None);
    };
    let mut ignore = cx.input.ignore_pids.clone();
    ignore.extend(cx.input.daemon.pid());
    let ws2 = ws.clone();
    let state = cx.state.clone();
    let mut found = blocking(move || crate::orphans::scan(&ws2, state.as_ref(), &ignore))
        .await
        .unwrap_or_default();
    let mut skipped = None;
    if cx.needs_docker() {
        match cx.docker().await {
            Ok(d) => {
                let known = cx
                    .state
                    .as_ref()
                    .map(|s| s.stems.values().map(|r| r.adopt_record()).collect())
                    .unwrap_or_default();
                let scope = OrphanScope {
                    workspace: ws.name.clone(),
                    known,
                };
                match d.scan_container_orphans(&scope).await {
                    Ok(c) => found.extend(c.into_iter().map(Orphan::from)),
                    Err(e) => skipped = Some(e.to_string()),
                }
                // Containers of stems-owned compose projects (15); they
                // carry no `stems.*` labels, so nothing is listed twice.
                if cx.needs_compose() {
                    use stems_runtime::Runtime as _;
                    let rt = ComposeRuntime::new(
                        d.clone(),
                        ComposeOptions::new(ws.name.clone(), cx.input.paths.dir.join("compose")),
                    );
                    found.extend(rt.scan_orphans(&scope).await);
                }
            }
            Err(e) => skipped = Some(e.message),
        }
    }
    (found, skipped)
}

struct OrphansCheck;

#[async_trait]
impl Check for OrphansCheck {
    fn id(&self) -> String {
        "orphans".into()
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let (found, skipped) = scan_orphans(&cx).await;
        let note = skipped
            .as_ref()
            .map(|_| " (containers not scanned: Docker is not reachable)")
            .unwrap_or("");
        if found.is_empty() {
            return vec![
                CheckResult::ok("orphans", format!("no orphans{note}"))
                    .details(json!({ "orphans": [], "containers_scanned": skipped.is_none() })),
            ];
        }
        let fixable = found.iter().any(|o| o.matches_start_command);
        let list: Vec<String> = found
            .iter()
            .map(|o| match (o.pid, o.port) {
                (Some(pid), Some(port)) => format!(
                    "pid {pid} on port {port} ({})",
                    o.stem.as_deref().unwrap_or("-")
                ),
                _ => o.command.clone(),
            })
            .collect();
        let mut r = CheckResult::warn(
            "orphans",
            format!(
                "{} orphan{}: {}{note}",
                found.len(),
                if found.len() == 1 { "" } else { "s" },
                list.join("; ")
            ),
        )
        .hint("`stems doctor --fix` kills processes that look like their stem's start command and removes stems-labelled containers; `--kill-foreign` also kills the rest; `stems doctor --orphans` to decide one by one")
        .code(ErrorCode::OrphansFound)
        .details(json!({ "orphans": found, "containers_scanned": skipped.is_none() }));
        r.fixable = fixable;
        vec![r]
    }
}

// ---------------------------------------------------------------------------
// disk, hot reload
// ---------------------------------------------------------------------------

/// Free bytes on the filesystem holding `path` (or its nearest existing ancestor).
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let st = nix::sys::statvfs::statvfs(p).ok()?;
    #[allow(clippy::useless_conversion)]
    Some(u64::from(st.blocks_available()) * u64::from(st.fragment_size()))
}

fn human_bytes(n: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    const MB: f64 = (1u64 << 20) as f64;
    if n as f64 >= GB {
        format!("{:.1} GB", n as f64 / GB)
    } else {
        format!("{:.0} MB", n as f64 / MB)
    }
}

struct DiskCheck {
    id: String,
    path: PathBuf,
}

#[async_trait]
impl Check for DiskCheck {
    fn id(&self) -> String {
        self.id.clone()
    }
    async fn run(&self, _cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let path = self.path.clone();
        let free = blocking(move || free_bytes(&path)).await.flatten();
        let d = |free: Option<u64>| json!({ "path": self.path, "free_bytes": free });
        vec![match free {
            None => CheckResult::warn(&self.id, format!("cannot read free space of {}", self.path.display()))
                .details(d(None)),
            Some(f) if f < MIN_FREE_BYTES => CheckResult::warn(
                &self.id,
                format!("only {} free under {}", human_bytes(f), self.path.display()),
            )
            .hint("free some disk space (logs, docker images: `docker system prune`); stems needs at least 1 GB")
            .details(d(Some(f))),
            Some(f) => CheckResult::ok(
                &self.id,
                format!("{} free under {}", human_bytes(f), self.path.display()),
            )
            .details(d(Some(f))),
        }]
    }
}

static HOT_RELOAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[\s/;&|(])(vite|next dev|nodemon|cargo[\s-]watch|air|uvicorn\b.*--reload|flask run.*--reload|webpack\b.*--watch)(?:$|[\s;&|)])",
    )
    .expect("valid hot-reload regex")
});

/// The hot-reloading dev server in `command` (`vite`, `next dev`, `nodemon`,
/// `cargo watch`, `air`, `uvicorn --reload`, `flask run --reload`,
/// `webpack --watch`), if any.
pub fn hot_reload_command(command: &str) -> Option<String> {
    HOT_RELOAD
        .captures(command)
        .and_then(|c| c.get(1))
        .map(|m| {
            m.as_str()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        })
}

/// Does a `watch` cover the sources a dev server reloads (`src/**` or `**`)?
pub fn watch_overlaps_sources(paths: &[String]) -> bool {
    paths.iter().any(|p| {
        let p = p.trim_start_matches("./");
        p == "**" || p == "**/*" || p == "src" || p.starts_with("src/")
    })
}

/// The start command of `stem` as text (process `command`/`scripts.start`,
/// docker `command`).
fn start_command(stem: &Stem) -> Option<String> {
    match &stem.runtime {
        StemRuntime::Process(_) => crate::supervisor::actor::start_script(stem)
            .ok()
            .map(|(_, s, _)| s),
        StemRuntime::Docker(d) => d.command.as_ref().map(|c| c.join(" ")),
        _ => None,
    }
}

struct HotReloadCheck {
    stem: String,
}

#[async_trait]
impl Check for HotReloadCheck {
    fn id(&self) -> String {
        format!("watch.hot-reload.{}", self.stem)
    }
    async fn run(&self, cx: Arc<DoctorCtx>) -> Vec<CheckResult> {
        let id = self.id();
        let Some(stem) = cx.workspace().and_then(|w| w.stem(&self.stem)) else {
            return Vec::new();
        };
        let cmd = start_command(stem).unwrap_or_default();
        let server = hot_reload_command(&cmd);
        let overlapping: Vec<&String> = stem
            .watch
            .iter()
            .filter(|w| watch_overlaps_sources(&w.paths))
            .flat_map(|w| w.paths.iter())
            .collect();
        vec![match server {
            Some(s) if !overlapping.is_empty() => CheckResult::warn(
                &id,
                format!(
                    "`{s}` already hot-reloads, and a watch on {} restarts {} too",
                    overlapping.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(", "),
                    self.stem
                ),
            )
            .hint(format!(
                "remove the watch from stems.{} (or narrow it to files the dev server does not reload, e.g. config), or `stems watch pause {}`",
                self.stem, self.stem
            ))
            .details(json!({ "command": cmd, "server": s, "paths": overlapping })),
            _ => CheckResult::ok(&id, "no overlap between the watch and a hot-reloading dev server")
                .details(json!({ "command": cmd, "server": server })),
        }]
    }
}
