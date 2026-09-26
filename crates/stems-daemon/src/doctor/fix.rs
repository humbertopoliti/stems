//! `stems doctor --fix`: repair what is safe to repair.
//!
//! | check | fix |
//! |---|---|
//! | `daemon` (stale lock / socket) | remove the lock file (only while it is still stale) and a socket nobody listens on |
//! | `overlays.<stem>` (stale) | under the workspace lock (so no daemon starts meanwhile): delete each unchanged file, leave modified ones in place, forget the records |
//! | `orphans` | kill processes that look like their stem's start command (`kill_foreign`: every process orphan) and remove stems-labelled containers |
//!
//! Everything else (ports held by foreign processes, tool versions, Docker)
//! needs a human.

use std::sync::Arc;
use std::time::Duration;

use stems_core::overlays::{CleanupPlan, ExistingFile, plan_cleanup};
use stems_runtime::OrphanKind;

use super::checks::{scan_orphans, stem_running};
use super::{DaemonProbe, DoctorCtx, DoctorReport, FixOutcome};
use crate::lock::{self, LockState};
use crate::state::StateFile;

/// SIGTERM grace when killing an orphan.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// Options of `--fix`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FixOptions {
    /// Also kill process orphans that do not look like their stem's start command.
    pub kill_foreign: bool,
}

fn outcome(id: &str, action: &str, ok: bool, message: impl Into<String>) -> FixOutcome {
    FixOutcome {
        id: id.to_string(),
        action: action.to_string(),
        ok,
        message: message.into(),
    }
}

/// Apply the fixable items of `report`.
pub async fn apply(
    cx: &Arc<DoctorCtx>,
    report: &DoctorReport,
    opts: FixOptions,
) -> Vec<FixOutcome> {
    let mut out = Vec::new();
    let ids: Vec<String> = report.fixable().map(|c| c.id.clone()).collect();
    if ids.iter().any(|i| i == "daemon") {
        fix_daemon(cx, &mut out);
    }
    let stale_overlays: Vec<String> = ids
        .iter()
        .filter_map(|i| i.strip_prefix("overlays.").map(str::to_string))
        .collect();
    if !stale_overlays.is_empty() {
        fix_overlays(cx, &stale_overlays, &mut out);
    }
    if ids.iter().any(|i| i == "orphans") {
        fix_orphans(cx, opts, &mut out).await;
    }
    out
}

fn fix_daemon(cx: &DoctorCtx, out: &mut Vec<FixOutcome>) {
    if !matches!(cx.input.daemon, DaemonProbe::NotRunning) {
        return;
    }
    let paths = &cx.input.paths;
    let state = lock::probe(paths);
    if let LockState::Stale { pid } = state {
        out.push(match std::fs::remove_file(&paths.lock) {
            Ok(()) => outcome(
                "daemon",
                "remove_stale_lock",
                true,
                format!("removed the stale lock of pid {pid}"),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => outcome(
                "daemon",
                "remove_stale_lock",
                true,
                "the stale lock was already gone",
            ),
            Err(e) => outcome(
                "daemon",
                "remove_stale_lock",
                false,
                format!("cannot remove {}: {e}", paths.lock.display()),
            ),
        });
    }
    if !matches!(lock::probe(paths), LockState::Held { .. }) && paths.socket.exists() {
        out.push(match std::fs::remove_file(&paths.socket) {
            Ok(()) => outcome(
                "daemon",
                "remove_stale_socket",
                true,
                "removed the stale socket",
            ),
            Err(e) => outcome(
                "daemon",
                "remove_stale_socket",
                false,
                format!("cannot remove {}: {e}", paths.socket.display()),
            ),
        });
    }
}

fn fix_overlays(cx: &DoctorCtx, stems: &[String], out: &mut Vec<FixOutcome>) {
    let paths = &cx.input.paths;
    // Holding the workspace lock keeps a daemon from starting (and writing
    // state.json) while we edit the ledger; a running daemon refuses us.
    let guard = match lock::acquire(paths) {
        Ok(g) => g,
        Err(e) => {
            for s in stems {
                out.push(outcome(
                    &format!("overlays.{s}"),
                    "remove_overlay",
                    false,
                    format!("{} (stop the daemon with `stems down` first)", e.message),
                ));
            }
            return;
        }
    };
    let Some(mut file) = StateFile::peek(&paths.state) else {
        drop(guard);
        return;
    };
    let mut changed = false;
    for stem in stems {
        let id = format!("overlays.{stem}");
        if stem_running(Some(&file), stem) {
            continue;
        }
        let Some(recs) = file.overlays.get(stem).cloned() else {
            continue;
        };
        let mut keep = Vec::new();
        for rec in recs {
            if rec.keep {
                keep.push(rec);
                continue;
            }
            let existing = match ExistingFile::probe(&rec.dest) {
                Ok(x) => x,
                Err(e) => {
                    out.push(outcome(
                        &id,
                        "remove_overlay",
                        false,
                        format!("cannot inspect {}: {e}", rec.dest.display()),
                    ));
                    keep.push(rec);
                    continue;
                }
            };
            let d = rec.dest.display().to_string();
            match plan_cleanup(&rec, &existing) {
                CleanupPlan::Remove => match std::fs::remove_file(&rec.dest) {
                    Ok(()) => {
                        out.push(outcome(&id, "remove_overlay", true, format!("removed {d}")))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        out.push(outcome(
                            &id,
                            "forget_overlay",
                            true,
                            format!("{d} was already gone"),
                        ));
                    }
                    Err(e) => {
                        out.push(outcome(
                            &id,
                            "remove_overlay",
                            false,
                            format!("cannot remove {d}: {e}"),
                        ));
                        keep.push(rec);
                        continue;
                    }
                },
                CleanupPlan::LeaveModified => out.push(outcome(
                    &id,
                    "leave_modified_overlay",
                    true,
                    format!(
                        "{d} was modified since stems wrote it: left in place, record forgotten"
                    ),
                )),
                CleanupPlan::AlreadyGone => {
                    out.push(outcome(
                        &id,
                        "forget_overlay",
                        true,
                        format!("{d} was already gone"),
                    ));
                }
                CleanupPlan::LeaveKept => {
                    keep.push(rec);
                    continue;
                }
            }
            changed = true;
        }
        if keep.is_empty() {
            file.overlays.remove(stem);
        } else {
            file.overlays.insert(stem.clone(), keep);
        }
    }
    if changed && let Err(e) = file.write_atomic(&paths.state) {
        out.push(outcome(
            "overlays",
            "write_state",
            false,
            format!("cannot write {}: {e}", paths.state.display()),
        ));
    }
    drop(guard);
}

async fn fix_orphans(cx: &Arc<DoctorCtx>, opts: FixOptions, out: &mut Vec<FixOutcome>) {
    let (found, _) = scan_orphans(cx).await;
    for o in found {
        match o.kind {
            OrphanKind::Process => {
                if !o.matches_start_command && !opts.kill_foreign {
                    continue;
                }
                let what = format!(
                    "pid {} on port {}",
                    o.pid.unwrap_or(0),
                    o.port.map_or("-".into(), |p| p.to_string())
                );
                out.push(match crate::orphans::kill(&o, KILL_GRACE).await {
                    Ok(_) => outcome("orphans", "kill_orphan", true, format!("killed {what}")),
                    Err(e) => outcome(
                        "orphans",
                        "kill_orphan",
                        false,
                        format!("cannot kill {what}: {e}"),
                    ),
                });
            }
            OrphanKind::Container => {
                let Some(id) = o.container_id.clone() else {
                    continue;
                };
                let Ok(d) = cx.docker().await else {
                    continue;
                };
                out.push(match d.remove_container_id(&id, false).await {
                    Ok(()) => outcome(
                        "orphans",
                        "remove_container",
                        true,
                        format!("removed container {}", o.command),
                    ),
                    Err(e) => outcome(
                        "orphans",
                        "remove_container",
                        false,
                        format!("cannot remove container {}: {e}", o.command),
                    ),
                });
            }
        }
    }
}
