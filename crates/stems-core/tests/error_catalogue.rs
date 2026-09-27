//! The error catalogue: a reviewed golden of the human rendering of every
//! [`ErrorCode`] with a representative message and hint, and a check that
//! REQUIREMENTS.md §7.5 lists every code.
//!
//! `sample` is an exhaustive match: adding a code does not compile until it
//! has a sample here, and then the golden diff must be reviewed.

use std::path::PathBuf;

use stems_core::{Error, ErrorCode, Span};

fn at(line: usize, col: usize) -> Option<Span> {
    Some(Span {
        file: PathBuf::from("stems.yaml"),
        line,
        col,
    })
}

fn cfg(code: ErrorCode, path: &str, line: usize, message: &str, hint: &str) -> Error {
    Error::new(code, message)
        .with_path(path.parse().unwrap())
        .with_span(at(line, 5))
        .with_hint(hint)
}

fn rt(code: ErrorCode, message: &str, hint: &str) -> Error {
    Error::new(code, message).with_hint(hint)
}

fn sample(code: ErrorCode) -> Error {
    use ErrorCode::*;
    match code {
        SchemaInvalid => cfg(
            code,
            "stems.shop-api.type",
            8,
            "unknown variant `rocket`, expected one of `process`, `docker`, `compose`, `external`",
            "fix `stems.shop-api.type`; every field and allowed value is in schema/stems.schema.json (point your editor at it for completion)",
        ),
        SchemaVersionUnsupported => cfg(
            code,
            "schema_version",
            1,
            "schema_version 2 requires a newer stems than 0.1.0 (supported: 1)",
            "upgrade stems (`brew upgrade stems`) to one that supports schema_version 2, or set `schema_version: 1`",
        ),
        WorkspaceNotFound => rt(
            code,
            "no stems.yaml found in /work/app or any parent directory",
            "run `stems init` to create one, or pass --workspace <dir> / set STEMS_WORKSPACE",
        ),
        IncludeNotFound => cfg(
            code,
            "include[0]",
            6,
            "include `teams/payments.yaml` not found",
            "fix the path (it is relative to the file that names it) or create the file",
        ),
        IncludeCycle => cfg(
            code,
            "include[0]",
            6,
            "include cycle: stems.yaml -> teams/a.yaml -> stems.yaml",
            "remove one of the include/extends entries",
        ),
        ConfigReadFailed => rt(
            code,
            "stems.local.yaml cannot be read: permission denied",
            "check that the file is readable by your user (permissions, not a directory)",
        ),
        DuplicateStem => cfg(
            code,
            "stems.payments",
            12,
            "stem `payments` is defined in both teams/payments.yaml and teams/billing.yaml",
            "keep one definition; override fields from stems.local.yaml instead of redefining the stem",
        ),
        UnresolvedVariable => cfg(
            code,
            "stems.shop-api.env.DATABASE_URL",
            15,
            "unresolved reference `${var.nope}`: variable `nope` is not declared",
            "declare it under `vars:`",
        ),
        UnknownStem => cfg(
            code,
            "profiles.backend[2]",
            30,
            "profile `backend` lists `shop-apii`, which is not a stem",
            "did you mean `shop-api`? otherwise remove `shop-apii` from profile `backend`",
        ),
        UnknownDependency => cfg(
            code,
            "stems.shop-api.depends_on",
            12,
            "stem `shop-api` depends on `postgres`, which is not defined",
            "define a stem named `postgres` under `stems:` or remove it from stems.shop-api.depends_on",
        ),
        DependencyDisabled => cfg(
            code,
            "stems.shop-web.depends_on",
            40,
            "stem `shop-web` needs `shop-api`, which is disabled (`enabled: false`)",
            "enable `shop-api` (remove `enabled: false`, usually in stems.local.yaml), disable `shop-web` too, or mark the edge `soft: true`",
        ),
        Cycle => cfg(
            code,
            "stems.shop-api.depends_on",
            13,
            "dependency cycle: shop-api -> shop-worker -> shop-api",
            "remove the edge shop-worker -> shop-api or mark it `soft: true` (soft edges do not order starts)",
        ),
        SeededWithoutSeed => cfg(
            code,
            "stems.shop-worker.depends_on",
            20,
            "stem `shop-worker` waits for `postgres` to be seeded, but `postgres` has no `seed` script",
            "add a `seed` script to stems.postgres.scripts, or use `condition: healthy` on this edge",
        ),
        PortConflict => cfg(
            code,
            "stems.postgres-replica.ports",
            20,
            "port 5432 is declared by both `postgres` (stems.postgres.ports[0]) and `postgres-replica` (stems.postgres-replica.ports[0])",
            "map a different host port for `postgres-replica` (keep `container_port`, change `port`); two stems cannot listen on the same host port",
        ),
        ScriptNotFound => cfg(
            code,
            "stems.shop-api.scripts.start",
            15,
            "script file `scripts/nope.sh` (stems.shop-api.scripts.start) does not exist in the integration repo",
            "create `scripts/nope.sh` in /work/integration or fix the `file:` path (it is relative to the directory of stems.yaml)",
        ),
        ScriptOutsideWorkspace => cfg(
            code,
            "stems.shop-api.scripts.seed",
            18,
            "script file `/etc/seed.sh` (stems.shop-api.scripts.seed) is outside the integration repo /work/integration",
            "scripts live in the integration repo (FR-WS-2): move the script under it and reference it relative to stems.yaml, e.g. `file: scripts/<stem>/<name>.sh`",
        ),
        ScriptArgsInvalid => rt(
            code,
            "script `create-test-user` of `shop-api` requires `--email`",
            "pass it after `--`: `stems run shop-api create-test-user -- --email a@b.c` (see `stems run shop-api --list`)",
        ),
        CodebaseNotFound => cfg(
            code,
            "stems.shop-api.codebase",
            9,
            "codebase of `shop-api` does not exist: /work/repos/does-not-exist",
            "fix `codebase:` (relative paths start at /work/integration) or point it at your checkout in stems.local.yaml: `stems: { shop-api: { codebase: ~/src/shop-api } }`",
        ),
        OverlayConflict => cfg(
            code,
            "stems.local-svc.overlays",
            15,
            "overlay destination `config/local.ini` already exists in the codebase of `local-svc` and was not written by stems",
            "stems never overwrites files it did not create: remove or rename /work/repo/config/local.ini if it is a stale copy, or choose another `dest`",
        ),
        OverlayTrackedFile => cfg(
            code,
            "stems.shop-api.overlays",
            42,
            "overlay destination `config/local.ini` of `shop-api` is tracked in the codebase's git index",
            "materialising it would show up as a change in the repo: add it to .gitignore and `git rm --cached config/local.ini`, or choose another `dest`",
        ),
        ToolVersion => cfg(
            code,
            "requires.node",
            6,
            "`node` 24.6.0 does not satisfy the required range >=99",
            "install node >=99 (e.g. with brew, nvm, asdf or pyenv) and make sure it is first on PATH (`node --version` printed `v24.6.0`), or relax requires.node",
        ),
        ProfileMissingDependency => cfg(
            code,
            "profiles.web-only",
            33,
            "profile `web-only` omits `shop-api`, a hard dependency of `shop-web`, and `strict_profiles` is on",
            "add `shop-api` to the profile, or set `strict_profiles: false` to pull dependencies in automatically",
        ),
        UnknownProfile => rt(
            code,
            "profile `web-onyl` is not defined",
            "did you mean `web-only`? profiles: default, backend, web-only (see `stems profiles`)",
        ),
        UnknownVariant => cfg(
            code,
            "stems.shop-api.variant",
            4,
            "stem `shop-api` has no variant `dockr`",
            "variants of `shop-api`: docker (or `local` for the base definition; see `stems switch shop-api`)",
        ),
        DestructiveNotConfirmed => rt(
            code,
            "`down --volumes` deletes data and needs confirmation",
            "re-run with --yes (CLI) or `confirm: true` (MCP)",
        ),
        DestructiveNotAllowed => rt(
            code,
            "destructive MCP operation `reset` refused: `agent.allow_destructive` is false",
            "set `agent: { allow_destructive: true }` in stems.yaml (or stems.local.yaml) if agents may reset data",
        ),
        AlreadyInitialised => rt(
            code,
            "/work/integration already contains stems.yaml",
            "edit the existing stems.yaml, or run `stems init` in an empty directory",
        ),
        Usage => rt(
            code,
            "unrecognized subcommand `upp`",
            "did you mean `up`? run `stems --help` for the list of commands",
        ),
        PortInUse => rt(
            code,
            "port 18080 of `shop-api` is in use by pid 4242 (python3 -m http.server 18080)",
            "stop that process (`kill 4242`) or change stems.shop-api.ports; `stems doctor --orphans` lists leftovers from earlier runs",
        ),
        ScriptFailed => rt(
            code,
            "script `seed` of `postgres` exited with status 1",
            "see the last lines in details, or `stems logs postgres --script seed`",
        ),
        ScriptRequiresUnmet => rt(
            code,
            "workspace script `nuke-databases` requires `postgres`, which is not healthy",
            "start it first (`stems up postgres`) or pass --start-deps",
        ),
        SetupFailed => rt(
            code,
            "setup of `shop-api` failed (exit status 2); the stem was not started",
            "fix the script and run `stems up shop-api` again; `stems logs shop-api --script setup` shows its output",
        ),
        HealthTimeout => rt(
            code,
            "`shop-api` did not become healthy within 60s (http://localhost:18080/healthz)",
            "check `stems logs shop-api`; raise `health.start_timeout` if it is just slow to boot",
        ),
        StartTimeout => rt(
            code,
            "`postgres` did not start within 60s",
            "check `stems logs postgres` and `docker ps -a` for the container state",
        ),
        StartFailed => rt(
            code,
            "`shop-worker` exited with code 3 before it became ready",
            "check `stems logs shop-worker`; run its start command by hand in the codebase to see why it exits",
        ),
        StopTimeoutKilled => rt(
            code,
            "`shop-worker` ignored SIGTERM for 2s and was killed",
            "handle SIGTERM in the service, or raise `stop_grace`",
        ),
        MaxRestarts => rt(
            code,
            "`shop-worker` crashed 6 times in 10m; giving up",
            "check `stems logs shop-worker`, fix the crash, then `stems start shop-worker`",
        ),
        NotManaged => rt(
            code,
            "`httpbin` is an external stem; stems never starts or stops it",
            "start it yourself; stems only monitors its health",
        ),
        HasDependants => rt(
            code,
            "cannot stop `shop-api`: `shop-web` depends on it and is running",
            "stop the dependants first, or pass --cascade to stop them too",
        ),
        DockerUnavailable => rt(
            code,
            "cannot reach the Docker daemon at unix:///var/run/docker.sock",
            "start Docker Desktop (or colima), then retry; `stems doctor` checks the connection",
        ),
        ImagePullFailed => rt(
            code,
            "pulling `postgres:16` failed: manifest unknown",
            "check the image name and tag, and `docker login` if the registry is private",
        ),
        ComposeFailed => rt(
            code,
            "`docker compose -p hello-shop up -d redis` exited with status 1",
            "see the compose output in details; run the same command by hand to reproduce",
        ),
        ComposeProjectInUse => rt(
            code,
            "compose project `hello-shop` is already running outside stems",
            "stop it (`docker compose -p hello-shop down`) or set `adopt: true` on the stem to take it over",
        ),
        GitNotInstalled => rt(
            code,
            "`shop-api` has a git codebase but `git` is not on PATH",
            "install git (`xcode-select --install` or `brew install git`)",
        ),
        GitCloneFailed => rt(
            code,
            "cloning git@github.com:acme/shop-api.git failed: permission denied (publickey)",
            "check your SSH key / access to the repository, then `stems repos sync shop-api`",
        ),
        LockHeld => rt(
            code,
            "another stems daemon (pid 777) holds the lock for this workspace",
            "use that daemon (`stems status`), or stop it with `stems daemon stop`",
        ),
        UpgradeFailed => rt(
            code,
            "`brew upgrade stems` exited with status 1",
            "run `brew upgrade stems` yourself to see why; `stems upgrade --dry-run` prints the command",
        ),
        NotImplemented => rt(
            code,
            "`stems graph` is not implemented until deliverable 23",
            "upgrade stems, or use a command that is available (`stems --help`)",
        ),
        Internal => rt(
            code,
            "state file has an unexpected shape",
            "this is a bug in stems; please report it with the command you ran",
        ),
        OrphansFound => rt(
            code,
            "2 processes from a previous run are still alive (ports 18080, 18081)",
            "inspect them with `stems doctor --orphans`, then `stems doctor --orphans --yes` to kill them",
        ),
        DaemonNotRunning => rt(
            code,
            "no stems daemon is running for this workspace",
            "start one with `stems up` (or `stems daemon start`)",
        ),
        DaemonVersionMismatch => rt(
            code,
            "the daemon is stems 0.0.1 but this client is 0.1.0",
            "restart the daemon: `stems daemon stop && stems up`",
        ),
    }
}

#[test]
fn every_code_renders_with_message_and_hint() {
    let mut out = String::new();
    for &code in ErrorCode::all() {
        let e = sample(code);
        assert_eq!(e.code, code);
        assert!(e.hint.as_deref().is_some_and(|h| !h.is_empty()), "{code}");
        out.push_str(&format!(
            "# exit {} — {}\n{e}\n\n",
            code.exit_code(),
            code.meaning()
        ));
    }
    insta::assert_snapshot!(out);
}

#[test]
fn requirements_catalogue_lists_every_code() {
    let req = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../REQUIREMENTS.md"
    ))
    .unwrap();
    let start = req.find("### 7.5 Error catalogue").expect("§7.5 heading");
    let section = &req[start..];
    let end = section[4..].find("\n## ").map_or(section.len(), |i| i + 4);
    let section = &section[..end];
    let missing: Vec<&str> = ErrorCode::all()
        .iter()
        .map(|c| c.as_str())
        .filter(|c| !section.contains(&format!("`{c}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "add to REQUIREMENTS.md §7.5: {missing:?}"
    );
}
