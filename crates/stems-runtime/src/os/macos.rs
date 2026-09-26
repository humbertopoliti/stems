//! macOS implementation: libproc for process facts, `lsof` for sockets.

use std::os::raw::c_char;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use libproc::bsd_info::BSDInfo;
use libproc::proc_pid::pidinfo;
use libproc::processes::{ProcFilter, pids_by_type};
use libproc::task_info::TaskInfo;

use super::{Listener, Probe, ProcInfo, StartTime, port_from_addr};

/// `SZOMB` from `<sys/proc.h>`.
const SZOMB: u32 = 5;

fn bsd_info(pid: i32) -> Option<BSDInfo> {
    let info = pidinfo::<BSDInfo>(pid, 0).ok()?;
    (info.pbi_pid as i32 == pid).then_some(info)
}

fn start_of(info: &BSDInfo) -> StartTime {
    StartTime(info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000)
}

pub(super) fn probe(pid: i32) -> Option<Probe> {
    let info = bsd_info(pid)?;
    Some(Probe {
        start_time: start_of(&info),
        zombie: info.pbi_status == SZOMB,
    })
}

pub(super) fn start_time_to_system_time(st: StartTime) -> Option<SystemTime> {
    UNIX_EPOCH.checked_add(Duration::from_millis(st.0))
}

fn c_name(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn command_of(info: &BSDInfo) -> String {
    let name = c_name(&info.pbi_name);
    if name.is_empty() {
        c_name(&info.pbi_comm)
    } else {
        name
    }
}

/// Mach absolute-time units -> nanoseconds ratio (1/1 on Intel, 125/3 on Apple silicon).
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

fn timebase() -> (u64, u64) {
    static TB: OnceLock<(u64, u64)> = OnceLock::new();
    *TB.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: plain FFI call writing into a stack struct we own.
        let rc = unsafe { mach_timebase_info(&mut info) };
        if rc != 0 || info.numer == 0 || info.denom == 0 {
            (1, 1)
        } else {
            (u64::from(info.numer), u64::from(info.denom))
        }
    })
}

fn mach_to_ms(t: u64) -> u64 {
    let (numer, denom) = timebase();
    let ns = u128::from(t) * u128::from(numer) / u128::from(denom);
    (ns / 1_000_000) as u64
}

pub(super) fn process_tree(pgid: i32) -> Vec<ProcInfo> {
    let Ok(pids) = pids_by_type(ProcFilter::ByProgramGroup {
        pgrpid: pgid as u32,
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pid in pids {
        let pid = pid as i32;
        if pid <= 0 {
            continue;
        }
        let Some(info) = bsd_info(pid) else { continue };
        if info.pbi_pgid as i32 != pgid || info.pbi_status == SZOMB {
            continue;
        }
        let (cpu_time_ms, rss_bytes) = match pidinfo::<TaskInfo>(pid, 0) {
            Ok(t) => (
                mach_to_ms(t.pti_total_user + t.pti_total_system),
                t.pti_resident_size,
            ),
            Err(_) => (0, 0),
        };
        out.push(ProcInfo {
            pid,
            ppid: info.pbi_ppid as i32,
            pgid,
            cpu_time_ms,
            rss_bytes,
            command: command_of(&info),
        });
    }
    out
}

fn lsof(args: &[&str]) -> String {
    let bin = if std::path::Path::new("/usr/sbin/lsof").exists() {
        "/usr/sbin/lsof"
    } else {
        "lsof"
    };
    match Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        // lsof exits 1 when nothing matched; its stdout is still authoritative.
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to run lsof");
            String::new()
        }
    }
}

pub(super) fn listeners_on_port(port: u16) -> Vec<Listener> {
    let filter = format!("-iTCP:{port}");
    let out = lsof(&["-nP", &filter, "-sTCP:LISTEN", "-Fpc"]);
    let mut v = Vec::new();
    let mut cur: Option<Listener> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix('p') {
            if let Some(l) = cur.take() {
                v.push(l);
            }
            if let Ok(pid) = p.parse() {
                cur = Some(Listener {
                    pid,
                    command: String::new(),
                });
            }
        } else if let Some(c) = line.strip_prefix('c')
            && let Some(l) = cur.as_mut()
        {
            l.command = c.to_string();
        }
    }
    v.extend(cur);
    v
}

pub(super) fn command_line(pid: i32) -> Option<String> {
    let out = Command::new("/bin/ps")
        .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(super) fn listening_ports(pids: &[i32]) -> Vec<u16> {
    let list = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let out = lsof(&["-nP", "-a", "-p", &list, "-iTCP", "-sTCP:LISTEN", "-Fn"]);
    out.lines()
        .filter_map(|l| l.strip_prefix('n'))
        .filter_map(port_from_addr)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn c_name_stops_at_nul() {
        let buf: Vec<std::os::raw::c_char> = b"sleep\0junk".iter().map(|&b| b as _).collect();
        assert_eq!(super::c_name(&buf), "sleep");
    }
}
