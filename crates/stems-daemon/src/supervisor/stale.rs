//! Stale images of compose stems (deliverable 35): the effective `build`
//! flag, the check before a start without a rebuild (a warning), the guard
//! that keeps a crashing stem with a stale image from being restarted in a
//! loop (`IMAGE_STALE`, reported like a stem that was given up on), and the
//! pure error/event builders shared with `doctor`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use stems_api::EventKind;
use stems_core::{Error, ErrorCode};
use stems_runtime::ComposeSpec;
use stems_runtime::stale::{self, Verdict};

use super::Core;
use super::actor::StemCell;
use crate::events::EventDraft;

/// Bound on the whole check (`compose config`, one image inspect, the
/// scan): the crash guard must never hold a restart for long.
const CHECK_BOUND: Duration = Duration::from_secs(60);

/// The `build` flag in effect for a run: the `--build`/`--no-build`
/// parameter wins over the stem's `build:` (default true).
pub fn effective_build(param: Option<bool>, config: bool) -> bool {
    param.unwrap_or(config)
}

/// `2026-10-19 10:02:11 UTC`.
pub fn fmt_time(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t)
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string()
}

fn rfc3339(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()
}

/// `details` of `IMAGE_STALE` and `image.stale`: `stem`, `image`,
/// `image_created`, `newest_input`, `newest_input_path` (RFC 3339 times).
pub fn stale_details(stem: &str, image: &str, verdict: &Verdict) -> Value {
    match verdict {
        Verdict::Stale {
            image_created,
            newest,
            path,
        } => json!({
            "stem": stem,
            "image": image,
            "image_created": rfc3339(*image_created),
            "newest_input": rfc3339(*newest),
            "newest_input_path": path,
        }),
        _ => json!({ "stem": stem, "image": image }),
    }
}

/// The hint of every stale report.
pub fn stale_hint(stem: &str) -> String {
    format!(
        "`stems restart {stem} --build`, or set `build: true` on `stems.{stem}` so every start rebuilds it"
    )
}

/// `IMAGE_STALE` for `stem` running (or about to run) `image`.
pub fn image_stale_error(stem: &str, image: &str, verdict: &Verdict) -> Error {
    let message = match verdict {
        Verdict::Stale {
            image_created,
            newest,
            path,
        } => format!(
            "`{stem}`'s image {image} was built before its build inputs last changed ({} modified {}, image built {})",
            path.display(),
            fmt_time(*newest),
            fmt_time(*image_created)
        ),
        _ => format!("`{stem}`'s image {image} is older than its build inputs"),
    };
    Error::new(ErrorCode::ImageStale, message)
        .with_hint(stale_hint(stem))
        .with_details(stale_details(stem, image, verdict))
}

/// `image.stale {stem, image, image_created, newest_input, newest_input_path}`.
pub fn stale_event(stem: &str, image: &str, verdict: &Verdict) -> EventDraft {
    EventDraft::new(EventKind::IMAGE_STALE, stems_api::DAEMON_ACTOR)
        .stem(stem)
        .data(stale_details(stem, image, verdict))
}

/// What the check found for a compose service with `build:`.
#[derive(Debug, Clone)]
pub struct ImageCheck {
    pub image: String,
    pub verdict: Verdict,
}

/// Compose service facts → image facts → verdict. `None`: the service has
/// no `build:` (nothing to compare), or anything failed (logged; never
/// blocks a start or a restart).
pub(crate) async fn compose_check(core: &Core, spec: &ComposeSpec) -> Option<ImageCheck> {
    let c = core.runtimes.containers()?;
    let run = async {
        let rt = c.compose(&spec.workspace).await?;
        let cfg = rt.service_config(spec).await?;
        let Some(build) = cfg.build else {
            return Ok::<Option<ImageCheck>, stems_runtime::RuntimeError>(None);
        };
        let facts = rt.docker().image_facts(&cfg.image).await?;
        let created = facts.and_then(|f| f.created);
        let extras = vec![build.dockerfile.clone(), spec.file.clone()];
        let verdict = tokio::task::spawn_blocking(move || {
            stale::check(
                created,
                &build.context,
                &build.dockerfile,
                &extras,
                SystemTime::now(),
            )
        })
        .await
        .map_err(|e| stems_runtime::RuntimeError::Container(e.to_string()))?;
        Ok(Some(ImageCheck {
            image: cfg.image,
            verdict,
        }))
    };
    match tokio::time::timeout(CHECK_BOUND, run).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            tracing::debug!(stem = %spec.stem, error = %e, "stale image check failed");
            None
        }
        Err(_) => {
            tracing::debug!(stem = %spec.stem, "stale image check timed out");
            None
        }
    }
}

/// Before a compose stem starts without a rebuild: a stale image is a
/// warning (`Core.warnings`, event `image.stale`); the stem still starts.
pub(crate) async fn warn_before_start(core: &Core, spec: &ComposeSpec) {
    if spec.build {
        return;
    }
    let Some(check) = compose_check(core, spec).await else {
        return;
    };
    if !check.verdict.is_stale() {
        return;
    }
    tracing::info!(stem = %spec.stem, image = %check.image, "starting with a stale image (build: false)");
    core.events
        .emit(stale_event(&spec.stem, &check.image, &check.verdict));
    core.warnings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(image_stale_error(&spec.stem, &check.image, &check.verdict));
}

/// The crash guard: a running compose stem started with `build == false`
/// crashed and the policy would restart it. When its image is stale, the
/// restart would run the same image again: `Some(error)` fails the stem
/// instead. `None` lets the policy restart it (fresh, no image, unknown,
/// a docker/process stem, or any failure of the check).
pub(crate) async fn crash_guard(core: &Arc<Core>, cell: &Arc<StemCell>) -> Option<Error> {
    let c = core.runtimes.containers()?;
    if c.compose_build(&cell.name)? {
        return None;
    }
    let ws = cell.info().restart.workspace().cloned()?;
    let stem = ws.workspace.stem(&cell.name)?;
    let mut spec =
        super::containers::compose_spec(&ws.workspace, stem, &core.run_id, &BTreeMap::new())
            .ok()?;
    spec.build = false;
    let check = compose_check(core, &spec).await?;
    check
        .verdict
        .is_stale()
        .then(|| image_stale_error(&cell.name, &check.image, &check.verdict))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn effective_build_table() {
        for (param, config, want) in [
            (None, true, true),
            (None, false, false),
            (Some(true), false, true),
            (Some(true), true, true),
            (Some(false), true, false),
            (Some(false), false, false),
        ] {
            assert_eq!(effective_build(param, config), want, "{param:?} {config}");
        }
    }

    #[test]
    fn stale_error_and_event_carry_the_facts() {
        let v = Verdict::Stale {
            image_created: t(1_760_000_000),
            newest: t(1_760_003_600),
            path: PathBuf::from("/work/shop-api/app.py"),
        };
        let e = image_stale_error("api", "stems-shop-api", &v);
        assert_eq!(e.code, ErrorCode::ImageStale);
        insta::assert_snapshot!(e.message, @"`api`'s image stems-shop-api was built before its build inputs last changed (/work/shop-api/app.py modified 2025-10-09 09:53:20 UTC, image built 2025-10-09 08:53:20 UTC)");
        assert_eq!(e.details["stem"], json!("api"));
        assert_eq!(e.details["image"], json!("stems-shop-api"));
        assert_eq!(
            e.details["newest_input_path"],
            json!("/work/shop-api/app.py")
        );
        assert_eq!(
            e.details["image_created"],
            json!("2025-10-09T08:53:20+00:00")
        );
        assert_eq!(
            e.details["newest_input"],
            json!("2025-10-09T09:53:20+00:00")
        );
        assert!(
            e.hint
                .as_deref()
                .unwrap()
                .contains("stems restart api --build")
        );
        let ev = stale_event("api", "stems-shop-api", &v);
        assert_eq!(ev.kind, EventKind::IMAGE_STALE);
        assert_eq!(ev.data["stem"], json!("api"));
        assert_eq!(ev.data["newest_input_path"], json!("/work/shop-api/app.py"));
    }
}
