//! The `stems` command-line entry point.

use clap::Parser;

/// Local environment management toolkit for running multi-process systems.
#[derive(Debug, Parser)]
#[command(name = "stems", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
