//! The `Runtime` abstraction shared by the process (and later docker/compose) runtimes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::os::{ProcInfo, StartTime};
use crate::output::OutputStream;

/// What to start. Docker/compose variants are added by later deliverables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartSpec {
    Process(ProcessSpec),
    /// A monitor-only stem ([`crate::ExternalRuntime`]): nothing is spawned.
    External {
        /// Stem name.
        stem: String,
    },
}

/// A native process to run in its own session / process group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpec {
    /// Program (or, with `shell`, the script passed to `/bin/sh -c`).
    pub command: String,
    /// Extra arguments. With `shell`, they become `$0`, `$1`, ... of the script.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub shell: bool,
    pub cwd: PathBuf,
    /// Variables added to (or, with `clear_env`, replacing) the inherited environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub clear_env: bool,
}

impl ProcessSpec {
    /// `/bin/sh -c <script>` in `cwd` with the inherited environment.
    pub fn shell(script: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            command: script.into(),
            args: Vec::new(),
            shell: true,
            cwd: cwd.into(),
            env: BTreeMap::new(),
            clear_env: false,
        }
    }
}

/// Runtime-local identifier of a started or adopted unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HandleId(pub u64);

impl std::fmt::Display for HandleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "h{}", self.0)
    }
}

/// Identifying facts of a running unit. Cheap to clone and serialisable; the
/// live state (child, pipes, wait task) stays inside the runtime's registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Handle {
    /// Spawned by this runtime instance: output is captured and the exit status known.
    Process {
        id: HandleId,
        pid: i32,
        pgid: i32,
        start_time: StartTime,
    },
    /// Re-attached after a daemon restart: not our child, so no output stream and
    /// no exit status (only liveness).
    Adopted {
        id: HandleId,
        pid: i32,
        pgid: i32,
        start_time: StartTime,
    },
    /// A monitor-only stem ([`crate::ExternalRuntime`]): there is no
    /// process, so `pid`/`pgid` are `0` (never signalled: the OS helpers
    /// refuse pids/groups `<= 1`) and it is never persisted or adopted.
    External { id: HandleId },
}

impl Handle {
    pub fn id(&self) -> HandleId {
        match self {
            Handle::Process { id, .. } | Handle::Adopted { id, .. } | Handle::External { id } => {
                *id
            }
        }
    }
    pub fn pid(&self) -> i32 {
        match self {
            Handle::Process { pid, .. } | Handle::Adopted { pid, .. } => *pid,
            Handle::External { .. } => 0,
        }
    }
    pub fn pgid(&self) -> i32 {
        match self {
            Handle::Process { pgid, .. } | Handle::Adopted { pgid, .. } => *pgid,
            Handle::External { .. } => 0,
        }
    }
    pub fn start_time(&self) -> StartTime {
        match self {
            Handle::Process { start_time, .. } | Handle::Adopted { start_time, .. } => *start_time,
            Handle::External { .. } => StartTime(0),
        }
    }
    pub fn is_adopted(&self) -> bool {
        matches!(self, Handle::Adopted { .. })
    }
    /// A monitor-only stem's handle (no process).
    pub fn is_external(&self) -> bool {
        matches!(self, Handle::External { .. })
    }
}

/// How a stop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    /// The whole group exited within the grace period after SIGTERM.
    Graceful,
    /// SIGKILL was needed.
    Killed,
    /// Nothing of ours was running when stop was called.
    AlreadyDead,
}

/// How the leader process ended. Both `None` means unknown (e.g. adopted process).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitStatus {
    pub const UNKNOWN: ExitStatus = ExitStatus {
        code: None,
        signal: None,
    };

    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Facts needed for status, metrics and crash recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeFacts {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: StartTime,
    /// Every live process of the group (leader included).
    pub children: Vec<ProcInfo>,
    pub container_id: Option<String>,
    /// TCP ports any process of the group is listening on.
    pub ports: Vec<u16>,
    /// Output lines lost to slow subscribers so far.
    pub dropped_lines: u64,
}

/// What the state file remembers about a unit, used to re-attach after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptRecord {
    pub pid: i32,
    pub pgid: i32,
    pub start_time: StartTime,
    #[serde(default)]
    pub container_id: Option<String>,
}

/// What an orphan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrphanKind {
    /// A process listening on a port the workspace declares.
    Process,
    /// A container labelled for this workspace (deliverable 14).
    Container,
}

/// Something running that belongs to (or squats on) this workspace but is
/// not recorded in the daemon's state (FR-CR-4). See `docs/recovery.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orphan {
    pub kind: OrphanKind,
    /// The declared host port it listens on (processes).
    pub port: Option<u16>,
    pub pid: Option<i32>,
    pub pgid: Option<i32>,
    /// Full command line (processes) or image/name (containers).
    pub command: String,
    pub container_id: Option<String>,
    /// The stem whose port (or labels) it matches.
    pub stem: Option<String>,
    /// Heuristic: it looks like that stem's own start command, i.e. a stem
    /// started outside stems or by a previous run whose state was lost.
    /// Only these are killed by `--yes`.
    pub matches_start_command: bool,
}

/// What a runtime needs to know to look for its own orphans.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanScope {
    /// Workspace name (container label `stems.workspace`).
    pub workspace: String,
    /// Units recorded in state (never orphans).
    pub known: Vec<AdoptRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("failed to spawn `{command}`: {source}")]
    SpawnFailed {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("unknown runtime handle {0}")]
    NotFound(HandleId),
    /// The spec or handle belongs to another runtime.
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A way of running units (native processes today; containers later).
#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    /// Start a unit and return its handle as soon as it is spawned.
    async fn start(&self, spec: &StartSpec) -> Result<Handle, RuntimeError>;

    /// SIGTERM the group, wait up to `grace`, then SIGKILL whatever is left.
    async fn stop(&self, h: &Handle, grace: Duration) -> Result<StopOutcome, RuntimeError>;

    /// Is the leader still running (and still the same process)?
    async fn is_alive(&self, h: &Handle) -> bool;

    /// Current pid/pgid/start time, process tree and listening ports.
    async fn describe(&self, h: &Handle) -> Result<RuntimeFacts, RuntimeError>;

    /// Subscribe to output. The first subscription after `start` sees output from
    /// the very beginning (up to the channel capacity); later ones only new lines.
    /// `None` for adopted or unknown handles.
    fn output_stream(&self, h: &Handle) -> Option<OutputStream>;

    /// Resolves when the leader has exited.
    async fn wait(&self, h: &Handle) -> Result<ExitStatus, RuntimeError>;

    /// Re-attach to a unit from a previous daemon run if it is still the same
    /// process (alive *and* same start time). Returns a `Handle::Adopted`.
    async fn adopt(&self, record: &AdoptRecord) -> Option<Handle>;

    /// Forget runtime bookkeeping for a handle whose unit has exited/been stopped.
    fn release(&self, h: &Handle);

    /// Units of this runtime that belong to the workspace but are not in
    /// `scope.known` (containers labelled for it: deliverable 14). Process
    /// orphans are found by port in `stems_daemon::orphans`, so the process
    /// runtime keeps this default.
    async fn scan_orphans(&self, _scope: &OrphanScope) -> Vec<Orphan> {
        Vec::new()
    }
}
