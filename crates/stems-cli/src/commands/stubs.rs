//! Commands whose deliverable has not landed: `NOT_IMPLEMENTED` (exit 1)
//! with `details: { command, deliverable }`. The argument surface is final
//! (see `crate::cli`) so scenarios can be written against it now.

use serde_json::json;
use stems_core::{Error, ErrorCode};

use crate::cli::{Command, ConfigCommand};
use crate::output::CommandOutput;

/// `(command path, deliverable)` for a stub; `deliverable` is a plan
/// number (`"10"`) or `"unscheduled"`.
pub fn target(cmd: &Command) -> (String, &'static str) {
    let (name, nn): (&str, &str) = match cmd {
        Command::Exec(_) => ("exec", "unscheduled"),
        Command::Shell(_) => ("shell", "unscheduled"),
        Command::Add(_) => ("add", "unscheduled"),
        Command::Remove(_) => ("remove", "unscheduled"),
        Command::Edit(_) => ("edit", "unscheduled"),
        Command::Init(_)
        | Command::Mcp(_)
        | Command::Profiles(_)
        | Command::Config(
            ConfigCommand::Get(_)
            | ConfigCommand::Set(_)
            | ConfigCommand::Unset(_)
            | ConfigCommand::Diff
            | ConfigCommand::Apply(_),
        )
        | Command::Up(_)
        | Command::Down(_)
        | Command::Start(_)
        | Command::Stop(_)
        | Command::Restart(_)
        | Command::Reset(_)
        | Command::Build(_)
        | Command::Pull(_)
        | Command::Stamps(_)
        | Command::Repos(_)
        | Command::Run(_)
        | Command::Scripts(_)
        | Command::Overlays(_)
        | Command::Doctor(_)
        | Command::Attach(_)
        | Command::Status(_)
        | Command::Health(_)
        | Command::Metrics(_)
        | Command::Outputs(_)
        | Command::Watch(_)
        | Command::Graph(_)
        | Command::Logs(_)
        | Command::Events(_)
        | Command::Daemon(_)
        | Command::Validate(_)
        | Command::Show(_)
        | Command::Switch(_)
        | Command::Completions(_)
        | Command::Upgrade(_)
        | Command::Man { .. }
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
