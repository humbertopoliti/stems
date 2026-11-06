//! `stems exec <stem> [--host] [--cwd <dir>] [--fresh] -- <cmd…>` and
//! `stems shell <stem> [--host]` (deliverable 37, FR-LC-7/8,
//! `docs/exec.md`).
//!
//! The CLI runs the command itself, so it gets the user's terminal, stdin
//! and the command's exit code; the daemon only supplies the stem's context
//! (`stem_env`: what stems sets for it, its cwd, its container). Without a
//! daemon the context is computed from the config ([`context`]); a daemon
//! is never started.
//!
//! * Process/external stems (and `--host`): `<cmd…>` with a clean
//!   environment = the caller's minus `STEMS_*` (`--fresh`: only the
//!   minimal allowlist of it) + the stem's env, cwd = `--cwd`, else the
//!   stem's (codebase, else state dir). Stdio inherited.
//! * Docker/compose stems: `docker exec -i [-t] [-w <dir>] <container>
//!   <cmd…>`; not running → `STEM_NOT_RUNNING`.
//!
//! The exit code is the command's (128 + signal when it was killed).
//! Human output is the command's own; `--json` prints `{stem, container,
//! cwd, argv, exit}` after it.

use std::collections::{BTreeMap, HashMap};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;
use stems_api::{Method, StemEnvParams, StemEnvResult};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::supervisor::env::minimal_base;

use crate::cli::{ExecArgs, ShellArgs};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// The caller's environment for a request (37): this process's, minus
/// stems-injected `STEMS_*` (the CLI's own `STEMS_HOME` etc. pass through).
pub fn caller_env(env: &HashMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(k, _)| !stems_core::callerenv::is_stems_injected(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// `stem`'s context: from the daemon when it runs (`stem_env`), else from
/// the config with no port allocated and no output evaluated.
pub fn context(ctx: &Ctx, stem: &str, reveal: bool) -> Result<StemEnvResult, Errors> {
    let t = client::target(ctx)?;
    let params = StemEnvParams {
        stem: stem.to_string(),
        reveal,
    };
    let remote = block_on(async {
        match client::connect_to(&t, client::options(ctx)).await {
            Ok(c) => Some(c.call(Method::STEM_ENV, &params).await),
            Err(e) if e.code == ErrorCode::DaemonNotRunning => None,
            Err(e) => Some(Err(e)),
        }
    });
    match remote {
        Some(r) => r.map_err(Errors::from),
        None => local_context(ctx, &t.paths.dir, stem),
    }
}

fn local_context(
    ctx: &Ctx,
    data_dir: &std::path::Path,
    stem: &str,
) -> Result<StemEnvResult, Errors> {
    let ws = stems_config::load(ctx.load_options()).map_err(Errors::from)?;
    stems_daemon::supervisor::schedule::plan(&ws, &[stem.to_string()], true)
        .map_err(Errors::from)?;
    let st = ws
        .workspace
        .stem(stem)
        .ok_or_else(|| Errors::from(Error::internal(format!("stem `{stem}` vanished"))))?;
    let state_dir = data_dir.join("stems").join(stem);
    // Like the daemon's script runner: a stem without a codebase runs in it.
    let _ = std::fs::create_dir_all(&state_dir);
    let (env, unresolved) = stems_daemon::supervisor::run::exec_env(
        &ws.workspace,
        st,
        &stems_daemon::supervisor::PortBook::default().frozen(),
        None,
        None,
        &state_dir,
    )
    .map_err(Errors::from)?;
    Ok(StemEnvResult {
        stem: st.name.clone(),
        kind: st.kind().to_string(),
        cwd: stems_daemon::supervisor::run::exec_cwd(st, &state_dir),
        env,
        container: None,
        state: stems_core::StemState::Stopped,
        running: false,
        unresolved,
        warnings: Vec::new(),
    })
}

/// The one-line warning for unresolved variables (human mode, stderr).
pub fn unresolved_warning(r: &StemEnvResult) -> Option<String> {
    (!r.unresolved.is_empty()).then(|| {
        format!(
            "warning: {} unresolved: an unallocated port or an unevaluated output (`{}` is not running); port variables are not set, references are kept as `${{stem...}}`\n",
            r.unresolved.join(", "),
            r.stem
        )
    })
}

/// Container stems run their command in the container.
fn is_container(kind: &str) -> bool {
    matches!(kind, "docker" | "compose")
}

/// `docker exec` arguments (after the program): `-i` always, `-t` when
/// the terminal is one, `-w <dir>` for `--cwd`.
pub fn docker_exec_argv(
    container: &str,
    cwd: Option<&str>,
    tty: bool,
    command: &[String],
) -> Vec<String> {
    let mut argv = vec!["exec".to_string(), "-i".to_string()];
    if tty {
        argv.push("-t".into());
    }
    if let Some(d) = cwd {
        argv.push("-w".into());
        argv.push(d.to_string());
    }
    argv.push(container.to_string());
    argv.extend(command.iter().cloned());
    argv
}

/// The environment of a host command: the caller's (minus `STEMS_*`;
/// `fresh`: the minimal allowlist), then the stem's.
pub fn host_env(
    caller: &HashMap<String, String>,
    stem_env: &BTreeMap<String, String>,
    fresh: bool,
) -> BTreeMap<String, String> {
    let base = caller_env(caller);
    let mut env = if fresh { minimal_base(&base) } else { base };
    env.extend(stem_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    env
}

/// What `exec` runs.
struct Plan {
    program: String,
    args: Vec<String>,
    env: Option<BTreeMap<String, String>>,
    cwd: Option<PathBuf>,
    container: Option<String>,
}

fn not_running(stem: &str) -> Error {
    Error::new(
        ErrorCode::StemNotRunning,
        format!("`{stem}` is not running: there is no container to exec into"),
    )
    .with_hint(format!(
        "start it with `stems start {stem}`, or run on the host with `--host`"
    ))
    .with_details(json!({ "stem": stem }))
}

fn plan(
    ctx: &Ctx,
    r: &StemEnvResult,
    host: bool,
    cwd: Option<&PathBuf>,
    fresh: bool,
    host_cmd: Vec<String>,
    container_cmd: Vec<String>,
) -> Result<Plan, Error> {
    if is_container(&r.kind) && !host {
        let Some(c) = r.container.clone().filter(|_| r.running) else {
            return Err(not_running(&r.stem));
        };
        let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let dir = cwd.map(|d| d.display().to_string());
        return Ok(Plan {
            program: stems_runtime::docker::docker_program()
                .to_string_lossy()
                .into_owned(),
            args: docker_exec_argv(&c, dir.as_deref(), tty, &container_cmd),
            env: None,
            cwd: None,
            container: Some(c),
        });
    }
    let mut it = host_cmd.into_iter();
    let program = it.next().unwrap_or_default();
    Ok(Plan {
        program,
        args: it.collect(),
        env: Some(host_env(&ctx.env, &r.env, fresh)),
        cwd: Some(cwd.map_or_else(|| r.cwd.clone(), |d| ctx.cwd.join(d))),
        container: None,
    })
}

/// Run `p` in the foreground; the CLI ignores SIGINT/SIGQUIT meanwhile
/// (the terminal sends them to the command too) so it can report the exit.
fn run_plan(p: &Plan) -> Result<i32, Error> {
    let mut cmd = Command::new(&p.program);
    cmd.args(&p.args);
    if let Some(env) = &p.env {
        cmd.env_clear().envs(env);
    }
    if let Some(d) = &p.cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn().map_err(|e| {
        Error::new(
            ErrorCode::StartFailed,
            format!("cannot run `{}`: {e}", p.program),
        )
        .with_hint("check the command (it must be on PATH) and `--cwd`")
        .with_details(json!({ "program": p.program, "cwd": p.cwd }))
    })?;
    let ignore = |s| unsafe { libc::signal(s, libc::SIG_IGN) };
    let (old_int, old_quit) = (ignore(libc::SIGINT), ignore(libc::SIGQUIT));
    let status = child.wait();
    unsafe {
        libc::signal(libc::SIGINT, old_int);
        libc::signal(libc::SIGQUIT, old_quit);
    }
    let status = status.map_err(|e| Error::internal(format!("waiting for the command: {e}")))?;
    use std::os::unix::process::ExitStatusExt;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

fn execute(
    ctx: &Ctx,
    stem: &str,
    host: bool,
    cwd: Option<&PathBuf>,
    fresh: bool,
    host_cmd: Vec<String>,
    container_cmd: Vec<String>,
) -> CommandOutput {
    // The command gets the real values: its environment is never printed
    // (only `stems env` redacts secret outputs).
    let r = match context(ctx, stem, true) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let p = match plan(ctx, &r, host, cwd, fresh, host_cmd, container_cmd) {
        Ok(p) => p,
        Err(e) => return CommandOutput::failed(e),
    };
    if p.container.is_none()
        && !ctx.global.json
        && let Some(w) = unresolved_warning(&r)
    {
        eprint!("{w}");
    }
    let code = match run_plan(&p) {
        Ok(c) => c,
        Err(e) => return CommandOutput::failed(e).with_exit(127),
    };
    let mut argv = vec![p.program.clone()];
    argv.extend(p.args.iter().cloned());
    CommandOutput::data(json!({
        "stem": r.stem,
        "container": p.container,
        "cwd": p.cwd,
        "argv": argv,
        "exit": code,
    }))
    .with_raw("")
    .with_exit(code)
}

/// `stems exec`.
pub fn exec(ctx: &Ctx, args: &ExecArgs) -> CommandOutput {
    execute(
        ctx,
        &args.stem,
        args.host,
        args.cwd.as_ref(),
        args.fresh,
        args.command.clone(),
        args.command.clone(),
    )
}

/// `stems shell`: the caller's `SHELL -i` (else `/bin/sh -i`); in a
/// container `bash`, else `sh`.
pub fn shell(ctx: &Ctx, args: &ShellArgs) -> CommandOutput {
    let sh = ctx
        .env
        .get("SHELL")
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "/bin/sh".into());
    let in_container = [
        "sh".to_string(),
        "-c".to_string(),
        "command -v bash >/dev/null 2>&1 && exec bash || exec sh".to_string(),
    ];
    execute(
        ctx,
        &args.stem,
        args.host,
        None,
        false,
        vec![sh, "-i".into()],
        in_container.to_vec(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_warning_golden() {
        let r = StemEnvResult {
            stem: "worker".into(),
            kind: "process".into(),
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            container: None,
            state: stems_core::StemState::Stopped,
            running: false,
            unresolved: vec!["PORT".into(), "PORT_HTTP".into(), "URL".into()],
            warnings: Vec::new(),
        };
        insta::assert_snapshot!(unresolved_warning(&r).unwrap(), @"warning: PORT, PORT_HTTP, URL unresolved: an unallocated port or an unevaluated output (`worker` is not running); port variables are not set, references are kept as `${stem...}`");
    }

    #[test]
    fn docker_exec_args() {
        let cmd = vec!["redis-cli".to_string(), "ping".to_string()];
        assert_eq!(
            docker_exec_argv("abc", None, false, &cmd),
            ["exec", "-i", "abc", "redis-cli", "ping"]
        );
        assert_eq!(
            docker_exec_argv("abc", Some("/app"), true, &cmd),
            ["exec", "-i", "-t", "-w", "/app", "abc", "redis-cli", "ping"]
        );
    }

    #[test]
    fn host_env_layers() {
        let caller: HashMap<String, String> = [
            ("FOO", "mine"),
            ("PATH", "/bin"),
            ("STEMS_HOME", "/h"),
            ("STEMS_STEM", "leaked"),
            ("PORT", "1"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .into();
        let stem: BTreeMap<String, String> = [("PORT", "8080"), ("STEMS_STEM", "api")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into();
        let env = host_env(&caller, &stem, false);
        assert_eq!(env["FOO"], "mine");
        assert_eq!(env["PORT"], "8080");
        assert_eq!(env["STEMS_STEM"], "api");
        // The CLI's own configuration passes through (a nested `stems`).
        assert_eq!(env["STEMS_HOME"], "/h");
        let fresh = host_env(&caller, &stem, true);
        assert!(!fresh.contains_key("FOO"));
        assert_eq!(fresh["PATH"], "/bin");
        assert_eq!(fresh["PORT"], "8080");
    }
}
