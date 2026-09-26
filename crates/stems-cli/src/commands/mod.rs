//! Command implementations. Each returns a [`CommandOutput`]; `main`
//! renders it and exits with its code.

pub mod completions;
pub mod init;
pub mod show;
pub mod stubs;
pub mod validate;
pub mod version;

use std::collections::HashMap;
use std::path::PathBuf;

use stems_config::LoadOptions;

use crate::cli::{Command, GlobalArgs};
use crate::output::CommandOutput;

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

    /// The stems home (`--home` / `STEMS_HOME` / platform default).
    pub fn home(&self) -> PathBuf {
        let env = |k: &str| self.env.get(k).cloned();
        crate::paths::home_dir_with(self.global.home.as_deref(), &env, cfg!(target_os = "macos"))
    }
}

/// Run one subcommand.
pub fn dispatch(cmd: Command, ctx: &Ctx) -> CommandOutput {
    match cmd {
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
