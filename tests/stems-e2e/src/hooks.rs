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
//!    `stems.workspace=<name>`;
//! 5. kill whatever leaked, copy the scenario dir to
//!    `target/e2e-failures/<scenario>/` if the scenario failed (or leaked),
//!    delete the temp dir;
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
    std::process::Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Ids of containers labelled `stems.workspace=<ws>`; empty without Docker.
pub fn labelled_containers(ws: &str) -> Vec<String> {
    if !docker_available() {
        return Vec::new();
    }
    std::process::Command::new("docker")
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
    let until = Instant::now() + Duration::from_secs(3);
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
    if docker && let Some(ws) = &w.ws_name {
        let label = ws.rsplit('/').next().unwrap_or(ws);
        for id in labelled_containers(label) {
            leaks.push(format!(
                "container {id} with label stems.workspace={label} exists"
            ));
        }
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
