# Docker stems

`type: docker` stems run as containers through the Docker Engine API
(`bollard`), not the `docker` CLI (FR-ST-3, deliverable 14). The runtime is
`stems_runtime::DockerRuntime` (`crates/stems-runtime/src/docker/`); the
daemon turns a stem's `DockerSpec` into a runtime-facing `ContainerSpec` and
starts it with `StartSpec::Docker`.

## Connecting

`DockerRuntime::connect(DockerOptions { host, timeout, workspace })`:

1. Address: `host` if given, else `$DOCKER_HOST`, else the first existing
   socket among `/var/run/docker.sock`, `~/.docker/run/docker.sock`
   (Docker Desktop), `~/.colima/default/docker.sock`,
   `~/.orbstack/run/docker.sock`, `~/.rd/docker.sock`; fallback
   `unix:///var/run/docker.sock`. Supported schemes: `unix://`, a bare
   socket path, `tcp://`, `http://` (no TLS, no `ssh://`).
2. `GET /_ping`, bounded by `timeout`. Any failure (missing socket,
   refused connection, timeout, unsupported scheme) is
   `RuntimeError::DockerUnavailable { hint }`; the hint names the address
   and says "start Docker Desktop (or Colima), or point DOCKER_HOST at a
   running daemon".

`timeout` is also bollard's per-request timeout (rounded down to whole
seconds, minimum 1 s). It bounds the time to the response headers only;
log, pull, build and wait streams are not cut by it (`wait` retries when
it fires).

The daemon connects lazily, only when a docker (or compose) stem is
actually started, adopted, or `down --volumes` needs Docker, so a
process-only selection never talks to Docker (see "Daemon wiring").

## Naming and labels

| Thing | Value |
|---|---|
| container name | `<ws>-<stem>` |
| network | `<ws>_net` (bridge, created if absent, labelled `stems.workspace`), or the stem's `network:` override; `bridge`/`host`/`none` are never created. The container joins it with the alias `<stem>`, so other containers reach it as `<stem>:<container_port>` |
| named volume | `<ws>_<name>`; a leading `<ws>_` or `<ws>-` in the config name is dropped first, so `pgdata`, `hello-shop_pgdata` and `hello-shop-pgdata` all become `hello-shop_pgdata` |
| built image tag | `stems/<ws>/<stem>:<run_id>` (lower-cased repository) |

Labels on every container (user labels from `docker.labels` are kept, but
stems' own always win):

| Label | Value | Used by |
|---|---|---|
| `stems.workspace` | workspace name | orphan scan, adoption, `--volumes` safety |
| `stems.stem` | stem name | adoption |
| `stems.run_id` | daemon run id at creation | information only: an adopted container keeps the run id of the run that created it, so nothing matches on it |
| `stems.spec_hash` | 128-bit hex hash of the spec | restart: recreate or `docker restart` |

The constants are `stems_runtime::docker::{LABEL_WORKSPACE, LABEL_STEM,
LABEL_RUN_ID, LABEL_SPEC_HASH}`.

## The container

`to_bollard(&ContainerSpec)` is a pure mapping to bollard's create body,
`HostConfig` and `NetworkingConfig`, golden-tested in
`crates/stems-runtime/src/docker/snapshots/`:

- `Image`: the build tag, else `image`.
- `Env`: `KEY=VALUE`, sorted. `Cmd` = `command`, `Entrypoint` = `entrypoint`.
- Ports `host:container[/proto]` (tcp default): `ExposedPorts` plus
  `PortBindings` on all host interfaces (Docker's default). Host ports are
  the ones the daemon allocated/remapped.
- Volumes: `source:target[:ro|rw]` in config. A source starting with `/`,
  `.` or `~` is a bind mount (relative paths resolved against the stem's
  directory, stored absolute); anything else is a named volume, prefixed.
  Both go to `HostConfig.Binds`; Docker creates named volumes on first use.
- `Healthcheck`: the `docker.healthcheck` override (unset fields keep the
  image's). `State.Health.Status` is reported by `describe` as
  `RuntimeFacts.container_health` for deliverable 21's `docker` probe.
- `StopTimeout` = `stop_grace` rounded up to seconds (so a manual
  `docker stop` honours it too), `AutoRemove=false` always (stems removes
  containers explicitly so logs stay readable after exit), no restart
  policy (the supervisor owns restarts).

## Lifecycle

| Operation | What happens |
|---|---|
| `start` | ensure network → pull the image if it is not present (or build) → if a container with our name exists: remove it when it is ours (labels) and stopped, else fail (`Container`) → create → start (a created container that fails to start is removed) → follow logs from the beginning |
| pull | `POST /images/create` with repository and tag (`latest` if none; digests supported). Progress is coalesced to one `ImageProgress::Pull { image, layer, status }` per layer status change and sent best-effort (never blocks) to `ContainerSpec.progress` → daemon `docker.pull` events |
| build | the context directory is tarred (see below) and sent to `POST /build` with `t=stems/<ws>/<stem>:<run_id>`, `rm`, `forcerm`; each output line is sent as `ImageProgress::Build { stem, line }` (`docker.build` events); failure keeps the last 20 lines |
| `stop(grace)` | not running → `AlreadyDead`; else `docker stop -t ceil(grace)` (SIGTERM, then Docker's own SIGKILL). Still running afterwards → `kill` (SIGKILL) → `Killed`. Exit code 137 after the stop → `Killed`; otherwise `Graceful` |
| `remove(handle, volumes)` | `DELETE /containers/<id>?force=1&v=<volumes>`; with `volumes` also every named volume mounted by it whose name starts with `<ws>_` (read from the container's `stems.workspace` label; a user's own volumes and bind mounts are never removed). This is `down` / `down --volumes` (FR-LC-2) |
| `restart(handle, spec)` | recreate (stop, remove keeping volumes, start) when `spec.spec_hash()` differs from the container's `stems.spec_hash` label, else `docker restart -t ceil(grace)`. The old handle is released and a new one returned either way |
| `wait` | `POST /containers/<id>/wait?condition=not-running` → exit code (`signal` is always `None`; 137 means killed) |
| `is_alive` | inspect `State.Running` |
| `describe` | container id, `State.Pid` as `pid` (pgid 0, no process tree), host ports as bound (`NetworkSettings.Ports`), `container_health` |
| `output_stream` | `GET /containers/<id>/logs?follow&stdout&stderr&timestamps&since=` demultiplexed into the same `OutputLine`/`OutputStream` broadcast as processes (lines split like process pipes; the timestamp is Docker's). The stream ends when the container stops |

The spec hash covers workspace, stem, image, build, ports, volumes, env,
command, entrypoint, network (resolved, so an explicit `<ws>_net` equals the
default), user labels and healthcheck. It excludes the run id, `stop_grace`
and the progress sink.

### Build context

`context_tar(context, dockerfile)` walks the context in sorted order and
honours a minimal `.dockerignore`: `#` comments, blank lines, globs
(`*`, `?`, `**`; `*` does not cross `/`) anchored at the context root,
leading `/` or `./` and trailing `/` ignored, `!` re-includes (last match
wins), and a pattern matching a directory excludes all of it. `.git` is
always skipped; `.dockerignore` and the Dockerfile are always included. A
Dockerfile outside the context is added as `.stems.Dockerfile`. Symlinks are
archived as links.

## Handles

`Handle::Container { id, container_id, name }`: `pid()`/`pgid()` are `0` and
`start_time()` is `0` (never signalled locally); `Handle::container_id()`
returns the id. The daemon persists the id in `StemRecord.container_id`.

## Adoption (FR-CR-2)

A recorded container is adopted when `verify_adoption(inspect, record, ws,
stem)` holds:

1. the record has a `container_id` and inspect finds that container (short
   ids match by prefix);
2. its labels are `stems.workspace=<ws>` and `stems.stem=<stem>`
   (`verify_labels`, shared with the compose runtime); `stems.run_id` is
   deliberately **not** compared;
3. `State.Running` is true.

The daemon calls `DockerRuntime::adopt_container(record, ws, stem)` during
crash recovery (`containers::adopt`); the container id comes from
`StemRecord.container_id` in `state.json`.
The trait-level `Runtime::adopt(record)` also works: it needs
`DockerOptions.workspace` and derives the stem from the container name
`<ws>-<stem>`, which must then match the `stems.stem` label. Adopted
containers re-attach their log stream from the adoption time on (lines
written while no daemon ran are not replayed; the log file of the previous
run still has everything before the crash).

## Orphans (FR-CR-4)

`scan_orphans(scope)` lists all containers (any state) with the label
`stems.workspace=<scope.workspace>` whose id is not a `container_id` in
`scope.known`. Each is a `ContainerOrphan { id, name, stem_label, state,
image }`, converted into the shared `Orphan { kind: container,
container_id, stem, command: "<image> (<name>, <state>)",
matches_start_command: true }` (the workspace label proves stems created
it, so `--yes` may remove it). Removal is
`DockerRuntime::remove_container_id(id, volumes)`. A failed scan logs a
warning and returns no orphans.

## Errors and their catalogue codes

The runtime returns `RuntimeError`; the daemon maps it to the §7.5
catalogue:

| `RuntimeError` | Code | Exit | Notes |
|---|---|---|---|
| `DockerUnavailable { hint }` | `DOCKER_UNAVAILABLE` | 1 | connect/ping failure, or a transport error (socket gone, connection refused, timeout) on any later call |
| `ImagePullFailed { image, message }` | `IMAGE_PULL_FAILED` | 1 | `message` is the registry's own text (e.g. `pull access denied for stems-does-not-exist ...`) |
| `BuildFailed { tail }` | `SETUP_FAILED` (§7.5 has no build code; building the image is the docker stem's setup) with `details.tail` and `details.build: true` | 1 | last 20 lines of build output |
| `Container(message)` containing `port is already allocated` | `PORT_IN_USE` (`details.message`) | 1 | Docker's own error message |
| `Container(message)`, `Unsupported`, anything else | `START_FAILED` | 1 | Docker's own error message |

The mapping is `stems_daemon::supervisor::containers::runtime_error`,
table-tested in `crates/stems-daemon/src/supervisor/containers/tests.rs`.
Every mapped error carries `details.stem`.

## Daemon wiring (deliverable 14)

`crates/stems-daemon/src/supervisor/containers.rs`:

- **Registry.** `RuntimeRegistry::with_containers(Containers)` registers a
  `LazyRuntime` for `docker` and `compose`. `Containers::docker()` connects
  (`DockerRuntime::connect` with `$DOCKER_HOST` or the usual sockets,
  bounded by 5 s) on first use; a failure is not cached, so the next start
  retries. Nothing connects for process-only selections (unit test
  `process_only_selections_never_connect` counts connects through a fake
  connector). `stems up` still runs its fast preflight (19) before the
  daemon starts, so an unreachable Docker is `DOCKER_UNAVAILABLE` (exit 1,
  hint "start Docker Desktop (or Colima) ...") before anything starts.
- **Spec.** `container_spec(ws, stem, run_id, env, host_ports)`: name
  `<ws>-<stem>`; ports `host:container` from `Port { port, container_port }`
  (host = the allocated/remapped port; `port: auto` is rejected for docker
  by validation); volumes via `VolumeMount::parse` relative to the
  integration repo; env = what env.rs resolves for the stem (config env,
  `env_files`, local overrides, `--pass-env`, `STEMS_*`) **without** the
  daemon's own environment, with `PORT` rewritten to the primary
  *container* port; user labels; `healthcheck` (`disable: true` → `NONE`);
  `stop_grace`; the daemon's run id; `build.context`/`dockerfile` are
  already absolute (resolved against the codebase, else the integration
  repo). Golden: `hello_shop_postgres` snapshot next to the tests.
- **Progress.** The spec's progress sink feeds a task that emits
  `docker.pull {stem, image, layer, status}` (already coalesced per layer
  status change) and `docker.build {stem, line}` events.
- **Lifecycle.** start/stop/wait/describe/output_stream go through the
  registry like processes; the output stream is attached to the log hub
  (`stems logs`, log files). `StemRecord.container_id` is persisted for
  every container stem. After a stop, the container is **kept** for
  `stems stop` (and `restart`), and **removed** (volumes kept) for `down`,
  daemon shutdown and `reset`. `restart` of a docker stem reuses the kept
  container through `DockerRuntime::restart(handle, spec)`: `docker
  restart` when the spec hash is unchanged, else recreate. `down` also
  removes stopped containers left by `stop` or a crash (only when they carry
  this workspace's labels) for the selected docker stems.
- **`down --volumes`** (FR-LC-2) removes the `<ws>_*` named volumes that
  the selected docker stems declare, after their containers are gone
  (`DownResult.volumes_removed`). It is destructive: the CLI needs `--yes`
  (or a `y` on a terminal), else `DESTRUCTIVE_NOT_CONFIRMED` (exit 2), like
  `reset`. Without a running daemon, `down --volumes --yes` starts one
  briefly. Bind mounts and compose volumes are never removed.
- **Status.** `status --json` shows no `pid`/`pgid` for container stems
  (Docker's pids are not local processes) and fills
  `health.container` with Docker's `State.Health.Status` (refreshed every
  second while running) for 21's `docker` probe.
- **Recovery and orphans.** Adoption as above. The orphan scan of
  `stems doctor` (and `doctor --orphans`) adds labelled containers not in
  state and containers of stems-owned compose projects when Docker is
  reachable, silently skipping them when it is not; `stems up` does the
  same for *running* containers only, and only when the selection needs
  Docker. `--yes` / `doctor --fix` remove such containers
  (`remove_container_id`, volumes kept); they are never adopted.

## Testing

Unit tests (`cargo test -p stems-runtime docker`) need no Docker: mapping
goldens, naming, volume parsing, spec-hash stability, adoption decisions on
fake inspect JSON, orphan selection, pull-progress coalescing, image
reference splitting, host resolution, log-line parsing, stop
classification, `.dockerignore` handling, and `DockerUnavailable` from a
missing socket.

The live test needs a daemon and is ignored by default:

```sh
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime docker_live_roundtrip -- --ignored
```

It starts `alpine:3`, reads its first log line, describes, adopts and
orphan-scans it, stops it (expects `Killed`: `sleep` as pid 1 ignores
SIGTERM), waits (137) and removes it with its volume.

The daemon wiring is unit-tested without Docker in
`cargo test -p stems-daemon containers` (spec goldens, error table,
events, status, lazy connect). The end-to-end scenarios are
`tests/features/docker/*.feature` (and `compose/`), tagged `@docker`:
`make e2e` skips them; `make e2e-docker` runs them serially. The harness
unit test `docker_feature_steps_are_all_defined` checks that every step of
those files exists.

## Verifying with Docker (checklist)

Nothing in this deliverable has run against a real Docker daemon (the
development machine has none). Someone with Docker Desktop, Colima or a
Linux Docker Engine plus `docker compose` v2 should run, from the
repository root:

```sh
docker pull postgres:16 && docker pull redis:7 && docker pull alpine:3 && docker pull python:3-slim
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime docker_live_roundtrip -- --ignored
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime compose_live_roundtrip -- --ignored
make e2e-docker                                   # all @docker scenarios, 300 s each
make e2e-docker FEATURE=tests/features/docker     # or one directory / file
make e2e-docker FEATURE=tests/features/scripts/hello-shop-zero-to-ready.feature
```

and check, beyond the scenarios passing:

1. `stems up` in `tests/fixtures/workspaces/docker-pg` emits `docker.pull`
   events on a cold image cache (one per layer status change, not one per
   progress tick) and the container carries `stems.workspace`,
   `stems.stem`, `stems.run_id`, `stems.spec_hash` and `team=shop`.
2. `IMAGE_PULL_FAILED`'s `details.message` is the registry's text
   (`pull access denied for stems-does-not-exist ...`).
3. `stems logs db` shows postgres's lines with Docker's timestamps, and the
   log stream ends (no busy loop) when the container stops.
4. `stems status --json` shows `health.container: "healthy"` once the
   `pg_isready` healthcheck passes (with 21's `docker` probe).
5. `stems stop db` leaves an `exited` container; `stems restart db` keeps
   the same container id (spec unchanged); editing `env` in
   `stems.local.yaml` and restarting recreates it (new id).
6. `kill -9` of the daemon, then `stems up`: `stem.adopted` with the same
   `container_id`, logs re-attached from the adoption time, one container.
7. `stems down` removes the container and keeps `docker-pg_pgdata`;
   `stems down --volumes --yes` removes it; nothing labelled
   `stems.workspace=docker-pg` is left (`docker ps -a --filter
   label=stems.workspace=docker-pg`).
8. A second `stems up` while a container with the same host port runs
   outside stems fails with `PORT_IN_USE` (Docker's `port is already
   allocated`).
9. `stems up` in `tests/fixtures/workspaces/docker-build` shows
   `docker.build` events and the image `stems/docker-build/api:<run_id>`;
   `curl localhost:<port>/healthz` answers.
10. `DOCKER_HOST=unix:///nonexistent stems up db` in `docker-mixed` still
    fails fast with `DOCKER_UNAVAILABLE` (19's preflight).

The `@docker` suite is meant for Linux CI (REQUIREMENTS §11 Q6); on macOS
use Docker Desktop or Colima locally.


## Not yet verified against a real daemon

Docker is not installed on the development machine, so these compile and
follow the API documentation but have not run (the daemon wiring above
and every `@docker` scenario included):

- connecting to Docker Desktop / Colima sockets and `DOCKER_HOST=tcp://`;
- pull (including the exact registry error text surfaced by
  `IMAGE_PULL_FAILED`) and progress coalescing on a real stream;
- build through the API with the tarred context (Dockerfile path handling,
  classic builder only, no BuildKit);
- create/start/stop/kill/remove, the 137 heuristic for `Killed`, named
  volume removal with `--volumes`;
- log following/demultiplexing and timestamp parsing on real frames, the
  stream ending when the container stops, and `since` on adoption;
- `wait` behaviour with the per-request timeout on long waits;
- restart recreate vs `docker restart`;
- orphan listing with the label filter;
- health status reporting for images with a `HEALTHCHECK`.
