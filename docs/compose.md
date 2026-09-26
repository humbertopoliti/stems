# Compose stems

`type: compose` stems wrap **one service** of an existing compose file, so a
team keeps its `compose.yaml` while stems owns lifecycle, logs and status
(FR-ST-3, deliverable 15). The runtime is `stems_runtime::ComposeRuntime`
(`crates/stems-runtime/src/compose/`); the daemon turns a stem's config
`ComposeSpec` into the runtime-facing `stems_runtime::ComposeSpec` and starts
it with `StartSpec::Compose`.

```rust
let rt = ComposeRuntime::new(
    docker.clone(),                                   // Arc<DockerRuntime>
    ComposeOptions::new(ws, stems_home.join(ws).join("compose")),
);
let spec = ComposeSpec::new(ws, "redis", run_id, abs_file, "redis"); // project stems-<ws>
let h = rt.start(&StartSpec::Compose(Box::new(spec))).await?;       // Handle::Container
```

`ComposeOptions { workspace, env_dir, binary }`: `env_dir` is
`$STEMS_HOME/<ws>/compose/` (env files and ownership markers), `binary`
defaults to `docker` on `PATH`.

## Who owns what

| stems | compose |
|---|---|
| when the service starts and stops (`depends_on` in `stems.yaml`, health, restarts) | the service definition: image, build, command, volumes, networks, `environment:` |
| the env values it interpolates with (stem `env`) | how those values are used (`${VAR:-default}`) |
| logs, status, metrics, adoption, orphan scan (through the Docker API by container id) | container creation, naming (`<project>-<service>-1`), its project network |

Everything container-level is delegated to `DockerRuntime` once `docker
compose ps` has named the container: the handle is an ordinary
`Handle::Container`, and `describe`, `output_stream` (logs followed from one
second before `up`), `wait` and `is_alive` are the docker runtime's.

## Commands

All invocations are `docker compose --ansi never -f <file> -p <project>
--env-file <env_dir>/<stem>.env ...`, run with captured output in the
compose file's directory (goldens in
`crates/stems-runtime/src/compose/snapshots/`):

| operation | arguments after the prefix | when |
|---|---|---|
| version | `compose version --format json` (no prefix) | once per runtime before the first start (a success is cached) |
| project check | `-p <project> ps --format json -a` (no `-f`) | every start |
| up | `up -d --no-deps <service>` | start |
| find container | `ps --format json -a <service>` | right after `up` |
| stop | `stop -t <grace, rounded up to seconds> <service>` | `stop` (the container is kept) |
| remove | `rm -f -s <service>` | `ComposeRuntime::remove(handle)`, i.e. `down` |
| config | `config --services` | available, not used by the lifecycle |

`ps --format json` output is accepted both as a JSON array (compose before
2.21) and as NDJSON (one object per line, 2.21+); non-JSON lines (warnings)
are skipped. Invocations are serialised per project (a `tokio::Mutex` per
project name) to avoid compose's own locking errors.

`stop` falls back to a container-level `docker stop` when `compose stop`
itself fails (e.g. the compose file no longer loads), and kills the
container if it is still running afterwards (`StopOutcome::Killed`).

`DOCKER_HOST` is exported to every compose process as the address the
`DockerRuntime` connected to (unless the stem's env sets it), so the CLI
and the API client always talk to the same engine.

## Project naming

`project_name` defaults to `stems-<ws>` (lower-cased; characters compose
rejects become `-`). Every compose stem of a workspace shares that project
unless it sets its own `project_name`.

## Env overrides and precedence

The stem's resolved `env` is

1. written to `<env_dir>/<stem>.env` (mode 0600, rewritten on every start;
   `KEY='value'`, or a double-quoted value with `\\ \" \n \r \$` escaped when
   the value contains `'` or a newline), passed with `--env-file`, **and**
2. exported into the compose process environment.

Compose interpolates the file with: process environment > `--env-file` >
the file's own `${VAR:-default}`. So **stem env > compose file defaults**;
the exported values are authoritative and the env file is what lets a human
reproduce the invocation by hand. Variables stems does not set still come
from the daemon's environment before the file's defaults.

Compose only puts a variable into the *container* if the file uses it
(`environment:`, `command: ${REDIS_ARGS:-}`, ...): stem env is an
interpolation input, not an implicit `environment:` block.

## Ownership marker and `COMPOSE_PROJECT_IN_USE`

Compose containers do not carry stems' `stems.*` labels, so ownership is
recorded on disk: after every successful `up`, stems writes
`<env_dir>/<project>.owned`. Before `up`, `docker compose -p <project> ps -a`
decides (`project_decision`):

| project has containers | marker exists or project is `stems-<ws>` | `adopt: true` | result |
|---|---|---|---|
| no | – | – | start (free) |
| yes | yes | – | start (ours) |
| yes | no | yes | start (adopt; the marker is then written) |
| yes | no | no | `COMPOSE_PROJECT_IN_USE` |

This answers REQUIREMENTS §11 Q2: **refuse by default, adopt with `adopt: true`**.
Adopting means stems co-manages the project from then on: `up -d --no-deps`
leaves an up-to-date running container untouched, and `down` removes the
stem's service container.

## Adoption after a daemon restart

- `ComposeRuntime::adopt_service(record, spec)`: the recorded container id
  must still exist, carry `com.docker.compose.project == spec.project_name`
  and `com.docker.compose.service == spec.service`, and be running.
- The trait-level `adopt(record)` takes project and service from the
  container's compose labels and additionally requires the project to be
  stems-owned (`stems-<ws>` or marked); stop/rm then address the project by
  name only (`-p <project>` without `-f`).

## Orphans

`scan_orphans(scope)` runs `docker compose -p <project> ps -a` for
`stems-<ws>` and every project with an ownership marker, and reports each
container whose id is not in `scope.known` (`OrphanKind::Container`,
`matches_start_command: true`, `stem: None`). The Docker runtime's own scan
does not see these (it filters on `stems.workspace`), so nothing is
reported twice.

## Errors

| `RuntimeError` | error code | when |
|---|---|---|
| `ComposeUnavailable { hint }` | `DOCKER_UNAVAILABLE` | `docker` not on `PATH`, `'compose' is not a docker command`, `docker compose version` failing or unparseable, or Compose v1 |
| `DockerUnavailable { hint }` | `DOCKER_UNAVAILABLE` | compose output says the engine is unreachable (`Cannot connect to the Docker daemon`, `error during connect`), or `DockerRuntime::connect` failed |
| `ComposeFailed { command, exit, tail }` | `COMPOSE_FAILED` | any other non-zero exit (bad image, unknown service, invalid file); `tail` is the last 20 non-empty lines of stdout then stderr; `exit: None` on timeout (60 s for quick commands, grace + 30 s for `stop`, none for `up`, which may pull) |
| `ComposeProjectInUse { project }` | `COMPOSE_PROJECT_IN_USE` | the table above |

## Limitations (v1)

- One service per stem. Several stems may share a project (one per service).
- `--no-deps` always: compose `depends_on` is ignored; stems' graph is
  authoritative.
- `remove` removes the service container only; the project network
  (`<project>_default`) and named volumes are left to `docker compose -p
  <project> down [-v]`.
- `wait` reports the container exit; compose `restart:` policies inside the
  file still apply and can fight stems' own restart policy, so leave
  `restart:` unset in wrapped services.
- `docker compose` must be v2 and on the daemon's `PATH`; `requires`
  auto-check of the binary is the daemon's job (not in this crate).
