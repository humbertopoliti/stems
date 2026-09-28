//! Every default value, in one place. `defaults_table()` is golden-tested so
//! that any change is a reviewed diff.

use crate::types::{ArgType, AutoApply, WatchAction, WatchRoot};
use crate::types::{
    ByteSize, Condition, Dur, FileMode, HealthType, PullPolicy, RestartPolicy, StemType,
};

/// `schema_version`.
pub const SCHEMA_VERSION: u32 = 1;
/// `strict_profiles`.
pub const STRICT_PROFILES: bool = false;
/// `agent.allow_destructive`.
pub const AGENT_ALLOW_DESTRUCTIVE: bool = false;
/// `logs.max_size`.
pub const LOGS_MAX_SIZE: ByteSize = ByteSize(50 * 1024 * 1024);
/// `logs.keep`.
pub const LOGS_KEEP: u32 = 3;
/// `logs.ring` (lines per stem held in memory).
pub const LOGS_RING: u32 = 10_000;
/// `metrics.interval`.
pub const METRICS_INTERVAL: Dur = Dur::from_secs(2);
/// `metrics.persist`.
pub const METRICS_PERSIST: bool = false;
/// `config.reload.auto_apply` (33): changes wait for `stems config apply`.
pub const RELOAD_AUTO_APPLY: AutoApply = AutoApply::Off;
/// `repos_dir`, relative to the workspace root.
pub const REPOS_DIR: &str = ".stems/repos";

/// `stems.<n>.enabled`.
pub const STEM_ENABLED: bool = true;
/// `stems.<n>.stop_grace`.
pub const STOP_GRACE: Dur = Dur::from_secs(10);
/// `stems.<n>.shell` (process).
pub const PROCESS_SHELL: &str = "/bin/sh";
/// `stems.<n>.stdin` (process).
pub const PROCESS_STDIN: bool = false;
/// `stems.<n>.build.dockerfile` (docker), relative to the context.
pub const DOCKERFILE: &str = "Dockerfile";
/// `stems.<n>.adopt` (compose).
pub const COMPOSE_ADOPT: bool = false;

/// Name given to the i-th port when it has none.
pub fn port_name(i: usize) -> String {
    format!("port{i}")
}

/// `depends_on[].condition`.
pub const DEP_CONDITION: Condition = Condition::Healthy;
/// `depends_on[].soft`.
pub const DEP_SOFT: bool = false;

/// Health `type` when a stem declares no `health:` at all (`None`: no check).
pub fn health_type_for(kind: StemType) -> Option<HealthType> {
    match kind {
        StemType::Process => Some(HealthType::Process),
        StemType::Docker | StemType::Compose => Some(HealthType::Docker),
        StemType::External => None,
    }
}
/// `health.host`.
pub const HEALTH_HOST: &str = "localhost";
/// `health.interval`.
pub const HEALTH_INTERVAL: Dur = Dur::from_secs(2);
/// `health.timeout`.
pub const HEALTH_TIMEOUT: Dur = Dur::from_secs(1);
/// `health.retries`.
pub const HEALTH_RETRIES: u32 = 3;
/// `health.start_period`.
pub const HEALTH_START_PERIOD: Dur = Dur::from_secs(0);
/// `health.start_timeout`.
pub const HEALTH_START_TIMEOUT: Dur = Dur::from_secs(60);

/// `restart.policy` (plan 22: crash-looping stems restart up to `max` times).
pub const RESTART_POLICY: RestartPolicy = RestartPolicy::OnFailure;
/// `restart.max`.
pub const RESTART_MAX: u32 = 5;
/// `restart.window`.
pub const RESTART_WINDOW: Dur = Dur::from_secs(600);
/// `restart.backoff.initial`.
pub const BACKOFF_INITIAL: Dur = Dur::from_millis(500);
/// `restart.backoff.max`.
pub const BACKOFF_MAX: Dur = Dur::from_secs(30);
/// `restart.backoff.factor`.
pub const BACKOFF_FACTOR: f64 = 2.0;
/// `restart.on_unhealthy`.
pub const RESTART_ON_UNHEALTHY: bool = false;
/// `restart.unhealthy_grace`.
pub const UNHEALTHY_GRACE: Dur = Dur::from_secs(10);
/// `restart.cascade`.
pub const RESTART_CASCADE: bool = false;

/// Built-in watch ignores; user `ignore` entries are appended.
pub const WATCH_IGNORE: [&str; 6] = [
    "**/.git/**",
    "**/node_modules/**",
    "**/target/**",
    "**/dist/**",
    "**/build/**",
    "**/__pycache__/**",
];
/// `watch[].debounce`.
pub const WATCH_DEBOUNCE: Dur = Dur::from_millis(500);
/// `watch[].action`.
pub const WATCH_ACTION: WatchAction = WatchAction::Restart;
/// `watch[].settle`.
pub const WATCH_SETTLE: Dur = Dur::from_secs(0);
/// `watch[].root`.
pub const WATCH_ROOT: WatchRoot = WatchRoot::Codebase;

/// `overlays[].keep`.
pub const OVERLAY_KEEP: bool = false;
/// `overlays[].mode`.
pub const OVERLAY_MODE: FileMode = FileMode(0o644);

/// `scripts.<s>.retries`.
pub const SCRIPT_RETRIES: u32 = 0;
/// `scripts.<s>.concurrent`.
pub const SCRIPT_CONCURRENT: bool = false;
/// `scripts.<s>.args[].type`.
pub const ARG_TYPE: ArgType = ArgType::String;
/// `scripts.<s>.args[].required`.
pub const ARG_REQUIRED: bool = false;

/// The defaults as `(config key, value)` rows, for docs and the golden test.
pub fn defaults_table() -> Vec<(&'static str, String)> {
    let t = |d: Dur| d.to_string();
    let json = |v: &dyn erased::Show| v.show();
    vec![
        ("schema_version", SCHEMA_VERSION.to_string()),
        ("name", "<name of the workspace root directory>".into()),
        ("strict_profiles", STRICT_PROFILES.to_string()),
        (
            "agent.allow_destructive",
            AGENT_ALLOW_DESTRUCTIVE.to_string(),
        ),
        ("agent.allowed_tools", "[] (all tools)".into()),
        ("agent.denied_tools", "[]".into()),
        ("logs.max_size", LOGS_MAX_SIZE.to_string()),
        ("logs.keep", LOGS_KEEP.to_string()),
        ("logs.ring", LOGS_RING.to_string()),
        ("metrics.interval", t(METRICS_INTERVAL)),
        ("metrics.persist", METRICS_PERSIST.to_string()),
        ("config.reload.auto_apply", "false".into()),
        ("repos_dir", REPOS_DIR.into()),
        ("stems.<n>.enabled", STEM_ENABLED.to_string()),
        ("stems.<n>.stop_grace", t(STOP_GRACE)),
        ("stems.<n>.ports[i].name", port_name(0).replace('0', "<i>")),
        (
            "stems.<n>.ports[i].container_port",
            "docker/compose: the host port; process: none".into(),
        ),
        (
            "stems.<n>.cwd (process)",
            "<codebase, else workspace root>".into(),
        ),
        ("stems.<n>.shell (process)", PROCESS_SHELL.into()),
        ("stems.<n>.stdin (process)", PROCESS_STDIN.to_string()),
        (
            "stems.<n>.build.context (docker)",
            "<codebase, else workspace root>".into(),
        ),
        ("stems.<n>.build.dockerfile (docker)", DOCKERFILE.into()),
        ("stems.<n>.pull (docker)", json(&PullPolicy::default())),
        ("stems.<n>.service (compose)", "<stem name>".into()),
        (
            "stems.<n>.project_name (compose)",
            "stems-<workspace name>".into(),
        ),
        ("stems.<n>.adopt (compose)", COMPOSE_ADOPT.to_string()),
        ("stems.<n>.depends_on[].condition", json(&DEP_CONDITION)),
        ("stems.<n>.depends_on[].soft", DEP_SOFT.to_string()),
        (
            "stems.<n>.health (absent)",
            format!(
                "process: {}, docker: {}, compose: {}, external: none",
                json(&health_type_for(StemType::Process)),
                json(&health_type_for(StemType::Docker)),
                json(&health_type_for(StemType::Compose)),
            ),
        ),
        (
            "stems.<n>.health.type",
            "http if url, command if command, tcp if port, else as for an absent health".into(),
        ),
        ("stems.<n>.health.host", HEALTH_HOST.into()),
        (
            "stems.<n>.health.port (tcp, grpc)",
            "<the stem's first port>".into(),
        ),
        ("stems.<n>.health.status (http)", "any 2xx".into()),
        ("stems.<n>.health.interval", t(HEALTH_INTERVAL)),
        ("stems.<n>.health.timeout", t(HEALTH_TIMEOUT)),
        ("stems.<n>.health.retries", HEALTH_RETRIES.to_string()),
        ("stems.<n>.health.start_period", t(HEALTH_START_PERIOD)),
        ("stems.<n>.health.start_timeout", t(HEALTH_START_TIMEOUT)),
        ("stems.<n>.restart.policy", json(&RESTART_POLICY)),
        ("stems.<n>.restart.max", RESTART_MAX.to_string()),
        ("stems.<n>.restart.window", t(RESTART_WINDOW)),
        ("stems.<n>.restart.backoff.initial", t(BACKOFF_INITIAL)),
        ("stems.<n>.restart.backoff.max", t(BACKOFF_MAX)),
        (
            "stems.<n>.restart.backoff.factor",
            BACKOFF_FACTOR.to_string(),
        ),
        (
            "stems.<n>.restart.on_unhealthy",
            RESTART_ON_UNHEALTHY.to_string(),
        ),
        ("stems.<n>.restart.unhealthy_grace", t(UNHEALTHY_GRACE)),
        ("stems.<n>.restart.cascade", RESTART_CASCADE.to_string()),
        (
            "stems.<n>.watch[].ignore",
            format!("{WATCH_IGNORE:?} + user entries"),
        ),
        ("stems.<n>.watch[].debounce", t(WATCH_DEBOUNCE)),
        ("stems.<n>.watch[].action", WATCH_ACTION.to_string()),
        ("stems.<n>.watch[].settle", t(WATCH_SETTLE)),
        ("stems.<n>.watch[].root", json(&WATCH_ROOT)),
        (
            "stems.<n>.watch[].cascade",
            "stems.<n>.restart.cascade".into(),
        ),
        ("stems.<n>.overlays[].keep", OVERLAY_KEEP.to_string()),
        ("stems.<n>.overlays[].mode", OVERLAY_MODE.to_string()),
        (
            "scripts.<s>.cwd",
            "<stem codebase, else workspace root>".into(),
        ),
        ("scripts.<s>.timeout", "none".into()),
        ("scripts.<s>.retries", SCRIPT_RETRIES.to_string()),
        ("scripts.<s>.concurrent", SCRIPT_CONCURRENT.to_string()),
        ("scripts.<s>.args[].type", json(&ARG_TYPE)),
        ("scripts.<s>.args[].required", ARG_REQUIRED.to_string()),
    ]
}

mod erased {
    /// Render a serde value as its YAML scalar text.
    pub trait Show {
        fn show(&self) -> String;
    }
    impl<T: serde::Serialize> Show for T {
        fn show(&self) -> String {
            serde_yaml_ng::to_string(self)
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        }
    }
}
