//! `stems overlays [stem]` (deliverable 18, `docs/overlays.md`): the files
//! stems materialised into codebases and what is on disk now.
//!
//! `data = OverlaysResult { overlays: [{stem, dest, status, keep, sha256,
//! run_id}] }` with `status` one of `present` (the bytes stems wrote),
//! `modified` (changed since; `down` leaves it in place) or `missing`. From
//! the daemon when one runs, else from `state.json` (daemonless).

use serde_json::json;
use stems_api::{Method, OverlayFileStatus, OverlaysParams, OverlaysResult};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::state::StateFile;

use crate::cli::OverlaysArgs;
use crate::client::{self, block_on, connect_to};
use crate::commands::Ctx;
use crate::output::CommandOutput;

fn human(r: &OverlaysResult) -> String {
    if r.overlays.is_empty() {
        return "no overlays recorded\n".into();
    }
    let w = r
        .overlays
        .iter()
        .map(|o| o.stem.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut out = format!("{:<w$}  {:<8}  {:<4}  DEST\n", "STEM", "STATUS", "KEEP");
    for o in &r.overlays {
        let status = match o.status {
            OverlayFileStatus::Present => "present",
            OverlayFileStatus::Modified => "modified",
            OverlayFileStatus::Missing => "missing",
        };
        out.push_str(&format!(
            "{:<w$}  {:<8}  {:<4}  {}\n",
            o.stem,
            status,
            if o.keep { "yes" } else { "no" },
            o.dest.display()
        ));
    }
    out
}

/// `stems overlays`.
pub fn run(ctx: &Ctx, args: &OverlaysArgs) -> CommandOutput {
    block_on(async {
        let t = client::target(ctx)?;
        let p = OverlaysParams {
            stem: args.stem.clone(),
        };
        let r: OverlaysResult = match connect_to(&t, client::options(ctx)).await {
            Ok(c) => c.call(Method::OVERLAYS, &p).await?,
            Err(e) if e.code == ErrorCode::DaemonNotRunning => {
                if let Some(stem) = &args.stem {
                    let ws = stems_config::load(ctx.load_options()).map_err(Errors::from)?;
                    if ws.workspace.stem(stem).is_none() {
                        return Err(Errors::from(
                            Error::new(
                                ErrorCode::UnknownStem,
                                format!("stem `{stem}` does not exist"),
                            )
                            .with_details(json!({ "stem": stem })),
                        ));
                    }
                }
                let ledger = StateFile::peek(&t.paths.state)
                    .map(|f| f.overlays)
                    .unwrap_or_default();
                OverlaysResult {
                    overlays: stems_daemon::supervisor::overlays::entries(
                        &ledger,
                        p.stem.as_deref(),
                    ),
                }
            }
            Err(e) => return Err(e.into()),
        };
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::OverlayEntry;

    #[test]
    fn human_table() {
        let r = OverlaysResult {
            overlays: vec![OverlayEntry {
                stem: "shop-api".into(),
                dest: "/code/shop-api/config/local.ini".into(),
                status: OverlayFileStatus::Modified,
                keep: false,
                sha256: "ab".repeat(32),
                run_id: "r".into(),
            }],
        };
        insta::assert_snapshot!(human(&r), @r"
        STEM      STATUS    KEEP  DEST
        shop-api  modified  no    /code/shop-api/config/local.ini
        ");
        assert_eq!(human(&OverlaysResult::default()), "no overlays recorded\n");
    }
}
