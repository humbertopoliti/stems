//! `stems env <stem> [--format dotenv|shell|json] [--all] [--reveal]`
//! (deliverable 37, `docs/exec.md`): the environment stems gives a stem.
//!
//! Same context as `stems exec` ([`super::exec::context`]: the daemon's
//! `stem_env`, else the config). `data: {stem, env, cwd, container,
//! unresolved}`; dotenv (default) and shell formats are printed raw (no
//! envelope unless `--json`). `--all` puts the caller's environment (minus
//! `STEMS_*`) under the stem's. Secret outputs, and the values of secret
//! names (47: [`stems_core::redact::Redactor`], `docs/config.md#secrets`),
//! are `<redacted>` unless `--reveal` in human mode on a terminal (like
//! `stems outputs`).

use std::collections::BTreeMap;
use std::io::IsTerminal;

use serde_json::json;

use crate::cli::{EnvArgs, EnvFormat};
use crate::commands::Ctx;
use crate::commands::exec::{caller_env, context, unresolved_warning};
use crate::output::{CommandOutput, Mode};

/// A dotenv value: single quotes (literal) unless the value holds `'` or
/// a newline, then double quotes with `\\ \" \n \r \$` escaped (what the
/// compose env file writer does).
pub fn dotenv_quote(v: &str) -> String {
    if !v.contains('\'') && !v.contains('\n') && !v.contains('\r') {
        return format!("'{v}'");
    }
    let mut out = String::from("\"");
    for c in v.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '$' => out.push_str(r"\$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A POSIX shell word: `'...'` with `'` as `'\''`.
pub fn shell_quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', r"'\''"))
}

/// `env` as `KEY=value` lines (`shell`: `export KEY='value'`).
pub fn render(env: &BTreeMap<String, String>, shell: bool) -> String {
    env.iter()
        .map(|(k, v)| {
            if shell {
                format!("export {k}={}\n", shell_quote(v))
            } else {
                format!("{k}={}\n", dotenv_quote(v))
            }
        })
        .collect()
}

/// `stems env`.
pub fn run(ctx: &Ctx, args: &EnvArgs, mode: Mode) -> CommandOutput {
    let reveal = args.reveal && mode == Mode::Human && std::io::stdout().is_terminal();
    let r = match context(ctx, &args.stem, reveal) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let mut env = if args.all {
        let mut all = caller_env(&ctx.env);
        all.extend(r.env.clone());
        all
    } else {
        r.env.clone()
    };
    if !reveal {
        let redactor = stems_config::load(ctx.load_options())
            .map(|ws| stems_core::redact::Redactor::new(&ws.workspace))
            .unwrap_or_default();
        redactor.redact_pairs(Some(&r.stem), env.iter_mut());
    }
    let out = CommandOutput::data(json!({
        "stem": r.stem,
        "env": env,
        "cwd": r.cwd,
        "container": r.container,
        "unresolved": r.unresolved,
    }));
    if args.format == EnvFormat::Json {
        let text = serde_json::to_string_pretty(&out.envelope()).unwrap_or_default();
        return out.with_human(text);
    }
    if !ctx.global.json
        && let Some(w) = unresolved_warning(&r)
    {
        eprint!("{w}");
    }
    out.with_raw(render(&env, args.format == EnvFormat::Shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        let env: BTreeMap<String, String> = [
            ("A", "plain"),
            ("B", "it's"),
            ("C", "two\nlines $X"),
            ("D", "$HOME and \"q\""),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .into();
        insta::assert_snapshot!(render(&env, false), @r#"
        A='plain'
        B="it's"
        C="two\nlines \$X"
        D='$HOME and "q"'
        "#);
        insta::assert_snapshot!(render(&env, true), @r#"
        export A='plain'
        export B='it'\''s'
        export C='two
        lines $X'
        export D='$HOME and "q"'
        "#);
    }
}
