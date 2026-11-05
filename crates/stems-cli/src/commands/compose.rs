//! `stems compose <stem> [--service] -- <args…>` (deliverable 43, FR-ST-3,
//! `docs/compose.md`): run `docker compose` in a compose stem's context.
//!
//! Like `exec` the CLI runs the command itself (terminal, stdin, exit
//! code); the daemon is never started. The context is [`exec::context`]
//! (live from the daemon, else computed from the config); the env file
//! (and the `inject_env` override) are written here when missing, or, with
//! no daemon, when they differ from what the config yields.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use serde_json::json;
use stems_core::{Error, ErrorCode};
use stems_runtime::compose::{
    ComposeTarget, compose_env, env_file_path, override_file_path, render_env_file,
};

use crate::cli::ComposeArgs;
use crate::commands::{Ctx, exec};
use crate::output::CommandOutput;

/// The arguments after the program: `compose --ansi auto [-f <file> [-f
/// <override>]] -p <project> [--env-file <f>] <args…> [<service>]`.
pub fn argv(t: &ComposeTarget, args: &[String], with_service: bool) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["compose".into(), "--ansi".into(), "auto".into()];
    if let Some(f) = &t.file {
        v.extend(["-f".into(), f.into()]);
        if let Some(o) = &t.override_file {
            v.extend(["-f".into(), o.into()]);
        }
    }
    v.extend(["-p".into(), t.project.clone().into()]);
    if let Some(e) = &t.env_file {
        v.extend(["--env-file".into(), e.into()]);
    }
    v.extend(args.iter().map(OsString::from));
    if with_service {
        v.push(t.service.clone().into());
    }
    v
}

fn write_if_needed(path: &Path, content: &str, daemon_up: bool) -> std::io::Result<()> {
    let current = std::fs::read_to_string(path).ok();
    let write = match current {
        None => true,
        Some(c) => !daemon_up && c != content,
    };
    if write {
        stems_runtime::compose::command::write_private(path, content)?;
    }
    Ok(())
}

fn fail(msg: String) -> Error {
    Error::new(ErrorCode::StartFailed, msg)
}

/// Write `spec`'s env file and, whenever `-f <override>` is passed
/// (`inject_env` or build secrets), its override file with the runtime's
/// exact content (secrets and build services included). With a daemon up,
/// its files are only created when absent, never replaced.
fn write_inputs(
    spec: &stems_runtime::compose::ComposeSpec,
    dir: &Path,
    daemon_up: bool,
) -> Result<(), stems_core::Errors> {
    let env_file = env_file_path(dir, &spec.stem);
    let content = render_env_file(&spec.env).map_err(fail)?;
    write_if_needed(&env_file, &content, daemon_up)
        .map_err(|e| fail(format!("cannot write {}: {e}", env_file.display())))?;
    if let Some(body) = spec.override_body(&env_file) {
        let o = override_file_path(dir, &spec.stem);
        write_if_needed(&o, &body, daemon_up)
            .map_err(|e| fail(format!("cannot write {}: {e}", o.display())))?;
    }
    Ok(())
}

fn prepare(
    ctx: &Ctx,
    args: &ComposeArgs,
) -> Result<(String, ComposeTarget, BTreeMap<String, String>), stems_core::Errors> {
    let r = exec::context(ctx, &args.stem, true)?;
    if r.kind != "compose" {
        return Err(Error::new(
            ErrorCode::Usage,
            format!(
                "`{}` is a {} stem, not a compose stem: `stems compose` needs `type: compose`",
                r.stem, r.kind
            ),
        )
        .with_hint("use `stems exec` for process, docker and external stems")
        .into());
    }
    let ws = stems_config::load(ctx.load_options()).map_err(stems_core::Errors::from)?;
    let st = ws
        .workspace
        .stem(&r.stem)
        .ok_or_else(|| Error::internal(format!("stem `{}` vanished", r.stem)))?;
    let spec = stems_daemon::supervisor::containers::compose_spec(&ws.workspace, st, "", &r.env)?;
    let target = client_paths(ctx)?;
    let t = ComposeTarget::for_spec(&spec, &target);
    // Without a daemon nothing keeps the files fresh; with one they are
    // its (possibly port-allocated) files and are only created if absent.
    let daemon_up = r.running || r.state != stems_core::StemState::Stopped;
    write_inputs(&spec, &target, daemon_up)?;
    let host = stems_runtime::docker::resolve_host(
        None,
        ctx.env.get("DOCKER_HOST").map(String::as_str),
        ctx.env.get("HOME").map(Path::new),
        |p| p.exists(),
    );
    let base = exec::host_env(&ctx.env, &spec.env, false);
    let env = compose_env(&base, Some(&host));
    Ok((r.stem, t, env))
}

fn client_paths(ctx: &Ctx) -> Result<std::path::PathBuf, stems_core::Errors> {
    Ok(crate::client::target(ctx)?.paths.dir.join("compose"))
}

/// `stems compose`.
pub fn run(ctx: &Ctx, args: &ComposeArgs) -> CommandOutput {
    let (stem, t, env) = match prepare(ctx, args) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let argv = argv(&t, &args.args, args.service);
    let program = stems_runtime::docker::docker_program();
    let mut cmd = Command::new(&program);
    cmd.args(&argv).env_clear().envs(&env);
    if let Some(d) = t.file.as_deref().and_then(Path::parent) {
        cmd.current_dir(d);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutput::failed(
                Error::new(
                    ErrorCode::DockerUnavailable,
                    format!("cannot run `{}`: {e}", program.display()),
                )
                .with_hint("install Docker with compose v2, or set STEMS_DOCKER_CLI"),
            )
            .with_exit(127);
        }
    };
    // The terminal sends ^C to compose too; report its exit code.
    let ignore = |s| unsafe { libc::signal(s, libc::SIG_IGN) };
    let (old_int, old_quit) = (ignore(libc::SIGINT), ignore(libc::SIGQUIT));
    let status = child.wait();
    unsafe {
        libc::signal(libc::SIGINT, old_int);
        libc::signal(libc::SIGQUIT, old_quit);
    }
    let status = match status {
        Ok(s) => s,
        Err(e) => {
            return CommandOutput::failed(Error::internal(format!("waiting for compose: {e}")));
        }
    };
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    let mut full = vec![program.to_string_lossy().into_owned()];
    full.extend(argv.iter().map(|a| a.to_string_lossy().into_owned()));
    CommandOutput::data(json!({ "stem": stem, "argv": full, "exit": code }))
        .with_raw("")
        .with_exit(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn target(override_file: bool) -> ComposeTarget {
        ComposeTarget {
            file: Some(PathBuf::from("/w/compose.yml")),
            project: "stems-w".into(),
            service: "redis".into(),
            env_file: Some(PathBuf::from("/e/cache.env")),
            override_file: override_file.then(|| PathBuf::from("/e/cache.override.yaml")),
        }
    }

    fn s(v: Vec<OsString>) -> Vec<String> {
        v.into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn override_written_for_build_secrets_like_the_runtime() {
        use stems_runtime::compose::ComposeSpec;
        use stems_runtime::docker::{BuildSecret, SecretSource};
        let d = tempfile::tempdir().unwrap();
        let mut spec = ComposeSpec::new("w", "api", "", "/w/compose.yml", "api");
        spec.bake.services = vec!["api".into(), "migrate".into()];
        spec.bake.secrets = vec![BuildSecret {
            id: "adc".into(),
            source: SecretSource::File(PathBuf::from("/h/adc.json")),
        }];
        // inject_env off: the override is still passed (`for_spec`), so it
        // must exist and declare the secrets.
        let t = ComposeTarget::for_spec(&spec, d.path());
        let o = t.override_file.clone().expect("override passed");
        write_inputs(&spec, d.path(), false).unwrap();
        let body = std::fs::read_to_string(&o).unwrap();
        assert_eq!(
            Some(body.clone()),
            spec.override_body(&d.path().join("api.env"))
        );
        insta::assert_snapshot!("compose_override_secrets", body);

        // A daemon's file is never replaced.
        std::fs::write(&o, "daemon's\n").unwrap();
        write_inputs(&spec, d.path(), true).unwrap();
        assert_eq!(std::fs::read_to_string(&o).unwrap(), "daemon's\n");

        // Plain stem: no override passed, none written.
        let plain = ComposeSpec::new("w", "db", "", "/w/compose.yml", "db");
        assert!(
            ComposeTarget::for_spec(&plain, d.path())
                .override_file
                .is_none()
        );
        write_inputs(&plain, d.path(), false).unwrap();
        assert!(!d.path().join("db.override.yaml").exists());
    }

    #[test]
    fn argv_golden() {
        let a = vec!["run".to_string(), "--rm".into()];
        assert_eq!(
            s(argv(&target(false), &a, false)),
            "compose --ansi auto -f /w/compose.yml -p stems-w --env-file /e/cache.env run --rm"
                .split(' ')
                .collect::<Vec<_>>()
        );
        assert_eq!(
            s(argv(&target(true), &a, true)),
            "compose --ansi auto -f /w/compose.yml -f /e/cache.override.yaml -p stems-w --env-file /e/cache.env run --rm redis"
                .split(' ')
                .collect::<Vec<_>>()
        );
    }
}
