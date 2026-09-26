//! The durable state file (FR-CR-1, deliverable 11): what the daemon runs,
//! so a new daemon can adopt it after a crash (`docs/recovery.md`).
//!
//! `<home>/<ws-hash>/state.json` ([`DaemonPaths::state`](crate::DaemonPaths)):
//!
//! ```json
//! { "version": 1, "run_id": "<ULID>", "daemon": { "pid": 1, "start_time": 1 },
//!   "stems": { "<name>": { "pid", "pgid", "start_time", "container_id",
//!              "ports": [{"name", "port", "auto"}], "overlays": [OverlayRecord],
//!              "state", "started_at", "log_file" } },
//!   "stamps": { "<stem>": { "<script>": Stamp } },
//!   "overlays": { "<stem>": [OverlayRecord] } }
//! ```
//!
//! [`StateStore`] keeps the current [`StateFile`] in memory and rewrites the
//! file atomically (temp file in the same directory, `fsync`, `rename`,
//! `fsync` of the directory) on every change, so a reader — or a daemon
//! started after `kill -9` — sees either the previous or the next complete
//! document, never a torn one. A leftover temp file is ignored.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use stems_core::StemState;
use stems_core::overlays::OverlayRecord;
use stems_core::stamps::StampStore;
use stems_runtime::os::StartTime;
use stems_runtime::{AdoptRecord, os};

/// Schema version of `state.json`.
pub const STATE_VERSION: u32 = 1;

/// The daemon that wrote the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRecord {
    /// Daemon pid.
    pub pid: u32,
    /// Its start time (see [`StartTime`]).
    pub start_time: u64,
}

/// One host port of a running stem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRecord {
    /// Port name.
    pub name: String,
    /// Host port.
    pub port: u16,
    /// Allocated for `port: auto` (restored as sticky on adoption).
    pub auto: bool,
}

/// A running stem, as persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StemRecord {
    /// Leader pid.
    pub pid: i32,
    /// Process group (signals go to `-pgid`).
    pub pgid: i32,
    /// Leader start time (pid-reuse defence).
    pub start_time: StartTime,
    /// Container id (docker/compose stems, deliverable 14).
    #[serde(default)]
    pub container_id: Option<String>,
    /// Host ports.
    #[serde(default)]
    pub ports: Vec<PortRecord>,
    /// Materialised overlays (deliverable 18).
    #[serde(default)]
    pub overlays: Vec<OverlayRecord>,
    /// Last known state.
    pub state: StemState,
    /// When it was started.
    pub started_at: DateTime<Utc>,
    /// Where its output is logged (deliverable 12), so adopted stems keep their history.
    #[serde(default)]
    pub log_file: Option<PathBuf>,
}

impl StemRecord {
    /// What the runtime needs to verify and re-attach.
    pub fn adopt_record(&self) -> AdoptRecord {
        AdoptRecord {
            pid: self.pid,
            pgid: self.pgid,
            start_time: self.start_time,
            container_id: self.container_id.clone(),
        }
    }

    /// Alive and still the same process (pid + start time)? Containers are
    /// verified by their runtime (14), so they count as alive here.
    pub fn is_alive(&self) -> bool {
        self.container_id.is_some() || os::is_alive(self.pid, self.start_time)
    }
}

/// The whole document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    /// [`STATE_VERSION`].
    pub version: u32,
    /// The run (a ULID) that wrote it.
    pub run_id: String,
    /// The daemon that wrote it.
    pub daemon: DaemonRecord,
    /// Running stems by name.
    #[serde(default)]
    pub stems: BTreeMap<String, StemRecord>,
    /// Script stamps (deliverable 16); carried over from run to run.
    #[serde(default)]
    pub stamps: StampStore,
    /// The last [`SCRIPT_RUNS_KEPT`] script runs, oldest first (deliverable 16).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_runs: Vec<ScriptRun>,
    /// Overlays stems materialised, by stem (deliverable 18). Independent of
    /// `stems` (a record is written *before* its file, i.e. before the stem
    /// runs, and `keep: true` ones outlive the stem); carried from run to run.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overlays: BTreeMap<String, Vec<OverlayRecord>>,
}

/// Script runs kept in [`StateFile::script_runs`].
pub const SCRIPT_RUNS_KEPT: usize = 100;

/// One finished script run (deliverable 16).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptRun {
    /// Stem (`None`: a workspace script such as `bootstrap`).
    #[serde(default)]
    pub stem: Option<String>,
    /// Script name.
    pub script: String,
    /// Start.
    pub started: DateTime<Utc>,
    /// End.
    pub ended: DateTime<Utc>,
    /// Exit code (`None`: killed by a signal, e.g. a timeout).
    pub exit: Option<i32>,
    /// Killed because it exceeded its `timeout`.
    #[serde(default)]
    pub timed_out: bool,
}

/// Outcome of [`StateFile::load`].
#[derive(Debug, Default)]
pub struct Loaded {
    /// The previous state (`None`: no file, or it was corrupt).
    pub file: Option<StateFile>,
    /// A corrupt file was moved here (`state.json.corrupt-<ts>`), with the parse error.
    pub corrupt: Option<(PathBuf, String)>,
}

/// A fresh run id: a ULID (time-ordered, unique).
pub fn new_run_id() -> String {
    ulid::Ulid::generate().to_string()
}

impl StateFile {
    /// An empty state for `run_id` written by `daemon`.
    pub fn new(run_id: impl Into<String>, daemon: DaemonRecord) -> Self {
        Self {
            version: STATE_VERSION,
            run_id: run_id.into(),
            daemon,
            stems: BTreeMap::new(),
            stamps: StampStore::default(),
            script_runs: Vec::new(),
            overlays: BTreeMap::new(),
        }
    }

    /// Read `path` without side effects (`None` if missing or unreadable).
    /// For the CLI, which must never move the daemon's files.
    pub fn peek(path: &Path) -> Option<StateFile> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Load `path` for recovery: a missing file is `file: None`; a corrupt
    /// one is renamed to `state.json.corrupt-<ts>` (with a warning) and
    /// treated as empty.
    pub fn load(path: &Path) -> Loaded {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read the state file; starting empty");
                return Loaded::default();
            }
        };
        match serde_json::from_str::<StateFile>(&text) {
            Ok(f) => Loaded {
                file: Some(f),
                corrupt: None,
            },
            Err(e) => {
                let ts = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
                let name = format!(
                    "{}.corrupt-{ts}",
                    path.file_name()
                        .map_or("state.json".into(), |n| n.to_string_lossy())
                );
                let dest = path.with_file_name(name);
                if let Err(re) = std::fs::rename(path, &dest) {
                    tracing::warn!(error = %re, "cannot move the corrupt state file aside");
                }
                tracing::warn!(
                    path = %path.display(),
                    moved_to = %dest.display(),
                    error = %e,
                    "the state file is corrupt; moved aside, starting empty"
                );
                Loaded {
                    file: None,
                    corrupt: Some((dest, e.to_string())),
                }
            }
        }
    }

    /// Write atomically: `<path>.tmp-<pid>`, fsync, rename over `path`,
    /// fsync the directory.
    pub fn write_atomic(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomic(path, &bytes)
    }
}

/// Temp file name used by [`write_atomic`] for `path`.
pub fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or("state.json".into(), |n| n.to_string_lossy());
    path.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

/// Replace `path` by `bytes` atomically (same-directory temp + rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Summary for `stems daemon status` (`data.state`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSummary {
    /// Run that wrote the file.
    pub run_id: String,
    /// Stems recorded.
    pub stems: usize,
    /// Of those, still alive (pid + start time).
    pub alive: usize,
}

impl StateSummary {
    /// Summary of the file at `path` (`None` if missing/unreadable).
    pub fn of(path: &Path) -> Option<StateSummary> {
        let f = StateFile::peek(path)?;
        Some(StateSummary {
            run_id: f.run_id.clone(),
            stems: f.stems.len(),
            alive: f.stems.values().filter(|r| r.is_alive()).count(),
        })
    }
}

/// The in-memory state plus its file. Every mutation rewrites the file
/// (atomically) before returning; write errors are logged, never fatal.
#[derive(Debug)]
pub struct StateStore {
    path: Option<PathBuf>,
    file: Mutex<StateFile>,
}

impl StateStore {
    /// A store persisted at `path`, starting empty for `run_id`. Nothing is
    /// written until the first change (or [`StateStore::flush`]).
    pub fn new(path: impl Into<PathBuf>, run_id: impl Into<String>, daemon: DaemonRecord) -> Self {
        Self {
            path: Some(path.into()),
            file: Mutex::new(StateFile::new(run_id, daemon)),
        }
    }

    /// A store that never touches the disk (tests).
    pub fn in_memory(run_id: impl Into<String>) -> Self {
        Self {
            path: None,
            file: Mutex::new(StateFile::new(run_id, DaemonRecord::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, StateFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The file path (`None` for in-memory stores).
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// This run's id.
    pub fn run_id(&self) -> String {
        self.lock().run_id.clone()
    }

    /// A copy of the current state.
    pub fn snapshot(&self) -> StateFile {
        self.lock().clone()
    }

    fn persist(&self, f: &StateFile) {
        if let Some(p) = &self.path
            && let Err(e) = f.write_atomic(p)
        {
            tracing::warn!(path = %p.display(), error = %e, "cannot write the state file");
        }
    }

    /// Mutate the state and write it (under one lock, so writes are ordered).
    pub fn update<R>(&self, f: impl FnOnce(&mut StateFile) -> R) -> R {
        let mut g = self.lock();
        let r = f(&mut g);
        self.persist(&g);
        r
    }

    /// Write the current state as is.
    pub fn flush(&self) {
        let g = self.lock();
        self.persist(&g);
    }

    /// Record (or, with `None`, forget) a stem. Fields the supervisor does
    /// not own (`overlays`, `log_file`, `container_id`) are kept from the
    /// previous record when the new one leaves them empty.
    pub fn set_stem(&self, name: &str, record: Option<StemRecord>) {
        if record.is_none() && !self.lock().stems.contains_key(name) {
            return;
        }
        self.update(|f| match record {
            Some(mut r) => {
                if let Some(old) = f.stems.get(name) {
                    if r.overlays.is_empty() {
                        r.overlays = old.overlays.clone();
                    }
                    if r.log_file.is_none() {
                        r.log_file = old.log_file.clone();
                    }
                    if r.container_id.is_none() {
                        r.container_id = old.container_id.clone();
                    }
                }
                f.stems.insert(name.to_string(), r);
            }
            None => {
                f.stems.remove(name);
            }
        });
    }

    /// Change a recorded stem in place (no-op if it is not recorded).
    pub fn update_stem(&self, name: &str, change: impl FnOnce(&mut StemRecord)) {
        self.update(|f| {
            if let Some(r) = f.stems.get_mut(name) {
                change(r);
            }
        });
    }

    /// The recorded stem, if any.
    pub fn stem(&self, name: &str) -> Option<StemRecord> {
        self.lock().stems.get(name).cloned()
    }

    /// Replace the stamps (deliverable 16 calls this after a script run).
    pub fn set_stamps(&self, stamps: StampStore) {
        self.update(|f| f.stamps = stamps);
    }

    /// The recorded stamps.
    pub fn stamps(&self) -> StampStore {
        self.lock().stamps.clone()
    }

    /// Change the stamps in place (and write).
    pub fn update_stamps<R>(&self, f: impl FnOnce(&mut StampStore) -> R) -> R {
        self.update(|file| f(&mut file.stamps))
    }

    /// The overlay records of `stem` (deliverable 18).
    pub fn overlays(&self, stem: &str) -> Vec<OverlayRecord> {
        self.lock().overlays.get(stem).cloned().unwrap_or_default()
    }

    /// Record (or replace, by `dest`) an overlay of `stem`, and write.
    pub fn record_overlay(&self, stem: &str, rec: OverlayRecord) {
        self.update(|f| {
            let list = f.overlays.entry(stem.to_string()).or_default();
            list.retain(|r| r.dest != rec.dest);
            list.push(rec);
        });
    }

    /// Forget the overlay of `stem` at `dest`, and write.
    pub fn forget_overlay(&self, stem: &str, dest: &Path) {
        self.update(|f| {
            if let Some(list) = f.overlays.get_mut(stem) {
                list.retain(|r| r.dest != dest);
                if list.is_empty() {
                    f.overlays.remove(stem);
                }
            }
        });
    }

    /// Append a script run, keeping the last [`SCRIPT_RUNS_KEPT`].
    pub fn record_script_run(&self, run: ScriptRun) {
        self.update(|f| {
            f.script_runs.push(run);
            let extra = f.script_runs.len().saturating_sub(SCRIPT_RUNS_KEPT);
            f.script_runs.drain(..extra);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use stems_core::stamps::Stamp;

    fn sample() -> StateFile {
        let mut f = StateFile::new(
            "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P",
            DaemonRecord {
                pid: 4242,
                start_time: 1_790_000_000_000,
            },
        );
        let at = Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap();
        f.stems.insert(
            "echo-svc".into(),
            StemRecord {
                pid: 5001,
                pgid: 5001,
                start_time: StartTime(1_790_000_000_500),
                container_id: None,
                ports: vec![
                    PortRecord {
                        name: "http".into(),
                        port: 18090,
                        auto: false,
                    },
                    PortRecord {
                        name: "admin".into(),
                        port: 49152,
                        auto: true,
                    },
                ],
                overlays: vec![OverlayRecord {
                    dest: "/ws/repos/api/.env".into(),
                    sha256: "ab".repeat(32),
                    run_id: "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P".into(),
                    keep: false,
                }],
                state: StemState::Healthy,
                started_at: at,
                log_file: Some("/h/abc/logs/echo-svc.log".into()),
            },
        );
        f.stamps.record(
            "echo-svc",
            "setup",
            Stamp {
                hash: "cd".repeat(32),
                computed_at: at,
                inputs: vec!["requirements.txt".into()],
            },
        );
        f
    }

    #[test]
    fn round_trip_golden() {
        let f = sample();
        insta::assert_json_snapshot!("state_file", f);
        let text = serde_json::to_string(&f).unwrap();
        let back: StateFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn script_runs_are_a_ring() {
        let s = StateStore::in_memory(new_run_id());
        let at = Utc.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap();
        for i in 0..(SCRIPT_RUNS_KEPT + 5) {
            s.record_script_run(ScriptRun {
                stem: Some("a".into()),
                script: format!("s{i}"),
                started: at,
                ended: at,
                exit: Some(0),
                timed_out: false,
            });
        }
        let runs = s.snapshot().script_runs;
        assert_eq!(runs.len(), SCRIPT_RUNS_KEPT);
        assert_eq!(runs[0].script, "s5");
        // Old files without the field still load.
        let f: StateFile =
            serde_json::from_str(r#"{"version":1,"run_id":"x","daemon":{"pid":1,"start_time":1}}"#)
                .unwrap();
        assert!(f.script_runs.is_empty());
    }

    #[test]
    fn missing_file_loads_empty() {
        let d = tempfile::tempdir().unwrap();
        let l = StateFile::load(&d.path().join("state.json"));
        assert!(l.file.is_none() && l.corrupt.is_none());
        assert!(StateFile::peek(&d.path().join("state.json")).is_none());
    }

    #[test]
    fn atomic_write_and_simulated_crash() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        let old = sample();
        old.write_atomic(&path).unwrap();
        assert!(!temp_path(&path).exists(), "temp file renamed away");
        // A crash between writing the temp file and the rename: the temp file
        // is garbage (half written), state.json is still the old document.
        std::fs::write(temp_path(&path), b"{\"version\": 1, \"run_id\": \"x").unwrap();
        let l = StateFile::load(&path);
        assert_eq!(l.file.as_ref(), Some(&old));
        assert!(l.corrupt.is_none());
        // The next write replaces both.
        let mut new = old.clone();
        new.stems.clear();
        new.write_atomic(&path).unwrap();
        assert_eq!(StateFile::load(&path).file, Some(new));
        assert!(!temp_path(&path).exists());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let l = StateFile::load(&path);
        assert!(l.file.is_none());
        let (moved, err) = l.corrupt.expect("corrupt reported");
        assert!(!err.is_empty());
        assert!(!path.exists());
        assert!(
            moved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("state.json.corrupt-")
        );
        assert_eq!(std::fs::read(&moved).unwrap(), b"{ not json");
    }

    #[test]
    fn store_writes_on_every_change_and_keeps_foreign_fields() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        let s = StateStore::new(&path, new_run_id(), DaemonRecord::default());
        assert!(!path.exists(), "nothing written before the first change");
        let rec = sample().stems["echo-svc"].clone();
        s.set_stem("echo-svc", Some(rec.clone()));
        assert_eq!(StateFile::peek(&path).unwrap().stems["echo-svc"], rec);
        // A supervisor update without overlays/log_file keeps them.
        let mut bare = rec.clone();
        bare.overlays.clear();
        bare.log_file = None;
        bare.state = StemState::Stopping;
        s.set_stem("echo-svc", Some(bare));
        let on_disk = StateFile::peek(&path).unwrap();
        assert_eq!(on_disk.stems["echo-svc"].overlays, rec.overlays);
        assert_eq!(on_disk.stems["echo-svc"].state, StemState::Stopping);
        s.set_stem("echo-svc", None);
        assert!(StateFile::peek(&path).unwrap().stems.is_empty());
        assert_eq!(s.run_id().len(), 26, "ULID");
    }

    #[test]
    fn summary_counts_live_records() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        let mut f = sample();
        let me = std::process::id() as i32;
        let mut live = f.stems["echo-svc"].clone();
        live.pid = me;
        live.start_time = os::process_start_time(me).unwrap();
        f.stems.insert("me".into(), live);
        f.write_atomic(&path).unwrap();
        let s = StateSummary::of(&path).unwrap();
        assert_eq!((s.stems, s.alive), (2, 1));
    }
}
