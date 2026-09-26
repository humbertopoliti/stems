//! `ProcessRuntime`: native processes in their own session / process group.

use std::collections::HashMap;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use crate::os::{self, Signal, StartTime};
use crate::output::{OUTPUT_CHANNEL_CAPACITY, OutputLine, OutputStream, OutputStreamKind, pump};
use crate::runtime::{
    AdoptRecord, ExitStatus, Handle, HandleId, ProcessSpec, Runtime, RuntimeError, RuntimeFacts,
    StartSpec, StopOutcome,
};

/// Poll interval used while waiting for processes we cannot `wait()` on.
const POLL: Duration = Duration::from_millis(20);
/// How long to keep re-sending SIGKILL to stragglers after the grace period.
const KILL_SWEEP: Duration = Duration::from_secs(3);

type ExitRx = watch::Receiver<Option<ExitStatus>>;

/// Live, non-serialisable state for a handle.
struct Live {
    /// Exit status of the leader, published by its wait task. `None` for adopted handles.
    exit: Option<ExitRx>,
    /// Weak so that the channel closes once both pipe readers finish.
    output: Option<broadcast::WeakSender<OutputLine>>,
    /// Receiver created before the pipe readers started, handed to the first
    /// subscriber so it sees output from the very first line.
    first_rx: Option<broadcast::Receiver<OutputLine>>,
    dropped: Arc<AtomicU64>,
}

/// Runs [`ProcessSpec`]s. Cheap to share behind an `Arc`.
pub struct ProcessRuntime {
    next_id: AtomicU64,
    live: Arc<Mutex<HashMap<HandleId, Live>>>,
}

impl Default for ProcessRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRuntime {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            live: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn alloc_id(&self) -> HandleId {
        HandleId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, HashMap<HandleId, Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn exit_rx(&self, h: &Handle) -> Option<ExitRx> {
        self.registry().get(&h.id()).and_then(|l| l.exit.clone())
    }

    /// Output lines lost to slow subscribers of `h` so far.
    pub fn dropped_lines(&self, h: &Handle) -> u64 {
        self.registry()
            .get(&h.id())
            .map_or(0, |l| l.dropped.load(Ordering::Relaxed))
    }

    /// Leader state as far as we can tell without blocking.
    fn leader_alive(&self, h: &Handle, exit: Option<&ExitRx>) -> bool {
        match exit {
            Some(rx) => rx.borrow().is_none(),
            None => os::is_alive(h.pid(), h.start_time()),
        }
    }

    async fn start_process(&self, spec: &ProcessSpec) -> Result<Handle, RuntimeError> {
        let mut cmd = if spec.shell {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(&spec.command).args(&spec.args);
            c
        } else {
            let mut c = Command::new(&spec.command);
            c.args(&spec.args);
            c
        };
        cmd.current_dir(&spec.cwd);
        if spec.clear_env {
            cmd.env_clear();
        }
        cmd.envs(&spec.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        // SAFETY: setsid(2) is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|source| RuntimeError::SpawnFailed {
            command: spec.command.clone(),
            source,
        })?;
        let pid = child
            .id()
            .map(|p| p as i32)
            .ok_or_else(|| io::Error::other("spawned child has no pid (already reaped)"))?;
        // Must be read before anything else can reap the leader.
        let start_time = os::process_start_time(pid).unwrap_or_else(|| {
            tracing::warn!(pid, "could not read start time of freshly spawned process");
            StartTime(0)
        });
        let id = self.alloc_id();
        let handle = Handle::Process {
            id,
            pid,
            pgid: pid,
            start_time,
        };

        let (tx, first_rx) = broadcast::channel(OUTPUT_CHANNEL_CAPACITY);
        let weak = tx.downgrade();
        if let Some(out) = child.stdout.take() {
            tokio::spawn(pump(out, OutputStreamKind::Out, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(pump(err, OutputStreamKind::Err, tx));
        }

        let (exit_tx, exit_rx) = watch::channel(None);
        tokio::spawn(async move {
            let status = match child.wait().await {
                Ok(s) => ExitStatus {
                    code: s.code(),
                    signal: s.signal(),
                },
                Err(e) => {
                    tracing::warn!(pid, error = %e, "waiting for child failed");
                    ExitStatus::UNKNOWN
                }
            };
            tracing::debug!(pid, ?status, "process leader exited");
            exit_tx.send_replace(Some(status));
        });

        self.registry().insert(
            id,
            Live {
                exit: Some(exit_rx),
                output: Some(weak),
                first_rx: Some(first_rx),
                dropped: Arc::new(AtomicU64::new(0)),
            },
        );
        tracing::debug!(pid, command = %spec.command, "process started");
        Ok(handle)
    }

    /// Wait until the leader has exited or `deadline` passes. True if it exited.
    async fn wait_leader_until(
        &self,
        h: &Handle,
        exit: Option<&ExitRx>,
        deadline: Instant,
    ) -> bool {
        match exit {
            Some(rx) => {
                let mut rx = rx.clone();
                tokio::time::timeout_at(deadline, rx.wait_for(Option::is_some))
                    .await
                    .is_ok()
            }
            None => loop {
                if !os::is_alive(h.pid(), h.start_time()) {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(POLL).await;
            },
        }
    }
}

/// Any live (non-zombie) process left in the group?
fn group_alive(pgid: i32) -> bool {
    os::group_exists(pgid) && !os::process_tree(pgid).is_empty()
}

async fn wait_group_empty_until(pgid: i32, deadline: Instant) -> bool {
    loop {
        if !group_alive(pgid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

fn signal_group(pgid: i32, sig: Signal) -> io::Result<()> {
    match os::kill_group(pgid, Some(sig)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[async_trait::async_trait]
impl Runtime for ProcessRuntime {
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError> {
        match spec {
            StartSpec::Process(p) => self.start_process(p).await,
            StartSpec::External { stem } => Err(RuntimeError::Unsupported(format!(
                "`{stem}` is an external stem; the process runtime does not start it"
            ))),
        }
    }

    async fn stop(&self, h: &Handle, grace: Duration) -> Result<StopOutcome, RuntimeError> {
        let pgid = h.pgid();
        let exit = self.exit_rx(h);
        let leader_alive = self.leader_alive(h, exit.as_ref());
        if !leader_alive {
            // pid-reuse defence: if the leader pid now belongs to a different
            // process, our group is necessarily gone (a pid is never reused while
            // its process group exists) and `pgid` may name someone else's group.
            if exit.is_none()
                && os::process_start_time(h.pid()).is_some_and(|st| st != h.start_time())
            {
                return Ok(StopOutcome::AlreadyDead);
            }
            if !group_alive(pgid) {
                return Ok(StopOutcome::AlreadyDead);
            }
        }

        let deadline = Instant::now() + grace;
        tracing::debug!(pgid, ?grace, "stop: SIGTERM to group");
        signal_group(pgid, Signal::SIGTERM)?;
        let leader_gone = self.wait_leader_until(h, exit.as_ref(), deadline).await;
        if leader_gone && wait_group_empty_until(pgid, deadline).await {
            return Ok(StopOutcome::Graceful);
        }

        tracing::debug!(pgid, "stop: grace elapsed, SIGKILL to group");
        signal_group(pgid, Signal::SIGKILL)?;
        let sweep_deadline = Instant::now() + KILL_SWEEP;
        self.wait_leader_until(h, exit.as_ref(), sweep_deadline)
            .await;
        // Sweep: anything still in the group (e.g. forked while we were killing) gets
        // SIGKILL again, until the group is empty or the sweep deadline passes.
        while group_alive(pgid) && Instant::now() < sweep_deadline {
            signal_group(pgid, Signal::SIGKILL)?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if group_alive(pgid) {
            tracing::warn!(pgid, "processes survived SIGKILL sweep");
        }
        Ok(StopOutcome::Killed)
    }

    async fn is_alive(&self, h: &Handle) -> bool {
        let exit = self.exit_rx(h);
        if exit.as_ref().is_some_and(|rx| rx.borrow().is_some()) {
            return false;
        }
        os::is_alive(h.pid(), h.start_time())
    }

    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError> {
        let pgid = h.pgid();
        let (children, ports) = tokio::task::spawn_blocking(move || {
            let children = os::process_tree(pgid);
            let pids: Vec<i32> = children.iter().map(|p| p.pid).collect();
            let ports = os::listening_ports(&pids);
            (children, ports)
        })
        .await
        .map_err(io::Error::other)?;
        Ok(RuntimeFacts {
            pid: h.pid(),
            pgid,
            start_time: h.start_time(),
            children,
            container_id: None,
            ports,
            dropped_lines: self.dropped_lines(h),
        })
    }

    fn output_stream(&self, h: &Handle) -> Option<OutputStream> {
        let mut reg = self.registry();
        let live = reg.get_mut(&h.id())?;
        let rx = match live.first_rx.take() {
            Some(rx) => rx,
            None => live.output.as_ref()?.upgrade()?.subscribe(),
        };
        Some(OutputStream::new(rx, live.dropped.clone()))
    }

    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError> {
        match (self.exit_rx(h), h) {
            (Some(mut rx), _) => Ok(match rx.wait_for(Option::is_some).await {
                Ok(s) => s.unwrap_or(ExitStatus::UNKNOWN),
                Err(_) => ExitStatus::UNKNOWN,
            }),
            (
                None,
                Handle::Adopted {
                    pid, start_time, ..
                },
            ) => {
                while os::is_alive(*pid, *start_time) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(ExitStatus::UNKNOWN)
            }
            (None, Handle::Process { id, .. } | Handle::External { id }) => {
                Err(RuntimeError::NotFound(*id))
            }
        }
    }

    async fn adopt(&self, record: &AdoptRecord) -> Option<Handle> {
        if record.container_id.is_some() || !os::is_alive(record.pid, record.start_time) {
            return None;
        }
        let actual_pgid = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(record.pid)))
            .ok()?
            .as_raw();
        if actual_pgid != record.pgid {
            tracing::debug!(
                pid = record.pid,
                expected = record.pgid,
                actual_pgid,
                "adopt: pgid mismatch"
            );
            return None;
        }
        let id = self.alloc_id();
        self.registry().insert(
            id,
            Live {
                exit: None,
                output: None,
                first_rx: None,
                dropped: Arc::new(AtomicU64::new(0)),
            },
        );
        Some(Handle::Adopted {
            id,
            pid: record.pid,
            pgid: record.pgid,
            start_time: record.start_time,
        })
    }

    fn release(&self, h: &Handle) {
        self.registry().remove(&h.id());
    }
}
