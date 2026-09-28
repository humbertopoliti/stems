//! Detecting installed tool versions for `requires:` (validate, doctor).

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regex::Regex;
use stems_config::Requirement;

use crate::version::{Version, find_version};

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
    /// The version of `tool`, per `req` (custom command/regex).
    fn detect(&mut self, tool: &str, req: &Requirement) -> ToolVersion;
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

/// Probes the real system, spawning each command at most once per instance
/// (create one per validate/doctor run).
#[derive(Debug, Default)]
pub struct SystemTools {
    cache: HashMap<(String, Option<String>), ToolVersion>,
}

impl SystemTools {
    /// New prober with an empty cache.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ToolVersions for SystemTools {
    fn detect(&mut self, tool: &str, req: &Requirement) -> ToolVersion {
        let command = version_command(tool, req);
        let key = (command.clone(), req.regex().map(str::to_string));
        if let Some(hit) = self.cache.get(&key) {
            return hit.clone();
        }
        let result = probe(tool, &command, req.regex());
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

fn probe(tool: &str, command: &str, regex: Option<&str>) -> ToolVersion {
    let re = match regex.map(Regex::new).transpose() {
        Ok(r) => r,
        Err(e) => {
            return ToolVersion::BadRegex {
                reason: e.to_string(),
            };
        }
    };
    let out = match run_with_timeout(command, VERSION_TIMEOUT) {
        Ok(o) => o,
        Err(reason) => return ToolVersion::Missing { reason },
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

/// Run `sh -c command`, killing it after `timeout`.
fn run_with_timeout(command: &str, timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not run `{command}`: {e}"))?;
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
                return Err(format!(
                    "`{command}` did not finish within {}s",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("waiting for `{command}` failed: {e}")),
        }
    };
    Ok(Output {
        status,
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use stems_config::RequirementSpec;

    use super::*;

    fn custom(command: &str, regex: Option<&str>) -> Requirement {
        Requirement::Custom(RequirementSpec {
            version: ">=1".into(),
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
            t.detect("fake", &custom("echo 'fake tool v1.4.2'", None)),
            ToolVersion::Found {
                version: Version::new(1, 4, 2),
                output: "fake tool v1.4.2".into()
            }
        );
        // The regex picks the second number.
        match t.detect(
            "fake",
            &custom("echo 'build 7 release 3.2'", Some(r"release (\S+)")),
        ) {
            ToolVersion::Found { version, .. } => assert_eq!(version, Version::new(3, 2, 0)),
            other => panic!("{other:?}"),
        }
        // Version on stderr (old pythons).
        match t.detect("fake", &custom("echo 'Python 2.7.18' >&2", None)) {
            ToolVersion::Found { version, .. } => assert_eq!(version, Version::new(2, 7, 18)),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.detect("fake", &custom("echo nothing", None)),
            ToolVersion::Unparseable { .. }
        ));
        assert!(matches!(
            t.detect("fake", &custom("echo 1", Some("("))),
            ToolVersion::BadRegex { .. }
        ));
    }

    #[test]
    fn missing_tools_and_failures() {
        let mut t = SystemTools::new();
        match t.detect("stems-no-such-tool", &Requirement::Version(">=1".into())) {
            ToolVersion::Missing { reason } => assert!(reason.contains("not found on PATH")),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            t.detect("fake", &custom("exit 3", None)),
            ToolVersion::Missing { .. }
        ));
    }

    #[test]
    fn slow_commands_time_out() {
        let start = Instant::now();
        let r = probe("slow", "sleep 5", None);
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
        t.detect("c", &req);
        t.detect("c", &req);
        let n = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(n, 1);
    }
}
