//! `stems upgrade [--dry-run]` (FR-DS-2): upgrade stems the way it was
//! installed.
//!
//! The install method is detected from the running binary's path:
//! - **brew**: under a Homebrew `Cellar/` (or `/opt/homebrew`,
//!   `/home/linuxbrew/.linuxbrew`) → runs `brew upgrade stems`;
//! - **cargo**: under `$CARGO_HOME/bin` (default `~/.cargo/bin`) → prints the
//!   `cargo install` command;
//! - **tarball** (everything else: `installer.sh`, an unpacked release
//!   tarball, a source build) → prints the `installer.sh` one-liner.
//!
//! Only brew is run for you (it owns the files); the other two print the
//! command, because re-running an installer over your PATH is your call.
//! `STEMS_FAKE_INSTALL_METHOD=brew|tarball|cargo` overrides detection (tests).
//! `--dry-run` never runs anything. `data: { install_method, command, ran }`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;
use stems_core::{Error, ErrorCode};

use crate::cli::UpgradeArgs;
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// Env var overriding install-method detection (tests only).
pub const ENV_FAKE_INSTALL_METHOD: &str = "STEMS_FAKE_INSTALL_METHOD";

/// The repository releases are published from (`repository` in Cargo.toml).
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How this binary was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallMethod {
    /// Homebrew (`brew install <org>/tap/stems`).
    Brew,
    /// A release tarball or `installer.sh` (also the fallback).
    Tarball,
    /// `cargo install`.
    Cargo,
}

impl InstallMethod {
    /// Stable name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            InstallMethod::Brew => "brew",
            InstallMethod::Tarball => "tarball",
            InstallMethod::Cargo => "cargo",
        }
    }

    /// Parse a stable name.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "brew" => Some(InstallMethod::Brew),
            "tarball" => Some(InstallMethod::Tarball),
            "cargo" => Some(InstallMethod::Cargo),
            _ => None,
        }
    }

    /// The command that upgrades an install of this kind.
    pub fn command(self) -> String {
        let repo = REPOSITORY.trim_end_matches('/');
        match self {
            InstallMethod::Brew => "brew upgrade stems".to_string(),
            InstallMethod::Tarball => format!(
                "curl --proto '=https' --tlsv1.2 -LsSf {repo}/releases/latest/download/stems-cli-installer.sh | sh"
            ),
            InstallMethod::Cargo => format!("cargo install --locked --git {repo} stems-cli"),
        }
    }
}

/// Classify an executable path (already canonicalised). `cargo_bin` is
/// `$CARGO_HOME/bin` (or `~/.cargo/bin`) when known.
pub fn classify(exe: &Path, cargo_bin: Option<&Path>) -> InstallMethod {
    let s = exe.to_string_lossy();
    if s.contains("/Cellar/")
        || s.starts_with("/opt/homebrew/")
        || s.starts_with("/home/linuxbrew/.linuxbrew/")
    {
        return InstallMethod::Brew;
    }
    if let Some(bin) = cargo_bin
        && exe.parent() == Some(bin)
    {
        return InstallMethod::Cargo;
    }
    InstallMethod::Tarball
}

/// `$CARGO_HOME/bin`, else `~/.cargo/bin`.
fn cargo_bin(ctx: &Ctx) -> Option<PathBuf> {
    let home = ctx
        .env
        .get("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            ctx.env
                .get("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| Path::new(h).join(".cargo"))
        })?;
    let bin = home.join("bin");
    Some(std::fs::canonicalize(&bin).unwrap_or(bin))
}

/// The install method of the running binary (honouring
/// `STEMS_FAKE_INSTALL_METHOD`).
pub fn detect(ctx: &Ctx) -> InstallMethod {
    if let Some(m) = ctx
        .env
        .get(ENV_FAKE_INSTALL_METHOD)
        .and_then(|v| InstallMethod::parse(v))
    {
        return m;
    }
    let Ok(exe) = std::env::current_exe() else {
        return InstallMethod::Tarball;
    };
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    classify(&exe, cargo_bin(ctx).as_deref())
}

/// Run `upgrade`.
pub fn run(ctx: &Ctx, args: &UpgradeArgs) -> CommandOutput {
    let method = detect(ctx);
    let command = method.command();
    let runs = method == InstallMethod::Brew && !args.dry_run;
    let data = |ran: bool| {
        json!({
            "install_method": method.as_str(),
            "command": command,
            "ran": ran,
            "dry_run": args.dry_run,
        })
    };
    if !runs {
        let human = if args.dry_run {
            format!(
                "installed with {}; would run:\n  {command}\n",
                method.as_str()
            )
        } else {
            format!(
                "installed with {}; to upgrade, run:\n  {command}\n",
                method.as_str()
            )
        };
        return CommandOutput::data(data(false)).with_human(human);
    }
    // brew's own output goes to stderr so stdout stays the envelope.
    let status = Command::new("brew")
        .args(["upgrade", "stems"])
        .stdin(Stdio::null())
        .stdout(stderr_stdio())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) if s.success() => CommandOutput::data(data(true))
            .with_human("stems upgraded (restart running daemons: `stems daemon stop`)\n"),
        Ok(s) => CommandOutput::data(data(true)).with_errors(
            Error::new(
                ErrorCode::UpgradeFailed,
                format!("`{command}` exited with {s}"),
            )
            .with_hint(format!("run `{command}` yourself to see why"))
            .with_details(json!({ "install_method": method.as_str(), "command": command })),
        ),
        Err(e) => CommandOutput::data(data(false)).with_errors(
            Error::new(
                ErrorCode::UpgradeFailed,
                format!("could not run `{command}`: {e}"),
            )
            .with_hint("is `brew` on PATH? run the command yourself")
            .with_details(json!({ "install_method": method.as_str(), "command": command })),
        ),
    }
}

/// A `Stdio` writing to this process's stderr.
fn stderr_stdio() -> Stdio {
    use std::os::fd::AsFd;
    std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::inherit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_cellar_paths_are_brew() {
        for p in [
            "/usr/local/Cellar/stems/0.1.0/bin/stems",
            "/opt/homebrew/Cellar/stems/0.1.0/bin/stems",
            "/opt/homebrew/bin/stems",
            "/home/linuxbrew/.linuxbrew/Cellar/stems/0.1.0/bin/stems",
        ] {
            assert_eq!(classify(Path::new(p), None), InstallMethod::Brew, "{p}");
        }
    }

    #[test]
    fn cargo_bin_is_cargo_and_the_rest_is_tarball() {
        let bin = Path::new("/Users/me/.cargo/bin");
        assert_eq!(
            classify(Path::new("/Users/me/.cargo/bin/stems"), Some(bin)),
            InstallMethod::Cargo
        );
        assert_eq!(
            classify(Path::new("/Users/me/.local/bin/stems"), Some(bin)),
            InstallMethod::Tarball
        );
        assert_eq!(
            classify(Path::new("/tmp/stems-x86_64-apple-darwin/stems"), None),
            InstallMethod::Tarball
        );
    }

    #[test]
    fn commands() {
        assert_eq!(InstallMethod::Brew.command(), "brew upgrade stems");
        let t = InstallMethod::Tarball.command();
        assert!(
            t.contains("/releases/latest/download/stems-cli-installer.sh"),
            "{t}"
        );
        assert!(t.starts_with("curl "), "{t}");
        assert!(InstallMethod::Cargo.command().starts_with("cargo install"));
        assert_eq!(InstallMethod::parse("brew"), Some(InstallMethod::Brew));
        assert_eq!(InstallMethod::parse("nope"), None);
    }
}
