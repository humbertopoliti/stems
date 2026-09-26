//! `stems show [stem] [--effective]` (FR-ST-7): the resolved workspace.
//!
//! `data` is the resolved model (`stems_config::Workspace` serialised: `name`,
//! `root`, `vars`, `env`, `requires`, `profiles`, `logs`, `metrics`,
//! `scripts`, `stems.<name>` with `enabled`, `env`, `ports`, `codebase`,
//! `health`, …). Defaults are already applied by the loader, so every field
//! is present. With a stem argument, `data` is that stem's object.
//! `--effective` adds `data.defaults`: the table of defaults (config key ->
//! value) the loader applies to unset fields.

use serde_json::{Map, Value, json};
use stems_core::{Error, ErrorCode, Errors};

use crate::cli::ShowArgs;
use crate::commands::Ctx;
use crate::output::{CommandOutput, human_value};

/// Run `show`.
pub fn run(ctx: &Ctx, args: &ShowArgs) -> CommandOutput {
    let resolved = match stems_config::load(ctx.load_options()) {
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
            Some(stem) => serde_json::to_value(stem),
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
