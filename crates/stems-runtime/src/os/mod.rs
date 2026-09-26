//! Thin OS layer: process start times, process-group enumeration, port listeners
//! and group signalling.
//!
//! Per-OS code lives in `macos.rs` (libproc + `lsof`) and `linux.rs` (`/proc`).
//! Everything here is synchronous and cheap enough to call from a blocking
//! context; async callers should use `tokio::task::spawn_blocking` for
//! [`process_tree`], [`listeners_on_port`] and [`listening_ports`].

use std::io;
use std::time::SystemTime;

use nix::errno::Errno;
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

pub use nix::sys::signal::Signal;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("stems-runtime supports macOS and Linux only");

/// Opaque, comparable process start time.
///
/// On macOS this is milliseconds since the Unix epoch (from `pbi_start_tvsec/usec`);
/// on Linux it is the raw `starttime` field of `/proc/<pid>/stat` (clock ticks since
/// boot), which is stable across reads. Only equality is meaningful across OSes;
/// use [`StartTime::approx_system_time`] for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StartTime(pub u64);

impl StartTime {
    /// Best-effort wall-clock time the process started (for display / sanity checks).
    pub fn approx_system_time(self) -> Option<SystemTime> {
        imp::start_time_to_system_time(self)
    }
}

/// One process of a process group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// User + system CPU time consumed so far, in milliseconds.
    pub cpu_time_ms: u64,
    /// Resident set size in bytes (0 if unavailable).
    pub rss_bytes: u64,
    /// Short command name (`comm`), not the full argv.
    pub command: String,
}

/// A process listening on a TCP port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listener {
    pub pid: i32,
    pub command: String,
}

/// Minimal per-pid probe used by the higher-level functions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Probe {
    pub start_time: StartTime,
    pub zombie: bool,
}

/// Start time of `pid`, or `None` if there is no such process (or it is not readable).
pub fn process_start_time(pid: i32) -> Option<StartTime> {
    if pid <= 0 {
        return None;
    }
    imp::probe(pid).map(|p| p.start_time)
}

/// True when `pid` exists, is not a zombie, and started at `start_time`.
///
/// This is the pid-reuse defence: a recycled pid has a different start time.
pub fn is_alive(pid: i32, start_time: StartTime) -> bool {
    if pid <= 0 {
        return false;
    }
    match nix::sys::signal::kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => {}
        Err(_) => return false,
    }
    matches!(imp::probe(pid), Some(p) if !p.zombie && p.start_time == start_time)
}

/// All live (non-zombie) processes whose process group id is `pgid`.
pub fn process_tree(pgid: i32) -> Vec<ProcInfo> {
    if pgid <= 1 {
        return Vec::new();
    }
    let mut v = imp::process_tree(pgid);
    v.sort_by_key(|p| p.pid);
    v
}

/// Processes with a TCP socket in LISTEN state on `port` (IPv4 or IPv6).
pub fn listeners_on_port(port: u16) -> Vec<Listener> {
    let mut v = imp::listeners_on_port(port);
    v.sort_by_key(|l| l.pid);
    v.dedup_by_key(|l| l.pid);
    v
}

/// TCP ports that any of `pids` is listening on, sorted and de-duplicated.
pub fn listening_ports(pids: &[i32]) -> Vec<u16> {
    if pids.is_empty() {
        return Vec::new();
    }
    let mut v = imp::listening_ports(pids);
    v.sort_unstable();
    v.dedup();
    v
}

/// Send `signal` to every process in group `pgid` (`kill(-pgid, sig)`).
///
/// Refuses `pgid <= 1` and the caller's own process group, so a bogus record can
/// never signal the daemon itself or every process of the user.
/// Returns `ErrorKind::NotFound` when the group has no members (ESRCH).
pub fn kill_group(pgid: i32, signal: Option<Signal>) -> io::Result<()> {
    if pgid <= 1 || pgid == nix::unistd::getpgrp().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process group {pgid}"),
        ));
    }
    match nix::sys::signal::killpg(Pid::from_raw(pgid), signal) {
        Ok(()) => Ok(()),
        Err(Errno::ESRCH) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no process in group {pgid}"),
        )),
        Err(e) => Err(io::Error::from(e)),
    }
}

/// True while at least one process (possibly a zombie) is in group `pgid`.
pub fn group_exists(pgid: i32) -> bool {
    match kill_group(pgid, None) {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::PermissionDenied,
    }
}

/// Parse the port out of an address like `*:8080`, `127.0.0.1:80`, `[::1]:443`.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn port_from_addr(addr: &str) -> Option<u16> {
    addr.rsplit_once(':')?.1.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn spawn_sleep() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep")
    }

    #[test]
    fn own_start_time_is_stable() {
        let pid = std::process::id() as i32;
        let a = process_start_time(pid).expect("own start time");
        let b = process_start_time(pid).expect("own start time");
        assert_eq!(a, b);
        assert!(is_alive(pid, a));
    }

    #[test]
    fn child_start_time_is_close_to_spawn() {
        let before = SystemTime::now();
        let mut child = spawn_sleep();
        let st = process_start_time(child.id() as i32).expect("child start time");
        let when = st.approx_system_time().expect("approx time");
        let diff = match when.duration_since(before) {
            Ok(d) => d,
            Err(e) => e.duration(),
        };
        assert!(diff < Duration::from_secs(2), "start time off by {diff:?}");
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn own_pgid_tree_contains_own_pid() {
        let pgid = nix::unistd::getpgrp().as_raw();
        let me = std::process::id() as i32;
        let tree = process_tree(pgid);
        let mine = tree.iter().find(|p| p.pid == me).expect("own pid in tree");
        assert_eq!(mine.pgid, pgid);
        assert!(mine.rss_bytes > 0, "rss should be reported: {mine:?}");
        assert!(!mine.command.is_empty());
    }

    #[test]
    fn is_alive_false_for_dead_pid() {
        let mut child = spawn_sleep();
        let pid = child.id() as i32;
        let st = process_start_time(pid).unwrap();
        assert!(is_alive(pid, st));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!is_alive(pid, st));
        assert!(process_start_time(pid).is_none() || !is_alive(pid, st));
    }

    #[test]
    fn is_alive_false_with_wrong_start_time() {
        let pid = std::process::id() as i32;
        let st = process_start_time(pid).unwrap();
        assert!(!is_alive(pid, StartTime(st.0 + 1)));
        assert!(!is_alive(pid, StartTime(0)));
    }

    #[test]
    fn kill_group_refuses_own_and_init_groups() {
        let own = nix::unistd::getpgrp().as_raw();
        assert!(kill_group(own, None).is_err());
        assert!(kill_group(1, None).is_err());
        assert!(kill_group(0, None).is_err());
        assert!(kill_group(-5, None).is_err());
    }

    #[test]
    fn port_parsing() {
        assert_eq!(port_from_addr("*:8080"), Some(8080));
        assert_eq!(port_from_addr("127.0.0.1:80"), Some(80));
        assert_eq!(port_from_addr("[::1]:443"), Some(443));
        assert_eq!(port_from_addr("nonsense"), None);
    }

    #[test]
    fn listeners_and_ports_for_bound_socket() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let me = std::process::id() as i32;
        let who = listeners_on_port(port);
        assert!(who.iter().any(|x| x.pid == me), "{who:?}");
        assert!(listening_ports(&[me]).contains(&port));
    }
}
