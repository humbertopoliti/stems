//! `stems switch <stem> [<variant>|local] [--no-apply]` (FR-ST-8,
//! `docs/config.md#variants-fr-st-8`).
//!
//! - Without a variant: lists the stem's choices. `data: { stem, variant,
//!   variants: [{name, type, active}] }` (`local`, the base definition,
//!   first). Human: one line per choice, `*` marks the active one.
//! - With one: writes `stems.<stem>.variant: <variant>` to
//!   `stems.local.yaml` with the comment-preserving editor (`local` removes
//!   the key, or writes `variant: local` when `stems.yaml` selects a default
//!   variant), re-validates the workspace and restores the file if that
//!   introduces errors (exit 2). When the daemon runs and `--no-apply` is
//!   not given, the daemon does all of this (`switch_variant {stem,
//!   variant}`, shared with the TUI's variant picker) and then applies the
//!   change to that stem only (`config_apply {stems: [stem], yes: true}`):
//!   it is stopped in its old form and started in the new one. `data: { stem, from, to, type, path, file, changed,
//!   diff, daemon, applied, status }` where `applied` is the
//!   `ConfigApplyResult` and `status` the stem's `StemStatus` afterwards
//!   (both `null` when nothing was applied).
//!
//! An unknown variant is `UNKNOWN_VARIANT` (exit 2, nothing written); an
//! unknown stem `UNKNOWN_STEM`.

use serde_json::{Value as Json, json};
use stems_api::{
    Method, StatusParams, StatusResult, StemStatus, SwitchVariantParams, SwitchVariantResult,
};
use stems_config::variants::{self, Choice};
use stems_config::{ConfigPath, edit};
use stems_core::{Error, ErrorCode, Errors};

use crate::cli::SwitchArgs;
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::config;
use crate::output::CommandOutput;

fn unknown_stem(ws: &stems_config::Workspace, name: &str) -> Error {
    let known: Vec<&String> = ws.stems.keys().collect();
    Error::new(ErrorCode::UnknownStem, format!("no stem named `{name}`"))
        .with_hint(format!(
            "stems in this workspace: {}",
            known
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .with_details(json!({ "stem": name, "known": known }))
}

fn unknown_variant(stem: &str, variant: &str, choices: &[Choice]) -> Error {
    let names: Vec<&str> = choices
        .iter()
        .map(|c| c.name.as_str())
        .filter(|n| *n != variants::BASE_VARIANT)
        .collect();
    let hint = if names.is_empty() {
        format!(
            "`{stem}` declares no `variants` (see docs/config.md#variants-fr-st-8 to add one, e.g. a docker form)"
        )
    } else {
        format!(
            "variants of `{stem}`: {} (or `{}` for the base definition)",
            names.join(", "),
            variants::BASE_VARIANT
        )
    };
    Error::new(
        ErrorCode::UnknownVariant,
        format!("stem `{stem}` has no variant `{variant}`"),
    )
    .with_path(ConfigPath::root().key("stems").key(stem).key("variant"))
    .with_hint(hint)
    .with_details(json!({ "stem": stem, "variant": variant, "known": names }))
}

/// `stems switch <stem>`: the choices, the active one marked.
fn list(stem: &str, active: &str, choices: &[Choice]) -> CommandOutput {
    let rows: Vec<Json> = choices
        .iter()
        .map(|c| json!({ "name": c.name, "type": c.kind, "active": c.name == active }))
        .collect();
    let width = choices.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut human = format!("{stem}:\n");
    for c in choices {
        let mark = if c.name == active { "*" } else { " " };
        let kind = c.kind.as_deref().unwrap_or("-");
        let base = if c.name == variants::BASE_VARIANT {
            "  (base definition)"
        } else {
            ""
        };
        human.push_str(&format!("{mark} {:<width$}  {kind}{base}\n", c.name));
    }
    if choices.len() > 1 {
        human.push_str(&format!("switch with `stems switch {stem} <variant>`\n"));
    } else {
        human.push_str(&format!(
            "`{stem}` declares no variants (see docs/config.md#variants-fr-st-8)\n"
        ));
    }
    CommandOutput::data(json!({ "stem": stem, "variant": active, "variants": rows }))
        .with_human(human)
}

/// What the daemon's switch gave: its result and the stem's status after it.
type Switched = (Box<SwitchVariantResult>, Option<Box<StemStatus>>);

/// `switch_variant {stem, variant}` on the workspace's daemon, which edits
/// `stems.local.yaml`, validates and applies the change to that stem; then
/// the stem's status. `None` when no daemon runs (the CLI edits the file
/// itself then).
fn via_daemon(ctx: &Ctx, stem: &str, to: &str) -> Result<Option<Switched>, Errors> {
    block_on(async {
        let c = match client::connect(ctx).await {
            Ok(c) => c,
            Err(e) if e.0.iter().all(|e| e.code == ErrorCode::DaemonNotRunning) => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let p = SwitchVariantParams {
            stem: stem.to_string(),
            variant: Some(to.to_string()),
        };
        let r: SwitchVariantResult = c
            .call_with_timeout(Method::SWITCH_VARIANT, &p, config::APPLY_TIMEOUT)
            .await?;
        let st: StatusResult = c
            .call(
                Method::STATUS,
                StatusParams {
                    stems: vec![stem.to_string()],
                    verbose: false,
                },
            )
            .await?;
        let status = st.stems.into_iter().find(|s| s.name == stem).map(Box::new);
        Ok(Some((Box::new(r), status)))
    })
}

/// The first lines of the human output: the switch, then the file's diff.
fn head_human(
    name: &str,
    from: &str,
    to: &str,
    kind: &str,
    file: Option<&std::path::Path>,
    diff: Option<&[Json]>,
) -> String {
    let mut human = if from == to {
        format!("{name}: already `{to}` ({kind})\n")
    } else {
        format!("{name}: {from} -> {to} ({kind})\n")
    };
    if let (Some(file), Some(diff)) = (file, diff) {
        human.push_str(&format!("{}:\n", file.display()));
        for l in diff.iter().filter_map(Json::as_str) {
            human.push_str(&format!("  {l}\n"));
        }
    }
    human
}

/// The output of a switch the daemon made (`daemon: true`).
fn daemon_output(r: SwitchVariantResult, status: Option<Box<StemStatus>>) -> CommandOutput {
    let name = r.stem.clone();
    let from = r.from.clone();
    let diff: Vec<Json> = r.diff.iter().map(|l| json!(l)).collect();
    let mut human = head_human(
        &name,
        &r.from,
        &r.to,
        r.kind.as_deref().unwrap_or("-"),
        r.file.as_deref().filter(|_| r.changed),
        Some(&diff),
    );
    let mut data = serde_json::Map::new();
    data.insert("stem".into(), json!(name));
    data.insert("from".into(), json!(r.from));
    data.insert("to".into(), json!(r.to));
    data.insert("type".into(), json!(r.kind));
    data.insert("path".into(), json!(r.path));
    data.insert("file".into(), json!(r.file));
    data.insert("changed".into(), json!(r.changed));
    data.insert("diff".into(), json!(r.diff));
    data.insert("daemon".into(), json!(true));
    data.insert(
        "applied".into(),
        r.applied
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok())
            .unwrap_or(Json::Null),
    );
    data.insert(
        "status".into(),
        status
            .as_deref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(Json::Null),
    );
    let mut errors = Errors::new();
    if let Some(a) = &r.applied {
        human.push_str(&config::apply_human(a));
        for f in &a.failed {
            let mut e = f.error.clone();
            if e.hint.is_none() {
                e.hint = Some(format!(
                    "fix it and run `stems config apply {name} --yes`, or switch back with `stems switch {name} {from}`"
                ));
            }
            errors.push(e);
        }
    }
    if let Some(s) = status {
        let st = StatusResult {
            summary: stems_api::StatusSummary::of(std::slice::from_ref(&*s)),
            stems: vec![*s],
        };
        human.push_str(&super::status::table(&st, &super::status::Style::default()));
    }
    CommandOutput::data(Json::Object(data))
        .with_human(human)
        .with_errors(errors)
}

/// Run `stems switch`.
pub fn run(ctx: &Ctx, args: &SwitchArgs) -> CommandOutput {
    let resolved = match stems_config::load(ctx.load_options()) {
        Ok(r) => r,
        Err(e) => {
            let mut errors = Errors::from(e);
            errors.sort();
            return CommandOutput::failed(errors);
        }
    };
    let ws = &resolved.workspace;
    let Some(stem) = ws.stem(&args.stem) else {
        return CommandOutput::failed(unknown_stem(ws, &args.stem));
    };
    let name = stem.name.clone();
    let from = stem
        .variant
        .clone()
        .unwrap_or_else(|| variants::BASE_VARIANT.to_string());
    let (file, committed) = match config::local_file(ctx) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let original = match config::read_local(&file) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    let local_tree: Option<serde_yaml_ng::Value> = serde_yaml_ng::from_str(&original).ok();
    let choices = variants::choices(&committed, local_tree.as_ref(), &name);

    let Some(target) = &args.variant else {
        return list(&name, &from, &choices);
    };
    let to = if variants::is_base(target) {
        variants::BASE_VARIANT.to_string()
    } else {
        target.clone()
    };
    let Some(choice) = choices.iter().find(|c| c.name == to) else {
        return CommandOutput::failed(unknown_variant(&name, target, &choices));
    };

    // A daemon runs: it edits, validates and applies (`switch_variant`).
    if !args.no_apply {
        match via_daemon(ctx, &name, &to) {
            Err(e) => return CommandOutput::failed(e),
            Ok(Some((r, status))) => return daemon_output(*r, status),
            Ok(None) => {}
        }
    }

    // No daemon (or --no-apply): edit stems.local.yaml here.
    let path = ConfigPath::root().key("stems").key(&name).key("variant");
    let no_base = |_: &ConfigPath| None;
    let committed_default =
        variants::committed_selection(&committed, &name).filter(|v| !variants::is_base(v));
    let edited = if to == variants::BASE_VARIANT && committed_default.is_none() {
        edit::unset(&original, &path).map(|(t, _)| t)
    } else {
        edit::set(&original, &path, &to, &no_base)
    };
    let new = match edited {
        Ok(t) => t,
        Err(e) => {
            return CommandOutput::failed(
                Error::new(
                    ErrorCode::Usage,
                    format!("cannot edit `{path}` in {}: {}", file.display(), e.0),
                )
                .with_path(path.clone())
                .with_hint(format!(
                    "set `variant: {to}` under `stems.{name}` in {} by hand",
                    file.display()
                )),
            );
        }
    };
    let mut data = serde_json::Map::new();
    data.insert("stem".into(), json!(name));
    data.insert("from".into(), json!(from));
    data.insert("to".into(), json!(to));
    data.insert("type".into(), json!(choice.kind));
    data.insert("path".into(), json!(path.to_string()));
    let written = config::commit(ctx, &file, &original, &new, data);
    if !written.errors.is_empty() {
        return written;
    }
    let Json::Object(mut data) = written.data else {
        return CommandOutput::failed(Error::internal("switch: unexpected output shape"));
    };
    let changed = data.get("changed").and_then(Json::as_bool).unwrap_or(false);

    let diff = data.get("diff").and_then(Json::as_array).cloned();
    let mut human = head_human(
        &name,
        &from,
        &to,
        choice.kind.as_deref().unwrap_or("-"),
        Some(file.as_path()).filter(|_| changed),
        diff.as_deref(),
    );
    data.insert("applied".into(), Json::Null);
    data.insert("status".into(), Json::Null);
    if args.no_apply {
        data.insert("daemon".into(), Json::Null);
        human.push_str(&format!(
            "not applied (--no-apply): run `stems config apply {name} --yes` to restart it in its new form\n"
        ));
    } else {
        data.insert("daemon".into(), json!(false));
        human.push_str("the daemon is not running: the next `stems up` uses it\n");
    }
    CommandOutput::data(Json::Object(data)).with_human(human)
}
