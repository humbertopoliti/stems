//! The clap command tree: global flags and every subcommand.
//!
//! The whole surface exists now (deliverable 07) so scenarios can be written
//! against it; commands whose deliverable has not landed return
//! `NOT_IMPLEMENTED` (exit 1) with `details.deliverable`. Doc comments are the
//! `--help` text, golden-tested in `tests/help.rs` and rendered into
//! `docs/cli.md` by `stems __docs`.

use std::path::PathBuf;

use clap::builder::FalseyValueParser;
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};

/// Local environment management toolkit for multi-process systems.
///
/// stems reads an integration repo's `stems.yaml`, validates it, and (from
/// later releases) starts, supervises and tears down every stem it declares.
///
/// Output: human text on a terminal, a JSON envelope
/// `{ "ok", "data", "errors", "version" }` when stdout is not a terminal or
/// with `--json`. Exit codes: 0 ok, 1 runtime failure, 2 config, validation
/// or usage error, 3 partial success or orphans, 4 daemon unavailable.
#[derive(Debug, Parser)]
#[command(
    name = "stems",
    bin_name = "stems",
    disable_version_flag = true,
    term_width = 100
)]
pub struct Cli {
    /// Global flags, accepted before or after the subcommand.
    #[command(flatten)]
    pub global: GlobalArgs,

    /// Print version information (`--json` adds commit and build date).
    #[arg(short = 'V', long)]
    pub version: bool,

    /// The command to run.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags every command accepts.
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Global options")]
pub struct GlobalArgs {
    /// Workspace directory or stems.yaml path (default: $STEMS_WORKSPACE, else
    /// walk up from the current directory).
    #[arg(long, global = true, value_name = "PATH")]
    pub workspace: Option<PathBuf>,

    /// Emit the JSON envelope (the default when stdout is not a terminal).
    #[arg(long, global = true, conflicts_with = "human")]
    pub json: bool,

    /// Emit human text even when stdout is not a terminal.
    #[arg(long, global = true)]
    pub human: bool,

    /// Disable colours in human output.
    #[arg(
        long,
        global = true,
        env = "STEMS_NO_COLOR",
        hide_env_values = true,
        action = ArgAction::SetTrue,
        value_parser = FalseyValueParser::new()
    )]
    pub no_color: bool,

    /// Print only errors in human mode.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// More detail (repeat for more: -vv).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Root for daemon state, sockets, locks and logs (default:
    /// ~/Library/Application Support/stems on macOS, $XDG_STATE_HOME/stems or
    /// ~/.local/state/stems on Linux).
    #[arg(
        long,
        global = true,
        env = "STEMS_HOME",
        hide_env_values = true,
        value_name = "PATH"
    )]
    pub home: Option<PathBuf>,
}

/// Workspace template for `stems init --from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Template {
    /// One process stem, no Docker.
    Minimal,
    /// The full example: docker, compose, process and external stems.
    HelloShop,
}

/// Shells for `stems completions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// GNU bash.
    Bash,
    /// Z shell.
    Zsh,
    /// fish.
    Fish,
}

/// Stem types for `stems add --type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StemKind {
    /// A local process started from a codebase.
    Process,
    /// A single docker container.
    Docker,
    /// A service of a docker compose file.
    Compose,
    /// A service stems only monitors.
    External,
}

/// `stems attach --view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum View {
    /// The stem table.
    Table,
    /// The selected stem's detail pane.
    Detail,
}

/// `stems graph --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GraphFormat {
    /// Layered box drawing.
    Text,
    /// Mermaid flowchart.
    Mermaid,
    /// Graphviz dot.
    Dot,
    /// JSON nodes and edges.
    Json,
}

/// `stems metrics --sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MetricsSort {
    /// By CPU usage.
    Cpu,
    /// By memory usage.
    Mem,
}

/// `stems mcp --transport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum McpTransport {
    /// MCP over stdin/stdout.
    Stdio,
    /// MCP over local HTTP/SSE.
    Http,
}

/// Every subcommand.
#[derive(Debug, Subcommand)]
pub enum Command {
    // --- config-only (07) --------------------------------------------------
    /// Scaffold a new integration repo in the current (or --workspace) directory.
    ///
    /// Creates stems.yaml (with a `yaml-language-server` schema reference),
    /// .gitignore, scripts/, env/ and overlays/. Refuses to overwrite an
    /// existing stems.yaml (ALREADY_INITIALISED) unless --force.
    Init(InitArgs),
    /// Check the workspace config without starting anything.
    ///
    /// Reports every error (schema, paths, dependencies, cycles, port
    /// conflicts, scripts, overlays, tool versions) sorted by location, and
    /// the start order on success.
    Validate(ValidateArgs),
    /// Print the resolved workspace config (or one stem's).
    ///
    /// The resolved model after merging stems.local.yaml, includes and
    /// extends, applying defaults and substituting `${…}` (every field is
    /// present; disabled stems are listed with `enabled: false`). With a stem
    /// name, `data` is that stem. `--effective` adds `defaults`: the table of
    /// default values applied to unset fields.
    Show(ShowArgs),
    /// Print a shell completion script.
    Completions(CompletionsArgs),

    // --- lifecycle ---------------------------------------------------------
    /// Start stems (and their dependencies), attached or detached.
    Up(UpArgs),
    /// Stop stems in reverse dependency order.
    Down(DownArgs),
    /// Start stems (and unstarted hard dependencies).
    Start(StartArgs),
    /// Stop stems.
    Stop(StopArgs),
    /// Restart stems (stop, then start).
    Restart(RestartArgs),
    /// Follow the workspace daemon (plain event stream until the TUI lands).
    Attach(AttachArgs),
    /// Show the state of every stem.
    Status(StatusArgs),
    /// Show or follow stem and script logs.
    Logs(LogsArgs),
    /// Show or follow the event stream (NDJSON with --json).
    ///
    /// Prints the daemon's buffered events (the last 10 000), oldest first;
    /// with -f keeps streaming new ones until the daemon stops or Ctrl-C.
    /// With --json the output is NDJSON, one event object per line
    /// (`{ts, seq, kind, stem, from, to, reason, actor, data}`) and no
    /// envelope: the hook for external automation. Errors still use the
    /// envelope (e.g. exit 4 DAEMON_NOT_RUNNING).
    Events(EventsArgs),
    /// Show CPU, memory and disk usage per stem.
    Metrics(MetricsArgs),
    /// Show health check results.
    Health(HealthArgs),
    /// Draw the dependency graph.
    Graph(GraphArgs),

    // --- scripts -----------------------------------------------------------
    /// Run a stem or workspace script.
    Run(RunArgs),
    /// List scripts.
    Scripts(ScriptsArgs),
    /// Run an ad-hoc command in a stem's context (env, cwd or container).
    Exec(ExecArgs),
    /// Open an interactive shell in a stem's context.
    Shell(ShellArgs),
    /// Run a stem's `reset` script and clear its stamps.
    Reset(ResetArgs),
    /// Run build scripts (or docker builds).
    Build(BuildArgs),
    /// Show or clear setup/seed stamps.
    Stamps(StampsArgs),
    /// Show the overlays stems has written into codebases.
    Overlays(OverlaysArgs),

    // --- workspace ---------------------------------------------------------
    /// Diagnose the environment: Docker, tool versions, ports, orphans.
    Doctor(DoctorArgs),
    /// Clone or update git codebases.
    #[command(subcommand)]
    Repos(ReposCommand),
    /// Pause or resume watchdogs.
    #[command(subcommand)]
    Watch(WatchCommand),
    /// List profiles.
    Profiles(ProfilesArgs),
    /// Show values published by stems' `outputs:`.
    Outputs(OutputsArgs),
    /// Read or change local config overrides.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Add a stem to stems.yaml.
    Add(AddArgs),
    /// Remove a stem from stems.yaml.
    Remove(RemoveArgs),
    /// Open $EDITOR at a stem's block in stems.yaml.
    Edit(EditArgs),

    // --- integrations ------------------------------------------------------
    /// Run the MCP server (agent interface).
    Mcp(McpArgs),
    /// Upgrade stems (delegates to Homebrew when installed with brew).
    Upgrade(UpgradeArgs),
    /// Manage the workspace daemon.
    Daemon(DaemonArgs),

    /// Print docs/cli.md (Markdown reference of every command).
    #[command(name = "__docs", hide = true)]
    Docs,
}

/// `stems init`.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// Workspace name (default: the directory name).
    #[arg(long)]
    pub name: Option<String>,
    /// Start from an example workspace instead of an empty template.
    #[arg(long, value_enum)]
    pub from: Option<Template>,
    /// Overwrite an existing stems.yaml.
    #[arg(long)]
    pub force: bool,
}

/// `stems validate`.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Do not run `<tool> --version` for `requires:`.
    #[arg(long)]
    pub skip_requires: bool,
}

/// `stems show`.
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Only this stem.
    pub stem: Option<String>,
    /// Also print the table of defaults applied to unset fields.
    #[arg(long)]
    pub effective: bool,
}

/// `stems completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Target shell.
    #[arg(value_enum)]
    pub shell: CompletionShell,
}

/// `stems up`.
#[derive(Debug, Args)]
pub struct UpArgs {
    /// Stems to start (default: the profile's stems, or all).
    pub stems: Vec<String>,
    /// Profile to start.
    #[arg(long)]
    pub profile: Option<String>,
    /// Return once stems are up instead of staying attached.
    #[arg(short, long)]
    pub detach: bool,
    /// Ignore stamps: run reset, setup and seed again.
    #[arg(long)]
    pub fresh: bool,
    /// Overall deadline (e.g. 90s).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
    /// Stop starting further stems after the first failure (the default).
    #[arg(long, conflicts_with = "no_fail_fast")]
    pub fail_fast: bool,
    /// Keep starting stems that do not depend on a failed one.
    #[arg(long)]
    pub no_fail_fast: bool,
    /// Stems starting concurrently.
    #[arg(long, value_name = "N", default_value_t = 4)]
    pub max_parallel: usize,
    /// Pass these variables of the current shell to every stem (applied
    /// last; repeat or comma-separate).
    #[arg(long, value_name = "VAR", value_delimiter = ',')]
    pub pass_env: Vec<String>,
    /// Do not start watchdogs.
    #[arg(long)]
    pub no_watch: bool,
    /// Fetch git codebases before starting.
    #[arg(long)]
    pub sync: bool,
    /// Back up conflicting overlay destinations and proceed.
    #[arg(long)]
    pub force_overlays: bool,
}

/// `stems down`.
#[derive(Debug, Args)]
pub struct DownArgs {
    /// Stems to stop (default: all started by this workspace).
    pub stems: Vec<String>,
    /// Stop everything and shut the daemon down.
    #[arg(long)]
    pub all: bool,
    /// Also remove docker volumes (destructive).
    #[arg(long)]
    pub volumes: bool,
    /// Confirm destructive operations.
    #[arg(short, long)]
    pub yes: bool,
    /// Per-stem stop deadline override (e.g. 5s).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

/// `stems start`.
#[derive(Debug, Args)]
pub struct StartArgs {
    /// Stems to start.
    #[arg(required = true)]
    pub stems: Vec<String>,
    /// Do not start unstarted dependencies.
    #[arg(long)]
    pub no_deps: bool,
    /// Deadline (e.g. 60s).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

/// `stems stop`.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// Stems to stop.
    #[arg(required = true)]
    pub stems: Vec<String>,
    /// Also stop running dependants.
    #[arg(long)]
    pub cascade: bool,
    /// Deadline (e.g. 10s).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

/// `stems restart`.
#[derive(Debug, Args)]
pub struct RestartArgs {
    /// Stems to restart.
    #[arg(required = true)]
    pub stems: Vec<String>,
    /// Do not start unstarted dependencies.
    #[arg(long)]
    pub no_deps: bool,
    /// Re-run the `build` script first.
    #[arg(long)]
    pub build: bool,
    /// Deadline (e.g. 60s).
    #[arg(long, value_name = "DURATION")]
    pub timeout: Option<String>,
}

/// `stems attach`.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Initial view.
    #[arg(long, value_enum)]
    pub view: Option<View>,
    /// Render frames as text without a terminal (for tests).
    #[arg(long)]
    pub headless: bool,
    /// Key script to replay (with --headless).
    #[arg(long, value_name = "FILE")]
    pub script: Option<PathBuf>,
    /// Write rendered frames into this directory (with --headless).
    #[arg(long, value_name = "DIR")]
    pub frames_out: Option<PathBuf>,
}

/// `stems status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Only these stems.
    pub stems: Vec<String>,
    /// Refresh continuously (default interval 1s).
    #[arg(long, value_name = "INTERVAL", num_args = 0..=1, default_missing_value = "1s")]
    pub watch: Option<String>,
}

/// `stems logs`.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Only these stems.
    pub stems: Vec<String>,
    /// Keep streaming new lines.
    #[arg(short, long)]
    pub follow: bool,
    /// Start at this age or timestamp (e.g. 10m).
    #[arg(long)]
    pub since: Option<String>,
    /// Stop at this age or timestamp.
    #[arg(long)]
    pub until: Option<String>,
    /// Only lines matching this regex.
    #[arg(long, value_name = "REGEX")]
    pub grep: Option<String>,
    /// Minimum level (e.g. warn, or warn+).
    #[arg(long)]
    pub level: Option<String>,
    /// Logs of this script run instead of the stem's process.
    #[arg(long, value_name = "NAME")]
    pub script: Option<String>,
    /// Last N lines.
    #[arg(long, value_name = "N")]
    pub tail: Option<usize>,
    /// Write a log bundle (.tar.gz) instead of printing.
    #[arg(long)]
    pub export: bool,
    /// Output file for --export.
    #[arg(short, long, value_name = "FILE")]
    pub output: Option<PathBuf>,
}

/// `stems events`.
#[derive(Debug, Args)]
pub struct EventsArgs {
    /// Keep streaming new events.
    #[arg(short, long)]
    pub follow: bool,
    /// Only events after this sequence number (default 0: every buffered
    /// event).
    #[arg(long, value_name = "SEQ")]
    pub since: Option<u64>,
}

/// `stems metrics`.
#[derive(Debug, Args)]
pub struct MetricsArgs {
    /// Only these stems.
    pub stems: Vec<String>,
    /// Refresh continuously.
    #[arg(long)]
    pub watch: bool,
    /// Include samples from this window (e.g. 5m).
    #[arg(long, value_name = "DURATION")]
    pub history: Option<String>,
    /// Sort order.
    #[arg(long, value_enum)]
    pub sort: Option<MetricsSort>,
    /// Also compute disk usage (slow).
    #[arg(long)]
    pub disk: bool,
}

/// `stems health`.
#[derive(Debug, Args)]
pub struct HealthArgs {
    /// Only this stem.
    pub stem: Option<String>,
}

/// `stems graph`.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: GraphFormat,
    /// Colour nodes by live status (default when the daemon runs).
    #[arg(long)]
    pub status: bool,
    /// Redraw on every state change.
    #[arg(long)]
    pub watch: bool,
    /// Only this stem and its neighbours.
    #[arg(long, value_name = "STEM")]
    pub focus: Option<String>,
    /// Only the stems of this profile.
    #[arg(long)]
    pub profile: Option<String>,
    /// Label edges with their protocol/via metadata.
    #[arg(long)]
    pub edges: bool,
}

/// `stems run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Stem whose script to run.
    #[arg(required_unless_present = "workspace_script")]
    pub stem: Option<String>,
    /// Script name.
    #[arg(required_unless_present = "workspace_script")]
    pub script: Option<String>,
    /// Run a workspace-level script instead of a stem script.
    #[arg(long, value_name = "SCRIPT", conflicts_with_all = ["stem", "script"])]
    pub workspace_script: Option<String>,
    /// Start the script's `requires:` stems if they are not healthy.
    #[arg(long)]
    pub start_deps: bool,
    /// How long to wait for a starting stem (e.g. 30s).
    #[arg(long, value_name = "DURATION")]
    pub wait: Option<String>,
    /// Script arguments (after `--`), e.g. `-- --email a@b.c`.
    #[arg(last = true)]
    pub args: Vec<String>,
}

/// `stems scripts`.
#[derive(Debug, Args)]
pub struct ScriptsArgs {
    /// Only this stem's scripts.
    pub stem: Option<String>,
}

/// `stems exec`.
#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Stem whose context to use.
    pub stem: String,
    /// Command and arguments (after `--`).
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

/// `stems shell`.
#[derive(Debug, Args)]
pub struct ShellArgs {
    /// Stem whose context to use.
    pub stem: String,
}

/// `stems reset`.
#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Only this stem (default: every stem with a reset script).
    pub stem: Option<String>,
    /// Confirm (reset is destructive).
    #[arg(short, long)]
    pub yes: bool,
}

/// `stems build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Only these stems.
    pub stems: Vec<String>,
}

/// `stems stamps`.
#[derive(Debug, Args)]
pub struct StampsArgs {
    /// Only this stem.
    pub stem: Option<String>,
    /// Clear the stamps so setup/seed run again.
    #[arg(long)]
    pub clear: bool,
}

/// `stems overlays`.
#[derive(Debug, Args)]
pub struct OverlaysArgs {
    /// Only this stem.
    pub stem: Option<String>,
}

/// `stems doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Apply fixable items (after confirmation).
    #[arg(long)]
    pub fix: bool,
    /// Scan for orphaned processes and containers.
    #[arg(long)]
    pub orphans: bool,
    /// Confirm fixes and orphan removal.
    #[arg(short, long)]
    pub yes: bool,
    /// Exit non-zero on warnings too.
    #[arg(long)]
    pub strict: bool,
    /// Also kill processes stems did not start that hold declared ports.
    #[arg(long)]
    pub kill_foreign: bool,
}

/// `stems repos`.
#[derive(Debug, Subcommand)]
pub enum ReposCommand {
    /// Clone missing git codebases and fetch/check out their refs.
    Sync(ReposArgs),
    /// Show each git codebase's branch, commit and cleanliness.
    Status(ReposArgs),
}

/// `stems repos sync|status`.
#[derive(Debug, Args)]
pub struct ReposArgs {
    /// Only these stems.
    pub stems: Vec<String>,
}

/// `stems watch`.
#[derive(Debug, Subcommand)]
pub enum WatchCommand {
    /// Pause watchdogs (all, or one stem's).
    Pause(WatchStemArgs),
    /// Resume watchdogs.
    Resume(WatchStemArgs),
    /// Show watchdog state.
    Status,
}

/// `stems watch pause|resume`.
#[derive(Debug, Args)]
pub struct WatchStemArgs {
    /// Only this stem.
    pub stem: Option<String>,
}

/// `stems profiles`.
#[derive(Debug, Args)]
pub struct ProfilesArgs {}

/// `stems outputs`.
#[derive(Debug, Args)]
pub struct OutputsArgs {
    /// Only this stem.
    pub stem: Option<String>,
}

/// `stems config`.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the value at a config path (e.g. stems.shop-api.env.PORT).
    Get(ConfigGetArgs),
    /// Set a value in stems.local.yaml.
    Set(ConfigSetArgs),
    /// Show what applying the changed config would do.
    Diff,
    /// Apply the changed config to running stems.
    Apply(ConfigApplyArgs),
}

/// `stems config get`.
#[derive(Debug, Args)]
pub struct ConfigGetArgs {
    /// Dotted config path.
    pub path: String,
}

/// `stems config set`.
#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// Dotted config path.
    pub path: String,
    /// Value (parsed as YAML).
    pub value: String,
}

/// `stems config apply`.
#[derive(Debug, Args)]
pub struct ConfigApplyArgs {
    /// Do not ask for confirmation.
    #[arg(short, long)]
    pub yes: bool,
}

/// `stems add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// New stem name.
    pub name: String,
    /// Stem type.
    #[arg(long = "type", value_enum)]
    pub kind: StemKind,
}

/// `stems remove`.
#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Stem name.
    pub name: String,
    /// Do not ask for confirmation.
    #[arg(short, long)]
    pub yes: bool,
}

/// `stems edit`.
#[derive(Debug, Args)]
pub struct EditArgs {
    /// Stem name.
    pub name: String,
}

/// `stems mcp`.
#[derive(Debug, Args)]
pub struct McpArgs {
    /// Transport.
    #[arg(long, value_enum, default_value = "stdio")]
    pub transport: McpTransport,
    /// Port for --transport http.
    #[arg(long)]
    pub port: Option<u16>,
    /// Start the daemon if it is not running.
    #[arg(long)]
    pub auto_start: bool,
}

/// `stems upgrade`.
#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Print what would run without running it.
    #[arg(long)]
    pub dry_run: bool,
}

/// `stems daemon`.
#[derive(Debug, Args)]
pub struct DaemonArgs {
    /// Daemon action (without one, runs the daemon: used by auto-spawn).
    #[command(subcommand)]
    pub action: Option<DaemonCommand>,
    /// Run the daemon in this terminal instead of detaching.
    #[arg(long, hide = true)]
    pub foreground: bool,
    /// Daemon log level (error, warn, info, debug, trace).
    #[arg(long, value_name = "LEVEL")]
    pub log_level: Option<String>,
}

/// `stems daemon start|stop|status`.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the workspace daemon in the background.
    ///
    /// Spawns the daemon detached (its own session, output appended to
    /// `$STEMS_HOME/<ws-hash>/stemsd.log`) and waits up to 5 s for its socket.
    /// If a daemon already serves this workspace, reports it with
    /// `already_running: true` (exit 0).
    Start(DaemonStartArgs),
    /// Stop the workspace daemon.
    ///
    /// Asks it to shut down (the same orderly path as SIGTERM) and waits up to
    /// 5 s for its socket and lock to disappear. Exits 4 (DAEMON_NOT_RUNNING)
    /// when none is running; a stale lock left by a crashed daemon is removed.
    Stop,
    /// Show the daemon's pid, version, socket and uptime.
    ///
    /// `data`: `{ running, pid, version, api_version, workspace, uptime_s,
    /// socket, lock, log, ... }`. Exits 4 (DAEMON_NOT_RUNNING, `running:
    /// false`) when no daemon answers; the hint says `stale lock` when a
    /// crashed daemon left its lock behind.
    Status,
}

/// `stems daemon start`.
#[derive(Debug, Args)]
pub struct DaemonStartArgs {
    /// Run the daemon in this terminal (logging to stderr too) instead of
    /// detaching; Ctrl-C stops it.
    #[arg(long)]
    pub foreground: bool,
}

/// The clap `Command` for the whole tree (built: usage strings are final).
pub fn command() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_flags_parse_after_the_subcommand() {
        let cli = Cli::try_parse_from(["stems", "validate", "--json", "--workspace", "/w", "-vv"])
            .unwrap();
        assert!(cli.global.json);
        assert_eq!(cli.global.verbose, 2);
        assert_eq!(cli.global.workspace, Some(PathBuf::from("/w")));
    }

    #[test]
    fn json_and_human_conflict() {
        assert!(Cli::try_parse_from(["stems", "validate", "--json", "--human"]).is_err());
    }

    #[test]
    fn run_takes_script_args_after_dashdash() {
        let cli =
            Cli::try_parse_from(["stems", "run", "shop-api", "seed", "--", "--rows", "5"]).unwrap();
        let Some(Command::Run(r)) = cli.command else {
            panic!("not run")
        };
        assert_eq!(r.stem.as_deref(), Some("shop-api"));
        assert_eq!(r.args, ["--rows", "5"]);
        let cli = Cli::try_parse_from(["stems", "run", "--workspace-script", "nuke"]).unwrap();
        let Some(Command::Run(r)) = cli.command else {
            panic!("not run")
        };
        assert_eq!(r.workspace_script.as_deref(), Some("nuke"));
    }
}
