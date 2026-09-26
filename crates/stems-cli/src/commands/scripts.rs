//! `stems build | reset | stamps` (deliverable 16, `docs/scripts.md`).
//!
//! * **`build [stems…]`** runs each stem's `build` script (default: every
//!   enabled stem that has one). `data = BuildResult { ok, built: [{stem,
//!   script, exit, duration_ms}], skipped, failed: [{stem, error}] }`; a
//!   failed build is `SCRIPT_FAILED` (exit 1).
//! * **`reset [stems…] --yes`** stops the stems, runs their `reset` scripts
//!   and clears their stamps (default: every enabled stem). Destructive:
//!   without `--yes` it is `DESTRUCTIVE_NOT_CONFIRMED` (exit 2). `data =
//!   ResetResult { ok, stopped, reset, cleared, failed }`.
//! * **`stamps [stem] [--clear]`** lists the recorded stamps (`data =
//!   StampsResult { stamps: [{stem, script, hash, computed_at, inputs}],
//!   cleared }`), from the daemon when one runs, else from `state.json`.
//!
//! `build` and `reset` start the workspace daemon when none runs and shut it
//! down again afterwards if nothing is running.

use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use stems_api::{
    BuildParams, BuildResult, Method, ResetParams, ResetResult, StampsParams, StampsResult,
};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::state::StateFile;

use crate::cli::{BuildArgs, ResetArgs, StampsArgs};
use crate::client::{self, block_on, connect_to};
use crate::commands::Ctx;
use crate::commands::lifecycle::{connect_or_start, shutdown_if_idle};
use crate::output::CommandOutput;

/// Scripts may run long; the RPC is bounded generously.
const LONG: Duration = Duration::from_secs(3600);

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn list(v: &[String]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(", ")
    }
}

/// Call `method` on the daemon, starting it if needed (and stopping it
/// again if it was started here and runs nothing).
async fn call<P: Serialize, R: serde::de::DeserializeOwned>(
    ctx: &Ctx,
    method: Method,
    params: &P,
) -> Result<R, Errors> {
    let t = client::target(ctx)?;
    let (c, started) = connect_or_start(ctx, &t).await?;
    let r = c.call_with_timeout(method, params, LONG).await;
    if started {
        shutdown_if_idle(&t, &c).await;
    }
    Ok(r?)
}

/// `stems build`.
pub fn build(ctx: &Ctx, args: &BuildArgs) -> CommandOutput {
    block_on(async {
        let p = BuildParams {
            stems: args.stems.clone(),
        };
        let r: BuildResult = call(ctx, Method::BUILD, &p).await?;
        let mut human = String::new();
        for b in &r.built {
            human.push_str(&format!(
                "built {} ({} ms)\n",
                b.stem.as_deref().unwrap_or("-"),
                b.duration_ms
            ));
        }
        if !r.skipped.is_empty() {
            human.push_str(&format!("no build script: {}\n", list(&r.skipped)));
        }
        for f in &r.failed {
            human.push_str(&format!("failed: {}: {}\n", f.stem, f.error.message));
        }
        let errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
        Ok::<_, Errors>(
            CommandOutput::data(to_value(&r))
                .with_human(human)
                .with_errors(Errors(errors)),
        )
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stems reset`.
pub fn reset(ctx: &Ctx, args: &ResetArgs) -> CommandOutput {
    block_on(async {
        if !args.yes {
            let what = if args.stems.is_empty() {
                "every stem".to_string()
            } else {
                args.stems.join(", ")
            };
            return Err(Errors::from(
                Error::new(
                    ErrorCode::DestructiveNotConfirmed,
                    format!("`stems reset` stops {what}, runs their `reset` scripts and clears their stamps"),
                )
                .with_hint("rerun with `--yes` to confirm")
                .with_details(json!({ "command": "reset", "stems": args.stems })),
            ));
        }
        let p = ResetParams {
            stems: args.stems.clone(),
        };
        let r: ResetResult = call(ctx, Method::RESET, &p).await?;
        let mut human = String::new();
        if !r.stopped.is_empty() {
            human.push_str(&format!("stopped: {}\n", list(&r.stopped)));
        }
        for s in &r.reset {
            human.push_str(&format!(
                "reset {} ({} ms)\n",
                s.stem.as_deref().unwrap_or("-"),
                s.duration_ms
            ));
        }
        human.push_str(&format!("stamps cleared: {}\n", list(&r.cleared)));
        for f in &r.failed {
            human.push_str(&format!("failed: {}: {}\n", f.stem, f.error.message));
        }
        let errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
        Ok(CommandOutput::data(to_value(&r))
            .with_human(human)
            .with_errors(Errors(errors)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

fn stamps_human(r: &StampsResult) -> String {
    if r.stamps.is_empty() {
        return if r.cleared {
            "no stamps to clear\n".into()
        } else {
            "no stamps recorded\n".into()
        };
    }
    let w = r
        .stamps
        .iter()
        .map(|s| s.stem.len())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut out = format!(
        "{:<w$}  {:<10}  {:<12}  COMPUTED\n",
        "STEM", "SCRIPT", "HASH"
    );
    for s in &r.stamps {
        out.push_str(&format!(
            "{:<w$}  {:<10}  {:<12}  {}\n",
            s.stem,
            s.script,
            &s.hash[..s.hash.len().min(12)],
            s.computed_at.format("%Y-%m-%d %H:%M:%S")
        ));
    }
    if r.cleared {
        out.push_str("cleared\n");
    }
    out
}

/// `stems stamps`.
pub fn stamps(ctx: &Ctx, args: &StampsArgs) -> CommandOutput {
    block_on(async {
        let t = client::target(ctx)?;
        let p = StampsParams {
            stem: args.stem.clone(),
            clear: args.clear,
        };
        let r: StampsResult = match connect_to(&t, client::options(ctx)).await {
            Ok(c) => c.call(Method::STAMPS, &p).await?,
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
                offline(&t.paths.state, &p)?
            }
            Err(e) => return Err(e.into()),
        };
        Ok::<_, Errors>(CommandOutput::data(to_value(&r)).with_human(stamps_human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// `stamps` without a daemon: read (and with `clear`, rewrite) `state.json`.
fn offline(path: &std::path::Path, p: &StampsParams) -> Result<StampsResult, Error> {
    let Some(mut file) = StateFile::peek(path) else {
        return Ok(StampsResult {
            stamps: Vec::new(),
            cleared: p.clear,
        });
    };
    let stamps = stems_daemon::supervisor::hooks::entries(&file.stamps, p.stem.as_deref());
    if p.clear {
        match &p.stem {
            Some(s) => file.stamps.clear_stem(s),
            None => file.stamps.clear(),
        }
        file.write_atomic(path)
            .map_err(|e| Error::internal(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(StampsResult {
        stamps,
        cleared: p.clear,
    })
}
