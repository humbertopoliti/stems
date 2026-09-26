//! Minimal standalone daemon for manual protocol testing (`nc -U`) without
//! the CLI (normally: `stems daemon start` / `stems daemon start --foreground`):
//!
//! ```sh
//! cargo run -p stems-daemon --example stemsd -- <home> <workspace>
//! ```

fn main() {
    let mut args = std::env::args_os().skip(1);
    let home = args
        .next()
        .map(Into::into)
        .unwrap_or_else(stems_daemon::resolve_home);
    let mut opts = stems_daemon::RunOptions::new(home);
    opts.workspace = args.next().map(Into::into);
    opts.foreground = true;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(stems_daemon::Daemon::run(opts)) {
        eprintln!("{e}");
        std::process::exit(e.exit_code());
    }
}
