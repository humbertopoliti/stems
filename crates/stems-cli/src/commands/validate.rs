//! `stems validate [--skip-requires]` (FR-WS-6): load and check the
//! workspace without starting anything.
//!
//! `data`: `{ "ok": bool, "start_order": [[stem…]…] | null, "warnings":
//! [Error] }`; every error in `errors`, sorted by (file, line, col); exit 2
//! for config errors. Warnings (`OVERLAY_TRACKED_FILE`; `TOOL_VERSION` for
//! a missing `docker compose` v2 when the workspace has compose stems)
//! never change the exit code.
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
    // `checks:` that opt into `validate` (48), run once here only (never on
    // a daemon config load), and only when the config is otherwise valid.
    let check_warnings = if args.skip_requires {
        Vec::new()
    } else {
        let (errors, warnings) = stems_core::checks::validate_results(ws);
        if !errors.is_empty() {
            return failed(Errors(errors));
        }
        warnings
    };
    let mut human = format!(
        "{}: valid ({} stems, {} enabled)\n",
        ws.name,
        ws.stems.len(),
        ws.stems().count()
    );
    if args.skip_requires && has_requires(ws) {
        human.push_str("tool requirements not checked (--skip-requires)\n");
    }
    if !order.is_empty() {
        human.push_str("start order:\n");
        for (i, layer) in order.iter().enumerate() {
            human.push_str(&format!("  {}. {}\n", i + 1, layer.join(", ")));
        }
    }
    let mut warnings = stems_core::validate::warnings(&resolved);
    if args.skip_requires {
        // Nothing is checked anyway: the notice above says so.
        warnings.retain(|w| w.code != stems_core::ErrorCode::RequiresNotChecked);
    }
    // `requires` auto-check (15): compose stems need docker compose v2. A
    // warning, so process-only CI still validates such workspaces.
    if !args.skip_requires
        && let Some(w) = stems_daemon::supervisor::containers::compose_warning(ws)
    {
        warnings.push(w);
    }
    warnings.extend(check_warnings);
    for w in &warnings {
        human.push_str(&format!("warning: {}: {}\n", w.code, w.message));
        if let Some(h) = &w.hint {
            human.push_str(&format!("  hint: {h}\n"));
        }
    }
    CommandOutput::data(json!({ "ok": true, "start_order": order, "warnings": warnings }))
        .with_human(human)
}

/// Any `requires:` entry, workspace-level or per stem (40).
fn has_requires(ws: &stems_config::Workspace) -> bool {
    !ws.requires.is_empty() || ws.stems.values().any(|s| !s.requires.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(yaml: &str) -> stems_config::Workspace {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stems.yaml"), yaml).unwrap();
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir.path().to_path_buf()),
            ..Default::default()
        })
        .unwrap()
        .workspace
    }

    #[test]
    fn skip_requires_notice_counts_per_stem_requires() {
        let none = "schema_version: 1\nname: t\nstems:\n  a: { type: process, scripts: { start: 'true' } }\n";
        assert!(!has_requires(&ws(none)));
        let per_stem = "schema_version: 1\nname: t\nstems:\n  a: { type: process, scripts: { start: 'true' }, requires: { node: '>=1' } }\n";
        assert!(has_requires(&ws(per_stem)));
        let top = "schema_version: 1\nname: t\nrequires: { node: '>=1' }\nstems:\n  a: { type: process, scripts: { start: 'true' } }\n";
        assert!(has_requires(&ws(top)));
    }
}
