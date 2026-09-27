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

## Finding the docker CLI

The Engine API needs no CLI, but compose stems, `doctor`/`validate`'s
compose check, `command` health probes of container stems (`docker exec`)
and scripts such as hello-shop's `seed` do. Docker Desktop does not always
put `docker` on `PATH` (no `/usr/local/bin` symlinks, or a daemon started
from a minimal environment), so one helper,
`stems_runtime::docker::docker_cli_path()`, looks in this order:

1. `$STEMS_DOCKER_CLI` when set and non-empty, used as-is (no search; the
   e2e suite points it at a missing file to simulate a machine without
   Docker);
2. `docker` on `PATH`;
3. `/Applications/Docker.app/Contents/Resources/bin/docker` (Docker
   Desktop), `/usr/local/bin/docker`, `/opt/homebrew/bin/docker`,
   `~/.rd/bin/docker` (Rancher Desktop),
   `/Applications/OrbStack.app/Contents/MacOS/xbin/docker`.

Users of it: the compose runtime's binary (`ComposeOptions::new`), `stems
validate`'s `TOOL_VERSION` check, `doctor`'s `compose.version` (its
`details.cli` shows which CLI ran), and the e2e harness (`docker` steps,
the leak hook, and the `PATH` of every command it runs). The daemon's base
environment (inherited by scripts, health commands and process stems)
gets the CLI's directory prepended to `PATH` when `docker` is not already
on it (`ensure_docker_on_path`), so `docker exec ...` in a reset/seed
script works from a daemon started without Docker's bin directory on
`PATH`. Docker stems themselves never inherit the daemon's environment.

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
| `remove(handle, volumes)` | `DELETE /containers/<id>?force=1&v=1`: the container's *anonymous* volumes (an image `VOLUME` such as redis's `/data`) always go with it (`v` never touches named volumes); with `volumes` also every named volume mounted by it whose name starts with `<ws>_` (read from the container's `stems.workspace` label; a user's own volumes and bind mounts are never removed). Then each network it was on is removed when stems created it for this workspace (label `stems.workspace=<ws>`) and no container is attached any more (`network_removable`), so the last `down` also removes `<ws>_net`. This is `down` / `down --volumes` (FR-LC-2) |
| failed start | a pull, build, create or start failure removes the network the attempt created when nothing else uses it (a created-but-never-started container is removed too) |
| `restart(handle, spec)` | recreate (stop, remove keeping named volumes and the network, start) when `spec.spec_hash()` differs from the container's `stems.spec_hash` label, else `docker restart -t ceil(grace)`. The old handle is released and a new one returned either way |
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
  daemon shutdown and `reset`. A container whose start failed (readiness
  timeout) or whose crash the restart policy gave up on is kept stopped for
  inspection until `down`, or until the daemon shuts down (then it is
  removed too). `restart` of a docker stem reuses the kept
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

It starts `alpine:3`, reads its first log line, describes it, reads its
named volume's size, adopts and orphan-scans it, stops it (expects
`Killed`: `sleep` as pid 1 ignores SIGTERM), waits (137), starts it again
over the stopped leftover, and removes it with its volume, checking that
neither the volume nor `<ws>_net` is left.

The daemon wiring is unit-tested without Docker in
`cargo test -p stems-daemon containers` (spec goldens, error table,
events, status, lazy connect). The end-to-end scenarios are
`tests/features/docker/*.feature` (and `compose/`), tagged `@docker`:
`make e2e` skips them; `make e2e-docker` runs them serially. The harness
unit test `docker_feature_steps_are_all_defined` checks that every step of
those files exists.

## Verified against Docker (2026-09-27)

Run on macOS 26 (Intel) with Docker Desktop: Engine 29.8.0 (API 1.53),
compose v5.5.1, socket `~/.docker/run/docker.sock` (found through the
`/var/run/docker.sock` symlink by the default socket search), the `docker`
CLI **not** on `PATH` (found by `docker_cli_path` in the app bundle):

```sh
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime docker_live_roundtrip -- --ignored   # ok
STEMS_TEST_DOCKER=1 cargo test -p stems-runtime compose_live_roundtrip -- --ignored  # ok
make e2e-docker TAGS=@docker     # 22 scenarios, all pass (twice in a row)
make e2e-docker                  # the whole suite with Docker
```

The live test now also checks `volume_sizes` (metrics `--disk`), a fresh
start over a stopped leftover container, and that nothing (container,
named volume, `<ws>_net`) is left. Checklist results:

1. Cold pull: one `docker.pull` event per layer status change (8 for
   `busybox:1.37`); labels `stems.workspace`, `stems.stem`, `stems.run_id`,
   `stems.spec_hash` and the user's `team=shop` are on the container. ✓
2. `IMAGE_PULL_FAILED`'s `details.message` is the registry's text: `pull
   access denied for stems-does-not-exist, repository does not exist or may
   require 'docker login': denied: requested access to the resource is
   denied`. ✓
3. `stems logs` shows the container's lines with Docker's timestamps; the
   stream ends when the container stops. ✓
4. `health.container` / the `docker` probe (`health/docker.feature`). ✓
5. `stems stop` leaves an `exited` container; `stems restart` of a running
   stem keeps the container id; a changed `env` recreates it (new id); `stems
   start` after `stop` replaces the stopped leftover. ✓
6. `kill -9` of the daemon, then `stems up`: `stem.adopted` with the same
   `container_id` (`docker/adopt.feature`, `compose/adopt.feature`). ✓
7. `down` keeps `<ws>_pgdata`, `down --volumes --yes` removes it; no
   container, anonymous volume or `<ws>_net` is left. ✓
8. A host port taken by another container: `PORT_IN_USE`, reported by stems'
   own port preflight (it names `com.docker.backend`, Docker Desktop's port
   forwarder) before Docker's `port is already allocated` is reached. ✓
9. `docker-build`: `docker.build` events, image `stems/docker-build/api:
   <run_id>`, `/healthz` answers (`docker/build.feature`,
   `watch/docker-rebuild.feature`). ✓
10. `DOCKER_HOST=unix:///nonexistent`: `DOCKER_UNAVAILABLE` (doctor
    scenario). ✓

### What failed on first contact, and the fixes

- `metrics --disk` never reported `volumes_bytes`: bollard cannot
  URL-encode `system/df`'s `type` list (every call failed with "Unable to
  URLEncode"), and Engine 29 fills `VolumeUsage.Items` only with `verbose`.
  Now `GET /system/df?verbose=1` without `type` (`df_volume_options`).
- Removing a container left its **anonymous volumes** (redis's and
  postgres's image `VOLUME`s) and the workspace network `<ws>_net` (and a
  compose project's `<project>_default`) behind. Removal now passes `v=1`
  (compose: `rm -f -s -v`), prunes an unused stems-labelled network, and
  `down`s an emptied stems-owned compose project; a failed pull/build/start
  prunes the network it created.
- A failed or given-up stem's stopped container outlived the daemon; it is
  now removed on daemon shutdown too.
- The `docker` CLI was not found when it is not on `PATH` (Docker Desktop
  without its symlinks): see "Finding the docker CLI".

### Not verified

- `DOCKER_HOST=tcp://` and Colima/OrbStack/Rancher sockets (only Docker
  Desktop's socket was available).
- BuildKit (the build uses the classic builder through the API).
- `wait` on a container that runs longer than the per-request timeout
  (the retry path) was not exercised deliberately.
- Linux CI: the suite has run on macOS only.
