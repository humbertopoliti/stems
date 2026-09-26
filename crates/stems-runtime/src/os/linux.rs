//! Linux implementation: everything comes from `/proc`.

use std::collections::HashSet;
use std::fs;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Listener, Probe, ProcInfo, StartTime};

/// Parsed subset of `/proc/<pid>/stat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stat {
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    pub pgrp: i32,
    pub utime: u64,
    pub stime: u64,
    pub starttime: u64,
    pub rss_pages: i64,
}

/// Parse a `/proc/<pid>/stat` line. `comm` may contain spaces and parentheses, so
/// split at the *last* `)`. Fields after it start at field 3 (`state`).
pub(crate) fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let comm = line.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
    // rest[0] = field 3 (state); field N is rest[N - 3].
    let f = |n: usize| rest.get(n - 3).copied();
    Some(Stat {
        comm,
        state: f(3)?.chars().next()?,
        ppid: f(4)?.parse().ok()?,
        pgrp: f(5)?.parse().ok()?,
        utime: f(14)?.parse().ok()?,
        stime: f(15)?.parse().ok()?,
        starttime: f(22)?.parse().ok()?,
        rss_pages: f(24)?.parse().ok()?,
    })
}

fn read_stat(pid: i32) -> Option<Stat> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

fn clk_tck() -> u64 {
    static V: OnceLock<u64> = OnceLock::new();
    // SAFETY: sysconf has no preconditions.
    *V.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_CLK_TCK) } {
        n if n > 0 => n as u64,
        _ => 100,
    })
}

fn page_size() -> u64 {
    static V: OnceLock<u64> = OnceLock::new();
    // SAFETY: sysconf has no preconditions.
    *V.get_or_init(|| match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        n if n > 0 => n as u64,
        _ => 4096,
    })
}

fn boot_time_secs() -> Option<u64> {
    let s = fs::read_to_string("/proc/stat").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

pub(super) fn probe(pid: i32) -> Option<Probe> {
    let st = read_stat(pid)?;
    Some(Probe {
        start_time: StartTime(st.starttime),
        zombie: matches!(st.state, 'Z' | 'X' | 'x'),
    })
}

pub(super) fn start_time_to_system_time(st: StartTime) -> Option<SystemTime> {
    let ms = boot_time_secs()? * 1000 + st.0 * 1000 / clk_tck();
    UNIX_EPOCH.checked_add(Duration::from_millis(ms))
}

fn numeric_dirs(path: &str) -> impl Iterator<Item = i32> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
}

pub(super) fn process_tree(pgid: i32) -> Vec<ProcInfo> {
    let tck = clk_tck();
    let page = page_size();
    numeric_dirs("/proc")
        .filter_map(|pid| {
            let st = read_stat(pid)?;
            if st.pgrp != pgid || matches!(st.state, 'Z' | 'X' | 'x') {
                return None;
            }
            Some(ProcInfo {
                pid,
                ppid: st.ppid,
                pgid,
                cpu_time_ms: (st.utime + st.stime) * 1000 / tck,
                rss_bytes: st.rss_pages.max(0) as u64 * page,
                command: st.comm,
            })
        })
        .collect()
}

/// `(port, inode)` of every LISTEN socket in `/proc/net/tcp{,6}`.
fn listen_sockets() -> Vec<(u16, u64)> {
    let mut v = Vec::new();
    for file in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        v.extend(text.lines().skip(1).filter_map(parse_tcp_line));
    }
    v
}

/// Parse one row of `/proc/net/tcp`: `sl local rem st tx:rx tr:when retrnsmt uid timeout inode ...`.
/// Returns `(local port, inode)` for LISTEN (`0A`) rows.
pub(crate) fn parse_tcp_line(line: &str) -> Option<(u16, u64)> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.get(3)? != &"0A" {
        return None;
    }
    let port = u16::from_str_radix(cols.get(1)?.rsplit_once(':')?.1, 16).ok()?;
    let inode = cols.get(9)?.parse().ok()?;
    Some((port, inode))
}

fn socket_inodes_of(pid: i32) -> Vec<u64> {
    let Ok(dir) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| {
            let target = fs::read_link(e.path()).ok()?;
            let t = target.to_str()?;
            t.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
        })
        .collect()
}

fn comm_of(pid: i32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim_end().to_string())
        .unwrap_or_default()
}

pub(super) fn listeners_on_port(port: u16) -> Vec<Listener> {
    let inodes: HashSet<u64> = listen_sockets()
        .into_iter()
        .filter(|(p, _)| *p == port)
        .map(|(_, i)| i)
        .collect();
    if inodes.is_empty() {
        return Vec::new();
    }
    numeric_dirs("/proc")
        .filter(|&pid| socket_inodes_of(pid).iter().any(|i| inodes.contains(i)))
        .map(|pid| Listener {
            pid,
            command: comm_of(pid),
        })
        .collect()
}

pub(super) fn command_line(pid: i32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    Some(args.join(" "))
}

pub(super) fn listening_ports(pids: &[i32]) -> Vec<u16> {
    let socks = listen_sockets();
    if socks.is_empty() {
        return Vec::new();
    }
    let mine: HashSet<u64> = pids.iter().flat_map(|&p| socket_inodes_of(p)).collect();
    socks
        .into_iter()
        .filter(|(_, i)| mine.contains(i))
        .map(|(p, _)| p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_spaces_and_parens_in_comm() {
        let line = "1234 (my (weird) cmd) S 1 1234 1234 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 1 0 5555 1000000 250 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        let st = parse_stat(line).unwrap();
        assert_eq!(st.comm, "my (weird) cmd");
        assert_eq!(st.state, 'S');
        assert_eq!(st.ppid, 1);
        assert_eq!(st.pgrp, 1234);
        assert_eq!(st.utime, 7);
        assert_eq!(st.stime, 3);
        assert_eq!(st.starttime, 5555);
        assert_eq!(st.rss_pages, 250);
    }

    #[test]
    fn tcp_line_listen_only() {
        let listen = "   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 424242 1 0000000000000000 100 0 0 10 0";
        assert_eq!(parse_tcp_line(listen), Some((8080, 424242)));
        let est = "   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 1 1 0000000000000000 20 4 30 10 -1";
        assert_eq!(parse_tcp_line(est), None);
    }
}
