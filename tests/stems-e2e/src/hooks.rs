//! The global After hook: leak detection and cleanup.
//!
//! For every scenario, whatever its outcome:
//! 1. stray processes spawned by steps are deliberately *not* stopped first:
//!    if still alive they are exactly what the hook must report (see
//!    `harness/leak-hook.feature`);
//! 2. run `stems daemon stop --json` if a daemon may have been started (a
//!    step said so, or a socket/lock exists under `STEMS_HOME`), ignoring
//!    errors;
//! 3. wait (bounded, 3 s) for every recorded process group to disappear;
//! 4. report as leaks: live recorded/state-file process groups, processes
//!    whose command line or cwd (Linux: also environment) mention the
//!    scenario dir, sockets/locks under `STEMS_HOME`, and (for `@docker`
//!    scenarios with Docker present) containers labelled
//!    `stems.workspace=<name>`, containers of the workspace's compose
//!    projects (`<name>`, `stems-<name>`, `stems-e2e*`), their networks and
//!    `<name>_net` (all removed afterwards; named volumes `<name>_*` are
//!    removed too but not reported: a plain `down` keeps them by design);
//! 5. kill whatever leaked, copy the scenario dir to
//!    `target/e2e-failures/<scenario>/` if the scenario failed (or leaked),
//!    tagged with the run's e2e slot (`.e2e-slot`) so a concurrent run's
//!    start-up cleanup leaves it alone, delete the temp dir;
//! 6. panic with a message starting with `LEAK:` if anything leaked.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cucumber::event::ScenarioFinished;
use cucumber::gherkin;
use futures::FutureExt as _;
use futures::future::LocalBoxFuture;

use crate::world::{E2eWorld, find_files, is_socket_or_lock, repo_root};
use crate::{pending, procs};

/// Process groups and pids the hook found leaking (for the selftest sweep).
pub static LEAKED: Mutex<Vec<(i32, i32)>> = Mutex::new(Vec::new());

/// PENDING.txt entries, loaded once by the runner.
pub static PENDING: Mutex<Vec<pending::Entry>> = Mutex::new(Vec::new());

/// Where failing scenarios' temp dirs are preserved.
pub fn failures_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| repo_root().join("target"), PathBuf::from)
        .join("e2e-failures")
}

/// Marker file inside each preserved failure dir naming the e2e slot that
/// wrote it (`none` outside a slot).
const OWNER_FILE: &str = ".e2e-slot";

/// Where `scripts/e2e_slot.sh` keeps its slot dirs.
fn slots_dir() -> PathBuf {
    std::env::var_os("STEMS_E2E_SLOTS_DIR")
        .map_or_else(|| PathBuf::from("/tmp/stems-e2e-slots"), PathBuf::from)
}

/// Whether slot `owner` is held by a live run other than this one.
fn held_by_other_run(owner: &str, mine: Option<u32>) -> bool {
    let Ok(n) = owner.trim().parse::<u32>() else {
        return false;
    };
    if Some(n) == mine {
        return false;
    }
    std::fs::read_to_string(slots_dir().join(n.to_string()).join("pid"))
        .ok()
        .and_then(|p| p.trim().parse::<i32>().ok())
        .is_some_and(procs::pid_alive)
}

/// Removes the failure artefacts of earlier runs, keeping those written by a
/// run that currently holds another slot (concurrent runs share
/// [`failures_dir`]).
pub fn clean_failures() {
    let mine = crate::world::slot();
    let Ok(entries) = std::fs::read_dir(failures_dir()) else {
        return;
    };
    for e in entries.flatten() {
        let owner = std::fs::read_to_string(e.path().join(OWNER_FILE)).unwrap_or_default();
        if !held_by_other_run(&owner, mine) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Repo-relative path of a feature file.
pub fn feature_path(feature: &gherkin::Feature) -> String {
    let Some(p) = &feature.path else {
        return String::from("<unknown>");
    };
    let p = p.canonicalize().unwrap_or_else(|_| p.clone());
    p.strip_prefix(repo_root())
        .unwrap_or(&p)
        .display()
        .to_string()
}

fn sanitize(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    s.chars().take(100).collect()
}

fn docker_available() -> bool {
    std::process::Command::new(crate::docker::docker_program())
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Docker resources a scenario of workspace `ws` left behind, beyond the
/// containers labelled `stems.workspace=<ws>`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DockerLeftover {
    /// A container (id), e.g. of the workspace's compose project.
    Container(String),
    /// A network (name): `<ws>_net` or a compose project's `_default`.
    Network(String),
    /// A named volume (name) `<ws>_*`.
    Volume(String),
}

impl DockerLeftover {
    pub fn describe(&self, ws: &str) -> String {
        match self {
            Self::Container(id) => format!("compose container {id} of a project of {ws} exists"),
            Self::Network(n) => format!("docker network {n} of {ws} exists"),
            Self::Volume(v) => format!("docker volume {v} of {ws} exists"),
        }
    }

    fn remove(&self) {
        let args: Vec<&str> = match self {
            Self::Container(id) => vec!["rm", "-f", "-v", id],
            Self::Network(n) => vec!["network", "rm", n],
            Self::Volume(v) => vec!["volume", "rm", "-f", v],
        };
        let _ = std::process::Command::new(crate::docker::docker_program())
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Compose projects scenarios of `ws` create: `<ws>` (hello-shop's
/// explicit `project_name`), the default `stems-<ws>`, and the harness's
/// own `stems-e2e*` projects.
pub fn is_scenario_project(project: &str, ws: &str) -> bool {
    !project.is_empty()
        && (project == ws
            || project.strip_prefix("stems-") == Some(ws)
            || project.starts_with("stems-e2e"))
}

/// `docker ps -a` lines `<id>\t<compose project>` → containers of
/// scenario projects.
pub fn scenario_compose_containers(ps: &str, ws: &str) -> Vec<String> {
    ps.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(_, p)| is_scenario_project(p.trim(), ws))
        .map(|(id, _)| id.trim().to_string())
        .collect()
}

/// `docker network ls` lines `<name>\t<stems.workspace>\t<compose
/// project>` → networks stems or a scenario project created for `ws`.
pub fn scenario_networks(ls: &str, ws: &str) -> Vec<String> {
    ls.lines()
        .filter_map(|l| {
            let mut f = l.split('\t').map(str::trim);
            let (name, label, project) = (f.next()?, f.next()?, f.next().unwrap_or(""));
            (label == ws || is_scenario_project(project, ws)).then(|| name.to_string())
        })
        .collect()
}

/// Named volumes `<ws>_*` (stems prefixes every named volume so).
pub fn scenario_volumes(ls: &str, ws: &str) -> Vec<String> {
    let prefix = format!("{ws}_");
    ls.lines()
        .map(str::trim)
        .filter(|n| n.starts_with(&prefix))
        .map(str::to_string)
        .collect()
}

fn docker_stdout(args: &[&str]) -> String {
    std::process::Command::new(crate::docker::docker_program())
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Compose containers, networks and named volumes of `ws` (see
/// [`DockerLeftover`]); empty without Docker.
pub fn other_docker_leftovers(ws: &str) -> Vec<DockerLeftover> {
    if !docker_available() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let ps = docker_stdout(&[
        "ps",
        "-a",
        "--filter",
        "label=com.docker.compose.project",
        "--format",
        "{{.ID}}\t{{.Label \"com.docker.compose.project\"}}",
    ]);
    out.extend(
        scenario_compose_containers(&ps, ws)
            .into_iter()
            .map(DockerLeftover::Container),
    );
    let nets = docker_stdout(&[
        "network",
        "ls",
        "--format",
        "{{.Name}}\t{{.Label \"stems.workspace\"}}\t{{.Label \"com.docker.compose.project\"}}",
    ]);
    out.extend(
        scenario_networks(&nets, ws)
            .into_iter()
            .map(DockerLeftover::Network),
    );
    let vols = docker_stdout(&["volume", "ls", "-q"]);
    out.extend(
        scenario_volumes(&vols, ws)
            .into_iter()
            .map(DockerLeftover::Volume),
    );
    out
}

/// Ids of containers labelled `stems.workspace=<ws>`; empty without Docker.
pub fn labelled_containers(ws: &str) -> Vec<String> {
    if !docker_available() {
        return Vec::new();
    }
    std::process::Command::new(crate::docker::docker_program())
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=stems.workspace={ws}"),
        ])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Steps 2-4 of the hook: returns leak descriptions and kills what leaked.
pub async fn check_leaks(w: &mut E2eWorld, docker: bool) -> Vec<String> {
    let mut leaks = Vec::new();

    // An MCP server (31) goes first: it may hold a daemon it auto-started.
    crate::mcp::shutdown(w).await;

    let has_daemon_files = !find_files(&w.home, &is_socket_or_lock).is_empty();
    if w.daemon_started || has_daemon_files {
        let _ = w
            .run_quiet("stems daemon stop --json", Duration::from_secs(10))
            .await;
    }

    let mut pgids: BTreeSet<i32> = w.pgids.clone();
    pgids.extend(w.state_pgids());
    // Process groups the scenario started must be gone once `daemon stop`
    // returned. Under heavy load (several e2e slots plus cargo builds) a
    // daemon that already logged "daemon stopped" can take well over 3 s to
    // actually exit, which showed up as false `LEAK:` reports; give it 15 s
    // (`STEMS_E2E_LEAK_GRACE_SECS` overrides) before calling it a leak.
    let grace = std::env::var("STEMS_E2E_LEAK_GRACE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15);
    let until = Instant::now() + Duration::from_secs(grace);
    let mut alive: Vec<i32> = pgids
        .iter()
        .copied()
        .filter(|g| procs::pgid_alive(*g))
        .collect();
    while !alive.is_empty() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(100)).await;
        alive.retain(|g| procs::pgid_alive(*g));
    }

    let found = procs::scan(&w.root).await;
    for g in &alive {
        let members: Vec<String> = found
            .iter()
            .filter(|f| f.pgid == *g)
            .map(|f| format!("{} {}", f.pid, f.command))
            .collect();
        leaks.push(format!(
            "process group {g} is still alive [{}]",
            members.join("; ")
        ));
    }
    for f in &found {
        if !alive.contains(&f.pgid) {
            leaks.push(format!(
                "pid {} (pgid {}) `{}`: {}",
                f.pid, f.pgid, f.command, f.why
            ));
        }
    }
    for f in find_files(&w.home, &is_socket_or_lock) {
        leaks.push(format!("daemon file left behind: {}", f.display()));
    }
    let mut docker_leftovers = Vec::new();
    if docker && let Some(ws) = &w.ws_name {
        let label = ws.rsplit('/').next().unwrap_or(ws);
        for id in labelled_containers(label) {
            leaks.push(format!(
                "container {id} with label stems.workspace={label} exists"
            ));
            docker_leftovers.push(DockerLeftover::Container(id));
        }
        for l in other_docker_leftovers(label) {
            // A named volume after a plain `stems down` is by design (only
            // `--volumes` deletes data): cleaned up, not a leak.
            if !matches!(l, DockerLeftover::Volume(_)) {
                leaks.push(l.describe(label));
            }
            docker_leftovers.push(l);
        }
    }
    // Containers first: a network or volume in use cannot be removed.
    docker_leftovers.sort();
    for l in &docker_leftovers {
        l.remove();
    }

    // Clean up whatever leaked so it cannot poison later scenarios.
    {
        let mut record = LEAKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for g in &alive {
            procs::kill_group(*g);
            record.push((*g, 0));
        }
        for f in &found {
            procs::kill_pid(f.pid);
            record.push((f.pgid, f.pid));
        }
    }
    for mut child in std::mem::take(&mut w.strays) {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
    leaks
}

/// Copies the scenario dir (without the repos symlink) for CI artefacts.
fn preserve(w: &E2eWorld, name: &str, leaks: &[String]) {
    let dest = failures_dir().join(name);
    let _ = std::fs::remove_dir_all(&dest);
    let _ = copy_no_symlinks(&w.root, &dest);
    let mut report = String::new();
    if let Some(last) = &w.last {
        report.push_str(&format!("last command:\n{}\n\n", last.describe()));
    }
    for l in leaks {
        report.push_str(&format!("LEAK: {l}\n"));
    }
    let _ = std::fs::write(dest.join("harness-report.txt"), report);
    let owner = crate::world::slot().map_or_else(|| String::from("none"), |i| i.to_string());
    let _ = std::fs::write(dest.join(OWNER_FILE), owner);
}

fn copy_no_symlinks(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_symlink() {
            continue;
        } else if ty.is_dir() {
            copy_no_symlinks(&entry.path(), &to)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// The After hook registered with cucumber.
pub fn after<'a>(
    feature: &'a gherkin::Feature,
    rule: Option<&'a gherkin::Rule>,
    scenario: &'a gherkin::Scenario,
    finished: &'a ScenarioFinished,
    world: Option<&'a mut E2eWorld>,
) -> LocalBoxFuture<'a, ()> {
    async move {
        let Some(w) = world else { return };
        let docker = feature
            .tags
            .iter()
            .chain(rule.map(|r| r.tags.iter()).into_iter().flatten())
            .chain(scenario.tags.iter())
            .any(|t| t == "docker");
        let leaks = check_leaks(w, docker).await;

        let path = feature_path(feature);
        let line = scenario.position.line;
        let is_pending = {
            let p = PENDING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending::is_pending(&p, &path, line)
        };
        let failed = !matches!(finished, ScenarioFinished::StepPassed);
        if !leaks.is_empty() || (failed && !is_pending) {
            let stem = Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = format!("{}__L{line}__{}", sanitize(&stem), sanitize(&scenario.name));
            preserve(w, &name, &leaks);
        }
        if let Some(tmp) = w.take_tmp() {
            let _ = tmp.close();
        }
        if !leaks.is_empty() {
            panic!(
                "LEAK: scenario {path}:{line} {:?} left {} thing(s) behind:\n  - {}",
                scenario.name,
                leaks.len(),
                leaks.join("\n  - ")
            );
        }
    }
    .boxed_local()
}

#[cfg(test)]
mod docker_leftover_tests {
    use super::*;

    #[test]
    fn scenario_projects() {
        assert!(is_scenario_project("hello-shop", "hello-shop"));
        assert!(is_scenario_project("stems-compose-redis", "compose-redis"));
        assert!(is_scenario_project("stems-e2e-foreign", "compose-redis"));
        assert!(!is_scenario_project("homebridge", "compose-redis"));
        assert!(!is_scenario_project("", ""));
    }

    #[test]
    fn leftovers_from_cli_output() {
        let ps = "abc\thello-shop\ndef\thomebridge\nghi\tstems-e2e-foreign\n";
        assert_eq!(
            scenario_compose_containers(ps, "hello-shop"),
            ["abc", "ghi"]
        );
        let nets = "bridge\t\t\nhello-shop_net\thello-shop\t\nhello-shop_default\t\thello-shop\nhomebridge_default\t\thomebridge\nother_net\tother\t\n";
        assert_eq!(
            scenario_networks(nets, "hello-shop"),
            ["hello-shop_net", "hello-shop_default"]
        );
        let vols = "hello-shop_pgdata\nhello-shopper_x\nappwrite_config\n";
        assert_eq!(scenario_volumes(vols, "hello-shop"), ["hello-shop_pgdata"]);
        let mut l = [
            DockerLeftover::Volume("v".into()),
            DockerLeftover::Network("n".into()),
            DockerLeftover::Container("c".into()),
        ];
        l.sort();
        assert_eq!(l[0], DockerLeftover::Container("c".into()));
    }
}
