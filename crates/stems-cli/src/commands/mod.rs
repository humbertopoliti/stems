//! Command implementations. Each returns a [`CommandOutput`]; `main`
//! renders it and exits with its code.

pub mod completions;
pub mod daemon;
pub mod events;
pub mod init;
pub mod show;
pub mod stubs;
pub mod validate;
pub mod version;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use stems_config::LoadOptions;

use crate::cli::{Command, GlobalArgs};
use crate::output::{CommandOutput, Mode};

/// Per-invocation context shared by every command.
#[derive(Clone, Debug)]
pub struct Ctx {
    /// Global flags.
    pub global: GlobalArgs,
    /// Current directory.
    pub cwd: PathBuf,
    /// Environment (for `STEMS_WORKSPACE`, `${env.X}`, `HOME`).
    pub env: HashMap<String, String>,
}

impl Ctx {
    /// Workspace discovery options from `--workspace`, the cwd and the env.
    pub fn load_options(&self) -> LoadOptions {
        LoadOptions {
            workspace: self.global.workspace.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            skip_local: false,
        }
    }

    /// The stems home (`--home` / `STEMS_HOME` / platform default), made
    /// absolute against the cwd.
    pub fn home(&self) -> PathBuf {
        let home = self
            .global
            .home
            .clone()
            .or_else(|| {
                self.env
                    .get(crate::paths::ENV_HOME)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(crate::paths::default_home);
        self.cwd.join(home)
    }
}

/// Run one subcommand. `mode` and `stdout` are for commands that stream
/// (`events -f`); everything else returns its whole output.
pub fn dispatch(cmd: Command, ctx: &Ctx, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    match cmd {
        Command::Daemon(a) => daemon::run(ctx, &a),
        Command::Events(a) => events::run(ctx, &a, mode, stdout),
        Command::Init(a) => init::run(ctx, &a),
        Command::Validate(a) => validate::run(ctx, &a),
        Command::Show(a) => show::run(ctx, &a),
        Command::Completions(a) => completions::run(&a),
        Command::Docs => {
            CommandOutput::data(serde_json::json!({ "markdown": crate::docs::markdown() }))
                .with_raw(crate::docs::markdown())
        }
        other => stubs::run(&other),
    }
}
