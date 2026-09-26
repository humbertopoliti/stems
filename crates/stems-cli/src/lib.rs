//! The `stems` CLI as a library: the clap tree ([`cli`]), the output layer
//! ([`output`]), per-user paths ([`paths`], shared with the daemon), the
//! daemon client helpers ([`client`]), the commands ([`commands`]) and
//! [`run`], the whole binary minus `std::process::exit`.

pub mod cli;
pub mod client;
pub mod commands;
pub mod docs;
pub mod output;
pub mod paths;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{ColorChoice, CommandFactory, FromArgMatches};
use serde_json::json;
use stems_core::Error;

use crate::cli::Cli;
use crate::commands::Ctx;
use crate::output::{CommandOutput, OutputOptions, render};

/// Process-level inputs of one invocation.
#[derive(Clone, Debug)]
pub struct Invocation {
    /// `argv`, including the program name.
    pub args: Vec<OsString>,
    /// Current directory.
    pub cwd: PathBuf,
    /// Environment.
    pub env: HashMap<String, String>,
    /// Whether stdout is a terminal (JSON is implied when it is not).
    pub stdout_is_tty: bool,
}

impl Invocation {
    /// The current process's invocation.
    pub fn from_process() -> Self {
        use std::io::IsTerminal;
        Self {
            args: std::env::args_os().collect(),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            env: std::env::vars().collect(),
            stdout_is_tty: std::io::stdout().is_terminal(),
        }
    }
}

/// Parse, dispatch, render; returns the exit code. Parse errors go through
/// the envelope as `USAGE` (exit 2); `--help` prints help (exit 0).
pub fn run(inv: &Invocation, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    // Flags that matter before (or without) a successful parse.
    let before_dashdash: Vec<&OsString> =
        inv.args.iter().skip(1).take_while(|a| *a != "--").collect();
    let has = |flag: &str| before_dashdash.iter().any(|a| *a == flag);
    let env_no_color = inv
        .env
        .get("STEMS_NO_COLOR")
        .is_some_and(|v| !matches!(v.as_str(), "" | "0" | "false" | "no" | "off" | "n" | "f"));
    let no_color = has("--no-color") || env_no_color;
    let colored = inv.stdout_is_tty && !no_color;

    let cmd = Cli::command().color(if colored {
        ColorChoice::Always
    } else {
        ColorChoice::Never
    });
    let parsed = cmd
        .try_get_matches_from(&inv.args)
        .and_then(|m| Cli::from_arg_matches(&m));

    let cli = match parsed {
        Ok(cli) => cli,
        Err(e) => {
            let styled = e.render();
            let text = if colored {
                styled.ansi().to_string()
            } else {
                styled.to_string()
            };
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = stdout.write_all(text.as_bytes());
                return 0;
            }
            let opts = OutputOptions::new(has("--json"), has("--human"), inv.stdout_is_tty);
            if opts.mode == output::Mode::Human {
                let _ = stderr.write_all(text.as_bytes());
                return stems_core::exit::CONFIG;
            }
            let out = CommandOutput::failed(usage_error(&e, &styled.to_string()));
            render(&out, &opts, stdout, stderr);
            return out.exit_code();
        }
    };

    let g = &cli.global;
    let mut opts = OutputOptions::new(g.json, g.human, inv.stdout_is_tty);
    opts.quiet = g.quiet;
    opts.no_color = g.no_color || no_color;
    let ctx = Ctx {
        global: g.clone(),
        cwd: inv.cwd.clone(),
        env: inv.env.clone(),
    };
    let out = if cli.version {
        commands::version::run(&ctx)
    } else {
        match cli.command {
            Some(c) => commands::dispatch(c, &ctx, opts.mode, stdout),
            None => {
                let mut help = Cli::command();
                let text = help.render_help().to_string();
                if opts.mode == output::Mode::Human {
                    let _ = stderr.write_all(text.as_bytes());
                    return stems_core::exit::CONFIG;
                }
                CommandOutput::failed(Error::usage(
                    "no command given",
                    "run `stems --help` for the list of commands",
                ))
            }
        }
    };
    render(&out, &opts, stdout, stderr);
    out.exit_code()
}

/// A clap parse error as a `USAGE` error.
fn usage_error(e: &clap::Error, text: &str) -> Error {
    let first = text
        .lines()
        .next()
        .unwrap_or("invalid command line")
        .trim_start_matches("error: ")
        .to_string();
    Error::usage(
        first,
        "run `stems --help` (or `stems <command> --help`) for usage",
    )
    .with_details(json!({ "kind": format!("{:?}", e.kind()), "text": text.trim_end() }))
}
