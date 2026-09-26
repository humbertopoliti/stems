//! `tracing` setup: the daemon log file (and stderr in the foreground).

use std::path::Path;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Env var with a `tracing` filter overriding the configured level.
pub const ENV_LOG: &str = "STEMS_LOG";

/// Install the global subscriber (a no-op if one is already installed, e.g. a
/// second daemon run inside one test process).
pub fn init(log: &Path, foreground: bool, level: Option<&str>) {
    let filter = EnvFilter::try_from_env(ENV_LOG)
        .unwrap_or_else(|_| EnvFilter::new(level.unwrap_or("info")));
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(log)
    };
    let file_layer = file.ok().map(|f| {
        tracing_subscriber::fmt::layer()
            .with_writer(Arc::new(f))
            .with_ansi(false)
    });
    let stderr_layer =
        foreground.then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();
}
