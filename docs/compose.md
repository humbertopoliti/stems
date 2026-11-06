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

All invocations are `docker compose --ansi never -f <file> [-f
<env_dir>/<stem>.override.yaml] -p <project> --env-file <env_dir>/<stem>.env
...` (the second `-f` only with [`inject_env: true`](#inject_env-the-stems-env-in-the-container)),
run with captured output in the compose file's directory (goldens in
`crates/stems-runtime/src/compose/snapshots/`):

| operation | arguments after the prefix | when |
|---|---|---|
| version | `compose version --format json` (no prefix) | once per runtime before the first start (a success is cached) |
| project check | `-p <project> ps --format json -a` (no `-f`) | every start |
| up | `up -d --no-deps [--build] <service>` | start (`--build` when the stem's `build` is true, the default; see [Build](#build)) |
| find container | `ps --format json -a <service>` | right after `up` |
| stop | `stop -t <grace, rounded up to seconds> <service>` | `stop` (the container is kept) |
| remove | `rm -f -s -v <service>` (`-v`: the container's anonymous volumes, never named ones) | `ComposeRuntime::remove(handle)`, i.e. `down` |
| prune | `-p <project> down` (no `-f`, `-v` or `--rmi`) | after a remove, when the project is stems-owned and `ps -a` shows no container left: removes its `<project>_default` network |
| config | `config --services` | available, not used by the lifecycle |
| service facts | `config --format json` | the stale-image check (`ComposeRuntime::service_config`): `services.<svc>.image` (a built image without `image:` is `<project>-<service>`) and `services.<svc>.build.{context, dockerfile}` as absolute paths |

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

## One-off compose commands

`stems compose <stem> [--service] -- <args…>` runs `docker compose` with the
stem's context, so a one-off (a migration, `run --rm`, `config`, `ps`) needs
no hand-built `-f … -p …`:

```sh
stems compose cache -- config --services
stems compose cache --service -- ps --format json   # ... ps --format json redis
stems compose cache -- run --rm migrate
```

It runs `docker compose --ansi auto -f <file> [-f <override>, with
`inject_env`] -p <project> --env-file <env_dir>/<stem>.env <args…>` in your
terminal (stdin, TTY and exit code pass through), in the compose file's
directory, with your environment plus the stem's env and `DOCKER_HOST` set as
the runtime does. `--service` appends the stem's `service` as the last
argument. A stem that is not `type: compose` is a `USAGE` error (exit 2).
`--json` prints `{stem, argv, exit}` after the command.

Like `stems exec` it never starts a daemon: with a daemon the env is the
config's as it is now (env files re-read) with the daemon's ports, outputs
and `--pass-env`; without one stems computes it from the config (auto ports
and outputs stay unresolved). Either way it regenerates the env file (and
override) whenever they differ from that env, so a key removed from an env
file never reaches compose through a file an earlier start wrote. One
exception: for a running stem whose env still has unresolved values (a
dependency restarting or down, see [exec.md](exec.md)), the files on disk
(its start's) are kept, so a `${stem...}` placeholder never reaches a
container that `up -d` recreates. A
running container keeps the environment it was created with:
`stems compose` prints an `ENV_STALE` warning on stderr when it differs, and
`stems restart <stem>` (or `stems compose <stem> -- up -d`, which recreates
the service from the regenerated files) applies it. The daemon serialises its own compose calls per project; a long
`stems compose` while stems restarts the same stem may hit compose's own
project lock (compose retries).

## Build

A compose stem has a `build` setting (bool, **default `true`**): every
`up`, `start` and `restart` runs `compose up -d --no-deps --build
<service>`, so a service with `build:` in the compose file runs the code
that is checked out, not whatever image was built last week. A service
without `build:` is unaffected (`--build` is a no-op for an `image:`
service). The build output is streamed line by line to `docker.build
{stem, line}` events, exactly like a docker `build:` stem's (`stems
events`, the TUI).

```yaml
stems:
  api:
    type: compose
    file: compose.yml
    service: api
    build: false        # never rebuild on start (the image is somebody else's job)
```

`stems up|start|restart --build` and `--no-build` override the setting
for one run (`UpParams.build`, `StartParams.build`, `RestartParams.build`:
`true`/`false`/absent). On `restart`, `--build` also runs the stems'
`build` scripts, as before. Docker `build:` stems always build; the flags
do not apply to them. A bool on a docker stem, and `context`/`dockerfile`
on a compose stem, are `SCHEMA_INVALID` at `stems.<n>.build` (the service's
`build:` lives in the compose file).

### Build secrets, extra services and `before_setup` (Bake)

`compose up --build` cannot pass BuildKit secrets or build a one-shot
service that `up` does not start. The object form of `build:` does both:

```yaml
stems:
  api:
    type: compose
    file: compose.yml
    service: api
    build:
      services: [api, migrate]   # default: [<service>]; one-shot services too
      before_setup: true         # default false: build at start
      secrets:
        - { id: adc, file: "${env.HOME}/.config/gcloud/application_default_credentials.json" }
      allow_read:                # exact absolute paths; no globs, no symlinks
        - "${workspace.root}/../infra"
```

- The images are built with **Bake** from the compose file: `docker buildx
  bake --load --progress plain -f <file> -f <override> [--allow
  fs.read=<path>]… [--set <svc>.tags=<image>]… <svc>…`, then `compose up -d
  --no-deps --no-build <service>`. Each service is tagged with the image
  compose runs (`image:`, else `<project>-<service>`), so the following `up`
  and `docker compose run --rm migrate` find it. Every listed service must
  have `build:` in the compose file (else the build fails saying so); list
  the stem's own service too when it is built.
- **Secrets** (`{id, file}` or `{id, env}`, as for
  [docker stems](docker.md#build-secrets-and-paths-outside-the-context-buildx))
  are declared in the generated override `<env_dir>/<stem>.override.yaml`
  (the same file `inject_env` uses): a top-level `secrets: {adc: {file: …}}`
  and `services.<svc>.build.secrets: [adc]` for each built service, so the
  compose file needs no stems-specific edits (if it already declares them,
  compose merges the two). The Dockerfile mounts them with `RUN
  --mount=type=secret,id=adc`. With secrets, the override is passed as the
  second `-f` to every compose invocation of the stem.
- **`allow_read`** becomes `--allow fs.read=<path>` (Bake enforces the
  `fs.read` entitlement; stems sets `BUILDX_BAKE_ENTITLEMENTS_FS=1` for the
  Bake process, so an environment that turned the checks off does not
  leak in). Bake resolves paths from the compose file's directory, so a
  service whose `build.context` is a parent of it (`context: ..`) would be
  refused without a grant. Therefore the **workspace root is granted by
  default**: any context inside the workspace, parents of the compose file
  included, needs no configuration. Each service's context, its Dockerfile's
  directory and the secret files are granted too (normalised, plus their
  symlink-resolved form; the workspace root likewise, so a symlinked root
  such as macOS `/tmp` works). `allow_read` is for what lies **outside the
  workspace root** (a sibling checkout, a file under `$HOME`): exact absolute
  paths, no globs, no implicit parents, with the same validation as docker
  stems. The flags are emitted sorted, so the command is deterministic.
- The stem env is the Bake process's environment (plus
  `COMPOSE_PROJECT_NAME`), so the files interpolate like `compose up`'s.
  Build output lines are `docker.build {stem, line}` events; a failure is
  `SETUP_FAILED` with `details.tail`. A missing secret file / unset `env`
  secret / missing `allow_read` path fails before Bake runs, naming it
  (`details.secret`, `details.allow_read`).
- **`before_setup: true`** moves the build to the front of the start
  sequence: `[build images] → setup → pre_start → start` (the
  `docker.build` events come before `stem.state setup`). `setup` (and
  `pre_start`) get `STEMS_IMAGES=svc=image[,svc=image]` and `STEMS_IMAGE`
  (the stem's own service's image), so `setup` can `docker compose run
  --rm migrate`. The start that follows does not build again (the stem is
  marked as built for that one start). A policy respawn builds again, as
  `up --build` does. With the build off for the run (`--no-build`), nothing
  is built but `setup` still gets the image names. A stem without any
  lifecycle script builds at start.
- `build: true` (or no `build:`) keeps the plain `compose up --build` path;
  `build: false` and `--no-build` never run Bake. The stale-image check
  (below) looks at the stem's own service; secret files and `allow_read`
  trees are not stale inputs.

## Stale images

With `build: false` (or `--no-build`) the image can be **stale**: built
before the service's build inputs last changed. The build inputs are the
service's `build.context` walked with the same `.dockerignore` rules the
docker runtime uses for its tarball (`stems_runtime::docker::context`,
`.git` skipped), plus the Dockerfile and the compose file itself; the
newest modification time wins (`stems_runtime::stale`). An mtime in the
future counts as now; a context of more than 50 000 entries gives no
verdict (never a false warning). The comparison is against the image's
`Created` (`docker image inspect`), both in UTC: stale means strictly
older.

stems reports a stale image in three places and never loops on it:

- **`stems up`/`start`/`restart` without a rebuild**: the stem still
  starts, with `IMAGE_STALE` in the result's `warnings`
  (`data.warnings[].details {stem, image, image_created, newest_input,
  newest_input_path}`; the human output prints `  warning <stem>:
  <message>`; the exit code is unchanged) and an `image.stale` event with
  the same data.
- **A crash of a running stem started with `build == false`**: when the
  image is stale, the restart policy would run the same image again, so
  the crash is not retried. The stem fails with `IMAGE_STALE` (hint:
  `stems restart <stem> --build`, or set `build: true`) and
  `stem.gave_up {reason: "image_stale", exit_code, signal, image, ...}` is
  emitted, so the desktop notification ([notifications.md](notifications.md))
  fires and `status` shows `failed` with the error. A fresh image, a
  missing one, or a check that fails for any reason (Docker slow, compose
  config unreadable) leaves the policy restart alone (`Unknown` never
  blocks a restart). With `build: true` nothing changes: the restart
  rebuilds, and a rebuild that fails is a non-retried start failure.
- **`stems doctor`**: `image.<stem>` for each enabled compose stem whose
  service has `build:` (and each docker `build:` stem, for the image of
  the current daemon run), see [doctor.md](doctor.md).

Known blind spot: the check is mtime-based (what `git checkout` changes).
A `git stash`/revert that leaves every file older than the image is not
detected; content is not hashed.

## Project naming

`project_name` defaults to `stems-<ws>` (the config default; the daemon
lower-cases it and turns characters compose rejects into `-`). Every
compose stem of a workspace shares that project unless it sets its own
`project_name` (hello-shop's `redis` sets `hello-shop`).

## Env overrides and precedence

The stem's resolved environment (the layers of
[config.md](config.md#environment-fr-st-4): `env`, `env_files`,
`stems.local.yaml`, `--pass-env`, `PORT` = the primary **host** port,
`STEMS_*`; not the daemon's own environment) is

1. written to `<env_dir>/<stem>.env` (mode 0600, rewritten on every start;
   `KEY='value'`, or a double-quoted value with `\\ \" \n \r \$` escaped when
   the value contains `'` or a newline), passed with `--env-file`, **and**
2. exported into the compose process environment.

Compose interpolates the file with: process environment > `--env-file` >
the file's own `${VAR:-default}`. So **stem env > compose file defaults**;
the exported values are authoritative and the env file is what lets a human
reproduce the invocation by hand. Variables stems does not set still come
from the daemon's environment before the file's defaults.

**Stem env is an interpolation input, not the container's environment.**
Compose only puts a variable into the *container* when the compose file
uses it. With `env: { FOO: bar }` on the stem:

```yaml
services:
  api:
    image: shop-api
    # no mention of FOO              -> the container has no FOO
    environment:
      - FOO                          # -> FOO=bar (taken from compose's environment)
      - API_FOO=${FOO}               # -> API_FOO=bar (interpolated)
    command: serve --foo ${FOO:-x}   # -> interpolated into the command
```

### `inject_env`: the stem's env in the container

To give the container the whole stem environment without listing every
variable in the compose file, set `inject_env: true` (default `false`):

```yaml
stems:
  api:
    type: compose
    file: compose.yml
    service: api
    env: { FOO: bar }
    env_files: [env/api.env]
    inject_env: true
```

stems then also writes `<env_dir>/<stem>.override.yaml` (mode 0600) and
passes it as a second `-f` on every invocation that has `-f` (`up`, `stop`,
`rm`, `ps`, `config`):

```yaml
services:
  "api":
    env_file:
      - "/…/<env_dir>/api.env"    # the same file as --env-file
    environment:
      PORT: "8080"                # only when the primary port has a container_port
```

- Compose appends `env_file` lists across `-f` files, so the service's own
  `env_file:` still applies; for a key in both, stems' file (the later one)
  wins. The service's `environment:` wins over every `env_file`, so the
  compose file keeps the last word.
- `PORT` in the env file is the **host** port (what the stem's health check
  and dependants use). Inside the container the service listens on the
  container port, so when the stem's first port has a `container_port`
  the override sets `PORT` to it, as docker stems do.
- `STEMS_STEM`, `STEMS_WORKSPACE`, `STEMS_RUN_ID`, `STEMS_<DEP>_PORT`, …
  are injected too (host paths and host ports: they describe the host).
- `docker compose up` has no `-e`; the override file is how "pass the
  env with `-e`" is delivered.
- Both files are rewritten on every start and every `stems compose`, from
  the env files as they are then: removing a key from an env file removes
  it from the next container. The running container keeps its environment
  until it is recreated; `stems env <stem>` and `stems status` say so
  (`ENV_STALE`, see [exec.md](exec.md); dependency ports and outputs
  never count). While the stem runs, `stems compose` does not rewrite them
  with a value that is unresolved at that moment.
- The stale-image check, `doctor` and adoption after a daemon restart use
  the same `-f` pair and the same env (built like a real start, minus
  `--pass-env`).
- Stems that share a compose project should agree on `inject_env`:
  starting one recreates the project's services with its own setting, so
  another may come back without (or with) the injected environment.
  `stems validate` warns `COMPOSE_INJECT_ENV_MIXED` (a warning, exit 0;
  `details: {stem, project, without: [stems]}`) once per stem that sets
  `inject_env: true` in such a project; set the same value on every stem
  of the project.

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
  `stop_grace`, `inject_env` (with the primary port's `container_port`),
  and the env stems resolves for the stem (config env, `env_files`, local
  overrides, `--pass-env`, `PORT` = the primary host port, `STEMS_*`; not
  the daemon's own environment). Off the start path (adoption, `doctor`)
  `containers::config_env` builds the same env through `build_env`
  without `--pass-env`. Golden:
  `hello_shop_redis`. hello-shop's `redis` passes
  `REDIS_PORT: "${stem.self.port}"` so a local port override reaches the
  file's `${REDIS_PORT:-16379}`.
- Errors: `COMPOSE_FAILED` (`details.tail`, `command`, `exit`),
  `COMPOSE_PROJECT_IN_USE` (`details.project`), `DOCKER_UNAVAILABLE` for
  a missing/v1 compose or an unreachable engine, `IMAGE_STALE` (a
  warning on start, the crash guard's failure; `details.image`,
  `image_created`, `newest_input`, `newest_input_path`); every one
  carries `details.stem`.
- `build` (35): `compose_spec` copies the stem's `build:`; `start_spec`
  applies the operation's `--build`/`--no-build` (`Core.build_override`,
  set while the ops lock is held) and attaches the progress sink
  (`docker.build` events). `containers::start` remembers the effective
  flag per stem and, for `false`, runs `stale::warn_before_start`
  (`Core.warnings` → `UpResult.warnings`, event `image.stale`).
  `restart::on_exit` asks `stale::crash_guard` before scheduling a
  policy restart; a stale image fails the stem with `IMAGE_STALE` and
  `stem.gave_up {reason: "image_stale"}`.
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

9. `inject_env: true` (38): `docker exec … echo $FOO` prints the stem's
   `FOO`, `PORT` is the container port; without it the container has no
   `FOO` (`compose/inject-env.feature`, 2026-11-05). Compose's `env_file`
   merge across `-f` files was checked with `docker compose config`: the
   later file wins, `environment:` beats both. ✓

Not verified: compose v2.x before 2.21 (JSON-array `ps` output; unit-tested
only) and compose on Linux.

See also: [Docker](docker.md) · [Config](config.md) · [Ports](ports.md) · [What stems can do](features.md)
