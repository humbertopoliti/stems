//! Commands whose deliverable has not landed: `NOT_IMPLEMENTED` (exit 1)
//! with `details: { command, deliverable }`. The argument surface is final
//! (see `crate::cli`) so scenarios can be written against it now.

use serde_json::json;
use stems_core::{Error, ErrorCode};

use crate::cli::{Command, ConfigCommand, ReposCommand, WatchCommand};
use crate::output::CommandOutput;

/// `(command path, deliverable)` for a stub; `deliverable` is a plan
/// number (`"10"`) or `"unscheduled"`.
pub fn target(cmd: &Command) -> (String, &'static str) {
    let (name, nn): (&str, &str) = match cmd {
        Command::Up(_) => ("up", "10"),
        Command::Down(_) => ("down", "10"),
        Command::Start(_) => ("start", "10"),
        Command::Stop(_) => ("stop", "10"),
        Command::Restart(_) => ("restart", "10"),
        Command::Attach(_) => ("attach", "27"),
        Command::Status(_) => ("status", "13"),
        Command::Logs(_) => ("logs", "12"),
        Command::Metrics(_) => ("metrics", "25"),
        Command::Health(_) => ("health", "21"),
        Command::Graph(_) => ("graph", "23"),
        Command::Run(_) => ("run", "17"),
        Command::Scripts(_) => ("scripts", "17"),
        Command::Exec(_) => ("exec", "unscheduled"),
        Command::Shell(_) => ("shell", "unscheduled"),
        Command::Reset(_) => ("reset", "16"),
        Command::Build(_) => ("build", "16"),
        Command::Stamps(_) => ("stamps", "16"),
        Command::Overlays(_) => ("overlays", "18"),
        Command::Doctor(_) => ("doctor", "19"),
        Command::Repos(ReposCommand::Sync(_)) => ("repos sync", "20"),
        Command::Repos(ReposCommand::Status(_)) => ("repos status", "20"),
        Command::Watch(WatchCommand::Pause(_)) => ("watch pause", "24"),
        Command::Watch(WatchCommand::Resume(_)) => ("watch resume", "24"),
        Command::Watch(WatchCommand::Status) => ("watch status", "24"),
        Command::Profiles(_) => ("profiles", "26"),
        Command::Outputs(_) => ("outputs", "26"),
        Command::Config(ConfigCommand::Get(_)) => ("config get", "26"),
        Command::Config(ConfigCommand::Set(_)) => ("config set", "26"),
        Command::Config(ConfigCommand::Diff) => ("config diff", "33"),
        Command::Config(ConfigCommand::Apply(_)) => ("config apply", "33"),
        Command::Add(_) => ("add", "unscheduled"),
        Command::Remove(_) => ("remove", "unscheduled"),
        Command::Edit(_) => ("edit", "unscheduled"),
        Command::Mcp(_) => ("mcp", "31"),
        Command::Upgrade(_) => ("upgrade", "32"),
        Command::Init(_)
        | Command::Events(_)
        | Command::Daemon(_)
        | Command::Validate(_)
        | Command::Show(_)
        | Command::Completions(_)
        | Command::Docs => ("", "implemented"),
    };
    (name.to_string(), nn)
}

/// The `NOT_IMPLEMENTED` error for a stub.
pub fn error(cmd: &Command) -> Error {
    let (name, nn) = target(cmd);
    let e = if nn == "unscheduled" {
        Error::new(
            ErrorCode::NotImplemented,
            format!("`stems {name}` is not implemented yet (no deliverable scheduled)"),
        )
        .with_hint("see `stems --help` for the commands available in this build")
    } else {
        Error::not_implemented(&format!("stems {name}"), nn)
    };
    e.with_details(json!({ "command": name, "deliverable": nn }))
}

/// Run a stub.
pub fn run(cmd: &Command) -> CommandOutput {
    CommandOutput::failed(error(cmd))
}
