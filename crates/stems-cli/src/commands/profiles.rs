//! `stems profiles` (FR-WS-8, deliverable 26): every profile with its
//! resolved membership. Reads the config locally (no daemon).
//!
//! `data`: `{ "default": "<name>" | null, "profiles": [ { name, alias_of,
//! stems, closure, expanded, disabled, default, default_source?, error? } ] }`
//! in declaration order. `stems` are the enabled members after alias
//! resolution, `closure` adds hard dependencies (declaration order),
//! `expanded` is what the closure added, `disabled` the members left out by
//! `enabled: false`. `default` marks the profile `up` uses without
//! `--profile` / `STEMS_PROFILE` (`STEMS_PROFILE` is reported in
//! `data.env_profile` when set).

use serde_json::json;
use stems_core::Errors;
use stems_core::selection::{self, ENV_PROFILE};

use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Run `profiles`.
pub fn run(ctx: &Ctx) -> CommandOutput {
    let resolved = match stems_config::load(ctx.load_options()) {
        Ok(r) => r,
        Err(e) => {
            let mut errors = Errors::from(e);
            errors.sort();
            return CommandOutput::failed(errors);
        }
    };
    let ws = &resolved.workspace;
    let rows = selection::describe_profiles(ws);
    let default = selection::default_profile(ws).map(|(p, _)| p);
    let env_profile = ctx.env.get(ENV_PROFILE).filter(|v| !v.is_empty());
    let mut human = String::new();
    if rows.is_empty() {
        human.push_str("no profiles defined (`up` starts every enabled stem)\n");
    }
    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(0);
    for r in &rows {
        let mark = if r.default { "*" } else { " " };
        let body = match &r.error {
            Some(e) => format!("error: {}", e.message),
            None => {
                let mut b = r.stems.join(", ");
                if let Some(a) = &r.alias_of {
                    b = format!("= {a}: {b}");
                }
                if !r.expanded.is_empty() {
                    b.push_str(&format!("  (+ deps: {})", r.expanded.join(", ")));
                }
                if !r.disabled.is_empty() {
                    b.push_str(&format!("  (disabled: {})", r.disabled.join(", ")));
                }
                b
            }
        };
        human.push_str(&format!("{mark} {:<width$}  {body}\n", r.name));
    }
    if let Some(p) = env_profile {
        human.push_str(&format!("{ENV_PROFILE}={p} overrides the default\n"));
    } else if default.is_some() {
        human.push_str("* = used by `stems up` without --profile\n");
    }
    CommandOutput::data(json!({
        "default": default,
        "env_profile": env_profile,
        "profiles": rows,
    }))
    .with_human(human)
}
