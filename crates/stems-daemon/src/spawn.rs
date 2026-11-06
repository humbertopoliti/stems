//! Starting a detached daemon from the CLI and waiting for its socket.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use stems_core::{Error, ErrorCode};

use crate::paths::DaemonPaths;

/// `daemon --home <home> --workspace <workspace> [--foreground]`: the
/// argument list the CLI's hidden `daemon` subcommand accepts.
pub fn daemon_args(home: &Path, workspace: Option<&Path>, foreground: bool) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["daemon".into(), "--home".into(), home.into()];
    if let Some(ws) = workspace {
        v.push("--workspace".into());
        v.push(ws.into());
    }
    if foreground {
        v.push("--foreground".into());
    }
    v
}

/// Start `<binary> <args>` detached: its own session (`setsid`), cwd `/`, stdin from
/// `/dev/null`, stdout/stderr appended to the daemon log
/// ([`DaemonPaths::log_path`]). Returns the child's pid. The child is reaped
/// by a background thread so it never lingers as a zombie of the CLI.
pub fn spawn_detached(binary: &Path, args: &[OsString], paths: &DaemonPaths) -> Result<u32, Error> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;

    paths.check_socket_len()?;
    paths.ensure_dir().map_err(|e| {
        spawn_err(
            binary,
            format!("cannot create {}: {e}", paths.dir.display()),
        )
    })?;
    let log_path = paths.log_path();
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|e| spawn_err(binary, format!("cannot open {}: {e}", log_path.display())))?;
    if let (Ok(meta), Ok(mut m)) = (log.metadata(), spawn_offsets().lock()) {
        m.insert(log_path.clone(), meta.len());
    }
    let log2 = log
        .try_clone()
        .map_err(|e| spawn_err(binary, e.to_string()))?;
    let mut cmd = Command::new(binary);
    // cwd `/`: a daemon must not pin (or be found by scanning) whatever
    // directory the CLI happened to run in; callers pass absolute paths.
    cmd.args(args)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| spawn_err(binary, format!("cannot start daemon: {e}")))?;
    let pid = child.id();
    std::thread::Builder::new()
        .name("stemsd-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|e| spawn_err(binary, e.to_string()))?;
    Ok(pid)
}

fn spawn_err(binary: &Path, msg: String) -> Error {
    Error::new(ErrorCode::DaemonNotRunning, msg)
        .with_hint("check that the stems binary is executable and STEMS_HOME is writable")
        .with_details(json!({ "binary": binary }))
}

/// Log length at the last [`spawn_detached`] per log file: the log is opened
/// in append mode, so only bytes after this offset were written by the daemon
/// just spawned.
fn spawn_offsets() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, u64>> {
    static OFFSETS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, u64>>> =
        std::sync::OnceLock::new();
    OFFSETS.get_or_init(Default::default)
}

/// The last line of `log` after byte `from` (at most 8 KB of it) that looks
/// like an error, else its last line. `None` when nothing was written there.
fn last_error_line(log: &Path, from: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(log).ok()?;
    let len = f.metadata().ok()?.len();
    // A truncated or rotated log restarts at 0.
    let from = if from > len { 0 } else { from };
    f.seek(SeekFrom::Start(len.saturating_sub(8192).max(from)))
        .ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pick = lines
        .iter()
        .rev()
        .find(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("error") || l.contains("cannot") || l.contains("failed")
        })
        .or(lines.last())?;
    Some(pick.chars().take(300).collect())
}

/// Wait until something accepts connections on the daemon socket.
/// `DAEMON_NOT_RUNNING` (naming the log file) on timeout.
pub async fn wait_for_socket(paths: &DaemonPaths, timeout: Duration) -> Result<(), Error> {
    let from = spawn_offsets()
        .lock()
        .ok()
        .and_then(|m| m.get(&paths.log_path()).copied())
        .unwrap_or(0);
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(5);
    loop {
        if tokio::net::UnixStream::connect(&paths.socket).await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let log = paths.log_path();
            return Err(Error::new(
                ErrorCode::DaemonNotRunning,
                format!(
                    "the daemon did not start listening within {}ms",
                    timeout.as_millis()
                ),
            )
            .with_hint(match last_error_line(&log, from) {
                Some(l) => format!("the daemon log ({}) ends with: {l}", log.display()),
                None => format!("the daemon wrote nothing to its log ({})", log.display()),
            })
            .with_details(json!({ "socket": paths.socket, "log": log })));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_shape() {
        let a = daemon_args(Path::new("/h"), Some(Path::new("/w")), true);
        assert_eq!(
            a,
            [
                "daemon",
                "--home",
                "/h",
                "--workspace",
                "/w",
                "--foreground"
            ]
            .map(OsString::from)
            .to_vec()
        );
    }

    #[tokio::test]
    async fn spawn_detached_new_session_and_log() {
        let d = tempfile::tempdir().unwrap();
        let paths = DaemonPaths::in_dir(d.path().join("ws"));
        let pid = spawn_detached(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "echo started; ps -o sess= -p $$ >/dev/null; exit 0".into(),
            ],
            &paths,
        )
        .unwrap();
        assert!(pid > 0);
        // setsid made it a session leader: its sid equals its pid (checked
        // while it may still be alive; tolerate it having exited).
        if let Ok(sid) = nix::unistd::getsid(Some(nix::unistd::Pid::from_raw(pid as i32))) {
            assert_eq!(sid.as_raw(), pid as i32);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = std::fs::read_to_string(&paths.log).unwrap_or_default();
            if s.contains("started") {
                break;
            }
            assert!(Instant::now() < deadline, "log never written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn wait_for_socket_times_out() {
        let d = tempfile::tempdir().unwrap();
        let paths = DaemonPaths::in_dir(d.path());
        let e = wait_for_socket(&paths, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::DaemonNotRunning);
        assert!(e.hint.unwrap().contains("stemsd.log"));
    }
}
