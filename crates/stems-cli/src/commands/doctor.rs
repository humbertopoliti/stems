//! `stems doctor` (deliverable 19, FR-CL-3, FR-WD-4; `docs/doctor.md`).
//!
//! `stems doctor [--json] [--fix [--yes] [--kill-foreign]] [--strict]` runs
//! the checks of [`stems_daemon::doctor`] right here (no daemon needed; a
//! running daemon is only asked for its health) and prints the table
//! `CHECK STATUS MESSAGE` with hints below. `data` is a
//! [`DoctorReport`]: `{ ok, checks: [{id, status, message, hint, fixable,
//! details, code}], fixed: [{id, action, ok, message}], summary: {ok, warn,
//! fail} }`. Exit 0 when nothing fails (warnings too, unless `--strict`),
//! else 1; in `--json` mode every failing check is also an entry of
//! `errors` (its `code`, with `details.check`).
//!
//! `--fix` applies the fixable items after a confirmation prompt (human mode
//! on a terminal) or `--yes`, then runs the checks again: the report shows
//! the state after the fixes and `fixed` what was done. Without consent
//! nothing is touched (`data.fix_skipped` says why).
//!
//! `stems doctor --orphans [--yes] [--kill-foreign] [--json]` (deliverable
//! 11, FR-CR-4) only scans for orphans:
//! `data: { orphans: [Orphan + {action}], remaining: n }`. Exit 0 when there
//! are no orphans (or all were killed/adopted), 3 `ORPHANS_FOUND` (with
//! `details.orphans`) while some remain. Without `--yes` in `--json` mode (or
//! without a terminal) nothing is touched. The policy table is in
//! [`crate::commands::orphans`]; `docs/recovery.md` has the details.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use stems_api::client::Client;
use stems_api::{DaemonStatus, Method};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::doctor::{
    self, CheckStatus, DaemonProbe, DoctorCtx, DoctorInput, DoctorReport, fix,
};

use crate::cli::DoctorArgs;
use crate::client::{self, Target, block_on, connect_to};
use crate::commands::Ctx;
use crate::commands::orphans::{self, Policy};
use crate::output::{CommandOutput, Mode};

/// Run `stems doctor`.
pub fn run(ctx: &Ctx, args: DoctorArgs, mode: Mode) -> CommandOutput {
    if args.orphans {
        return block_on(orphans_check(ctx, &args, mode)).unwrap_or_else(CommandOutput::failed);
    }
    block_on(full(ctx, &args, mode)).unwrap_or_else(CommandOutput::failed)
}

/// Ask a running daemon for its health (bounded).
pub async fn probe_daemon(ctx: &Ctx, t: &Target) -> DaemonProbe {
    let probe = async {
        match Client::connect(&t.paths.socket, client::options(ctx)).await {
            Ok(c) => match c
                .call::<DaemonStatus>(Method::DAEMON_STATUS, json!({}))
                .await
            {
                Ok(st) => DaemonProbe::Running(Box::new(st)),
                Err(e) => DaemonProbe::Unreachable(e),
            },
            Err(e) if e.code == ErrorCode::DaemonNotRunning => DaemonProbe::NotRunning,
            Err(e) if e.code == ErrorCode::DaemonVersionMismatch => DaemonProbe::Incompatible(e),
            Err(e) => DaemonProbe::Unreachable(e),
        }
    };
    tokio::time::timeout(doctor::CHECK_TIMEOUT, probe)
        .await
        .unwrap_or_else(|_| {
            DaemonProbe::Unreachable(
                Error::new(
                    ErrorCode::DaemonNotRunning,
                    "the daemon did not answer `daemon_status` in time",
                )
                .with_hint("see its log, or restart it with `stems daemon stop` and `stems up`"),
            )
        })
}

fn confirm(report: &DoctorReport) -> bool {
    let mut err = std::io::stderr();
    let _ = writeln!(err, "stems doctor can fix:");
    for c in report.fixable() {
        let _ = writeln!(err, "  {}: {}", c.id, c.message);
    }
    let _ = write!(err, "apply these fixes? [y/N] ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

async fn full(ctx: &Ctx, args: &DoctorArgs, mode: Mode) -> Result<CommandOutput, Errors> {
    let t = client::target(ctx)?;
    let input = DoctorInput {
        paths: t.paths.clone(),
        home: t.home.clone(),
        load: ctx.load_options(),
        daemon: probe_daemon(ctx, &t).await,
        docker_host: ctx.env.get("DOCKER_HOST").cloned(),
        ignore_pids: vec![std::process::id() as i32],
    };
    let mut report = doctor::run(input.clone(), args.strict).await;
    let mut skipped = None;
    if args.fix && report.fixable().next().is_some() {
        let interactive = mode == Mode::Human && std::io::stdin().is_terminal();
        if args.yes || (interactive && confirm(&report)) {
            let cx = Arc::new(DoctorCtx::load(input.clone()));
            let opts = fix::FixOptions {
                kill_foreign: args.kill_foreign,
            };
            let fixed = fix::apply(&cx, &report, opts).await;
            // Let killed processes release their ports before re-checking.
            if fixed.iter().any(|f| f.action == "kill_orphan") {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            report = doctor::run(input, args.strict).await;
            report.fixed = fixed;
        } else {
            skipped = Some(if interactive {
                "not confirmed"
            } else {
                "nothing was fixed without confirmation: rerun with `--fix --yes`"
            });
        }
    }
    let mut human = report.render_human();
    let mut data = serde_json::to_value(&report).unwrap_or(Value::Null);
    if let Some(why) = skipped {
        human.push_str(&format!("fix: {why}\n"));
        if let Value::Object(m) = &mut data {
            m.insert("fix_skipped".into(), json!(why));
        }
    }
    let mut out = CommandOutput::data(data)
        .with_human(human)
        .with_exit(report.exit_code());
    if mode == Mode::Json {
        let errors: Vec<Error> = report
            .checks
            .iter()
            .filter(|c| {
                c.status == CheckStatus::Fail
                    || (args.strict && c.status == CheckStatus::Warn && c.code.is_some())
            })
            .map(doctor::CheckResult::to_error)
            .collect();
        out = out.with_errors(Errors(errors));
    }
    Ok(out)
}

async fn orphans_check(ctx: &Ctx, args: &DoctorArgs, mode: Mode) -> Result<CommandOutput, Errors> {
    let t = client::target(ctx)?;
    // A running daemon is never an orphan, and can adopt (interactive only).
    let daemon = connect_to(&t, client::options(ctx)).await.ok();
    let ignore: Vec<i32> = daemon.iter().map(|c| c.info().pid as i32).collect();
    let found = orphans::scan(ctx, &t, &ignore)?;
    let policy = Policy::new(
        args.yes,
        false,
        false,
        args.kill_foreign,
        mode == Mode::Json || !std::io::stdin().is_terminal(),
    );
    let resolved = orphans::resolve(&found, &policy, daemon.as_ref()).await;
    let remaining = orphans::remaining(&resolved);
    let mut human = if found.is_empty() {
        "no orphans\n".to_string()
    } else {
        orphans::header(&found)
    };
    human.push_str(&orphans::human(&resolved));
    let out = CommandOutput::data(json!({
        "orphans": resolved,
        "remaining": remaining.len(),
    }))
    .with_human(human);
    if remaining.is_empty() {
        return Ok(out);
    }
    let hint = if args.yes && !args.kill_foreign {
        "the remaining processes do not look like their stem's start command; stop them yourself, or rerun with `--kill-foreign` to kill them"
    } else {
        "`stems doctor --orphans --yes` kills those that look like their stem's start command; `--kill-foreign` also kills the rest"
    };
    Ok(out.with_errors(orphans::found_error(&remaining, hint)))
}
