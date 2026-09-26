//! The per-scenario `World`: an isolated temp dir, a copied workspace, a
//! unique `STEMS_HOME` and port block, and everything the steps and the
//! After hook need to know about what the scenario did.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use serde_yaml_ng::Mapping;
use tempfile::TempDir;
use tokio::process::{Child, Command};

use crate::{http, remap, util};

/// Ports per scenario block.
pub const PORT_BLOCK: u16 = 20;
/// First port of the first block.
pub const PORT_START: u16 = 20000;

/// The repository root (two levels above this crate's manifest).
pub fn repo_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        p.canonicalize().unwrap_or(p)
    })
}

/// The `stems` binary under test.
///
/// The harness is a separate crate from `stems-cli`, so Cargo's
/// `CARGO_BIN_EXE_<name>` (same-package only) is not available. Instead the
/// binary is located next to this test executable's `deps/` directory
/// (`target/<profile>/stems`), which also honours `CARGO_TARGET_DIR`.
/// `make e2e` builds it first (`cargo build -p stems-cli`).
/// `STEMS_E2E_BIN` overrides the location.
pub fn stems_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("STEMS_E2E_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    // target/<profile>/deps/e2e-<hash> -> target/<profile>/stems
    exe.parent()
        .and_then(Path::parent)
        .map(|dir| dir.join("stems"))
        .unwrap_or_else(|| repo_root().join("target/debug/stems"))
}

/// Per-scenario time budget (default 60 s, `STEMS_E2E_SCENARIO_TIMEOUT`).
pub fn scenario_timeout() -> Duration {
    let secs = std::env::var("STEMS_E2E_SCENARIO_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs)
}

/// Where scenario temp dirs are created. `/tmp` by default (not `$TMPDIR`)
/// because macOS `$TMPDIR` paths are long enough to push
/// `$STEMS_HOME/<ws-hash>/stemsd.sock` past the 104-byte Unix socket limit.
pub fn tmp_base() -> PathBuf {
    std::env::var_os("STEMS_E2E_TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

static NEXT_BLOCK: AtomicU32 = AtomicU32::new(0);

fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
        && std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// Allocates a block of [`PORT_BLOCK`] free ports: `20000 + n * 20` for the
/// n-th request, skipping blocks where any port is already bound.
pub fn allocate_port_block() -> u16 {
    loop {
        let n = NEXT_BLOCK.fetch_add(1, Ordering::SeqCst);
        let base = u32::from(PORT_START) + n * u32::from(PORT_BLOCK);
        assert!(
            base + u32::from(PORT_BLOCK) < 60000,
            "e2e harness ran out of port blocks"
        );
        let base = u16::try_from(base).unwrap_or(PORT_START);
        if (base..base + PORT_BLOCK).all(port_free) {
            return base;
        }
    }
}

/// Recursively copies `src` into `dst`, recreating symlinks and skipping
/// developer-local files (`stems.local.yaml`, `.stems/`, caches).
pub fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if matches!(
            name_s.as_ref(),
            "stems.local.yaml" | ".stems" | "__pycache__" | ".venv" | ".DS_Store"
        ) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)?;
        } else if ty.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Files named one of `names` anywhere under `dir`.
pub fn find_files(dir: &Path, pred: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ty) = entry.file_type() else { continue };
        if ty.is_dir() {
            out.extend(find_files(&path, pred));
        } else if pred(&entry.file_name().to_string_lossy()) {
            out.push(path);
        }
    }
    out
}

/// Socket and lock files the daemon must not leave behind.
pub fn is_socket_or_lock(name: &str) -> bool {
    name.ends_with(".sock") || name.ends_with(".lock")
}

/// Collects every numeric `"pgid"` value in a JSON document.
pub fn collect_pgids(v: &Value, out: &mut BTreeSet<i32>) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if k == "pgid"
                    && let Some(n) = val.as_i64().and_then(|n| i32::try_from(n).ok())
                {
                    out.insert(n);
                }
                collect_pgids(val, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_pgids(x, out)),
        _ => {}
    }
}

type Reader = (
    std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    tokio::task::JoinHandle<()>,
);

fn spawn_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(pipe: Option<R>) -> Reader {
    use tokio::io::AsyncReadExt as _;
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&buf);
    let handle = tokio::spawn(async move {
        let Some(mut pipe) = pipe else { return };
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(&chunk[..n]);
        }
    });
    (buf, handle)
}

/// Waits up to 2 s for EOF on a pipe, then returns what was read.
async fn collect_reader((buf, handle): Reader) -> String {
    // If a descendant still holds the pipe after 2 s, keep what was read.
    let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
    let bytes = buf
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Result of one `stems` invocation.
#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub json: Option<Value>,
    /// Wall-clock time from spawn to exit.
    pub elapsed: Duration,
}

/// A `stems` command started with `When I run "..." in the background`.
#[derive(Debug)]
pub struct Background {
    pub argv: Vec<String>,
    pub child: Child,
    pub pgid: i32,
    stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    /// Exit code once it has exited (128 + signal when killed).
    pub code: Option<i32>,
}

impl Background {
    fn text(buf: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> String {
        let bytes = buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Everything it printed on stdout so far.
    pub fn stdout(&self) -> String {
        Self::text(&self.stdout)
    }

    /// Everything it printed on stderr so far.
    pub fn stderr(&self) -> String {
        Self::text(&self.stderr)
    }

    /// Records the exit code if it has exited (non-blocking).
    pub fn poll_exit(&mut self) -> Option<i32> {
        if self.code.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.code = Some(
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
            );
        }
        self.code
    }

    /// Human summary for failure messages.
    pub fn describe(&self) -> String {
        format!(
            "background `{}` (exit {:?})\n--- stdout\n{}\n--- stderr\n{}",
            self.argv.join(" "),
            self.code,
            util::clip(self.stdout().trim_end(), 3000),
            util::clip(self.stderr().trim_end(), 2000)
        )
    }
}

impl CmdOutput {
    /// Human summary for failure messages.
    pub fn describe(&self) -> String {
        let hint = if self.is_usage_rejection() {
            " [the binary does not recognise this command/flag yet]"
        } else {
            ""
        };
        format!(
            "`{}` (cwd {}) exited {}{hint}\n--- stdout\n{}\n--- stderr\n{}",
            self.argv.join(" "),
            self.cwd.display(),
            self.code,
            util::clip(self.stdout.trim_end(), 2000),
            util::clip(self.stderr.trim_end(), 2000)
        )
    }

    /// clap-style usage error: exit 2 with "unrecognized subcommand" or
    /// "unexpected argument" on stderr and no JSON on stdout.
    pub fn is_usage_rejection(&self) -> bool {
        self.code == 2
            && self.json.is_none()
            && (self.stderr.contains("unrecognized subcommand")
                || self.stderr.contains("unexpected argument"))
    }

    /// Panics with "not implemented until deliverable NN" when the binary
    /// does not know the subcommand yet (clap usage error, exit 2).
    pub fn guard_implemented(&self, deliverable: &str) {
        if self.code != 2 {
            return;
        }
        let sub = self.argv.get(1).map(String::as_str).unwrap_or_default();
        if self.stderr.contains("unrecognized subcommand")
            || self
                .stderr
                .contains(&format!("unexpected argument '{sub}'"))
        {
            panic!(
                "not implemented until deliverable {deliverable}: the stems binary does not know `stems {sub}` yet\n{}",
                self.describe()
            );
        }
    }
}

/// Everything a scenario owns. Created fresh per scenario.
#[derive(Debug)]
pub struct E2eWorld {
    tmp: Option<TempDir>,
    /// Canonical scenario root (`/private/tmp/stems-e2e-XXXX` on macOS).
    pub root: PathBuf,
    /// `STEMS_HOME` for every command.
    pub home: PathBuf,
    /// Example name (`minimal`, `broken/cycle`), once chosen.
    pub ws_name: Option<String>,
    /// The copied workspace dir (`<root>/examples/workspaces/<name>`).
    pub ws_dir: Option<PathBuf>,
    /// First port of this scenario's block, if ports were remapped.
    pub port_base: Option<u16>,
    /// Declared port -> remapped port.
    pub port_map: BTreeMap<u16, u16>,
    /// Contents of the generated `stems.local.yaml`.
    pub local: Mapping,
    /// Last `stems` command.
    pub last: Option<CmdOutput>,
    /// Last chaos HTTP response.
    pub last_http: Option<http::Response>,
    /// Set when a step may have started a daemon; the After hook then runs
    /// `stems daemon stop`.
    pub daemon_started: bool,
    /// Every process group the scenario created or learned about.
    pub pgids: BTreeSet<i32>,
    /// Processes spawned directly by steps (not through `stems`).
    pub strays: Vec<Child>,
    /// Scenario deadline.
    pub deadline: Instant,
    /// The command started with `When I run "..." in the background`.
    pub background: Option<Background>,
    /// Values saved with `When I save the JSON at ... as "<name>"` (`${var:name}`).
    pub vars: BTreeMap<String, String>,
    /// The MCP test client and its `stems mcp` server (31).
    pub mcp: Option<crate::mcp::McpSession>,
}

impl cucumber::World for E2eWorld {
    type Error = String;

    async fn new() -> Result<Self, String> {
        Self::create()
    }
}

impl E2eWorld {
    /// Creates the temp root with `home/` and `outside/`.
    pub fn create() -> Result<Self, String> {
        let base = tmp_base();
        let tmp = tempfile::Builder::new()
            .prefix("stems-e2e-")
            .tempdir_in(&base)
            .map_err(|e| format!("cannot create temp dir in {}: {e}", base.display()))?;
        let root = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        let home = root.join("home");
        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root.join("outside")).map_err(|e| e.to_string())?;
        Ok(Self {
            tmp: Some(tmp),
            root,
            home,
            ws_name: None,
            ws_dir: None,
            port_base: None,
            port_map: BTreeMap::new(),
            local: Mapping::new(),
            last: None,
            last_http: None,
            daemon_started: false,
            pgids: BTreeSet::new(),
            strays: Vec::new(),
            deadline: Instant::now() + scenario_timeout(),
            background: None,
            vars: BTreeMap::new(),
            mcp: None,
        })
    }

    /// Remaining time budget; panics with `TIMEOUT:` once exhausted.
    pub fn remaining(&self) -> Duration {
        let r = self.deadline.saturating_duration_since(Instant::now());
        assert!(
            !r.is_zero(),
            "TIMEOUT: scenario exceeded its {}s budget (STEMS_E2E_SCENARIO_TIMEOUT)",
            scenario_timeout().as_secs()
        );
        r
    }

    /// The workspace dir, or a panic explaining which step is missing.
    pub fn ws(&self) -> &Path {
        self.ws_dir
            .as_deref()
            .expect("no workspace yet: start the scenario with `Given the \"<name>\" workspace`")
    }

    /// Copies `examples/workspaces/<name>` into the scenario and (unless
    /// `remap` is false or the workspace is under `broken/`) writes the
    /// port-remapping `stems.local.yaml`.
    pub fn use_workspace(&mut self, name: &str, remap: bool) {
        self.use_workspace_at("examples/workspaces", name, remap);
    }

    /// Like [`Self::use_workspace`] for `tests/fixtures/workspaces/<name>`
    /// (harness-only fixtures that are not user documentation).
    pub fn use_fixture_workspace(&mut self, name: &str, remap: bool) {
        self.use_workspace_at("tests/fixtures/workspaces", name, remap);
    }

    /// Copies `<repo>/<parent>/<name>` to `<root>/<parent>/<name>` (the same
    /// relative layout, so `../../repos/...` style codebase paths keep
    /// resolving) and symlinks `<root>/examples/repos` to the real repos.
    fn use_workspace_at(&mut self, parent: &str, name: &str, remap: bool) {
        assert!(
            self.ws_dir.is_none(),
            "a workspace was already chosen for this scenario"
        );
        let src = repo_root().join(parent).join(name);
        assert!(
            src.join("stems.yaml").is_file(),
            "unknown workspace {name:?}: {} has no stems.yaml",
            src.display()
        );
        let dest = self.root.join(parent).join(name);
        copy_dir(&src, &dest).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
        let repos_link = self.root.join("examples/repos");
        if !repos_link.exists() {
            std::fs::create_dir_all(self.root.join("examples"))
                .unwrap_or_else(|e| panic!("creating examples dir: {e}"));
            std::os::unix::fs::symlink(repo_root().join("examples/repos"), &repos_link)
                .unwrap_or_else(|e| panic!("symlinking repos: {e}"));
        }
        self.ws_name = Some(name.to_owned());
        self.ws_dir = Some(dest.clone());
        if remap && !name.starts_with("broken/") {
            let yaml = std::fs::read_to_string(dest.join("stems.yaml")).unwrap_or_default();
            let base = allocate_port_block();
            let r = remap::remap_ports(&yaml, base, PORT_BLOCK)
                .unwrap_or_else(|e| panic!("port remapping of {name}: {e}"));
            self.port_base = Some(base);
            self.port_map = r.map;
            self.local = r.local;
            self.write_local();
        }
    }

    /// Uses a new empty directory `<root>/new-ws` as the workspace dir (for
    /// `stems init`): commands run there and `${ws}` / file steps refer to it.
    pub fn use_empty_dir(&mut self) {
        assert!(
            self.ws_dir.is_none(),
            "a workspace was already chosen for this scenario"
        );
        let dir = self.root.join("new-ws");
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
        self.ws_dir = Some(dir);
    }

    /// Replaces the `examples/repos` symlink with a private copy.
    pub fn private_repos(&mut self) {
        let _ = self.ws();
        let link = self.root.join("examples/repos");
        if link.is_symlink() {
            std::fs::remove_file(&link).unwrap_or_else(|e| panic!("removing repos link: {e}"));
        }
        copy_dir(&repo_root().join("examples/repos"), &link)
            .unwrap_or_else(|e| panic!("copying repos: {e}"));
    }

    /// Sets `dotted=value` in the generated `stems.local.yaml`.
    pub fn set_override(&mut self, dotted: &str, raw: &str) {
        let _ = self.ws();
        let raw = self.expand(raw);
        remap::set_dotted(&mut self.local, dotted, &raw).unwrap_or_else(|e| panic!("{e}"));
        self.write_local();
    }

    /// Deep-merges a YAML mapping (placeholders expanded) into the
    /// generated `stems.local.yaml` (33: mapping keys merge recursively,
    /// anything else replaces).
    pub fn merge_override(&mut self, yaml: &str) {
        let _ = self.ws();
        let text = self.expand(yaml);
        let add: Mapping = serde_yaml_ng::from_str(&text)
            .unwrap_or_else(|e| panic!("the override is not a YAML mapping: {e}\n{text}"));
        fn merge(into: &mut Mapping, add: Mapping) {
            for (k, v) in add {
                match (into.get_mut(&k), v) {
                    (Some(serde_yaml_ng::Value::Mapping(a)), serde_yaml_ng::Value::Mapping(b)) => {
                        merge(a, b)
                    }
                    (_, v) => {
                        into.insert(k, v);
                    }
                }
            }
        }
        merge(&mut self.local, add);
        self.write_local();
    }

    /// Removes `dotted` (and mappings left empty above it) from the
    /// generated `stems.local.yaml` (33).
    pub fn remove_override(&mut self, dotted: &str) {
        let _ = self.ws();
        fn remove(m: &mut Mapping, path: &[&str]) -> bool {
            let key = serde_yaml_ng::Value::String(path[0].to_owned());
            if path.len() == 1 {
                return m.remove(&key).is_some();
            }
            let Some(serde_yaml_ng::Value::Mapping(child)) = m.get_mut(&key) else {
                return false;
            };
            let removed = remove(child, &path[1..]);
            if child.is_empty() {
                m.remove(&key);
            }
            removed
        }
        let path: Vec<&str> = dotted.split('.').collect();
        assert!(
            remove(&mut self.local, &path),
            "the local override has no `{dotted}`"
        );
        self.write_local();
    }

    fn write_local(&self) {
        let path = self.ws().join("stems.local.yaml");
        if self.local.is_empty() {
            let _ = std::fs::remove_file(path);
        } else {
            std::fs::write(&path, remap::render_local(&self.local))
                .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
        }
    }

    /// Expands `${ws}`, `${tmp}`, `${home}`, `${repo}`, `${outside}`,
    /// `${port:N}` (the remapped value of declared port N) and `${var:name}`
    /// (a value saved by `When I save the JSON at ... as "<name>"`).
    pub fn expand(&self, s: &str) -> String {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            regex::Regex::new(r"\$\{(ws|tmp|home|repo|outside|port:(\d+)|var:([\w-]+))\}")
                .expect("valid regex")
        });
        re.replace_all(s, |c: &regex::Captures<'_>| match &c[1] {
            "ws" => self
                .ws_dir
                .as_ref()
                .map_or_else(|| c[0].to_owned(), |p| p.display().to_string()),
            "tmp" => self.root.display().to_string(),
            "home" => self.home.display().to_string(),
            "repo" => repo_root().display().to_string(),
            "outside" => self.root.join("outside").display().to_string(),
            v if v.starts_with("var:") => self
                .vars
                .get(&c[3])
                .cloned()
                .unwrap_or_else(|| panic!("no saved value named {:?}", &c[3])),
            _ => {
                let n: u16 = c[2].parse().unwrap_or_default();
                self.port_map.get(&n).copied().unwrap_or(n).to_string()
            }
        })
        .into_owned()
    }

    /// Default cwd: the workspace, or `<root>/outside` when there is none.
    pub fn default_cwd(&self) -> PathBuf {
        self.ws_dir
            .clone()
            .unwrap_or_else(|| self.root.join("outside"))
    }

    fn command(&self, argv: &[String], cwd: &Path, env: &[(String, String)]) -> Command {
        // `sh -c <line>` for `I run the shell command` (16), else the binary.
        let mut cmd = if argv[0] == "sh" {
            Command::new("/bin/sh")
        } else {
            Command::new(stems_bin())
        };
        cmd.args(&argv[1..]).current_dir(cwd);
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("STEMS_") {
                cmd.env_remove(k);
            }
        }
        cmd.env("STEMS_HOME", &self.home).env("STEMS_NO_COLOR", "1");
        // `Given a fake tool ... on PATH` (19) puts scripts in `<root>/bin`.
        let bin = self.root.join("bin");
        if bin.is_dir() {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs = vec![bin];
            dirs.extend(std::env::split_paths(&path));
            if let Ok(joined) = std::env::join_paths(dirs) {
                cmd.env("PATH", joined);
            }
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        cmd
    }

    async fn exec(
        &mut self,
        argv: Vec<String>,
        cwd: PathBuf,
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<CmdOutput, String> {
        let mut cmd = self.command(&argv, &cwd, env);
        let child = cmd.spawn().map_err(|e| {
            format!(
                "cannot run {}: {e} (build it with `cargo build -p stems-cli`)",
                stems_bin().display()
            )
        })?;
        let mut child = child;
        let started = Instant::now();
        let pgid = child.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        self.pgids.insert(pgid);
        // Read the pipes in the background: a process that leaves a
        // descendant holding stdout open (e.g. a daemon that forgot to
        // detach its stdio) must not hang the step until the budget runs
        // out. After the command exits, the pipes get a short grace period.
        let stdout = spawn_reader(child.stdout.take());
        let stderr = spawn_reader(child.stderr.take());
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(format!("waiting for `{}`: {e}", argv.join(" "))),
            Err(_) => {
                crate::procs::kill_group(pgid);
                return Err(format!(
                    "TIMEOUT: `{}` did not finish within {:.1}s (scenario budget)",
                    argv.join(" "),
                    timeout.as_secs_f64()
                ));
            }
        };
        let elapsed = started.elapsed();
        let stdout = collect_reader(stdout).await;
        let stderr = collect_reader(stderr).await;
        let code = status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
        Ok(CmdOutput {
            json: util::parse_json_output(&stdout),
            argv,
            cwd,
            code,
            stdout,
            stderr,
            elapsed,
        })
    }

    /// Starts `stems ...` in the background (own process group, recorded for
    /// the leak check); its output is collected while it runs.
    pub fn run_background(&mut self, line: &str, env: &[(String, String)]) {
        assert!(
            self.background.is_none(),
            "a background command is already running in this scenario"
        );
        let expanded = self.expand(line);
        let argv = util::split_args(&expanded).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            argv.first().is_some_and(|a| a == "stems"),
            "commands must start with `stems`, got {expanded:?}"
        );
        let cwd = self.default_cwd();
        let mut cmd = self.command(&argv, &cwd, env);
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", stems_bin().display()));
        let pgid = child.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        self.pgids.insert(pgid);
        let (stdout, _) = spawn_reader(child.stdout.take());
        let (stderr, _) = spawn_reader(child.stderr.take());
        self.background = Some(Background {
            argv,
            child,
            pgid,
            stdout,
            stderr,
            code: None,
        });
    }

    /// The background command, or a panic if none was started.
    pub fn background(&mut self) -> &mut Background {
        self.background
            .as_mut()
            .expect("no background command: use `When I run \"...\" in the background`")
    }

    /// Runs `stems ...` (the line must start with `stems`) within the
    /// scenario budget, records it as the last command and returns it.
    pub async fn run(
        &mut self,
        line: &str,
        cwd: Option<PathBuf>,
        env: &[(String, String)],
    ) -> CmdOutput {
        let expanded = self.expand(line);
        let argv = util::split_args(&expanded).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            argv.first().is_some_and(|a| a == "stems"),
            "commands must start with `stems`, got {expanded:?}"
        );
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.clone(), self.expand(v)))
            .collect();
        let cwd = cwd.unwrap_or_else(|| self.default_cwd());
        let timeout = self.remaining();
        let out = self
            .exec(argv, cwd, &env, timeout)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        if let Some(j) = &out.json {
            collect_pgids(j, &mut self.pgids);
        }
        self.last = Some(out.clone());
        out
    }

    /// `sh -c <line>` (placeholders expanded) with the same cwd/env rules as
    /// [`Self::run`]; becomes the last command. For checks stems has no
    /// command for (`docker exec … psql`, `curl`).
    pub async fn run_shell(&mut self, line: &str) -> CmdOutput {
        let argv = vec!["sh".to_owned(), "-c".to_owned(), self.expand(line)];
        let cwd = self.default_cwd();
        let timeout = self.remaining();
        let out = self
            .exec(argv, cwd, &[], timeout)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        self.last = Some(out.clone());
        out
    }

    /// Best-effort command for the After hook: bounded by `timeout`, never
    /// panics, does not touch `last`.
    pub async fn run_quiet(&mut self, line: &str, timeout: Duration) -> Option<CmdOutput> {
        let argv = util::split_args(line).ok()?;
        let cwd = self.default_cwd();
        self.exec(argv, cwd, &[], timeout).await.ok()
    }

    /// The last command, or a panic if none ran.
    pub fn last(&self) -> &CmdOutput {
        self.last
            .as_ref()
            .expect("no command has been run yet in this scenario")
    }

    /// The last command's JSON output, or a panic explaining why not.
    pub fn last_json(&self) -> &Value {
        let last = self.last();
        last.json.as_ref().unwrap_or_else(|| {
            panic!(
                "the last command did not print JSON on stdout\n{}",
                last.describe()
            )
        })
    }

    /// Runs `stems status --json` and returns its JSON.
    pub async fn status(&mut self) -> Value {
        let out = self.run("stems status --json", None, &[]).await;
        out.guard_implemented("10");
        out.json
            .clone()
            .unwrap_or_else(|| panic!("`stems status --json` printed no JSON\n{}", out.describe()))
    }

    /// Process groups recorded in any `state.json` under `STEMS_HOME`.
    pub fn state_pgids(&self) -> BTreeSet<i32> {
        let mut out = BTreeSet::new();
        for f in find_files(&self.home, &|n| n == "state.json") {
            if let Some(v) = std::fs::read_to_string(&f)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
            {
                collect_pgids(&v, &mut out);
            }
        }
        out
    }

    /// Takes the temp dir out of the world (the After hook removes it).
    pub fn take_tmp(&mut self) -> Option<TempDir> {
        self.tmp.take()
    }
}

/// Finds a stem in `stems status --json`: `data.stems` as a map keyed by
/// name or an array of objects with `name`; `data` itself may be the array.
pub fn find_stem<'a>(status: &'a Value, name: &str) -> Option<&'a Value> {
    let data = status.get("data").unwrap_or(status);
    let stems = data.get("stems").unwrap_or(data);
    match stems {
        Value::Object(m) => m.get(name),
        Value::Array(a) => a
            .iter()
            .find(|s| s.get("name").and_then(Value::as_str) == Some(name)),
        _ => None,
    }
}

/// A stem's status string (`state` or `status`).
pub fn stem_state(stem: &Value) -> Option<&str> {
    stem.get("state")
        .or_else(|| stem.get("status"))
        .and_then(Value::as_str)
}

/// A stem's first port: `port`, or the first entry of `ports` (array of
/// numbers/objects with `port`, or a map name -> number/object).
pub fn stem_port(stem: &Value) -> Option<u16> {
    fn num(v: &Value) -> Option<u16> {
        v.as_u64()
            .or_else(|| v.get("port").and_then(Value::as_u64))
            .and_then(|n| u16::try_from(n).ok())
    }
    if let Some(p) = stem.get("port").and_then(num) {
        return Some(p);
    }
    match stem.get("ports")? {
        Value::Array(a) => a.iter().find_map(num),
        Value::Object(m) => m.values().find_map(num),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_shapes() {
        let as_map = json!({"ok": true, "data": {"stems": {"api": {"state": "healthy", "ports": {"http": 20001}}}}});
        let as_arr = json!({"ok": true, "data": {"stems": [{"name": "api", "status": "running", "ports": [{"name": "http", "port": 20002}]}]}});
        let a = find_stem(&as_map, "api").unwrap();
        assert_eq!(stem_state(a), Some("healthy"));
        assert_eq!(stem_port(a), Some(20001));
        let b = find_stem(&as_arr, "api").unwrap();
        assert_eq!(stem_state(b), Some("running"));
        assert_eq!(stem_port(b), Some(20002));
        assert!(find_stem(&as_arr, "nope").is_none());
    }

    #[test]
    fn pgids_are_collected() {
        let mut out = BTreeSet::new();
        collect_pgids(
            &json!({"stems": {"a": {"pgid": 42}, "b": [{"pgid": 7}]}}),
            &mut out,
        );
        assert_eq!(out, BTreeSet::from([7, 42]));
    }

    #[test]
    fn workspace_copy_remaps_ports_and_expands_placeholders() {
        let mut w = E2eWorld::create().unwrap();
        w.use_workspace("minimal", true);
        let ws = w.ws().to_path_buf();
        assert!(ws.join("stems.yaml").is_file());
        assert!(
            w.root.join("examples/repos/shop-api/app.py").is_file(),
            "repos resolve via symlink"
        );
        let local = std::fs::read_to_string(ws.join("stems.local.yaml")).unwrap();
        let base = w.port_base.unwrap();
        assert!(local.contains(&base.to_string()), "{local}");
        assert_eq!(w.expand("--port ${port:18090}"), format!("--port {base}"));
        assert_eq!(w.expand("${ws}/x"), format!("{}/x", ws.display()));
        w.set_override("stems.echo-svc.env.SHOP_CHAOS", "\"0\"");
        let local = std::fs::read_to_string(ws.join("stems.local.yaml")).unwrap();
        assert!(local.contains("SHOP_CHAOS"), "{local}");
        w.private_repos();
        assert!(!w.root.join("examples/repos").is_symlink());
        assert!(w.root.join("examples/repos/shop-api/app.py").is_file());
    }

    #[test]
    fn broken_workspaces_are_copied_verbatim() {
        let mut w = E2eWorld::create().unwrap();
        w.use_workspace("broken/port-conflict", true);
        assert!(!w.ws().join("stems.local.yaml").exists());
        assert!(w.port_map.is_empty());
    }

    #[test]
    fn port_blocks_are_distinct() {
        let a = allocate_port_block();
        let b = allocate_port_block();
        assert_ne!(a, b);
        assert_eq!((a.max(b) - a.min(b)) % PORT_BLOCK, 0);
    }
}
