//! Orphan scan (FR-CR-4, deliverable 11): things running on this
//! workspace's declared ports that the state file does not know about.
//!
//! * Processes: every TCP listener on a port declared by an enabled
//!   `process` stem (fixed ports, plus `auto` ports recorded in state) whose
//!   process group is not a recorded stem's and which is not the daemon (or
//!   the caller) itself.
//! * Containers: [`scan_containers`] (14/15): labelled containers and
//!   containers of stems-owned compose projects, when Docker is reachable.
//!
//! **`matches_start_command` heuristic.** The stem's start command
//! (`command`, else `scripts.start`) is reduced to its program and first
//! argument, skipping leading `VAR=value` assignments and `exec`/`env`
//! (`python3 app.py` → `python`, `app.py`). A listener matches when its
//! argv[0] basename has the same *normalised* name (lowercase, trailing
//! version digits and dots removed, so `python3`, `python3.13` and macOS's
//! framework `Python` agree) and, if there is a first argument, some later
//! argument equals it or ends with `/<it>`. Wrappers that exec something else
//! (`npm start` → `node`) do not match, which errs on the safe side: only
//! matching orphans are killed by `--yes`; everything else needs
//! `--kill-foreign`.

use std::collections::BTreeSet;
use std::time::Duration;

use stems_config::{PortRef, Stem, StemType, Workspace};
use stems_runtime::os::{self, Signal};
use stems_runtime::{AdoptRecord, ProcessRuntime, Runtime, StopOutcome};

pub use stems_runtime::{Orphan, OrphanKind, OrphanScope};

use crate::state::StateFile;

/// The program (normalised) and first argument of a stem's start command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartSignature {
    /// Normalised program name (see the module docs).
    pub program: String,
    /// First argument, if any.
    pub first_arg: Option<String>,
}

/// Lowercase basename without trailing version digits/dots (`Python3.13` → `python`).
pub fn normalise_program(p: &str) -> String {
    let base = p.rsplit('/').next().unwrap_or(p).to_lowercase();
    let trimmed = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    if trimmed.is_empty() {
        base
    } else {
        trimmed.to_string()
    }
}

/// The signature of a start script's text.
pub fn signature_of(script: &str) -> Option<StartSignature> {
    let mut toks = script
        .split_whitespace()
        .map(|t| t.trim_matches(|c| c == '\'' || c == '"'))
        .skip_while(|t| {
            *t == "exec"
                || *t == "env"
                || (t.contains('=') && !t.starts_with('-') && !t.starts_with('/'))
        });
    let program = toks.next()?;
    Some(StartSignature {
        program: normalise_program(program),
        first_arg: toks.next().map(str::to_string),
    })
}

/// The signature of `stem`'s start command (`None` if it has none).
pub fn start_signature(stem: &Stem) -> Option<StartSignature> {
    let (_, script, _) = crate::supervisor::actor::start_script(stem).ok()?;
    signature_of(&script)
}

/// Does a process with command line `cmdline` look like `sig`?
pub fn matches_start_command(cmdline: &str, sig: &StartSignature) -> bool {
    let mut args = cmdline.split_whitespace();
    let Some(argv0) = args.next() else {
        return false;
    };
    if normalise_program(argv0) != sig.program {
        return false;
    }
    match &sig.first_arg {
        None => true,
        Some(a) => args.any(|x| x == a || x.ends_with(&format!("/{a}"))),
    }
}

/// Ports to scan per stem: fixed ports of enabled process stems, plus
/// `auto` ports recorded in state.
fn declared_ports(ws: &Workspace, state: Option<&StateFile>) -> Vec<(String, u16)> {
    let mut out = BTreeSet::new();
    for s in ws.stems().filter(|s| s.kind() == StemType::Process) {
        for p in &s.ports {
            if let PortRef::Fixed(n) = p.port {
                out.insert((s.name.clone(), n));
            }
        }
        if let Some(r) = state.and_then(|f| f.stems.get(&s.name)) {
            for p in r.ports.iter().filter(|p| p.auto) {
                out.insert((s.name.clone(), p.port));
            }
        }
    }
    out.into_iter().collect()
}

/// Process orphans of `ws` given the recorded `state`. `ignore` lists pids
/// that are never orphans (the daemon, the caller).
pub fn scan(ws: &Workspace, state: Option<&StateFile>, ignore: &[i32]) -> Vec<Orphan> {
    let known_groups: BTreeSet<i32> = state
        .map(|f| f.stems.values().flat_map(|r| [r.pgid, r.pid]).collect())
        .unwrap_or_default();
    let mut out: Vec<Orphan> = Vec::new();
    for (stem, port) in declared_ports(ws, state) {
        for l in os::listeners_on_port(port) {
            if ignore.contains(&l.pid) || known_groups.contains(&l.pid) {
                continue;
            }
            let pgid = os::process_group(l.pid);
            if pgid.is_some_and(|g| known_groups.contains(&g)) {
                continue;
            }
            if out
                .iter()
                .any(|o| o.pid == Some(l.pid) && o.port == Some(port))
            {
                continue;
            }
            let command = os::command_line(l.pid).unwrap_or_else(|| l.command.clone());
            let matches = ws
                .stem(&stem)
                .and_then(start_signature)
                .is_some_and(|sig| matches_start_command(&command, &sig));
            out.push(Orphan {
                kind: OrphanKind::Process,
                port: Some(port),
                pid: Some(l.pid),
                pgid,
                command,
                container_id: None,
                stem: Some(stem.clone()),
                matches_start_command: matches,
            });
        }
    }
    out
}

/// Container orphans (14/15): containers labelled `stems.workspace=<ws>`
/// and containers of stems-owned compose projects that `state` does not
/// record. Empty (silently) when the workspace has no docker/compose stem
/// or Docker is not reachable. With `running_only` (the scan `stems up`
/// does), stopped containers are left out: they hold no ports and `up`
/// replaces its own stopped containers anyway; `doctor` reports them all.
pub async fn scan_containers(
    ws: &Workspace,
    state: Option<&StateFile>,
    docker_host: Option<String>,
    compose_dir: &std::path::Path,
    running_only: bool,
) -> Vec<Orphan> {
    let has = |k: StemType| ws.stems().any(|s| s.kind() == k);
    if !has(StemType::Docker) && !has(StemType::Compose) {
        return Vec::new();
    }
    let Ok(docker) = crate::doctor::connect_docker(docker_host, Some(ws)).await else {
        return Vec::new();
    };
    let scope = OrphanScope {
        workspace: ws.name.clone(),
        known: state
            .map(|f| f.stems.values().map(|r| r.adopt_record()).collect())
            .unwrap_or_default(),
    };
    let mut out: Vec<Orphan> = match docker.scan_container_orphans(&scope).await {
        Ok(v) => v
            .into_iter()
            .filter(|o| !running_only || o.state == "running")
            .map(Orphan::from)
            .collect(),
        Err(e) => {
            tracing::debug!(error = %e, "container orphan scan skipped");
            Vec::new()
        }
    };
    if has(StemType::Compose) {
        let compose = stems_runtime::ComposeRuntime::new(
            std::sync::Arc::new(docker),
            stems_runtime::ComposeOptions::new(&ws.name, compose_dir),
        );
        out.extend(
            compose
                .scan_orphans(&scope)
                .await
                .into_iter()
                // `compose <project>/<service>: <name> (<state>[, image])`
                .filter(|o| !running_only || o.command.contains("(running")),
        );
    }
    out
}

/// Remove a container orphan (force, volumes kept).
pub async fn remove_container(o: &Orphan, docker_host: Option<String>) -> Result<(), String> {
    let Some(id) = o.container_id.as_deref() else {
        return Err("not a container".into());
    };
    let docker = crate::doctor::connect_docker(docker_host, None)
        .await
        .map_err(|e| e.message)?;
    docker
        .remove_container_id(id, false)
        .await
        .map_err(|e| e.to_string())
}

/// The record to adopt a process orphan as its stem (`None` if it is gone).
pub fn adopt_record(o: &Orphan) -> Option<AdoptRecord> {
    let pid = o.pid?;
    Some(AdoptRecord {
        pid,
        pgid: os::process_group(pid)?,
        start_time: os::process_start_time(pid)?,
        container_id: None,
    })
}

/// Kill a process orphan: its whole process group when it leads one (the
/// way stems starts stems), otherwise just the process. SIGTERM, `grace`,
/// then SIGKILL. Refuses our own process group.
pub async fn kill(o: &Orphan, grace: Duration) -> std::io::Result<StopOutcome> {
    let Some(rec) = adopt_record(o) else {
        return Ok(StopOutcome::AlreadyDead);
    };
    let own = nix::unistd::getpgrp().as_raw();
    if rec.pgid == rec.pid && rec.pgid != own {
        let rt = ProcessRuntime::new();
        let Some(h) = rt.adopt(&rec).await else {
            return Ok(StopOutcome::AlreadyDead);
        };
        let r = rt.stop(&h, grace).await.map_err(std::io::Error::other);
        rt.release(&h);
        return r;
    }
    let pid = nix::unistd::Pid::from_raw(rec.pid);
    let alive = || os::is_alive(rec.pid, rec.start_time);
    if nix::sys::signal::kill(pid, Signal::SIGTERM).is_err() {
        return Ok(StopOutcome::AlreadyDead);
    }
    let deadline = tokio::time::Instant::now() + grace;
    while alive() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if !alive() {
        return Ok(StopOutcome::Graceful);
    }
    let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while alive() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(StopOutcome::Killed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(s: &str) -> StartSignature {
        signature_of(s).unwrap()
    }

    #[test]
    fn signatures() {
        assert_eq!(
            sig("python3 app.py"),
            StartSignature {
                program: "python".into(),
                first_arg: Some("app.py".into())
            }
        );
        assert_eq!(
            sig("PORT=1 exec python3 app.py --x").first_arg.unwrap(),
            "app.py"
        );
        assert_eq!(sig("exec '/w/bin/start.sh'").program, "start.sh");
        assert_eq!(sig("./server").first_arg, None);
        assert!(signature_of("   ").is_none());
        assert_eq!(normalise_program("/usr/bin/python3.13"), "python");
        assert_eq!(normalise_program("Python"), "python");
        assert_eq!(normalise_program("42"), "42");
    }

    #[test]
    fn matching() {
        let s = sig("python3 app.py");
        assert!(matches_start_command("python3 app.py", &s));
        assert!(matches_start_command(
            "/Library/Frameworks/Python.framework/Versions/3.13/Resources/Python.app/Contents/MacOS/Python app.py",
            &s
        ));
        assert!(matches_start_command(
            "/usr/bin/python3 /w/repos/shop-api/app.py",
            &s
        ));
        assert!(!matches_start_command("python3 -m http.server 18090", &s));
        assert!(!matches_start_command("node app.py", &s));
        assert!(!matches_start_command("", &s));
        assert!(matches_start_command("./server --port 1", &sig("./server")));
    }

    #[tokio::test]
    async fn kill_refuses_nothing_and_handles_gone_processes() {
        let gone = Orphan {
            kind: OrphanKind::Process,
            port: Some(1),
            pid: Some(i32::MAX - 7),
            pgid: None,
            command: String::new(),
            container_id: None,
            stem: None,
            matches_start_command: false,
        };
        assert_eq!(
            kill(&gone, Duration::from_millis(10)).await.unwrap(),
            StopOutcome::AlreadyDead
        );
    }

    #[tokio::test]
    async fn kill_a_group_leader() {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let o = Orphan {
            kind: OrphanKind::Process,
            port: None,
            pid: Some(pid),
            pgid: Some(pid),
            command: "sleep 30".into(),
            container_id: None,
            stem: None,
            matches_start_command: true,
        };
        let reaper = std::thread::spawn(move || child.wait());
        let out = kill(&o, Duration::from_secs(2)).await.unwrap();
        assert!(matches!(out, StopOutcome::Graceful | StopOutcome::Killed));
        reaper.join().unwrap().unwrap();
    }
}
