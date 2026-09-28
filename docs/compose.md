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
defaults to `stems_runtime::docker::docker_program()`: `$STEMS_DOCKER_CLI`,
else `docker` on `PATH`, else Docker Desktop's (and Homebrew's, Rancher's,
OrbStack's) well-known install location (see `docs/docker.md`, "Finding the
docker CLI").

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
| remove | `rm -f -s -v <service>` (`-v`: the container's anonymous volumes, never named ones) | `ComposeRuntime::remove(handle)`, i.e. `down` |
| prune | `-p <project> down` (no `-f`, `-v` or `--rmi`) | after a remove, when the project is stems-owned and `ps -a` shows no container left: removes its `<project>_default` network |
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

`project_name` defaults to `stems-<ws>` (the config default; the daemon
lower-cases it and turns characters compose rejects into `-`). Every
compose stem of a workspace shares that project unless it sets its own
`project_name` (hello-shop's `redis` sets `hello-shop`).

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

The policy: **refuse by default, adopt with `adopt: true`**.
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
| `ComposeUnavailable { hint }` | `DOCKER_UNAVAILABLE` | no `docker` CLI (not on `PATH` nor in a well-known location), `'compose' is not a docker command`, `docker compose version` failing or unparseable, or Compose v1 |
| `DockerUnavailable { hint }` | `DOCKER_UNAVAILABLE` | compose output says the engine is unreachable (`Cannot connect to the Docker daemon`, `error during connect`), or `DockerRuntime::connect` failed |
| `ComposeFailed { command, exit, tail }` | `COMPOSE_FAILED` | any other non-zero exit (bad image, unknown service, invalid file); `tail` is the last 20 non-empty lines of stdout then stderr; `exit: None` on timeout (60 s for quick commands, grace + 30 s for `stop`, none for `up`, which may pull) |
| `ComposeProjectInUse { project }` | `COMPOSE_PROJECT_IN_USE` | the table above |

## Limitations (v1)

- One service per stem. Several stems may share a project (one per service).
- `--no-deps` always: compose `depends_on` is ignored; stems' graph is
  authoritative.
- `remove` removes the service container (and its anonymous volumes); the
  project network (`<project>_default`) goes once the last container of a
  stems-owned project is removed. Named volumes are left to `docker compose
  -p <project> down -v`.
- `wait` reports the container exit; compose `restart:` policies inside the
  file still apply and can fight stems' own restart policy, so leave
  `restart:` unset in wrapped services.
- `docker compose` must be v2 (found on the daemon's `PATH` or in a
  well-known install location). With compose
  stems in the workspace, `stems doctor` checks it (`compose.version`,
  fail) and `stems validate` warns (`TOOL_VERSION` in `data.warnings`,
  `details.tool: "docker compose"`; skipped by `--skip-requires`); a
  warning, not an error, so process-only CI can still validate
  hello-shop.

## Daemon wiring (deliverable 15)

`crates/stems-daemon/src/supervisor/containers.rs` (shared with docker
stems, see `docs/docker.md`):

- The compose runtime is created lazily with the Docker connection, the
  first time a compose stem starts: `ComposeOptions { workspace, env_dir:
  <STEMS_HOME>/<ws-hash>/compose }` (the directory `doctor` uses too, so
  ownership markers agree).
- `compose_spec(ws, stem, run_id, env)`: the file (absolute, resolved
  against the integration repo), `service`, `project_name`, `adopt`,
  `stop_grace`, and the env stems resolves for the stem (config env,
  `env_files`, local overrides, `--pass-env`, `PORT` = the primary host
  port, `STEMS_*`; not the daemon's own environment). Golden:
  `hello_shop_redis`. hello-shop's `redis` passes
  `REDIS_PORT: "${stem.self.port}"` so a local port override reaches the
  file's `${REDIS_PORT:-16379}`.
- Errors: `COMPOSE_FAILED` (`details.tail`, `command`, `exit`),
  `COMPOSE_PROJECT_IN_USE` (`details.project`), `DOCKER_UNAVAILABLE` for
  a missing/v1 compose or an unreachable engine; every one carries
  `details.stem`.
- `down` (and daemon shutdown, `reset`) runs `rm -f -s -v <service>` for the
  stem's handle; a compose stem left stopped by `stems stop` is removed by
  `down` through `ComposeRuntime::remove_service(spec)`. `restart` is
  `compose stop` + `up -d --no-deps` (compose itself recreates the
  container when the service definition changed). `down --volumes` does
  not touch compose volumes.
- Crash recovery calls `adopt_service(record, spec)` with the spec built
  from the current config (the trait-level `adopt` when the stem is gone
  from the config). Orphans: `doctor` and `doctor --orphans` include the
  compose scan; `up` includes running ones when the selection needs Docker.

## Verified against Docker (2026-09-27)

Docker Desktop on macOS (Intel): Engine 29.8.0, **compose v5.5.1** (NDJSON
`ps --format json`; `compose version --format json` parses as v2+). `docker`
was not on `PATH`; the runtime used Docker Desktop's CLI via
`docker_cli_path`.

```sh
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime compose_live_roundtrip -- --ignored   # ok
make e2e-docker FEATURE=tests/features/compose    # 5 scenarios, pass
```

1. `compose-redis`: `stems-compose-redis-redis-1` running, published on
   the stem's port; `<STEMS_HOME>/<hash>/compose/cache.env` holds
   `REDIS_PORT`. ✓
2. A `REDIS_ARGS` local override is in the container's command
   (`env-override.feature`). ✓
3. `stems logs cache` shows redis's start-up lines. ✓
4. `kill -9` of the daemon + `stems up` adopts the same container. ✓
5. A bad image fails with `COMPOSE_FAILED` and the pull error in
   `details.tail`. ✓
6. A hand-started project under the stem's `project_name` is refused
   (`COMPOSE_PROJECT_IN_USE`) and co-managed with `adopt: true`. ✓
7. `stems down` removes the service container with its anonymous volumes
   and, once a stems-owned project is empty, its `<project>_default`
   network (first contact: both were left behind; fixed, see "Commands"). ✓
8. `stems validate --json` without a docker CLI: `TOOL_VERSION` warning
   (`validate/compose-requires.feature`, which now sets `STEMS_DOCKER_CLI`
   to a missing file, since an empty `PATH` no longer hides Docker
   Desktop's CLI). ✓

Not verified: compose v2.x before 2.21 (JSON-array `ps` output; unit-tested
only) and compose on Linux.
