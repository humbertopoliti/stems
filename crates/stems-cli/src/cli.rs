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
    /// The dependency graph.
    Graph,
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
    /// Open the dashboard (TUI) on the workspace daemon.
    ///
    /// Keys: j/k move, Enter detail, Tab cycles views, / filter, s sort,
    /// ? help, q quit (leaves the stems running). STEMS_TUI=0, a non-terminal
    /// stdout or --json give the plain event stream instead. --headless
    /// --script 'wait:healthy;frame;j;Enter;frame' replays keys and prints
    /// each frame as text (see docs/tui.md).
    Attach(AttachArgs),
    /// Show the state of every stem.
    ///
    /// Prints the table STEM TYPE STATUS REASON PID PORTS UPTIME RESTARTS
    /// (STATUS is a glyph and the state: ✓ healthy, ! degraded, ✗ failed,
    /// · stopped, ? unknown, ↻ transitioning; ASCII OK/WARN/FAIL/-/?/.. with
    /// --no-color, STEMS_ASCII=1 or a non-UTF-8 locale) and a summary line.
    /// --json gives `{stems, summary}`. --watch redraws every INTERVAL until
    /// Ctrl-C (NDJSON frames with --json).
    Status(StatusArgs),
    /// Show or follow stem and script logs.
    ///
    /// Prints captured lines (at most 10 000, the newest), several stems
    /// interleaved by time; with -f replays the last 10 lines (or --since /
    /// --tail) and keeps streaming until Ctrl-C or the daemon stops. With
    /// --json the output is NDJSON, one record per line
    /// (`{ts, stem, stream, tag, level, text, fields}`) and no envelope.
    /// --export writes a .tar.gz bundle (status, events, redacted config,
    /// log files) instead.
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
    ///
    /// The table has STEM CPU% MEM CHILDREN UPTIME RESTARTS and CPU/MEM
    /// sparklines over the last 30 samples, plus a TOTAL row. CPU% is percent
    /// of one core (can exceed 100); MEM is resident memory of the whole
    /// process tree (containers: usage minus cache). `--sort cpu|mem` puts
    /// the heaviest first ("what is eating my laptop"); `--history 5m` adds
    /// the samples of that window to --json; `--disk` also measures build
    /// outputs (target, dist, build, node_modules, .venv) and docker
    /// volumes (slow, cached 60 s). --json gives `{interval_ms, stems:
    /// [{name, type, state, latest: {ts, cpu_pct, rss_bytes, children,
    /// uptime_s, restarts}, history?, open_ports, limits, disk?}], totals:
    /// {cpu_pct, rss_bytes, children}}`. See docs/metrics.md.
    Metrics(MetricsArgs),
    /// Show the last health probe results per stem, with latency.
    ///
    /// The table has one row per probe: STEM TYPE OK LATENCY DETAIL TS.
    /// Without a stem it shows each stem's latest probe; with one, its last
    /// 10 (`--last` changes both; the daemon keeps 50 per stem). --json
    /// gives `{stems: [{name, type, state, consecutive_failures,
    /// transitions_60s, results: [{ts, ok, outcome, latency_ms, detail}]}]}`.
    Health(HealthArgs),
    /// Draw the dependency graph.
    ///
    /// Boxes per stem with its status glyph, dependants on the left, arrows
    /// to their dependencies; soft edges dashed; a legend line below. Live
    /// glyphs come from the daemon when it runs (`--no-status` for config
    /// only, `·` everywhere). `--format mermaid|dot|json` exports; --json
    /// gives `{nodes: [{name, type, status, glyph, reason}], edges: [{from,
    /// to, condition, soft, protocol, via}]}`.
    Graph(GraphArgs),

    // --- scripts -----------------------------------------------------------
    /// Run a stem or workspace script.
    ///
    /// `stems run <stem> <script> [-- args…]` runs a stem's script;
    /// `stems run --ws <script> [-- args…]` a workspace-level script
    /// (`--workspace-script` is an alias; `--workspace` stays the global flag
    /// that selects the workspace). Arguments after `--` are validated against
    /// the script's `args` schema (`SCRIPT_ARGS_INVALID`, exit 2) and passed as
    /// `--name value` flags plus `STEMS_ARG_<NAME>` env; scripts without a
    /// schema get them untouched. Exit 0 when the script succeeded, else 1
    /// (`SCRIPT_FAILED`). See docs/scripts.md.
    Run(RunArgs),
    /// List scripts.
    ///
    /// Workspace and stem scripts, lifecycle and custom, with their arguments.
    /// Reads the config locally (no daemon needed). `--json` prints the
    /// catalogue the TUI and MCP server use: `data: {scripts: [{stem, name,
    /// description, args, requires, kind, timeout, retries, concurrent,
    /// mcp_tool, input_schema}]}`.
    Scripts(ScriptsArgs),
    /// Run an ad-hoc command in a stem's context (env, cwd or container).
    Exec(ExecArgs),
    /// Open an interactive shell in a stem's context.
    Shell(ShellArgs),
    /// Run a stem's `reset` script and clear its stamps.
    Reset(ResetArgs),
    /// Run build scripts (or docker builds).
    Build(BuildArgs),
    /// Pull the images of docker stems now (a moved tag such as `latest`
    /// included).
    ///
    /// Default: every enabled docker stem with an `image`, except `pull:
    /// never` ones (pulled only when named). Uses the same registry
    /// credentials as `docker pull` (`docker login`, credential helpers).
    /// Running stems keep their old image until restarted; `--restart`
    /// restarts those whose image changed.
    Pull(PullArgs),
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
    /// Switch a stem to one of its variants (e.g. from a local process to a
    /// docker container), or back to `local`.
    ///
    /// Writes `stems.<stem>.variant` to stems.local.yaml (comments kept;
    /// `local` removes the key), validates the workspace (the file is
    /// restored on error) and, when the daemon runs, applies the change to
    /// that stem: it restarts in its new form (`--no-apply` only edits the
    /// file). Without a variant, lists the stem's variants and marks the
    /// active one. An unknown name is UNKNOWN_VARIANT (exit 2). See
    /// docs/config.md#variants-fr-st-8.
    Switch(SwitchArgs),
    /// Add a stem to stems.yaml.
    Add(AddArgs),
    /// Remove a stem from stems.yaml.
    Remove(RemoveArgs),
    /// Open $EDITOR at a stem's block in stems.yaml.
    Edit(EditArgs),

    // --- integrations ------------------------------------------------------
    /// Run the MCP server (agent interface).
    ///
    /// Serves the workspace's daemon to an MCP client (Claude Code, Cursor,
    /// ...) over stdio (default) or local HTTP: tools, custom scripts as
    /// `<stem>__<script>` tools, resources and prompts. See docs/mcp.md.
    Mcp(McpArgs),
    /// Upgrade stems (delegates to Homebrew when installed with brew).
    Upgrade(UpgradeArgs),
    /// Manage the workspace daemon.
    Daemon(DaemonArgs),

    /// Print docs/cli.md (Markdown reference of every command).
    #[command(name = "__docs", hide = true)]
    Docs,

    /// Write man pages (one per command) into OUTDIR (release packaging).
    #[command(name = "__man", hide = true)]
    Man {
        /// Directory to write `stems.1`, `stems-up.1`, … into.
        outdir: PathBuf,
    },
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

/// `stems switch`.
#[derive(Debug, Args)]
pub struct SwitchArgs {
    /// The stem.
    pub stem: String,
    /// A variant name, or `local` for the base definition. Omit to list
    /// the variants.
    pub variant: Option<String>,
    /// Only edit stems.local.yaml; do not apply the change to a running daemon.
    #[arg(long)]
    pub no_apply: bool,
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
    /// Adopt orphaned processes that look like their stem's start command.
    #[arg(long)]
    pub adopt_orphans: bool,
    /// Kill orphaned processes that look like their stem's start command.
    #[arg(long)]
    pub kill_orphans: bool,
    /// Also kill processes stems did not start that hold declared ports.
    #[arg(long)]
    pub kill_foreign: bool,
    /// Answer yes to the orphan prompt (same as --kill-orphans).
    #[arg(short, long)]
    pub yes: bool,
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
    /// Then restart their running hard dependants too, transitively, in
    /// dependency order (overrides `restart.cascade`; see docs/restart.md).
    #[arg(long, conflicts_with = "no_cascade")]
    pub cascade: bool,
    /// Restart only these stems, even if `restart.cascade` is set.
    #[arg(long)]
    pub no_cascade: bool,
}

impl RestartArgs {
    /// `--cascade` / `--no-cascade` / neither (the config decides).
    pub fn cascade_flag(&self) -> Option<bool> {
        match (self.cascade, self.no_cascade) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
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
    /// Key script to replay (implies --headless): tokens separated by `;`
    /// (`wait:healthy;frame;j;Enter;frame`), or `@FILE` to read them from a
    /// file.
    #[arg(long, value_name = "SPEC")]
    pub script: Option<String>,
    /// Write rendered frames into this directory (with --headless).
    #[arg(long, value_name = "DIR")]
    pub frames_out: Option<PathBuf>,
    /// Frame size with --headless (default 80x24).
    #[arg(long, value_name = "WxH")]
    pub size: Option<String>,
}

/// `stems status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Only these stems.
    pub stems: Vec<String>,
    /// Redraw until Ctrl-C, every INTERVAL (seconds or a duration such as
    /// `500ms`; default 1s).
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
    /// Level filter: `error` (exactly that level) or `warn+` (warn and above).
    #[arg(long)]
    pub level: Option<String>,
    /// Only output of this script (lines tagged with its name).
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
    /// Redraw until Ctrl-C, every INTERVAL (seconds or a duration such as
    /// `500ms`; default 2s).
    #[arg(long, value_name = "INTERVAL", num_args = 0..=1, default_missing_value = "2s")]
    pub watch: Option<String>,
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
    /// Probe results per stem (default 1 without a stem, 10 with one; max 50).
    #[arg(long, value_name = "N")]
    pub last: Option<usize>,
}

/// `stems graph`.
#[derive(Debug, Args)]
pub struct GraphArgs {
    /// Output format.
    #[arg(long, value_enum, default_value = "text")]
    pub format: GraphFormat,
    /// Live status glyphs from the daemon (the default when it runs; an
    /// error when it does not).
    #[arg(long, overrides_with = "no_status")]
    pub status: bool,
    /// Config only: every stem `·`, even when the daemon runs.
    #[arg(long, overrides_with = "status")]
    pub no_status: bool,
    /// Redraw until Ctrl-C, every INTERVAL (seconds or a duration such as
    /// `500ms`; default 1s).
    #[arg(long, value_name = "INTERVAL", num_args = 0..=1, default_missing_value = "1s")]
    pub watch: Option<String>,
    /// Only this stem and its neighbours.
    #[arg(long, value_name = "STEM")]
    pub focus: Option<String>,
    /// Only the stems of this profile (plus their hard dependencies).
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
    /// Run a workspace-level script instead of a stem script
    /// (`--workspace-script` is an alias).
    #[arg(
        long = "ws",
        alias = "workspace-script",
        value_name = "SCRIPT",
        conflicts_with_all = ["stem", "script"]
    )]
    pub workspace_script: Option<String>,
    /// Start the script's `requires:` stems if they are not healthy.
    #[arg(long)]
    pub start_deps: bool,
    /// How long to wait for the stem while it is still starting (default 30s).
    #[arg(long, value_name = "DURATION")]
    pub wait: Option<String>,
    /// Return the run id at once instead of waiting for the script to finish
    /// (follow it with `stems events -f`).
    #[arg(long)]
    pub no_wait: bool,
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
    /// Only these stems (default: every enabled stem). Each is stopped, its
    /// `reset` script runs and its stamps are cleared.
    pub stems: Vec<String>,
    /// Confirm (reset is destructive).
    #[arg(short, long)]
    pub yes: bool,
}

/// `stems build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Only these stems (default: every enabled stem with a `build` script).
    pub stems: Vec<String>,
}

/// `stems pull`.
#[derive(Debug, Args)]
pub struct PullArgs {
    /// Only these stems (default: every enabled docker stem with an `image`).
    pub stems: Vec<String>,
    /// Restart the running stems whose image changed.
    #[arg(long)]
    pub restart: bool,
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
    /// (sync) Pass `--recurse-submodules` to git clone / checkout.
    #[arg(long)]
    pub recurse_submodules: bool,
}

/// `stems watch`.
#[derive(Debug, Subcommand)]
pub enum WatchCommand {
    /// Pause watchdogs (all, or some stems').
    Pause(WatchStemArgs),
    /// Resume watchdogs (all, clearing per-stem pauses too, or some stems').
    Resume(WatchStemArgs),
    /// Show watchdog rules and state (--json gives `{stems, global_paused, disabled}`).
    Status,
}

/// `stems watch pause|resume`.
#[derive(Debug, Args)]
pub struct WatchStemArgs {
    /// Only these stems (default: every watchdog).
    pub stems: Vec<String>,
}

/// `stems profiles`.
#[derive(Debug, Args)]
pub struct ProfilesArgs {}

/// `stems outputs`.
#[derive(Debug, Args)]
pub struct OutputsArgs {
    /// Only this stem.
    pub stem: Option<String>,
    /// Show secret values (human output on a terminal only; JSON never reveals).
    #[arg(long)]
    pub reveal: bool,
}

/// `stems config`.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the value at a config path (e.g. stems.shop-api.env.PORT).
    Get(ConfigGetArgs),
    /// Set a value in stems.local.yaml.
    Set(ConfigSetArgs),
    /// Remove a value from stems.local.yaml.
    Unset(ConfigUnsetArgs),
    /// Show what applying the changed config would do (the daemon's plan:
    /// STEM ACTION FIELDS HOT).
    Diff,
    /// Apply the changed config to running stems: stop removed stems,
    /// restart changed ones in dependency order, hot-apply the rest.
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

/// `stems config unset`.
#[derive(Debug, Args)]
pub struct ConfigUnsetArgs {
    /// Dotted config path.
    pub path: String,
}

/// `stems config apply`.
#[derive(Debug, Args)]
pub struct ConfigApplyArgs {
    /// Only these stems' changes (removals of running stems always apply).
    pub stems: Vec<String>,
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
    /// Port for --transport http (binds 127.0.0.1 only) [default: 7070].
    #[arg(long)]
    pub port: Option<u16>,
    /// Start the daemon if it is not running; when the client disconnects,
    /// stop it again if this server started it and no stem is running.
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
        let cli = Cli::try_parse_from(["stems", "run", "--ws", "nuke", "--", "-x"]).unwrap();
        let Some(Command::Run(r)) = cli.command else {
            panic!("not run")
        };
        assert_eq!(r.workspace_script.as_deref(), Some("nuke"));
        assert_eq!(r.args, ["-x"]);
    }
}
