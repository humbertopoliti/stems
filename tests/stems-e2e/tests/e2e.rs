//! cucumber-rs entry point: `cargo test -p stems-e2e --test e2e`.
//! Build the binary first (`cargo build -p stems-cli`); `make e2e` does both.

fn main() -> std::process::ExitCode {
    stems_e2e::runner::main()
}
