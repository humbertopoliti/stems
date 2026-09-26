//! Process inspection used by the leak hook and the process steps.

use std::path::Path;
use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::Command;

/// A process found by [`scan`], with the reason it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub pid: i32,
    pub pgid: i32,
    pub command: String,
    pub why: String,
}

/// True if any process in group `pgid` exists (`kill(-pgid, 0)`); `EPERM`
/// counts as alive.
pub fn pgid_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    match kill(Pid::from_raw(-pgid), None) {
        Ok(()) => true,
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// True if process `pid` exists.
pub fn pid_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

/// True if `pid` is a zombie (exited, not yet reaped) according to `ps`.
pub fn is_zombie(pid: i32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_start()
                .starts_with('Z')
        })
}

/// SIGKILLs a whole process group, ignoring errors.
pub fn kill_group(pgid: i32) {
    if pgid > 1 {
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
    }
}

/// SIGKILLs one process, ignoring errors.
pub fn kill_pid(pid: i32) {
    if pid > 1 {
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

/// Parses a signal name such as `SIGKILL`, `KILL` or `9`.
pub fn parse_signal(name: &str) -> Result<Signal, String> {
    let name = name.trim();
    if let Ok(n) = name.parse::<i32>() {
        return Signal::try_from(n).map_err(|e| format!("bad signal {name}: {e}"));
    }
    let full = if name.starts_with("SIG") {
        name.to_owned()
    } else {
        format!("SIG{name}")
    };
    full.parse::<Signal>()
        .map_err(|e| format!("bad signal {name}: {e}"))
}

async fn output(cmd: &mut Command) -> Option<String> {
    cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `pid -> (pgid, command)` for every visible process.
async fn ps_table() -> Vec<(i32, i32, String)> {
    let Some(text) =
        output(Command::new("ps").args(["-ax", "-ww", "-o", "pid=,pgid=,command="])).await
    else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let pgid = it.next()?.parse().ok()?;
            let rest: Vec<&str> = it.collect();
            Some((pid, pgid, rest.join(" ")))
        })
        .collect()
}

/// Pids whose cwd is under `marker` (macOS: `lsof`; Linux: `/proc`).
#[cfg(target_os = "macos")]
async fn cwd_matches(marker: &str) -> Vec<(i32, String)> {
    let uid = nix::unistd::getuid().to_string();
    let Some(text) =
        output(Command::new("lsof").args(["-a", "-d", "cwd", "-u", &uid, "-F", "pn"])).await
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse::<i32>().ok();
        } else if let Some(path) = line.strip_prefix('n')
            && path_under(path, marker)
            && let Some(p) = pid
        {
            out.push((p, format!("cwd {path}")));
        }
    }
    out
}

/// Pids whose cwd or environment mention `marker` (Linux: `/proc`).
#[cfg(not(target_os = "macos"))]
async fn cwd_matches(marker: &str) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in dir.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if let Ok(cwd) = std::fs::read_link(entry.path().join("cwd"))
            && path_under(&cwd.to_string_lossy(), marker)
        {
            out.push((pid, format!("cwd {}", cwd.display())));
            continue;
        }
        if let Ok(env) = std::fs::read(entry.path().join("environ"))
            && env.windows(marker.len()).any(|w| w == marker.as_bytes())
        {
            out.push((pid, "environment mentions the scenario dir".to_owned()));
        }
    }
    out
}

fn path_under(path: &str, marker: &str) -> bool {
    path == marker.trim_end_matches('/') || path.starts_with(marker)
}

/// Finds live processes tied to a scenario directory: command line contains
/// `dir` (e.g. `--workspace <dir>/...`, `STEMS_HOME` passed as an argument),
/// cwd under `dir`, or (Linux only; macOS hides other processes'
/// environments) environment mentioning `dir`. The harness's own pid is
/// excluded.
pub async fn scan(dir: &Path) -> Vec<Found> {
    let mut marker = dir.to_string_lossy().into_owned();
    if !marker.ends_with('/') {
        marker.push('/');
    }
    let me = i32::try_from(std::process::id()).unwrap_or(-1);
    let table = ps_table().await;
    let mut found: Vec<Found> = Vec::new();
    for (pid, pgid, command) in &table {
        if *pid == me {
            continue;
        }
        if command.contains(&marker) || command.contains(marker.trim_end_matches('/')) {
            found.push(Found {
                pid: *pid,
                pgid: *pgid,
                command: command.clone(),
                why: "command line mentions the scenario dir".into(),
            });
        }
    }
    for (pid, why) in cwd_matches(&marker).await {
        if pid == me || found.iter().any(|f| f.pid == pid) || !pid_alive(pid) {
            continue;
        }
        let (pgid, command) = table
            .iter()
            .find(|(p, _, _)| *p == pid)
            .map(|(_, g, c)| (*g, c.clone()))
            .unwrap_or((pid, String::from("?")));
        found.push(Found {
            pid,
            pgid,
            command,
            why,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_parse() {
        assert_eq!(parse_signal("SIGKILL").unwrap(), Signal::SIGKILL);
        assert_eq!(parse_signal("TERM").unwrap(), Signal::SIGTERM);
        assert_eq!(parse_signal("9").unwrap(), Signal::SIGKILL);
        assert!(parse_signal("SIGNOPE").is_err());
    }

    #[test]
    fn path_under_is_prefix_based() {
        assert!(path_under("/tmp/a/ws", "/tmp/a/"));
        assert!(path_under("/tmp/a", "/tmp/a/"));
        assert!(!path_under("/tmp/ab", "/tmp/a/"));
    }

    #[tokio::test]
    async fn scan_finds_a_process_by_cwd() {
        let dir = tempfile::Builder::new()
            .prefix("stems-e2e-scan-")
            .tempdir_in("/tmp")
            .unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let found = scan(&dir).await;
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found.iter().any(|f| f.pid == pid), "{found:?}");
        assert!(!pid_alive(pid));
    }
}
