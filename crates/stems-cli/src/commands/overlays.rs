//! `stems overlays [stem]` (deliverable 18, `docs/overlays.md`): the files
//! stems materialised into codebases and what is on disk now.
//!
//! `data = OverlaysResult { overlays: [{stem, dest, status, keep, sha256,
//! run_id}] }` with `status` one of `present` (the bytes stems wrote),
//! `modified` (changed since; `down` leaves it in place) or `missing`. From
//! the daemon when one runs, else from `state.json` (daemonless).
//!
//! `stems overlays render <stem> [--dry-run]` (47, `docs/overlays.md#
//! rendering-on-demand`): `data = OverlaysRenderResult {stem, dry_run,
//! files: [{dest, mode, keep, content, written}]}`. By the daemon when one
//! runs (RPC `overlays_render`), else locally with the same code
//! ([`stems_daemon::supervisor::overlays::render_all`] /
//! [`write_rendered`](stems_daemon::supervisor::overlays::write_rendered))
//! on `state.json`, holding the workspace lock while writing.

use std::collections::HashMap;

use serde_json::json;
use stems_api::{
    Method, OverlayFileStatus, OverlaysParams, OverlaysRenderParams, OverlaysRenderResult,
    OverlaysResult,
};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::state::StateFile;
use stems_daemon::supervisor::overlays::{
    FileLedger, Written, render_all, render_result, write_rendered,
};

use crate::cli::{OverlaysArgs, OverlaysCommand, OverlaysRenderArgs};
use crate::client::{self, Target, block_on, connect_to};
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

/// Human text of `overlays render`: the files themselves for a dry run,
/// else what was written.
fn render_human(r: &OverlaysRenderResult) -> String {
    if r.files.is_empty() {
        return format!("`{}` declares no overlays\n", r.stem);
    }
    let mut out = String::new();
    for f in &r.files {
        if r.dry_run {
            out.push_str(&format!("# {} (mode {})\n", f.dest.display(), f.mode));
            out.push_str(&f.content);
            if !f.content.ends_with('\n') {
                out.push('\n');
            }
        } else {
            let what = if f.written { "written  " } else { "unchanged" };
            out.push_str(&format!("{what}  {}  {}\n", f.mode, f.dest.display()));
        }
    }
    out
}

/// Render daemonless: the stem's configured env over the caller's, ports
/// as configured; writes under the workspace lock.
fn render_locally(
    ctx: &Ctx,
    t: &Target,
    p: &OverlaysRenderParams,
) -> Result<OverlaysRenderResult, Errors> {
    let ws = stems_config::load(ctx.load_options()).map_err(Errors::from)?;
    stems_daemon::supervisor::schedule::plan(&ws, std::slice::from_ref(&p.stem), true)
        .map_err(Errors::from)?;
    let st = ws
        .workspace
        .stem(&p.stem)
        .ok_or_else(|| Errors::from(Error::internal(format!("stem `{}` vanished", p.stem))))?;
    let state_dir = t.paths.dir.join("stems").join(&p.stem);
    let (own, _) = stems_daemon::supervisor::run::exec_env(
        &ws.workspace,
        st,
        &stems_daemon::supervisor::PortBook::default().frozen(),
        None,
        None,
        &state_dir,
    )
    .map_err(Errors::from)?;
    let mut env: HashMap<String, String> = ctx
        .env
        .iter()
        .filter(|(k, _)| !stems_core::callerenv::is_stems_injected(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.extend(own);
    let rctx = stems_core::overlays::RenderCtx::for_stem(&ws.workspace, st, env);
    let files = render_all(st, &rctx).map_err(Errors::from)?;
    if p.dry_run || files.is_empty() {
        return Ok(render_result(&st.name, p.dry_run, &files, &[]));
    }
    let _lock = stems_daemon::lock::acquire(&t.paths).map_err(Errors::from)?;
    let ledger = FileLedger::open(&t.paths.state);
    let run_id = ledger.run_id();
    let mut written = Vec::with_capacity(files.len());
    for f in &files {
        let w = write_rendered(&ledger, &st.name, f, &run_id, None).map_err(Errors::from)?;
        written.push(matches!(w, Written::Written { .. }));
    }
    Ok(render_result(&st.name, false, &files, &written))
}

/// `stems overlays render`.
fn render(ctx: &Ctx, args: &OverlaysRenderArgs) -> CommandOutput {
    block_on(async {
        let t = client::target(ctx)?;
        let p = OverlaysRenderParams {
            stem: args.stem.clone(),
            dry_run: args.dry_run,
        };
        let r: OverlaysRenderResult = match connect_to(&t, client::options(ctx)).await {
            Ok(c) => c.call(Method::OVERLAYS_RENDER, &p).await?,
            Err(e) if e.code == ErrorCode::DaemonNotRunning => render_locally(ctx, &t, &p)?,
            Err(e) => return Err(e.into()),
        };
        let data = serde_json::to_value(&r).unwrap_or_default();
        Ok::<_, Errors>(CommandOutput::data(data).with_human(render_human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems overlays`.
pub fn run(ctx: &Ctx, args: &OverlaysArgs) -> CommandOutput {
    if let Some(OverlaysCommand::Render(a)) = &args.command {
        return render(ctx, a);
    }
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

    #[test]
    fn render_human_output() {
        let file = |dest: &str, written| stems_api::RenderedOverlay {
            dest: dest.into(),
            mode: "0600".into(),
            keep: false,
            content: "port = 8080".into(),
            written,
        };
        let mut r = OverlaysRenderResult {
            stem: "api".into(),
            dry_run: true,
            files: vec![file("/code/api/config/local.ini", false)],
        };
        insta::assert_snapshot!(render_human(&r), @r"
        # /code/api/config/local.ini (mode 0600)
        port = 8080
        ");
        r.dry_run = false;
        r.files.push(file("/code/api/.env.local", true));
        insta::assert_snapshot!(render_human(&r), @r"
        unchanged  0600  /code/api/config/local.ini
        written    0600  /code/api/.env.local
        ");
    }
}
