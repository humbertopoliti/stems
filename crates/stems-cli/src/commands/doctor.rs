//! `stems doctor` (deliverable 19). Only `--orphans` is implemented so far
//! (deliverable 11, FR-CR-4); every other check is still `NOT_IMPLEMENTED`.
//!
//! `stems doctor --orphans [--yes] [--kill-foreign] [--json]`:
//! `data: { orphans: [Orphan + {action}], remaining: n }`. Exit 0 when there
//! are no orphans (or all were killed/adopted), 3 `ORPHANS_FOUND` (with
//! `details.orphans`) while some remain. Without `--yes` in `--json` mode (or
//! without a terminal) nothing is touched. The policy table is in
//! [`crate::commands::orphans`]; `docs/recovery.md` has the details.

use std::io::IsTerminal;

use serde_json::json;
use stems_core::Errors;

use crate::cli::{Command, DoctorArgs};
use crate::client::{self, block_on, connect_to};
use crate::commands::Ctx;
use crate::commands::orphans::{self, Policy};
use crate::output::{CommandOutput, Mode};

/// Run `stems doctor`.
pub fn run(ctx: &Ctx, args: DoctorArgs, mode: Mode) -> CommandOutput {
    if !args.orphans || args.fix {
        return crate::commands::stubs::run(&Command::Doctor(args));
    }
    block_on(orphans_check(ctx, &args, mode)).unwrap_or_else(CommandOutput::failed)
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
