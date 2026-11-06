//! Orphan scan (FR-CR-4, deliverable 11): things running on this
//! workspace's declared ports that the state file does not know about.
//!
//! * Processes: every TCP listener on a port declared by an enabled
//!   `process` stem (fixed ports, plus `auto` ports recorded in state) that
//!   does not belong to a recorded stem ([`belongs_to_stem`]: neither it nor
//!   any ancestor up to pid 1, nor their process groups, is a recorded
//!   stem's pid or process group, so a dev server a launcher script moved
//!   into its own group with `setsid` is still its stem's child) and which
//!   is not the daemon (or the caller) itself.
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

/// Whether `pid` belongs to a recorded stem: `pid`, an ancestor of it
/// (parent chain up to pid 1, [`os::MAX_ANCESTRY`] steps), or the process
/// group of either is in `known` (the recorded stems' pids and pgids).
/// `parent_of` / `group_of` look the process table up ([`os::parent_pid`],
/// [`os::process_group`]; tests inject maps).
pub fn belongs_to_stem(
    pid: i32,
    known: &BTreeSet<i32>,
    parent_of: impl Fn(i32) -> Option<i32>,
    group_of: impl Fn(i32) -> Option<i32>,
) -> bool {
    let ours = |p: i32| known.contains(&p) || group_of(p).is_some_and(|g| known.contains(&g));
    ours(pid) || os::ancestors_with(pid, parent_of).into_iter().any(ours)
}

/// Pids and process groups of the recorded stems whose leader is still the
/// process that was recorded: alive with the recorded start time. After a
/// daemon crash a recorded pid may have been reused by an unrelated
/// long-lived process (login shell, tmux, IDE), and ancestry matching would
/// then hide every listener under it; such records contribute nothing.
/// Containers record no process; pids 0 and 1 never count. `alive` is
/// [`os::is_alive`] (tests inject a map).
fn known_pids(
    state: Option<&StateFile>,
    alive: impl Fn(i32, stems_runtime::StartTime) -> bool,
) -> BTreeSet<i32> {
    state
        .map(|f| {
            f.stems
                .values()
                .filter(|r| r.pid > 1 && alive(r.pid, r.start_time))
                .flat_map(|r| [r.pgid, r.pid])
                .filter(|p| *p > 1)
                .collect()
        })
        .unwrap_or_default()
}

/// Process orphans of `ws` given the recorded `state`. `ignore` lists pids
/// that are never orphans (the daemon, the caller).
pub fn scan(ws: &Workspace, state: Option<&StateFile>, ignore: &[i32]) -> Vec<Orphan> {
    let known_groups = known_pids(state, os::is_alive);
    let mut out: Vec<Orphan> = Vec::new();
    for (stem, port) in declared_ports(ws, state) {
        for l in os::listeners_on_port(port) {
            if ignore.contains(&l.pid)
                || belongs_to_stem(l.pid, &known_groups, os::parent_pid, os::process_group)
            {
                continue;
            }
            let pgid = os::process_group(l.pid);
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
    fn a_child_in_its_own_process_group_belongs_to_its_stem() {
        // Stem `web`: launcher pid/pgid 100 (its record), which started a
        // dev server 200 that called setsid (pgid 200), which forked a
        // worker 300 (pgid 200) that listens. The daemon is 50.
        let parents = std::collections::HashMap::from([
            (300, 200),
            (200, 100),
            (100, 50),
            (50, 1),
            (900, 1),
            (950, 900),
        ]);
        let groups = std::collections::HashMap::from([
            (300, 200),
            (200, 200),
            (100, 100),
            (50, 50),
            (900, 900),
            (950, 900),
        ]);
        let parent = |p: i32| parents.get(&p).copied();
        let group = |p: i32| groups.get(&p).copied();
        let known: BTreeSet<i32> = [100].into_iter().collect();
        assert!(belongs_to_stem(300, &known, parent, group));
        assert!(belongs_to_stem(200, &known, parent, group));
        assert!(belongs_to_stem(100, &known, parent, group));
        // Unrelated processes (and the daemon itself) are not the stem's.
        assert!(!belongs_to_stem(950, &known, parent, group));
        assert!(!belongs_to_stem(50, &known, parent, group));
        // A member of a recorded group whose leader is gone.
        let known: BTreeSet<i32> = [900].into_iter().collect();
        assert!(belongs_to_stem(950, &known, parent, group));
        assert!(!belongs_to_stem(300, &BTreeSet::new(), parent, group));
    }

    #[test]
    fn records_with_a_reused_or_dead_pid_contribute_nothing() {
        use crate::state::{StateFile, StemRecord};
        use stems_core::StemState;
        use stems_runtime::StartTime;
        let rec = |pid: i32, st: u64| StemRecord {
            pid,
            pgid: pid,
            start_time: StartTime(st),
            container_id: None,
            ports: Vec::new(),
            overlays: Vec::new(),
            state: StemState::Healthy,
            started_at: chrono::Utc::now(),
            log_file: None,
        };
        let mut f = StateFile::new(
            "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P",
            crate::state::DaemonRecord {
                pid: 50,
                start_time: 1,
            },
        );
        f.stems.insert("ok".into(), rec(100, 10));
        // pid 200 was reused by a process that started later.
        f.stems.insert("reused".into(), rec(200, 20));
        f.stems.insert("dead".into(), rec(300, 30));
        // Live processes: pid -> start time.
        let live = std::collections::HashMap::from([(100, 10), (200, 99)]);
        let alive = |pid: i32, st: StartTime| live.get(&pid) == Some(&st.0);
        let known = known_pids(Some(&f), alive);
        assert_eq!(known, [100].into_iter().collect::<BTreeSet<i32>>());
        assert!(known_pids(None, alive).is_empty());
        // The reused pid is a shell above a real orphan: not the stem's.
        let parent = |p: i32| (p == 400).then_some(200);
        let group = |p: i32| Some(p);
        assert!(!belongs_to_stem(400, &known, parent, group));
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
