//! `stems show [stem] [--effective]` (FR-ST-7): the resolved workspace.
//!
//! `data` is the resolved model (`stems_config::Workspace` serialised: `name`,
//! `root`, `vars`, `env`, `requires`, `profiles`, `logs`, `metrics`,
//! `scripts`, `stems.<name>` with `enabled`, `env`, `ports`, `codebase`,
//! `health`, …). Defaults are already applied by the loader, so every field
//! is present. With a stem argument, `data` is that stem's object.
//! `--effective` adds `data.defaults`: the table of defaults (config key ->
//! value) the loader applies to unset fields.
//!
//! `--variant <name>` (FR-ST-8, with a stem only) resolves that stem with
//! the variant active (`local`/`base` for the base) without writing
//! `stems.local.yaml`; an undeclared name is `UNKNOWN_VARIANT`. A stem with
//! variants or overrides carries `inherited_from` (where each field comes
//! from, see `docs/config.md#variants-fr-st-8`).
//!
//! Secret env values (47, `docs/config.md#secrets`: built-in name patterns,
//! `secrets:`, `redaction:`) in `env`, `local_env` and other env maps are
//! `<redacted>`; `--reveal` shows them in human output on a terminal and is
//! a `USAGE` error with `--json`.

use std::collections::BTreeMap;
use std::io::IsTerminal;

use serde_json::{Map, Value, json};
use stems_core::redact::Redactor;
use stems_core::{Error, ErrorCode, Errors};

use crate::cli::ShowArgs;
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode, human_value};

/// Run `show`.
pub fn run(ctx: &Ctx, args: &ShowArgs, mode: Mode) -> CommandOutput {
    if args.reveal && mode != Mode::Human {
        return CommandOutput::failed(Error::usage(
            "`--reveal` is for human output on a terminal, not `--json`",
            "drop `--json` (secrets never appear in JSON output)",
        ));
    }
    let reveal = args.reveal && std::io::stdout().is_terminal();
    let mut overrides = BTreeMap::new();
    if let Some(v) = &args.variant {
        let Some(stem) = &args.stem else {
            return CommandOutput::failed(Error::usage(
                "`--variant` needs a stem",
                "pass the stem: `stems show <stem> --variant <name>`",
            ));
        };
        overrides.insert(stem.clone(), v.clone());
    }
    let resolved = match stems_config::load_with_variants(ctx.load_options(), &overrides) {
        Ok(r) => r,
        Err(e) => {
            let mut errors = Errors::from(e);
            errors.sort();
            return CommandOutput::failed(errors);
        }
    };
    let ws = &resolved.workspace;
    let value = match &args.stem {
        None => serde_json::to_value(ws),
        Some(name) => match ws.stem(name) {
            Some(stem) => {
                if let Some(v) = &args.variant
                    && !stems_config::variants::is_base(v)
                    && !stem.variants.contains(v)
                {
                    return CommandOutput::failed(unknown_variant(name, v, &stem.variants));
                }
                serde_json::to_value(stem)
            }
            None => {
                let known: Vec<&String> = ws.stems.keys().collect();
                return CommandOutput::failed(
                    Error::new(ErrorCode::UnknownStem, format!("no stem named `{name}`"))
                        .with_hint(format!(
                            "stems in this workspace: {}",
                            known
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .with_details(json!({ "stem": name, "known": known })),
                );
            }
        },
    };
    let mut data = match value {
        Ok(v) => v,
        Err(e) => return CommandOutput::failed(Error::internal(format!("serialising: {e}"))),
    };
    if !reveal {
        Redactor::new(ws).redact_json(&mut data, args.stem.as_deref());
    }
    if args.effective
        && let Value::Object(obj) = &mut data
    {
        let defaults: Map<String, Value> = stems_config::defaults::defaults_table()
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v)))
            .collect();
        obj.insert("defaults".into(), Value::Object(defaults));
    }
    let human = human_value(&data);
    CommandOutput::data(data).with_human(human)
}

/// `UNKNOWN_VARIANT` for `--variant <variant>` on `stem`.
fn unknown_variant(stem: &str, variant: &str, known: &[String]) -> Error {
    let hint = if known.is_empty() {
        format!("`{stem}` declares no `variants`")
    } else {
        format!(
            "variants of `{stem}`: {} (or `{}` for the base definition)",
            known.join(", "),
            stems_config::variants::BASE_VARIANT
        )
    };
    Error::new(
        ErrorCode::UnknownVariant,
        format!("stem `{stem}` has no variant `{variant}`"),
    )
    .with_hint(hint)
    .with_details(json!({ "stem": stem, "variant": variant, "known": known }))
}
