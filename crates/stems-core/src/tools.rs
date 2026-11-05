//! Detecting installed tool versions for `requires:` (validate, doctor).

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use stems_config::Requirement;

use crate::version::{Version, VersionReq, find_version};

/// How long a `<tool> --version` may take.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Result of probing one tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolVersion {
    /// Found, with the version and the output line it came from.
    Found {
        /// Parsed version.
        version: Version,
        /// The first non-empty output line (for messages).
        output: String,
    },
    /// The tool is not installed / not on PATH (or its command failed).
    Missing {
        /// Why (e.g. "not found on PATH").
        reason: String,
    },
    /// The command ran but no version could be read from its output.
    Unparseable {
        /// The first non-empty output line.
        output: String,
    },
    /// The requirement's `regex` does not compile.
    BadRegex {
        /// The regex error.
        reason: String,
    },
}

/// Something that knows installed tool versions (a seam for tests).
pub trait ToolVersions {
    /// The version of `tool`, per `req` (custom command/regex). `cwd` is the
    /// directory to probe in (the stem's codebase), so version-manager shims
    /// (asdf, mise, nvm) resolve the project's version.
    fn detect(&mut self, tool: &str, req: &Requirement, cwd: Option<&Path>) -> ToolVersion;
}

/// The command used to print `tool`'s version: the requirement's `command`,
/// else the built-in for known tools, else `<tool> --version`.
pub fn version_command(tool: &str, req: &Requirement) -> String {
    if let Some(c) = req.command() {
        return c.to_string();
    }
    match tool {
        "go" => "go version".into(),
        // node, docker, python3, pnpm, npm, yarn, cargo, git and anything else.
        _ => format!("{tool} --version"),
    }
}

type ProbeKey = (String, Option<String>, Option<std::path::PathBuf>);

/// Probes the real system, spawning each command at most once per instance
/// (create one per validate/doctor run).
#[derive(Debug, Default)]
pub struct SystemTools {
    cache: HashMap<ProbeKey, ToolVersion>,
}

impl SystemTools {
    /// New prober with an empty cache.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolVersions for SystemTools {
    fn detect(&mut self, tool: &str, req: &Requirement, cwd: Option<&Path>) -> ToolVersion {
        let command = version_command(tool, req);
        let key = (
            command.clone(),
            req.regex().map(str::to_string),
            cwd.map(Path::to_path_buf),
        );
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let result = probe(tool, &command, req.regex(), cwd);
        self.cache.insert(key, result.clone());
        result
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn probe(tool: &str, command: &str, regex: Option<&str>, cwd: Option<&Path>) -> ToolVersion {
    let re = match regex.map(Regex::new).transpose() {
        Ok(r) => r,
        Err(e) => {
            return ToolVersion::BadRegex {
                reason: e.to_string(),
            };
        }
    };
    let out = match run_with_timeout(command, VERSION_TIMEOUT, cwd, &[]) {
        Ok(o) => o,
        Err(f) => return ToolVersion::Missing { reason: f.message },
    };
    if out.status != Some(0) {
        let reason = match out.status {
            Some(127) => format!("`{tool}` was not found on PATH"),
            Some(c) => format!("`{command}` exited with status {c}"),
            None => format!("`{command}` was killed"),
        };
        return ToolVersion::Missing { reason };
    }
    let text = format!("{}\n{}", out.stdout, out.stderr);
    let line = first_line(&text);
    let version = match &re {
        Some(re) => re.captures(&text).and_then(|c| {
            let m = c.get(1).or_else(|| c.get(0))?;
            find_version(m.as_str())
        }),
        None => find_version(&text),
    };
    match version {
        Some(version) => ToolVersion::Found {
            version,
            output: line,
        },
        None => ToolVersion::Unparseable { output: line },
    }
}

struct Output {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Why [`run_with_timeout`] produced no output.
struct RunFailure {
    message: String,
    timed_out: bool,
}

impl RunFailure {
    fn other(message: String) -> Self {
        Self {
            message,
            timed_out: false,
        }
    }
}

/// Run `sh -c command` (with `env` added to the inherited environment),
/// killing it after `timeout`.
fn run_with_timeout(
    command: &str,
    timeout: Duration,
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> Result<Output, RunFailure> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    if let Some(dir) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| RunFailure::other(format!("could not run `{command}`: {e}")))?;
    let reader = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = p {
                // Bounded: version output is tiny, and the pipe closes when
                // the child exits or is killed.
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let t_out = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let t_err = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if Instant::now() >= deadline => {
                // The command runs in its own process group: kill all of it
                // so no grandchild keeps the pipes open. killpg, not the
                // `kill` binary: Linux's procps `kill` parses `-KILL -<pgid>`
                // differently from BSD's.
                if let Ok(pgid) = i32::try_from(child.id())
                    && pgid > 1
                {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(pgid),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunFailure {
                    message: format!("`{command}` did not finish within {}s", timeout.as_secs()),
                    timed_out: true,
                });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                return Err(RunFailure::other(format!(
                    "waiting for `{command}` failed: {e}"
                )));
            }
        }
    };
    Ok(Output {
        status,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// User-defined checks (`checks:`)
// ---------------------------------------------------------------------------

/// Lines of output kept in [`CheckRun::tail`].
const CHECK_TAIL_LINES: usize = 20;

/// What running a `checks:` command (or its `fix`) produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRun {
    /// Exit code; `None` when it timed out, was killed or never started.
    pub exit: Option<i32>,
    /// The command was killed because it ran past its timeout.
    pub timed_out: bool,
    /// Last lines of stdout + stderr (or why it could not run).
    pub tail: String,
}

impl CheckRun {
    /// Exit 0.
    pub fn passed(&self) -> bool {
        self.exit == Some(0)
    }
}

/// Run a `checks:` command through `sh -c` in its own process group (killed
/// as a whole after `timeout`), in `cwd`, with `env` over the inherited
/// environment.
pub fn run_check_command(
    command: &str,
    timeout: Duration,
    cwd: &Path,
    env: &[(String, String)],
) -> CheckRun {
    match run_with_timeout(command, timeout, Some(cwd), env) {
        Ok(o) => {
            let text = format!("{}{}", o.stdout, o.stderr);
            let lines: Vec<&str> = text.lines().collect();
            let from = lines.len().saturating_sub(CHECK_TAIL_LINES);
            CheckRun {
                exit: o.status,
                timed_out: false,
                tail: lines[from..].join("\n"),
            }
        }
        Err(f) => CheckRun {
            exit: None,
            timed_out: f.timed_out,
            tail: f.message,
        },
    }
}

// ---------------------------------------------------------------------------
// Ranges read from files (`requires: { node: { from: .nvmrc } }`)
// ---------------------------------------------------------------------------

/// A range derived from a `from:` file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Derived {
    /// The range text (`20.11.1`, `>=18`), verbatim from the file.
    pub range: String,
    /// The `from:` entry that supplied it (`.nvmrc`, `package.json#packageManager`).
    pub from: String,
}

/// Why no range could be derived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceError {
    /// Message (no trailing period).
    pub message: String,
    /// What to do about it.
    pub hint: Option<String>,
}

fn cannot(value: &str, file: &str) -> SourceError {
    SourceError {
        message: format!("cannot derive a version from `{value}` in {file}"),
        hint: Some(format!(
            "pin a concrete version in {file}, or set `version:` instead of `from:`"
        )),
    }
}

/// The tool's name in an asdf / mise `.tool-versions` file.
fn asdf_name(tool: &str) -> &str {
    match tool {
        "node" => "nodejs",
        "go" => "golang",
        "python3" => "python",
        other => other,
    }
}

/// Read the range for `tool` from the first existing entry of `from`
/// (files relative to `dir`, optionally `file#pointer`). Pure apart from
/// reading those files.
pub fn requirement_source(tool: &str, from: &[String], dir: &Path) -> Result<Derived, SourceError> {
    for entry in from {
        let (file, pointer) = match entry.split_once('#') {
            Some((f, p)) => (f, Some(p)),
            None => (entry.as_str(), None),
        };
        let path = dir.join(file);
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| SourceError {
            message: format!("cannot read {file}: {e}"),
            hint: None,
        })?;
        let raw = derive_one(tool, file, pointer, &text)?;
        let range = raw.trim().trim_start_matches('v').trim().to_string();
        if range.is_empty() || range.parse::<VersionReq>().is_err() {
            return Err(cannot(&raw, entry));
        }
        return Ok(Derived {
            range,
            from: entry.clone(),
        });
    }
    Err(SourceError {
        message: format!("none of {} exists under {}", from.join(", "), dir.display()),
        hint: Some("add one, or set `version:`".into()),
    })
}

fn derive_one(
    tool: &str,
    file: &str,
    pointer: Option<&str>,
    text: &str,
) -> Result<String, SourceError> {
    let base = file.rsplit('/').next().unwrap_or(file);
    let first_line = || {
        text.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("")
            .to_string()
    };
    if let Some(ptr) = pointer {
        let value = if base.ends_with(".json") {
            json_pointer(text, ptr)
        } else if base.ends_with(".toml") {
            toml_pointer(text, ptr)
        } else {
            return Err(SourceError {
                message: format!("`#{ptr}` can only be used with a .json or .toml file ({file})"),
                hint: None,
            });
        }
        .map_err(|m| SourceError {
            message: format!("cannot derive a version from {file}#{ptr}: {m}"),
            hint: None,
        })?;
        // `pnpm@9.1.0+sha256.abc` (corepack): drop `name@` and the hash.
        let value = match value.rfind('@') {
            Some(i) if i > 0 => value[i + 1..].to_string(),
            _ => value,
        };
        return Ok(value.split('+').next().unwrap_or("").to_string());
    }
    if base == ".tool-versions" {
        let want = [tool, asdf_name(tool)];
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut words = line.split_whitespace();
            if words.next().is_some_and(|n| want.contains(&n)) {
                let v = words.next().unwrap_or("");
                if v.is_empty() || v == "system" || v.starts_with("ref:") || v.starts_with("path:")
                {
                    return Err(cannot(v, file));
                }
                return Ok(v.to_string());
            }
        }
        return Err(SourceError {
            message: format!("no `{}` line in {file}", asdf_name(tool)),
            hint: Some(format!("add `{} <version>` to {file}", asdf_name(tool))),
        });
    }
    if base == "go.mod" {
        return text
            .lines()
            .map(str::trim)
            .find_map(|l| {
                l.strip_prefix("go ")
                    .map(|v| v.split_whitespace().next().unwrap_or("").to_string())
            })
            .ok_or_else(|| SourceError {
                message: format!("no `go` directive in {file}"),
                hint: None,
            });
    }
    Ok(first_line())
}

/// `a.b` or `/a/b` into a JSON document; a string, or a number as text.
fn json_pointer(text: &str, ptr: &str) -> Result<String, String> {
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let found = if ptr.starts_with('/') {
        doc.pointer(ptr)
    } else {
        ptr.split('.').try_fold(&doc, |v, k| v.get(k))
    };
    match found {
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(serde_json::Value::Number(n)) => Ok(n.to_string()),
        Some(_) => Err("the value is not a string".into()),
        None => Err("no such key".into()),
    }
}

/// `a.b` into a TOML document.
fn toml_pointer(text: &str, ptr: &str) -> Result<String, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let found = ptr
        .trim_start_matches('/')
        .split(['.', '/'])
        .try_fold(&doc, |v, k| v.get(k));
    match found {
        Some(toml::Value::String(s)) => Ok(s.clone()),
        Some(toml::Value::Integer(n)) => Ok(n.to_string()),
        Some(toml::Value::Float(n)) => Ok(n.to_string()),
        Some(_) => Err("the value is not a string".into()),
        None => Err("no such key".into()),
    }
}

#[cfg(test)]
mod tests {
    use stems_config::RequirementSpec;

    use super::*;

    fn custom(command: &str, regex: Option<&str>) -> Requirement {
        Requirement::Custom(RequirementSpec {
            version: Some(">=1".into()),
            from: None,
            command: Some(command.into()),
            regex: regex.map(str::to_string),
        })
    }

    #[test]
    fn known_tool_commands() {
        let r = Requirement::Version(">=1".into());
        assert_eq!(version_command("go", &r), "go version");
        assert_eq!(version_command("node", &r), "node --version");
        assert_eq!(version_command("mytool", &r), "mytool --version");
        assert_eq!(version_command("x", &custom("x -V", None)), "x -V");
    }

    #[test]
    fn probes_custom_commands() {
        let mut t = SystemTools::new();
        assert_eq!(
            t.detect("fake", &custom("echo 'fake tool v1.4.2'", None), None),
            ToolVersion::Found {
                version: Version::new(1, 4, 2),
                output: "fake tool v1.4.2".into()
            }
        );
        // The regex picks the second number.
        match t.detect(
            "fake",
            &custom("echo 'build 7 release 3.2'", Some(r"release (\S+)")),
            None,
        ) {
            ToolVersion::Found { version, .. } => assert_eq!(version, Version::new(3, 2, 0)),
            other => panic!("{other:?}"),
        }
        // Version on stderr (old pythons).
        match t.detect("fake", &custom("echo 'Python 2.7.18' >&2", None), None) {
            ToolVersion::Found { version, .. } => assert_eq!(version, Version::new(2, 7, 18)),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.detect("fake", &custom("echo nothing", None), None),
            ToolVersion::Unparseable { .. }
        ));
        assert!(matches!(
            t.detect("fake", &custom("echo 1", Some("(")), None),
            ToolVersion::BadRegex { .. }
        ));
    }

    #[test]
    fn missing_tools_and_failures() {
        let mut t = SystemTools::new();
        match t.detect(
            "stems-no-such-tool",
            &Requirement::Version(">=1".into()),
            None,
        ) {
            ToolVersion::Missing { reason } => assert!(reason.contains("not found on PATH")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.detect("fake", &custom("exit 3", None), None),
            ToolVersion::Missing { .. }
        ));
    }

    #[test]
    fn slow_commands_time_out() {
        let start = Instant::now();
        let r = probe("slow", "sleep 5", None, None);
        assert!(matches!(r, ToolVersion::Missing { ref reason } if reason.contains("within 2s")));
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn results_are_cached_per_instance() {
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("n");
        let cmd = format!("echo x >> '{}'; echo 1.0.0", counter.display());
        let mut t = SystemTools::new();
        let req = custom(&cmd, None);
        t.detect("c", &req, None);
        t.detect("c", &req, None);
        let n = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(n, 1);
    }

    fn src(files: &[(&str, &str)], tool: &str, from: &[&str]) -> Result<Derived, SourceError> {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        let from: Vec<String> = from.iter().map(|s| s.to_string()).collect();
        requirement_source(tool, &from, dir.path())
    }

    fn range(files: &[(&str, &str)], tool: &str, from: &str) -> String {
        src(files, tool, &[from]).unwrap().range
    }

    #[test]
    fn version_files() {
        assert_eq!(
            range(&[(".nvmrc", "v20.11.1\n")], "node", ".nvmrc"),
            "20.11.1"
        );
        assert_eq!(range(&[(".nvmrc", "20\n")], "node", ".nvmrc"), "20");
        assert_eq!(
            range(
                &[(".python-version", "3.12.1\n3.11\n")],
                "python3",
                ".python-version"
            ),
            "3.12.1"
        );
        assert_eq!(
            range(&[(".ruby-version", ">=3.2")], "ruby", ".ruby-version"),
            ">=3.2"
        );
        for bad in ["lts/*", "lts/iron", "system", "latest"] {
            let e = src(&[(".nvmrc", bad)], "node", &[".nvmrc"]).unwrap_err();
            assert_eq!(
                e.message,
                format!("cannot derive a version from `{bad}` in .nvmrc")
            );
        }
    }

    #[test]
    fn tool_versions_with_asdf_aliases() {
        let f = [(
            ".tool-versions",
            "nodejs 20.11.1 18.0.0\n# c\npython 3.12.1 # x\ngolang 1.22.1\npnpm 9.1.0\n",
        )];
        assert_eq!(range(&f, "node", ".tool-versions"), "20.11.1");
        assert_eq!(range(&f, "python3", ".tool-versions"), "3.12.1");
        assert_eq!(range(&f, "go", ".tool-versions"), "1.22.1");
        assert_eq!(range(&f, "pnpm", ".tool-versions"), "9.1.0");
        let e = src(&f, "ruby", &[".tool-versions"]).unwrap_err();
        assert!(e.message.contains("no `ruby` line"));
        for bad in ["system", "ref:main", "path:/opt/node"] {
            let body = format!("nodejs {bad}\n");
            let e = src(&[(".tool-versions", &body)], "node", &[".tool-versions"]).unwrap_err();
            assert!(e.message.contains("cannot derive"), "{}", e.message);
        }
    }

    #[test]
    fn json_and_toml_pointers() {
        let pj = [(
            "package.json",
            r#"{"packageManager":"pnpm@9.1.0+sha256.abc","engines":{"node":">=18"}}"#,
        )];
        assert_eq!(range(&pj, "pnpm", "package.json#packageManager"), "9.1.0");
        assert_eq!(range(&pj, "node", "package.json#engines.node"), ">=18");
        assert_eq!(range(&pj, "node", "package.json#/engines/node"), ">=18");
        let e = src(&pj, "node", &["package.json#engines.npm"]).unwrap_err();
        assert!(e.message.contains("no such key"), "{}", e.message);
        let tt = [("rust-toolchain.toml", "[toolchain]\nchannel = \"1.96.1\"\n")];
        assert_eq!(
            range(&tt, "cargo", "rust-toolchain.toml#toolchain.channel"),
            "1.96.1"
        );
    }

    #[test]
    fn go_mod_and_other_files() {
        let gm = [("go.mod", "module x\n\ngo 1.22.1\n\nrequire y v1\n")];
        assert_eq!(range(&gm, "go", "go.mod"), "1.22.1");
        assert_eq!(range(&[("VERSION", "  4.5 \n")], "x", "VERSION"), "4.5");
    }

    #[test]
    fn from_list_uses_the_first_existing_file() {
        let f = [(".tool-versions", "python 3.12.1\n")];
        let d = src(&f, "python3", &[".python-version", ".tool-versions"]).unwrap();
        assert_eq!(d.from, ".tool-versions");
        let e = src(&[], "python3", &[".python-version", ".tool-versions"]).unwrap_err();
        assert!(
            e.message
                .starts_with("none of .python-version, .tool-versions exists under")
        );
        assert_eq!(e.hint.as_deref(), Some("add one, or set `version:`"));
    }
}
