//! `stems completions <bash|zsh|fish>`: the script on stdout (wrapped in the
//! envelope as `data: { shell, script }` only with an explicit `--json`).

use clap_complete::{Shell, generate};
use serde_json::json;

use crate::cli::{CompletionShell, CompletionsArgs};
use crate::output::CommandOutput;

/// The completion script for `shell`.
pub fn script(shell: CompletionShell) -> String {
    let sh = match shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
        CompletionShell::Fish => Shell::Fish,
    };
    let mut cmd = crate::cli::command();
    let mut buf = Vec::new();
    generate(sh, &mut cmd, "stems", &mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Run `completions`.
pub fn run(args: &CompletionsArgs) -> CommandOutput {
    let text = script(args.shell);
    let name = match args.shell {
        CompletionShell::Bash => "bash",
        CompletionShell::Zsh => "zsh",
        CompletionShell::Fish => "fish",
    };
    CommandOutput::data(json!({ "shell": name, "script": text })).with_raw(text)
}
