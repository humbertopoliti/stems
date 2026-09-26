//! The single-daemon-per-workspace lock (FR-CR-5).
//!
//! `stemsd.lock` holds `{pid, start_time, version, created_at}` as JSON. A lock
//! whose pid is dead or whose start time differs (pid reuse) is stale and is
//! reclaimed. The file is created exclusively (written to a temp file, then
//! hard-linked into place, which fails if a lock already exists), so two
//! daemons racing for the same workspace cannot both win.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use stems_core::{Error, ErrorCode};
use stems_runtime::os::{StartTime, is_alive, process_start_time};

use crate::paths::DaemonPaths;

/// Contents of `stemsd.lock`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// Daemon pid.
    pub pid: i32,
    /// Daemon process start time (pid-reuse defence).
    pub start_time: StartTime,
    /// stems version of the daemon.
    pub version: String,
    /// When the lock was taken.
    pub created_at: DateTime<Utc>,
}

impl Lock {
    /// A lock for the current process.
    pub fn for_current_process() -> Result<Self, Error> {
        let pid = std::process::id() as i32;
        let start_time = process_start_time(pid)
            .ok_or_else(|| Error::internal("cannot read this process's start time"))?;
        Ok(Self {
            pid,
            start_time,
            version: stems_api::VERSION.to_string(),
            created_at: Utc::now(),
        })
    }
}

/// What [`probe`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    /// No lock file.
    Free,
    /// A live daemon holds it.
    Held {
        /// Its pid.
        pid: i32,
        /// Its version.
        version: String,
    },
    /// A lock file exists but its owner is gone (or the file is unreadable garbage; `pid` 0).
    Stale {
        /// The dead owner's pid (0 when the file could not be parsed).
        pid: i32,
    },
}

/// Read the lock file, if any.
pub fn read(path: &Path) -> Option<Result<Lock, String>> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e.to_string())),
        Ok(s) => Some(serde_json::from_str(&s).map_err(|e| e.to_string())),
    }
}

/// Classify the lock with an injectable liveness check.
pub fn probe_with(paths: &DaemonPaths, alive: impl Fn(i32, StartTime) -> bool) -> LockState {
    match read(&paths.lock) {
        None => LockState::Free,
        Some(Err(_)) => LockState::Stale { pid: 0 },
        Some(Ok(l)) if alive(l.pid, l.start_time) => LockState::Held {
            pid: l.pid,
            version: l.version,
        },
        Some(Ok(l)) => LockState::Stale { pid: l.pid },
    }
}

/// Classify the lock of a workspace (for `DAEMON_NOT_RUNNING` hints: "stale lock").
pub fn probe(paths: &DaemonPaths) -> LockState {
    probe_with(paths, is_alive)
}

/// Holds the lock; removes the file on drop (only if it is still ours).
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    lock: Lock,
    /// Pid of the stale owner whose lock was reclaimed, if any.
    pub reclaimed_from: Option<i32>,
}

impl LockGuard {
    /// The lock we wrote.
    pub fn lock(&self) -> &Lock {
        &self.lock
    }

    /// Remove the lock file now (idempotent).
    pub fn release(&self) {
        if let Some(Ok(l)) = read(&self.path)
            && l.pid == self.lock.pid
            && l.start_time == self.lock.start_time
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// Take the workspace lock for the current process.
pub fn acquire(paths: &DaemonPaths) -> Result<LockGuard, Error> {
    acquire_as(paths, Lock::for_current_process()?, is_alive)
}

/// [`acquire`] with an explicit lock body and liveness check (tests).
pub fn acquire_as(
    paths: &DaemonPaths,
    ours: Lock,
    alive: impl Fn(i32, StartTime) -> bool,
) -> Result<LockGuard, Error> {
    paths
        .ensure_dir()
        .map_err(|e| io_err(&paths.dir, "create daemon directory", e))?;
    let body = serde_json::to_string_pretty(&ours).map_err(|e| Error::internal(e.to_string()))?;
    let tmp = paths
        .dir
        .join(format!(".stemsd.lock.{}.tmp", std::process::id()));
    write_private(&tmp, body.as_bytes()).map_err(|e| io_err(&tmp, "write lock", e))?;
    let mut reclaimed_from = None;
    let result = (|| {
        for _ in 0..5 {
            match std::fs::hard_link(&tmp, &paths.lock) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err(&paths.lock, "create lock", e)),
            }
            match read(&paths.lock) {
                None => continue, // vanished meanwhile: retry
                Some(Ok(l)) if alive(l.pid, l.start_time) => {
                    return Err(Error::new(
                        ErrorCode::LockHeld,
                        format!(
                            "another stems daemon (pid {}, version {}) holds this workspace's lock",
                            l.pid, l.version
                        ),
                    )
                    .with_hint("use the running daemon, or stop it first: `stems daemon stop`")
                    .with_details(json!({
                        "pid": l.pid,
                        "version": l.version,
                        "lock": paths.lock,
                    })));
                }
                Some(stale) => {
                    let pid = stale.map(|l| l.pid).unwrap_or(0);
                    tracing::warn!(pid, lock = %paths.lock.display(), "stale lock reclaimed");
                    reclaimed_from = Some(pid);
                    match std::fs::remove_file(&paths.lock) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(io_err(&paths.lock, "remove stale lock", e)),
                    }
                }
            }
        }
        Err(Error::new(
            ErrorCode::LockHeld,
            "could not take the workspace lock (another daemon keeps racing for it)",
        )
        .with_hint("retry, or run `stems daemon stop`"))
    })();
    let _ = std::fs::remove_file(&tmp);
    result?;
    Ok(LockGuard {
        path: paths.lock.clone(),
        lock: ours,
        reclaimed_from,
    })
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn io_err(path: &Path, what: &str, e: std::io::Error) -> Error {
    Error::new(
        ErrorCode::Internal,
        format!("cannot {what} at {}: {e}", path.display()),
    )
    .with_hint("check that the stems home directory is writable (STEMS_HOME)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, DaemonPaths) {
        let d = tempfile::tempdir().unwrap();
        let p = DaemonPaths::in_dir(d.path().join("ws"));
        (d, p)
    }

    fn lock_for(pid: i32, st: StartTime) -> Lock {
        Lock {
            pid,
            start_time: st,
            version: "0.0.9".into(),
            created_at: Utc::now(),
        }
    }

    fn write_lock(p: &DaemonPaths, l: &Lock) {
        p.ensure_dir().unwrap();
        std::fs::write(&p.lock, serde_json::to_string(l).unwrap()).unwrap();
    }

    #[test]
    fn acquire_free_writes_and_drop_removes() {
        let (_d, p) = paths();
        assert_eq!(probe(&p), LockState::Free);
        let g = acquire(&p).unwrap();
        assert!(g.reclaimed_from.is_none());
        let on_disk = read(&p.lock).unwrap().unwrap();
        assert_eq!(on_disk.pid, std::process::id() as i32);
        assert_eq!(on_disk.version, stems_api::VERSION);
        assert!(matches!(probe(&p), LockState::Held { pid, .. } if pid == on_disk.pid));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&p.lock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(g);
        assert!(!p.lock.exists());
        // no temp files left behind
        assert_eq!(std::fs::read_dir(&p.dir).unwrap().count(), 0);
    }

    #[test]
    fn held_by_live_process() {
        let (_d, p) = paths();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let st = process_start_time(pid).unwrap();
        write_lock(&p, &lock_for(pid, st));
        let err = acquire(&p).unwrap_err();
        assert_eq!(err.code, ErrorCode::LockHeld);
        assert_eq!(err.details["pid"], json!(pid));
        assert_eq!(
            probe(&p),
            LockState::Held {
                pid,
                version: "0.0.9".into()
            }
        );
        // the foreign lock is untouched
        assert_eq!(read(&p.lock).unwrap().unwrap().pid, pid);
        child.kill().unwrap();
        child.wait().unwrap();
        // now it is stale and reclaimable
        assert_eq!(probe(&p), LockState::Stale { pid });
        let g = acquire(&p).unwrap();
        assert_eq!(g.reclaimed_from, Some(pid));
    }

    #[test]
    fn reclaims_dead_pid() {
        let (_d, p) = paths();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id() as i32;
        child.wait().unwrap();
        write_lock(&p, &lock_for(pid, StartTime(1)));
        assert_eq!(probe(&p), LockState::Stale { pid });
        let g = acquire(&p).unwrap();
        assert_eq!(g.reclaimed_from, Some(pid));
        assert_eq!(
            read(&p.lock).unwrap().unwrap().pid,
            std::process::id() as i32
        );
    }

    #[test]
    fn reclaims_reused_pid_with_wrong_start_time() {
        let (_d, p) = paths();
        let me = std::process::id() as i32;
        let st = process_start_time(me).unwrap();
        write_lock(&p, &lock_for(me, StartTime(st.0 + 1)));
        assert_eq!(probe(&p), LockState::Stale { pid: me });
        let g = acquire(&p).unwrap();
        assert_eq!(g.reclaimed_from, Some(me));
    }

    #[test]
    fn garbage_lock_is_stale() {
        let (_d, p) = paths();
        p.ensure_dir().unwrap();
        std::fs::write(&p.lock, "not json").unwrap();
        assert_eq!(probe(&p), LockState::Stale { pid: 0 });
        let g = acquire(&p).unwrap();
        assert_eq!(g.reclaimed_from, Some(0));
    }

    #[test]
    fn fake_probe_decides() {
        let (_d, p) = paths();
        write_lock(&p, &lock_for(12345, StartTime(99)));
        assert!(matches!(
            probe_with(&p, |_, _| true),
            LockState::Held { pid: 12345, .. }
        ));
        assert_eq!(
            probe_with(&p, |_, _| false),
            LockState::Stale { pid: 12345 }
        );
        let ours = lock_for(1, StartTime(1));
        assert_eq!(
            acquire_as(&p, ours.clone(), |_, _| true).unwrap_err().code,
            ErrorCode::LockHeld
        );
        let g = acquire_as(&p, ours, |_, _| false).unwrap();
        assert_eq!(g.reclaimed_from, Some(12345));
    }

    #[test]
    fn guard_does_not_remove_someone_elses_lock() {
        let (_d, p) = paths();
        let g = acquire(&p).unwrap();
        write_lock(&p, &lock_for(12345, StartTime(99)));
        drop(g);
        assert!(p.lock.exists());
    }
}
