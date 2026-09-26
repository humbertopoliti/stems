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

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::json;
use stems_api::{EventKind, OverlayEntry};
use stems_config::{PortRef, Stem, Workspace};
use stems_core::overlays::{
    CleanupPlan, ExistingFile, MaterialisePlan, OverlayRecord, RenderCtx, backup_file, hash_bytes,
    plan_cleanup, plan_materialise, render_overlay, resolve_dest, status_of, write_atomic,
};
use stems_core::{Error, ErrorCode};

use super::Core;
use crate::events::EventDraft;

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
    let codebase = ctx.codebase.clone();
    for overlay in &stem.overlays {
        let bytes = render_overlay(overlay, &ctx)?;
        let dest = resolve_dest(&codebase, &overlay.dest)?;
        let sha256 = hash_bytes(&bytes);
        let existing =
            ExistingFile::probe(&dest).map_err(|e| io_error(&stem.name, &dest, "inspect", &e))?;
        let owned = store
            .overlays(&stem.name)
            .into_iter()
            .find(|r| r.dest == dest);
        let rec = OverlayRecord {
            dest: dest.clone(),
            sha256,
            run_id: core.run_id.clone(),
            keep: overlay.keep,
        };
        let backup = match plan_materialise(&dest, &rec.sha256, &existing, owned.as_ref(), force) {
            MaterialisePlan::Conflict(mut e) => {
                if let Some(m) = e.details.as_object_mut() {
                    m.insert("stem".into(), json!(stem.name));
                }
                e.message = format!("{} (overlay of `{}`)", e.message, stem.name);
                return Err(e);
            }
            MaterialisePlan::Unchanged => {
                store.record_overlay(&stem.name, rec);
                continue;
            }
            MaterialisePlan::Write => None,
            MaterialisePlan::WriteAfterBackup => {
                let dir = core
                    .scripts
                    .state_dir(Some(&stem.name))
                    .join("overlay-backups");
                let b = backup_file(&dest, &dir, &core.run_id)
                    .map_err(|e| io_error(&stem.name, &dest, "back up", &e))?;
                Some(b)
            }
        };
        // Record first: a crash before the write leaves a record of a
        // missing file (harmless), never an unrecorded file.
        store.record_overlay(&stem.name, rec);
        if let Err(e) = write_atomic(&dest, &bytes, overlay.mode) {
            match owned {
                Some(prev) => store.record_overlay(&stem.name, prev),
                None => store.forget_overlay(&stem.name, &dest),
            }
            return Err(io_error(&stem.name, &dest, "write", &e));
        }
        core.events.emit(
            EventDraft::new(EventKind::OVERLAY_MATERIALISED, actor)
                .stem(&stem.name)
                .data(json!({ "dest": dest, "keep": overlay.keep, "backup": backup })),
        );
    }
    Ok(())
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
