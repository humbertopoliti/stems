//! `stems pull`: pull the images of docker stems now, whatever their
//! `pull` policy, and optionally restart the running ones whose image
//! changed (`docs/docker.md`, "stems pull").
//!
//! Each distinct image is pulled once, all concurrently; progress goes out
//! as `docker.pull` events of the first stem using it. Credentials resolve
//! in the daemon's environment, exactly as for a start.

use std::collections::BTreeMap;
use std::time::Instant;

use stems_api::{PullParams, PullResult, PullSkip, PulledImage, RestartParams, StemFailure};
use stems_config::{PullPolicy, StemRuntime, Workspace};
use stems_core::Error;
use stems_runtime::RuntimeError;

use super::containers::{progress_sink, pull_error};
use super::{Supervisor, schedule};

/// Why a selected stem is not pulled.
pub const SKIP_NOT_DOCKER: &str = "not a docker stem";
pub const SKIP_BUILT: &str = "built from `build:`";
pub const SKIP_NEVER: &str = "`pull: never` (name the stem to pull it anyway)";

/// Split `order` (the selected stems, start order) into `(stem, image)`
/// targets and skips. With no stems `requested`, only docker stems are
/// considered (others are not reported) and `pull: never` ones are skipped;
/// a named stem is always pulled when it has an `image`.
pub fn pull_targets(
    ws: &Workspace,
    order: &[String],
    requested: &[String],
) -> (Vec<(String, String)>, Vec<PullSkip>) {
    let mut targets = Vec::new();
    let mut skipped = Vec::new();
    let skip = |stem: &str, reason: &str| PullSkip {
        stem: stem.to_string(),
        reason: reason.to_string(),
    };
    for name in order {
        let Some(stem) = ws.stem(name) else { continue };
        let named = requested.contains(name);
        match &stem.runtime {
            StemRuntime::Docker(d) => match &d.image {
                Some(_) if d.pull == PullPolicy::Never && !named => {
                    skipped.push(skip(name, SKIP_NEVER))
                }
                Some(image) => targets.push((name.clone(), image.clone())),
                None => skipped.push(skip(name, SKIP_BUILT)),
            },
            _ if named => skipped.push(skip(name, SKIP_NOT_DOCKER)),
            _ => {}
        }
    }
    (targets, skipped)
}

/// `pull --restart`: the running stems whose image changed.
pub fn to_restart(pulled: &[PulledImage], running: impl Fn(&str) -> bool) -> Vec<String> {
    pulled
        .iter()
        .filter(|x| x.changed && running(&x.stem))
        .map(|x| x.stem.clone())
        .collect()
}

/// A copy of `e` for each stem sharing the failed image (`RuntimeError` is
/// not `Clone`); the variants a pull returns are kept, anything else keeps
/// its text.
fn same_error(e: &RuntimeError) -> RuntimeError {
    match e {
        RuntimeError::ImagePullFailed { image, message } => RuntimeError::ImagePullFailed {
            image: image.clone(),
            message: message.clone(),
        },
        RuntimeError::DockerUnavailable { hint } => {
            RuntimeError::DockerUnavailable { hint: hint.clone() }
        }
        other => RuntimeError::Container(other.to_string()),
    }
}

impl Supervisor {
    /// `pull`: see the module docs.
    pub async fn pull(&self, p: PullParams, actor: &str) -> Result<PullResult, Error> {
        let ws = self.workspace(true, actor)?;
        let plan = schedule::plan(&ws, &p.stems, true)?;
        let (targets, skipped) = pull_targets(&ws.workspace, &plan.order, &p.stems);
        let mut res = PullResult {
            skipped,
            ..PullResult::default()
        };
        if !targets.is_empty() {
            let puller = self.core.runtimes.puller().cloned().ok_or_else(|| {
                Error::internal("this daemon has no docker runtime to pull images with")
            })?;
            // One pull per distinct image, reported for its first stem.
            let mut images: BTreeMap<&str, &str> = BTreeMap::new();
            for (stem, image) in &targets {
                images.entry(image.as_str()).or_insert(stem.as_str());
            }
            let pulls = images.iter().map(|(image, stem)| {
                let puller = puller.clone();
                let progress = progress_sink(&self.core, stem);
                async move {
                    let t0 = Instant::now();
                    let r = puller.pull(image, progress).await;
                    (*image, r, t0.elapsed().as_millis() as u64)
                }
            });
            let done: BTreeMap<_, _> = futures::future::join_all(pulls)
                .await
                .into_iter()
                .map(|(image, r, ms)| (image, (r, ms)))
                .collect();
            for (stem, image) in &targets {
                match &done[image.as_str()] {
                    (Ok(o), ms) => res.pulled.push(PulledImage {
                        stem: stem.clone(),
                        image: image.clone(),
                        before: o.before.clone(),
                        after: o.after.clone(),
                        changed: o.changed(),
                        duration_ms: *ms,
                    }),
                    (Err(e), _) => res.failed.push(StemFailure {
                        stem: stem.clone(),
                        error: pull_error(stem, image, same_error(e)),
                    }),
                }
            }
        }
        if p.restart {
            let stems = to_restart(&res.pulled, |s| self.cell(s).state().is_running());
            if !stems.is_empty() {
                let params = RestartParams {
                    stems: stems.clone(),
                    no_deps: true,
                    ..RestartParams::default()
                };
                match self.restart(params, actor).await {
                    Ok(up) => res.restarted = Some(up),
                    Err(error) => res.failed.extend(stems.into_iter().map(|stem| StemFailure {
                        stem,
                        error: error.clone(),
                    })),
                }
            }
        }
        res.ok = res.failed.is_empty() && res.restarted.as_ref().is_none_or(|r| r.ok);
        Ok(res)
    }
}
