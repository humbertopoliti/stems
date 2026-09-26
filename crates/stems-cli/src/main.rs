//! The `stems` command-line entry point. Everything lives in the library
//! (`stems_cli::run`); `main` only wires the process and exits with the code.

fn main() {
    let inv = stems_cli::Invocation::from_process();
    let code = stems_cli::run(&inv, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
