//! `stems validate [--skip-requires]` (FR-WS-6): load and check the
//! workspace without starting anything.
//!
//! `data`: `{ "ok": bool, "start_order": [[stem…]…] | null, "warnings":
//! [Error] }`; every error in `errors`, sorted by (file, line, col); exit 2
//! for config errors. Warnings (`OVERLAY_TRACKED_FILE`) never change the
//! exit code.
//!
//! Overlay destinations stems wrote itself (the `overlays` ledger of the
//! workspace's `state.json`, read without a daemon) are not
//! `OVERLAY_CONFLICT`s (18).

use serde_json::{Value, json};
use stems_core::{Errors, ValidateOptions, load_and_validate, start_order};

use crate::cli::ValidateArgs;
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Run `validate`.
pub fn run(ctx: &Ctx, args: &ValidateArgs) -> CommandOutput {
    let owned_overlays = crate::client::target(ctx)
        .ok()
        .and_then(|t| stems_daemon::state::StateFile::peek(&t.paths.state))
        .map(|f| f.overlays.into_values().flatten().map(|r| r.dest).collect())
        .unwrap_or_default();
    let opts = ValidateOptions {
        skip_requires: args.skip_requires,
        owned_overlays,
        ..ValidateOptions::default()
    };
    let failed = |mut errors: Errors| {
        errors.sort();
        CommandOutput::data(json!({ "ok": false, "start_order": Value::Null, "warnings": [] }))
            .with_errors(errors)
    };
    let resolved = match load_and_validate(ctx.load_options(), &opts) {
        Ok(r) => r,
        Err(errors) => return failed(errors),
    };
    let order = match start_order(&resolved.workspace) {
        Ok(o) => o,
        Err(e) => return failed(Errors::from(e)),
    };
    let ws = &resolved.workspace;
    let mut human = format!(
        "{}: valid ({} stems, {} enabled)\n",
        ws.name,
        ws.stems.len(),
        ws.stems().count()
    );
    if args.skip_requires && !ws.requires.is_empty() {
        human.push_str("tool requirements not checked (--skip-requires)\n");
    }
    if !order.is_empty() {
        human.push_str("start order:\n");
        for (i, layer) in order.iter().enumerate() {
            human.push_str(&format!("  {}. {}\n", i + 1, layer.join(", ")));
        }
    }
    let warnings = stems_core::validate::warnings(&resolved);
    for w in &warnings {
        human.push_str(&format!("warning: {}: {}\n", w.code, w.message));
        if let Some(h) = &w.hint {
            human.push_str(&format!("  hint: {h}\n"));
        }
    }
    CommandOutput::data(json!({ "ok": true, "start_order": order, "warnings": warnings }))
        .with_human(human)
}
