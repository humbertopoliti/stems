//! Command implementations. Each returns a [`CommandOutput`]; `main`
//! renders it and exits with its code.

pub mod completions;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod events;
pub mod graph;
pub mod health;
pub mod init;
pub mod lifecycle;
pub mod logs;
pub mod man;
pub mod mcp;
pub mod metrics;
pub mod orphans;
pub mod outputs;
pub mod overlays;
pub mod profiles;
pub mod pull;
pub mod repos;
pub mod run;
pub mod scripts;
pub mod show;
pub mod status;
pub mod stubs;
pub mod switch;
pub mod upgrade;
pub mod validate;
pub mod version;
pub mod watch;

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
        Command::Up(a) => lifecycle::up(ctx, &a, mode, stdout),
        Command::Down(a) => lifecycle::down(ctx, &a, mode),
        Command::Doctor(a) => doctor::run(ctx, a, mode),
        Command::Start(a) => lifecycle::start(ctx, &a),
        Command::Stop(a) => lifecycle::stop(ctx, &a),
        Command::Restart(a) => lifecycle::restart(ctx, &a),
        Command::Build(a) => scripts::build(ctx, &a),
        Command::Pull(a) => pull::run(ctx, &a, mode, stdout),
        Command::Reset(a) => scripts::reset(ctx, &a),
        Command::Stamps(a) => scripts::stamps(ctx, &a),
        Command::Repos(crate::cli::ReposCommand::Sync(a)) => repos::sync(ctx, &a),
        Command::Repos(crate::cli::ReposCommand::Status(a)) => repos::status(ctx, &a),
        Command::Run(a) => run::run(ctx, &a, mode, stdout),
        Command::Scripts(a) => run::scripts(ctx, &a),
        Command::Overlays(a) => overlays::run(ctx, &a),
        Command::Attach(a) => lifecycle::attach(ctx, &a, mode, stdout),
        Command::Status(a) => status::run(ctx, &a, mode, stdout),
        Command::Health(a) => health::run(ctx, &a),
        Command::Metrics(a) => metrics::run(ctx, &a, mode, stdout),
        Command::Outputs(a) => outputs::run(ctx, &a, mode),
        Command::Watch(c) => watch::run(ctx, &c),
        Command::Graph(a) => graph::run(ctx, &a, mode, stdout),
        Command::Logs(a) => logs::run(ctx, &a, mode, stdout),
        Command::Validate(a) => validate::run(ctx, &a),
        Command::Show(a) => show::run(ctx, &a),
        Command::Profiles(_) => profiles::run(ctx),
        Command::Config(crate::cli::ConfigCommand::Get(a)) => config::get(ctx, &a),
        Command::Config(crate::cli::ConfigCommand::Set(a)) => config::set(ctx, &a),
        Command::Config(crate::cli::ConfigCommand::Unset(a)) => config::unset(ctx, &a),
        Command::Config(crate::cli::ConfigCommand::Diff) => config::diff(ctx),
        Command::Config(crate::cli::ConfigCommand::Apply(a)) => config::apply(ctx, &a, mode),
        Command::Switch(a) => switch::run(ctx, &a),
        Command::Completions(a) => completions::run(&a),
        Command::Upgrade(a) => upgrade::run(ctx, &a),
        Command::Man { outdir } => man::run(&ctx.cwd, &outdir),
        Command::Mcp(a) => mcp::run(ctx, &a),
        Command::Docs => {
            CommandOutput::data(serde_json::json!({ "markdown": crate::docs::markdown() }))
                .with_raw(crate::docs::markdown())
        }
        other => stubs::run(&other),
    }
}
