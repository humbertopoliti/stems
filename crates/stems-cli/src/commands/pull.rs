//! `stems pull [stems…] [--restart]`: pull the images of docker stems now
//! (`docs/docker.md`, "stems pull").
//!
//! Default: every enabled docker stem with an `image`, except `pull: never`
//! ones (pulled only when named). Pulls run in the daemon (started when none
//! runs, stopped again afterwards if it runs nothing), so they use its
//! registry credentials exactly like a start. Human mode streams the
//! image-level `docker.pull` progress. `data = PullResult { ok, pulled:
//! [{stem, image, before, after, changed, duration_ms}], skipped: [{stem,
//! reason}], failed: [{stem, error}], restarted? }`; a failed pull is
//! `IMAGE_PULL_FAILED` (exit 1). `--restart` restarts the running stems
//! whose image changed.

use std::io::Write;
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use stems_api::{Method, PullParams, PullResult};
use stems_core::{Error, Errors};

use crate::cli::PullArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::lifecycle::{
    connect_or_start, progress_line, shutdown_if_idle, subscribe_now,
};
use crate::output::{CommandOutput, Mode};

/// Pulls of large images take a while; the RPC is bounded generously.
const LONG: Duration = Duration::from_secs(3600);

/// `sha256:0123456789abcdef…` → `0123456789ab` (like `docker images`).
fn short_id(id: &Option<String>) -> String {
    id.as_deref().map_or_else(
        || "-".into(),
        |i| i.trim_start_matches("sha256:").chars().take(12).collect(),
    )
}

/// The human summary of a pull.
pub fn human(r: &PullResult) -> String {
    let mut s = String::new();
    let w = r.pulled.iter().map(|p| p.stem.len()).max().unwrap_or(0);
    let iw = r.pulled.iter().map(|p| p.image.len()).max().unwrap_or(0);
    for p in &r.pulled {
        let what = match (&p.before, p.changed) {
            (None, _) => format!("new ({})", short_id(&p.after)),
            (Some(_), true) => format!(
                "updated ({} -> {})",
                short_id(&p.before),
                short_id(&p.after)
            ),
            (Some(_), false) => "up to date".into(),
        };
        s.push_str(&format!(
            "pulled  {:<w$}  {:<iw$}  {what}  ({:.1}s)\n",
            p.stem,
            p.image,
            p.duration_ms as f64 / 1000.0
        ));
    }
    for k in &r.skipped {
        s.push_str(&format!("skipped {}: {}\n", k.stem, k.reason));
    }
    // Failures are not repeated here: they render as the command's errors.
    if r.pulled.is_empty() && r.skipped.is_empty() && r.failed.is_empty() {
        s.push_str("nothing to pull: no docker stem with an `image` selected\n");
    }
    match &r.restarted {
        Some(up) => {
            if !up.ready.is_empty() {
                s.push_str(&format!("restarted: {}\n", up.ready.join(", ")));
            }
        }
        None => {
            let changed: Vec<&str> = r
                .pulled
                .iter()
                .filter(|p| p.changed && p.before.is_some())
                .map(|p| p.stem.as_str())
                .collect();
            if !changed.is_empty() {
                s.push_str(&format!(
                    "running stems keep their old image until restarted: `stems restart {}` (or `stems pull --restart`)\n",
                    changed.join(" ")
                ));
            }
        }
    }
    s
}

/// Every error of `r`: failed pulls, then failed restarts.
fn errors(r: &PullResult) -> Errors {
    let mut v: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
    if let Some(up) = &r.restarted {
        v.extend(up.failed.iter().map(|f| f.error.clone()));
    }
    Errors(v)
}

/// `stems pull`.
pub fn run(ctx: &Ctx, args: &PullArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    block_on(async {
        let t = client::target(ctx)?;
        let (c, started) = connect_or_start(ctx, &t).await?;
        let params = PullParams {
            stems: args.stems.clone(),
            restart: args.restart,
        };
        // Progress for humans; JSON stays a single envelope.
        let mut events = match mode {
            Mode::Human => subscribe_now(&c).await.ok(),
            Mode::Json => None,
        };
        let call = c.call_with_timeout::<PullResult>(Method::PULL, &params, LONG);
        tokio::pin!(call);
        let mut show = |e: &stems_api::Event| {
            if let Some(l) = progress_line(e) {
                let _ = writeln!(stdout, "{l}");
                let _ = stdout.flush();
            }
        };
        let r = loop {
            match events.as_mut() {
                Some(ev) => tokio::select! {
                    r = &mut call => break r,
                    Some(e) = ev.next() => show(&e),
                },
                None => break (&mut call).await,
            }
        };
        // Progress events are emitted asynchronously: drain the tail.
        if let Some(ev) = events.as_mut() {
            while let Ok(Some(e)) =
                tokio::time::timeout(Duration::from_millis(200), ev.next()).await
            {
                show(&e);
            }
        }
        if started {
            shutdown_if_idle(&t, &c).await;
        }
        let r = r?;
        let data = serde_json::to_value(&r).unwrap_or(Value::Null);
        Ok::<_, Errors>(
            CommandOutput::data(data)
                .with_human(human(&r))
                .with_errors(errors(&r)),
        )
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stems_api::{PullSkip, PulledImage, StemFailure, UpResult};
    use stems_core::ErrorCode;

    use super::*;

    fn pulled(stem: &str, before: Option<&str>, after: &str, ms: u64) -> PulledImage {
        PulledImage {
            stem: stem.into(),
            image: format!("ghcr.io/acme/{stem}:main"),
            before: before.map(Into::into),
            after: Some(after.into()),
            changed: before != Some(after),
            duration_ms: ms,
        }
    }

    fn sample() -> PullResult {
        PullResult {
            ok: false,
            pulled: vec![
                pulled(
                    "api",
                    Some("sha256:1111111111111111aaaa"),
                    "sha256:2222222222222222bbbb",
                    3200,
                ),
                pulled(
                    "postgres",
                    Some("sha256:3333333333333333"),
                    "sha256:3333333333333333",
                    150,
                ),
                pulled("worker", None, "sha256:4444444444444444", 9000),
            ],
            skipped: vec![PullSkip {
                stem: "web".into(),
                reason: "built from `build:`".into(),
            }],
            failed: vec![StemFailure {
                stem: "broken".into(),
                error: Error::new(
                    ErrorCode::ImagePullFailed,
                    "pulling `ghcr.io/acme/nope:1` for `broken` failed: denied",
                ),
            }],
            restarted: None,
        }
    }

    #[test]
    fn human_summary() {
        insta::assert_snapshot!(human(&sample()), @r"
        pulled  api       ghcr.io/acme/api:main       updated (111111111111 -> 222222222222)  (3.2s)
        pulled  postgres  ghcr.io/acme/postgres:main  up to date  (0.1s)
        pulled  worker    ghcr.io/acme/worker:main    new (444444444444)  (9.0s)
        skipped web: built from `build:`
        running stems keep their old image until restarted: `stems restart api` (or `stems pull --restart`)
        ");
    }

    #[test]
    fn human_after_a_restart_and_when_empty() {
        let mut r = sample();
        r.failed.clear();
        r.restarted = Some(UpResult {
            ok: true,
            ready: vec!["api".into()],
            ..UpResult::default()
        });
        assert!(human(&r).ends_with("restarted: api\n"), "{}", human(&r));
        assert!(!human(&r).contains("keep their old image"));
        assert_eq!(
            human(&PullResult::default()),
            "nothing to pull: no docker stem with an `image` selected\n"
        );
    }

    #[test]
    fn errors_include_failed_restarts() {
        let mut r = sample();
        r.restarted = Some(UpResult {
            failed: vec![StemFailure {
                stem: "api".into(),
                error: Error::new(ErrorCode::StartFailed, "boom"),
            }],
            ..UpResult::default()
        });
        let codes: Vec<ErrorCode> = errors(&r).0.iter().map(|e| e.code).collect();
        assert_eq!(codes, [ErrorCode::ImagePullFailed, ErrorCode::StartFailed]);
        assert_eq!(
            serde_json::to_value(&r).unwrap()["pulled"][0]["changed"],
            json!(true)
        );
    }
}
