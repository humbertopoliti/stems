//! Overlays (FR-WS-7, NFR-3; deliverable 18, `docs/overlays.md`): files the
//! integration repo materialises into a stem's codebase before it starts,
//! and removes again when it stops.
//!
//! The decisions are the pure functions of [`stems_core::overlays`]; this
//! module does the I/O around them and keeps the ledger
//! (`state.json` → `overlays.<stem>`, see [`crate::state::StateFile`]):
//!
//! * **materialise** (start sequence, after `setup`, before `pre_start`):
//!   render → [`resolve_dest`] → probe → [`plan_materialise`]; a record
//!   `{dest, sha256, run_id, keep}` is written to the state file *before*
//!   the file itself, so a crash in between leaves a harmless record of a
//!   missing file, never an unrecorded file. A destination stems does not
//!   own is `OVERLAY_CONFLICT` unless `up --force-overlays` (then it is
//!   backed up under `$STEMS_STATE_DIR/overlay-backups/<run_id>/` first).
//! * **cleanup** (stop sequence, after `post_stop`): [`plan_cleanup`] per
//!   record — an unmodified file is removed, a modified one is left in place
//!   (`overlay.modified_left_in_place`), `keep: true` stays recorded.
//! * **render on demand** (47, `stems overlays render`): the same
//!   [`render_all`] + [`write_rendered`] as a start, without starting the
//!   stem — by the daemon (RPC `overlays_render`) or, daemonless, by the CLI
//!   on `state.json` under the workspace lock ([`FileLedger`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::json;
use stems_api::{
    EventKind, OverlayEntry, OverlaysRenderParams, OverlaysRenderResult, RenderedOverlay,
};
use stems_config::{FileMode, PortRef, Stem, Workspace};
use stems_core::overlays::{
    CleanupPlan, ExistingFile, MaterialisePlan, OverlayRecord, RenderCtx, backup_file, hash_bytes,
    plan_cleanup, plan_materialise, render_overlay, resolve_dest, status_of, write_atomic,
};
use stems_core::{Error, ErrorCode};

use super::Core;
use crate::events::EventDraft;
use crate::state::{StateFile, StateStore};

/// One overlay rendered for writing.
#[derive(Clone, Debug)]
pub struct Rendered {
    /// Absolute destination.
    pub dest: PathBuf,
    /// File mode.
    pub mode: FileMode,
    /// `keep: true`.
    pub keep: bool,
    /// The bytes to write.
    pub bytes: Vec<u8>,
}

/// Render every overlay of `stem` (nothing is written). Every failing
/// overlay is reported (47), merged by [`combine`].
pub fn render_all(stem: &Stem, ctx: &RenderCtx) -> Result<Vec<Rendered>, Error> {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for overlay in &stem.overlays {
        let r = render_overlay(overlay, ctx).and_then(|bytes| {
            Ok(Rendered {
                dest: resolve_dest(&ctx.codebase, &overlay.dest)?,
                mode: overlay.mode,
                keep: overlay.keep,
                bytes,
            })
        });
        match r {
            Ok(r) => out.push(r),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(combine(&stem.name, errors))
    }
}

/// One error for several: the first one's code and hint, every message
/// (and every `problems` entry) listed.
fn combine(stem: &str, mut errors: Vec<Error>) -> Error {
    if errors.len() == 1 {
        return errors.remove(0);
    }
    let problems: Vec<serde_json::Value> = errors
        .iter()
        .flat_map(
            |e| match e.details.get("problems").and_then(|p| p.as_array()) {
                Some(p) => p.clone(),
                None => vec![json!(e.message)],
            },
        )
        .collect();
    let first = &errors[0];
    let mut e = Error::new(
        first.code,
        format!(
            "{} overlays of `{stem}` cannot be rendered: {}",
            errors.len(),
            errors
                .iter()
                .map(|e| e.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ),
    )
    .with_details(json!({
        "stem": stem,
        "problems": problems,
        "errors": errors.iter().map(|e| json!({"code": e.code, "message": e.message, "details": e.details})).collect::<Vec<_>>(),
    }));
    if let Some(h) = &first.hint {
        e = e.with_hint(h.clone());
    }
    e
}

/// Where the overlay ledger lives: the daemon's [`StateStore`] or, for the
/// daemonless CLI, a [`FileLedger`].
pub trait Ledger {
    /// The record of `dest` of `stem`, if any.
    fn owned(&self, stem: &str, dest: &Path) -> Option<OverlayRecord>;
    /// Save (insert or replace) a record durably.
    fn record(&self, stem: &str, rec: OverlayRecord);
    /// Drop a record durably.
    fn forget(&self, stem: &str, dest: &Path);
}

impl Ledger for StateStore {
    fn owned(&self, stem: &str, dest: &Path) -> Option<OverlayRecord> {
        self.overlays(stem).into_iter().find(|r| r.dest == dest)
    }
    fn record(&self, stem: &str, rec: OverlayRecord) {
        self.record_overlay(stem, rec);
    }
    fn forget(&self, stem: &str, dest: &Path) {
        self.forget_overlay(stem, dest);
    }
}

/// `state.json` edited directly (no daemon runs; the caller holds the
/// workspace lock): every change is written atomically at once.
pub struct FileLedger {
    path: PathBuf,
    file: std::sync::Mutex<StateFile>,
}

impl FileLedger {
    /// The ledger of the state file at `path` (a new, empty one if missing).
    pub fn open(path: &Path) -> Self {
        let file = StateFile::peek(path).unwrap_or_else(|| {
            StateFile::new(
                crate::state::new_run_id(),
                crate::state::DaemonRecord::default(),
            )
        });
        Self {
            path: path.to_path_buf(),
            file: std::sync::Mutex::new(file),
        }
    }

    /// The run id records are written with.
    pub fn run_id(&self) -> String {
        self.lock().run_id.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StateFile> {
        self.file.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, f: &StateFile) {
        if let Err(e) = f.write_atomic(&self.path) {
            tracing::warn!(path = %self.path.display(), error = %e, "cannot write the state file");
        }
    }
}

impl Ledger for FileLedger {
    fn owned(&self, stem: &str, dest: &Path) -> Option<OverlayRecord> {
        self.lock()
            .overlays
            .get(stem)
            .and_then(|v| v.iter().find(|r| r.dest == dest).cloned())
    }
    fn record(&self, stem: &str, rec: OverlayRecord) {
        let mut f = self.lock();
        let v = f.overlays.entry(stem.to_string()).or_default();
        v.retain(|r| r.dest != rec.dest);
        v.push(rec);
        self.save(&f);
    }
    fn forget(&self, stem: &str, dest: &Path) {
        let mut f = self.lock();
        if let Some(v) = f.overlays.get_mut(stem) {
            v.retain(|r| r.dest != dest);
            if v.is_empty() {
                f.overlays.remove(stem);
            }
        }
        self.save(&f);
    }
}

/// What [`write_rendered`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// The file already held exactly these bytes (record refreshed).
    Unchanged,
    /// Written (after backing up a foreign file to `backup`, if any).
    Written {
        /// The backup of the file that was there (`--force-overlays`).
        backup: Option<PathBuf>,
    },
}

/// Write one rendered overlay of `stem` under the "never clobbers" rules:
/// record first, then write atomically. `backup_dir` = `--force-overlays`
/// (back a foreign file up there, then overwrite); `None` makes a foreign
/// file `OVERLAY_CONFLICT`.
pub fn write_rendered(
    ledger: &dyn Ledger,
    stem: &str,
    r: &Rendered,
    run_id: &str,
    backup_dir: Option<&Path>,
) -> Result<Written, Error> {
    let dest = &r.dest;
    let sha256 = hash_bytes(&r.bytes);
    let existing = ExistingFile::probe(dest).map_err(|e| io_error(stem, dest, "inspect", &e))?;
    let owned = ledger.owned(stem, dest);
    let rec = OverlayRecord {
        dest: dest.clone(),
        sha256,
        run_id: run_id.to_string(),
        keep: r.keep,
    };
    let force = backup_dir.is_some();
    let backup = match plan_materialise(dest, &rec.sha256, &existing, owned.as_ref(), force) {
        MaterialisePlan::Conflict(mut e) => {
            if let Some(m) = e.details.as_object_mut() {
                m.insert("stem".into(), json!(stem));
            }
            e.message = format!("{} (overlay of `{stem}`)", e.message);
            return Err(e);
        }
        MaterialisePlan::Unchanged => {
            ledger.record(stem, rec);
            return Ok(Written::Unchanged);
        }
        MaterialisePlan::Write => None,
        MaterialisePlan::WriteAfterBackup => {
            let dir = backup_dir.unwrap_or(Path::new("."));
            let b =
                backup_file(dest, dir, run_id).map_err(|e| io_error(stem, dest, "back up", &e))?;
            Some(b)
        }
    };
    // Record first: a crash before the write leaves a record of a
    // missing file (harmless), never an unrecorded file.
    ledger.record(stem, rec);
    if let Err(e) = write_atomic(dest, &r.bytes, r.mode) {
        match owned {
            Some(prev) => ledger.record(stem, prev),
            None => ledger.forget(stem, dest),
        }
        return Err(io_error(stem, dest, "write", &e));
    }
    Ok(Written::Written { backup })
}

/// The `overlays_render` result for `files` of `stem`; `written[i]` says
/// whether file `i` was written.
pub fn render_result(
    stem: &str,
    dry_run: bool,
    files: &[Rendered],
    written: &[bool],
) -> OverlaysRenderResult {
    OverlaysRenderResult {
        stem: stem.to_string(),
        dry_run,
        files: files
            .iter()
            .enumerate()
            .map(|(i, f)| RenderedOverlay {
                dest: f.dest.clone(),
                mode: f.mode.to_string(),
                keep: f.keep,
                content: String::from_utf8_lossy(&f.bytes).into_owned(),
                written: written.get(i).copied().unwrap_or(false),
            })
            .collect(),
    }
}

fn io_error(stem: &str, dest: &Path, what: &str, e: &std::io::Error) -> Error {
    Error::new(
        ErrorCode::StartFailed,
        format!(
            "cannot {what} overlay `{}` of `{stem}`: {e}",
            dest.display()
        ),
    )
    .with_hint("check the permissions of the codebase directory")
    .with_details(json!({ "stem": stem, "dest": dest, "reason": "overlay" }))
}

/// The template context of `stem`: declared ports, with `auto` ports of the
/// stem itself and its dependencies allocated (sticky, announced), and the
/// already allocated ones of every other stem.
fn render_ctx(core: &Core, ws: &Workspace, stem: &Stem, env: HashMap<String, String>) -> RenderCtx {
    let mut ctx = RenderCtx::for_stem(ws, stem, env);
    // `${stem.<n>.outputs.X}` of running stems (26).
    ctx.outputs = core.outputs.values().into_iter().collect();
    let mut allocated = Vec::new();
    for (name, s) in &ws.stems {
        let own = *name == stem.name || stem.depends_on.iter().any(|d| d.stem == *name);
        for p in s.ports.iter().filter(|p| matches!(p.port, PortRef::Auto)) {
            let port = if own {
                core.ports
                    .resolve_ref(ws, name, Some(&p.name), &mut allocated)
            } else {
                core.ports.allocated(name, &p.name)
            };
            if let Some(port) = port {
                ctx.set_port(name, &p.name, port);
            }
        }
    }
    for (s, n, p) in &allocated {
        core.events.emit(
            EventDraft::new(EventKind::STEM_PORT_ALLOCATED, stems_api::DAEMON_ACTOR)
                .stem(s)
                .data(json!({ "port": p, "name": n })),
        );
    }
    ctx
}

/// Materialise every overlay of `stem`. `env` is what `${env.X}` sees.
/// Errors (`OVERLAY_CONFLICT`, `UNRESOLVED_VARIABLE`, I/O) fail the start;
/// overlays written before the error stay recorded (cleaned on stop/down).
pub(crate) fn materialise(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    env: &BTreeMap<String, String>,
    actor: &str,
) -> Result<(), Error> {
    if stem.overlays.is_empty() {
        return Ok(());
    }
    let Some(store) = &core.state else {
        return Ok(());
    };
    let force = core.force_overlays.load(Ordering::SeqCst);
    let env: HashMap<String, String> = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let ctx = render_ctx(core, ws, stem, env);
    let files = render_all(stem, &ctx)?;
    write_all(core, store, stem, &files, force, actor).map(|_| ())
}

/// Write `files` of `stem` in order (stopping at the first error), with an
/// `overlay.materialised` event per file written. Returns, per file,
/// whether it was written.
fn write_all(
    core: &Core,
    store: &StateStore,
    stem: &Stem,
    files: &[Rendered],
    force: bool,
    actor: &str,
) -> Result<Vec<bool>, Error> {
    let backup_dir = force.then(|| {
        core.scripts
            .state_dir(Some(&stem.name))
            .join("overlay-backups")
    });
    let mut written = Vec::with_capacity(files.len());
    for f in files {
        match write_rendered(store, &stem.name, f, &core.run_id, backup_dir.as_deref())? {
            Written::Unchanged => written.push(false),
            Written::Written { backup } => {
                written.push(true);
                core.events.emit(
                    EventDraft::new(EventKind::OVERLAY_MATERIALISED, actor)
                        .stem(&stem.name)
                        .data(json!({ "dest": f.dest, "keep": f.keep, "backup": backup })),
                );
            }
        }
    }
    Ok(written)
}

/// Clean the recorded overlays of `stem` (it is not running any more):
/// remove unmodified files, leave modified ones (warning event) and
/// `keep: true` ones (still recorded).
pub(crate) fn cleanup(core: &Core, stem: &str, actor: &str) {
    let Some(store) = &core.state else { return };
    for rec in store.overlays(stem) {
        let existing = match ExistingFile::probe(&rec.dest) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(stem, dest = %rec.dest.display(), error = %e, "cannot inspect overlay; left recorded");
                continue;
            }
        };
        let event = |kind: EventKind| {
            core.events.emit(
                EventDraft::new(kind, actor)
                    .stem(stem)
                    .data(json!({ "dest": rec.dest })),
            );
        };
        match plan_cleanup(&rec, &existing) {
            CleanupPlan::Remove => match std::fs::remove_file(&rec.dest) {
                Ok(()) => {
                    store.forget_overlay(stem, &rec.dest);
                    event(EventKind::OVERLAY_REMOVED);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    store.forget_overlay(stem, &rec.dest);
                }
                Err(e) => {
                    tracing::warn!(stem, dest = %rec.dest.display(), error = %e, "cannot remove overlay; left recorded");
                }
            },
            CleanupPlan::LeaveModified => {
                tracing::warn!(stem, dest = %rec.dest.display(), "overlay modified since stems wrote it; left in place");
                store.forget_overlay(stem, &rec.dest);
                event(EventKind::OVERLAY_MODIFIED_LEFT_IN_PLACE);
            }
            CleanupPlan::LeaveKept => event(EventKind::OVERLAY_KEPT),
            CleanupPlan::AlreadyGone => store.forget_overlay(stem, &rec.dest),
        }
    }
}

/// `overlays` entries of a ledger (every stem, or only `stem`), with the
/// status of each file on disk. Shared by the daemon and the daemonless CLI.
pub fn entries(
    ledger: &BTreeMap<String, Vec<OverlayRecord>>,
    stem: Option<&str>,
) -> Vec<OverlayEntry> {
    let mut out: Vec<OverlayEntry> = ledger
        .iter()
        .filter(|(s, _)| stem.is_none_or(|x| x == s.as_str()))
        .flat_map(|(s, recs)| {
            recs.iter().map(move |r| {
                let status = match ExistingFile::probe(&r.dest) {
                    Ok(x) => status_of(r, &x),
                    // Unreadable (a directory?): not what stems wrote.
                    Err(_) => stems_core::overlays::OverlayStatus::Modified,
                };
                OverlayEntry {
                    stem: s.clone(),
                    dest: r.dest.clone(),
                    status: status.into(),
                    keep: r.keep,
                    sha256: r.sha256.clone(),
                    run_id: r.run_id.clone(),
                }
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.stem, &a.dest).cmp(&(&b.stem, &b.dest)));
    out
}

impl super::Supervisor {
    /// Clean the overlays of every recorded stem (only those in `only`, when
    /// not empty) that is not running: after `down`, and on recovery for the
    /// stems a crashed daemon left that did not survive.
    pub(crate) fn cleanup_idle_overlays(&self, only: &[String], actor: &str) {
        let Some(store) = &self.core.state else {
            return;
        };
        let stems: Vec<String> = store
            .snapshot()
            .overlays
            .into_keys()
            .filter(|s| only.is_empty() || only.contains(s))
            .collect();
        for stem in stems {
            let running = {
                let cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
                cells.get(&stem).is_some_and(|c| c.state().is_running())
            };
            if !running {
                cleanup(&self.core, &stem, actor);
            }
        }
    }

    /// `overlays_render` (47): render `stem`'s overlays now, with the env
    /// and ports a start would use, and (unless `dry_run`) write and record
    /// them like a start does, so a later `down` removes them.
    pub fn overlays_render(
        &self,
        p: &OverlaysRenderParams,
        actor: &str,
    ) -> Result<OverlaysRenderResult, Error> {
        let ws = self.workspace(false, actor)?;
        super::schedule::plan(&ws, std::slice::from_ref(&p.stem), true)?;
        let stem = ws
            .workspace
            .stem(&p.stem)
            .ok_or_else(|| Error::internal(format!("stem `{}` vanished", p.stem)))?;
        let env = super::hooks::stem_env(&self.core, &ws.workspace, stem, &BTreeMap::new())?;
        let env: HashMap<String, String> = env.into_iter().collect();
        let ctx = render_ctx(&self.core, &ws.workspace, stem, env);
        let files = render_all(stem, &ctx)?;
        let written = match (&self.core.state, p.dry_run) {
            (Some(store), false) => write_all(&self.core, store, stem, &files, false, actor)?,
            _ => Vec::new(),
        };
        Ok(render_result(&stem.name, p.dry_run, &files, &written))
    }

    /// `overlays`: the recorded overlays and the status of each file.
    pub fn overlays(
        &self,
        p: &stems_api::OverlaysParams,
        actor: &str,
    ) -> Result<stems_api::OverlaysResult, Error> {
        if let Some(s) = &p.stem {
            let ws = self.workspace(false, actor)?;
            super::schedule::plan(&ws, std::slice::from_ref(s), true)?;
        }
        let ledger = self
            .core
            .state
            .as_ref()
            .map(|s| s.snapshot().overlays)
            .unwrap_or_default();
        Ok(stems_api::OverlaysResult {
            overlays: entries(&ledger, p.stem.as_deref()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_report_status_per_file() {
        let d = tempfile::tempdir().unwrap();
        let present = d.path().join("a.ini");
        let modified = d.path().join("b.ini");
        std::fs::write(&present, "a").unwrap();
        std::fs::write(&modified, "changed").unwrap();
        let rec = |dest: &Path, body: &[u8], keep| OverlayRecord {
            dest: dest.to_path_buf(),
            sha256: hash_bytes(body),
            run_id: "r".into(),
            keep,
        };
        let mut ledger = BTreeMap::new();
        ledger.insert(
            "api".to_string(),
            vec![
                rec(&modified, b"b", false),
                rec(&present, b"a", true),
                rec(&d.path().join("gone.ini"), b"c", false),
            ],
        );
        ledger.insert("web".to_string(), vec![rec(&present, b"a", false)]);
        let all = entries(&ledger, None);
        let got: Vec<(&str, &str, stems_api::OverlayFileStatus, bool)> = all
            .iter()
            .map(|e| {
                (
                    e.stem.as_str(),
                    e.dest.file_name().unwrap().to_str().unwrap(),
                    e.status,
                    e.keep,
                )
            })
            .collect();
        use stems_api::OverlayFileStatus as S;
        assert_eq!(
            got,
            [
                ("api", "a.ini", S::Present, true),
                ("api", "b.ini", S::Modified, false),
                ("api", "gone.ini", S::Missing, false),
                ("web", "a.ini", S::Present, false),
            ]
        );
        assert_eq!(entries(&ledger, Some("web")).len(), 1);
        assert!(entries(&ledger, Some("nope")).is_empty());
    }
}
