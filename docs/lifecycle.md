# Lifecycle: up, down, start, stop, restart, status

The workspace daemon supervises every stem through a small state machine,
ordered by the dependency graph (REQUIREMENTS §4.5, §6.4). This page
describes what each command does, the states, how a stem's environment is
built, and the attached/detached rules. Wire shapes: [protocol.md](protocol.md).

## States

```text
                  ┌──────────────────── stop / down ─────────────────────┐
                  │                                                      ▼
Stopped ──start──▶ Starting ──ready──▶ Healthy ──stop──▶ Stopping ──▶ Stopped
   │                  ▲                  │  ▲
   └──▶ Setup ────────┘                  ▼  │ seeded
        (setup script,                 Seeding (seed script,
         stamp changed)                          stamp changed)
   │  ▲               │                  │  ▲                │
   │  │               │ exit / timeout   │  │ (probes, 21)   │ stop failed
   │  │               ▼                  ▼  │                ▼
   │  └──── down ─── Failed ◀── exit ── Unhealthy          Failed
   │                  │
   │                  └──start──▶ Starting
   ▼
Unknown   (external stems: monitored, never started)
```

| State | Meaning | Glyph |
|---|---|---|
| `stopped` | not running | `·` |
| `starting` | spawned, not ready yet | `↻` |
| `healthy` | its health check passes (see *Readiness*); `!` degraded when a warning applies ([health.md](health.md)) | `✓` / `!` |
| `unhealthy` | running, `retries` probes in a row failed ([health.md](health.md)) | `✗` |
| `stopping` | SIGTERM sent, waiting for the group to exit | `↻` |
| `failed` | could not start, exited, or failed to become ready | `✗` |
| `unknown` | external stem whose probe has not passed or cannot run | `?` |
| `setup` | the `setup` script runs (its stamp was missing or changed), before `starting` | `↻` |
| `seeding` | the `seed` script runs, after `healthy` (stamp missing or changed) | `↻` |

A stem whose start sequence is complete — including a `seed` that ran or
whose stamp was current — is `healthy` with `seeded: true` in `status`;
there is no separate "ready" state. The full order, hooks and stamps are in
[scripts.md](scripts.md). A failed `setup` is `failed` with `SETUP_FAILED`,
a failed `seed`/`pre_start`/`post_start` `failed` with `SCRIPT_FAILED` (the
process is stopped first).

The legal transitions are the table in
`crates/stems-daemon/src/supervisor/state.rs` (unit-tested exhaustively).
Every transition emits a `stem.state` event with `from`, `to`, `reason` and
`actor` (`cli:<user>` for what a command caused, `daemon` for what the daemon
observed, e.g. `starting → healthy` or a crash).

A process that exits while `starting` is `failed` with `START_FAILED` (exit
code and signal in `details`). One that exits later, without being asked to,
goes to its restart policy ([restart.md](restart.md), FR-LC-6): with the
default `on-failure` a non-zero exit is restarted with backoff (the stem is
`starting`, reason `restarting in 500ms (attempt 1)`, event
`stem.restarting`), a clean exit (code 0) is `stopped`; with `never` it is
`stopped` (code 0) or `failed`; too many restarts in `restart.window` are
`failed` with `MAX_RESTARTS` (event `stem.gave_up`). A stop, down or restart
you ask for is never undone by the policy, and it cancels a pending restart.
`healthy → starting` / `unhealthy → starting` is such a restart.

## Readiness

Since deliverable 21 readiness is the stem's health check
([health.md](health.md)). The probe task starts with the process; the first
passing probe moves `starting → healthy`, and a stem still `starting` after
`health.start_timeout` is `failed` with `START_TIMEOUT` (its process is
stopped). Edge conditions wait on the real state:

* `started` — the process is alive (spawned and not exited);
* `healthy` — the stem's health check passed (state `healthy`);
* `seeded` — `healthy`, then the stem's `seed` script finished (or its
  stamp was current): the stem actor runs `post_start` and `seed` after
  `healthy`, and the scheduler releases `condition: seeded` dependants (and
  counts the stem ready) only then. `condition: healthy` dependants start as
  soon as the stem is `healthy`.

After readiness the probes keep running: `healthy ⇄ unhealthy` transitions
(`stem.health` events) and the derived `degraded` flag are described in
[health.md](health.md).

### External stems (FR-ST-3)

A `type: external` stem is monitored, never started or stopped. Its runtime
is the no-op `ExternalRuntime` (`crates/stems-runtime/src/external.rs`,
always registered in the `RuntimeRegistry`): `start` spawns nothing and the
stem moves to `unknown`; `stop` does nothing; there is no pid, process
group, output or adoption. With a health check its probe drives `unknown ⇄
healthy/unhealthy` ([health.md](health.md)); `down` ends the monitoring
(state `stopped`).

* `up` and `down` never fail because of an external stem: `up` reports it
  in `ready` (state `unknown`), `down` in `skipped`.
* `start`, `stop` and `restart` of an external stem fail with `NOT_MANAGED`
  (exit 1, hint "start it yourself; stems only monitors it").
* Edges to an external stem: `condition: started` is satisfied immediately.
  `condition: healthy` (and `seeded`) waits for the external's probe to
  pass, bounded by its `health.start_timeout` (then the external is reported
  `failed` in `up` with `HEALTH_TIMEOUT` and its dependants are skipped; its
  state stays `unknown`/`unhealthy`). An external stem **without** a health
  check stays `unknown` and such edges are satisfied immediately — stems
  cannot tell whether the service is up.

## Commands

### `stems up [stems…] [--detach] [--fresh] [--timeout D] [--no-fail-fast] [--max-parallel N] [--pass-env VARS] [--yes|--kill-orphans|--adopt-orphans] [--kill-foreign]`

Watchdogs (24, [watchdogs.md](watchdogs.md)): a stem's `watch:` rules start
watching when its start begins (after a missing git clone, before `setup`),
so edits during a slow start count, and stop when it becomes `stopped` or
`failed`. `up --no-watch` starts no watchers (and stops running ones) until
the next `up` without it; `status` shows `watch: {paused, rules}`.

Before anything starts, `up` scans the workspace's declared ports for
orphans (processes not started by stems); with orphans and no consent flag
it exits 3 `ORPHANS_FOUND` and starts nothing. A new daemon first adopts the
stems a crashed one left running. See [recovery.md](recovery.md). Even
earlier (before the daemon starts), a selection that needs a docker or
compose stem fails with `DOCKER_UNAVAILABLE` when Docker is not reachable
(the fast `doctor` subset, [doctor.md](doctor.md)); process-only selections
never need Docker.

1. Starts the workspace daemon if none runs (detached, own session) and
   remembers that `up` started it.
2. Re-loads and validates the workspace (config errors exit 2).
3. Plans the **closure**: the requested stems (if none: the profile from
   `--profile` / `STEMS_PROFILE` / `profile:` / `default_profile` / a profile
   named `default`, else all enabled stems) plus their hard dependencies,
   transitively (`stems_core::Selection`, 26; rules in `docs/config.md`).
   Unknown or disabled names are `UNKNOWN_STEM`, an unknown profile
   `UNKNOWN_PROFILE`, a strict profile missing a dependency
   `PROFILE_MISSING_DEPENDENCY` (all exit 2).
3a. Runs the workspace `bootstrap` script, if any (a failure aborts `up`
   with `SETUP_FAILED`, exit 1, before any stem starts). With `--fresh`,
   stops the planned stems that run, runs their `reset` scripts and clears
   their stamps, so `setup` and `seed` run again ([scripts.md](scripts.md)).
4. Starts the closure layer by layer (`start_order`). Each stem waits for
   each hard dependency's edge condition (`started`, `healthy`, `seeded`),
   then takes one of `--max-parallel` (default 4) slots, starts, and holds
   the slot until it is ready or failed. Independent stems start in parallel.
   A stem's start is its whole script sequence: `setup` (stamped),
   `pre_start`, the process, readiness, `post_start`, `seed` (stamped).
5. A stem whose dependency failed is **skipped**. With fail-fast (the
   default) the first failure also skips every stem that has not started
   yet; stems already running stay up. `--no-fail-fast` only skips the failed
   stem's dependants.
6. Already running stems are left alone (so `up` twice is a no-op).
7. External stems move to `unknown` and count as ready — unless a planned
   dependant needs them `healthy` and they have a health check: then they
   are ready once their probe passes (see *External stems*).

Before spawning a process stem the daemon checks each declared port: a port
someone listens on fails the stem with `PORT_IN_USE`, naming the listener's
pid and command (the foreign process is never touched). `port: auto` ports
are allocated from the ephemeral range on first use (`stem.port_allocated`)
and kept for the daemon's lifetime, including across `restart`.

Result (`data`): `{ ok, requested, ready: [stem], failed: [{stem, error}],
skipped: [stem] }`; exit **0** when everything is ready, **3** when some
stems are ready and some failed or were skipped, **1** when none is ready.
Each failure's error is also in the envelope's `errors`.

Progress: human mode prints one line per `stem.state` (and port allocation);
`--json` prints every event as an NDJSON line while `up` runs and then the
result envelope as the **last line** (compact). Events: `profile.expanded
{profile, added, requested}` (when a profile's hard dependencies were added;
see `docs/config.md`), `up.started {profile, requested, stems, layers, detach,
fresh}`, `stem.state…`, `script.started` /
`script.finished`, `up.finished {requested, started, failed, skipped, ok}`.

### `stems down [stems…] [--all] [--timeout D]`

Stops the given stems (default: every running or failed stem) in reverse
dependency order, stems of one layer in parallel: SIGTERM to the process
group, `stop_grace` (or `--timeout`), then SIGKILL. Stopped stems are listed
as `stopped`, stems that were not running as `skipped`; external stems are
never touched. An in-flight `up` is cancelled first.

The daemon then shuts down if `--all` was given, or if nothing runs any more
and the daemon was auto-started by `stems up` (`data.daemon_stopping`); the
command waits until its socket and lock are gone. With no daemon running,
`down` exits 4 (`DAEMON_NOT_RUNNING`) — unless `state.json` lists stems of a
crashed daemon: then it starts a daemon, which adopts them, stops them all
and exits (`data.recovered: true`; [recovery.md](recovery.md)).
Docker and compose stems' containers are removed (volumes kept); with
`--volumes` the selected docker stems' `<ws>_*` named volumes are removed
too (`data.volumes_removed`). `--volumes` is destructive: it needs `--yes`
(or a `y` on a terminal), else `DESTRUCTIVE_NOT_CONFIRMED` (exit 2); with no
daemon running it starts one briefly ([docker.md](docker.md)). Stems with `pre_stop`/`post_stop` hooks or a
custom `stop` script run them around the stop ([scripts.md](scripts.md)).
`--all` ends with the workspace `teardown` script; a failing teardown is
reported in `failed` as stem `_workspace` and the daemon still stops.

### `stems start <stems…> [--no-deps]`

Starts the stems and their unstarted hard dependencies (`--no-deps`: only the
named stems), with the same scheduler and result as `up`. External stems:
`NOT_MANAGED` (exit 1).

### `stems stop <stems…> [--cascade]`

Stops the stems. If running stems depend on them (hard edges, transitively)
the command refuses with `HAS_DEPENDANTS` (exit 1, `details.dependants`)
unless `--cascade`, which stops those dependants first. External stems:
`NOT_MANAGED`.

### `stems restart <stems…> [--no-deps] [--build] [--cascade | --no-cascade]`

Stop, then start (starting missing dependencies unless `--no-deps`); the
stem gets a new process on the same ports. A user restart also resets the
restart policy's window and backoff (the lifetime `restarts` count stays). The workspace is re-read first,
so config changes apply. `--build` runs each stem's `build` script between
the stop and the start (a failed build is `SCRIPT_FAILED` and nothing is
started); stems without one just restart. See [scripts.md](scripts.md).
`--cascade` (or `restart.cascade: true` on the stem, unless
`--no-cascade`) then restarts the stems' running hard dependants, layer by
layer, once the stems are healthy again; `data.cascade` lists them per
layer (FR-LC-9, [restart.md](restart.md#cascading-restarts-fr-lc-9)).

### `stems status [stems…] [--watch [INTERVAL]] [-v]`

`data: { stems: [ { name, type, state, glyph, reason, pid, pgid, ports:
[{name, port, auto}], uptime_s, started_at, restarts, restarts_in_window, seeded, degraded, health, error } ],
summary: { healthy, degraded, failed, unhealthy, stopped, unknown, starting } }` —
enabled stems in declaration order (only the named ones when given;
`UNKNOWN_STEM` otherwise). `summary` counts stems per glyph, except that
`unhealthy` stems (running, or external, with a failing probe) are counted
apart from `failed` although both show `✗`; `starting`
counts every transitional state (`setup`, `starting`, `seeding`,
`stopping`). `health` is `{type, last: {ts, ok, outcome, latency_ms,
detail}, consecutive_failures, transitions_60s}` (`null` without a health
check); `degraded` and `reason` follow [health.md](health.md) (`reason` is
empty for a plain healthy stem). `restarts` counts policy restarts since
the daemon started and `restarts_in_window` those within `restart.window`
(3 or more degrade a healthy stem, reason `restarts (N recently)`;
[restart.md](restart.md)). With
`-v`/`--verbose` every running stem also has `env`: the variables stems set
for its process (config env, env files, local overrides, `--pass-env`,
`PORT`, `STEMS_*`), never the inherited daemon environment.

Human output is a table and a summary line:

```
STEM    TYPE      STATUS       REASON                       PID    PORTS       UPTIME  RESTARTS
api     process   ! healthy    dependency hosted unhealthy  41234  http:18601  2m05s   0
web     process   ✓ healthy    -                            41240  http:18602  2m03s   0
hosted  external  ✗ unhealthy  connection refused           -      -           -       0

1 healthy, 1 degraded, 0 failed, 1 unhealthy, 0 starting, 0 stopped, 0 unknown
```

(When the summary line is wider than the terminal only its non-zero
counts are shown: `1 healthy, 1 degraded, 1 unhealthy`.)

* `STATUS` is the glyph and the state. Glyphs (`stems_core::Glyph`, shared
  with `graph` and the TUI): `✓` healthy (green), `!` degraded (yellow), `✗`
  failed/unhealthy (red), `·` stopped (grey), `?` unknown (magenta), `↻`
  setup/starting/seeding/stopping (cyan). The ASCII fallback — `OK`, `WARN`,
  `FAIL`, `-`, `?`, `..` — is used with `--no-color` (or `STEMS_NO_COLOR`),
  `STEMS_ASCII=1`, or when the locale (`LC_ALL`, `LC_CTYPE`, `LANG`) is not
  UTF-8. Colour is only used on a terminal.
* `PORTS` is `name:port` (`name:auto` for an `auto` port not allocated yet).
* Width-aware: stem names are cut at 24 characters and reasons at 40 (with
  `…`, `...` in ASCII); with `COLUMNS` set or on a terminal, `REASON`, then
  `STEM`, then `PORTS` shrink further so a row fits.
* `--watch [INTERVAL]` (default `1s`; a bare number is seconds, e.g.
  `--watch 0.5`; minimum 100 ms) redraws until Ctrl-C/SIGTERM (exit 0). On
  a terminal the screen is cleared before each frame; on a pipe frames are
  separated by a blank line. In JSON mode each frame is one NDJSON line
  holding `data` (no envelope). If a refresh fails (the daemon stopped), the
  error is shown in that frame and the next refresh reconnects; only a
  failing first refresh ends the command (e.g. exit 4).

## A stem's environment (FR-ST-4)

Layers, later wins:

1. the daemon's environment, minus `STEMS_*` variables;
2. `PORT` = the stem's first declared host port (fixed or allocated), when it
   declares ports — so `port: auto` reaches the process;
3. workspace `env`, then the stem's `env`;
4. `env_files`, in order (`KEY=value` lines, `export ` prefix and matching
   quotes allowed, `#` comments; a missing file is `START_FAILED`);
5. `stems.local.yaml` env (workspace-level and the stem's);
6. the shell environment of `stems up --pass-env VAR,…` (captured by the CLI);
7. `STEMS_STEM`, `STEMS_WORKSPACE` (integration repo root), `STEMS_CODEBASE`
   (when the stem has a codebase), `STEMS_RUN_ID` (per daemon run) and
   `STEMS_<DEP>_PORT` (first port of each dependency, name upper-cased with
   `-` → `_`, e.g. `STEMS_SHOP_API_PORT`).

Finally every value is re-rendered: `${stem.<n>.port}`,
`${stem.<n>.ports.<p>}` and `${stem.self.…}` references to `port: auto`
ports, which the config loader leaves in place, become the allocated ports
(allocating on demand). Health `url`/`port` references are rendered the same
way.

The process runs `<shell> -c <command>` (`shell` defaults to `/bin/sh`;
`command`, else `scripts.start`) in the stem's `cwd`, in its own session and
process group ([process-model.md](process-model.md)).

## Attached and detached

* `stems up --detach` returns after the result.
* `stems up` (attached, the default) prints the result and keeps streaming
  progress until **SIGINT (Ctrl-C), SIGTERM, SIGHUP**, or **EOF on stdin**
  (only when stdin is a terminal or a pipe; `/dev/null` or a file never
  counts). It then runs `down --all` with a hard deadline of the sum of the
  enabled stems' `stop_grace` + 5 s, waits for the daemon to exit, and exits
  0. A signal during `up` itself tears down the same way (the in-flight `up`
  is cancelled). If the daemon goes away on its own (e.g. `stems down --all`
  from another terminal) the attached command exits 0.
* `stems attach` follows a running daemon (human: the status table, then one
  line per transition; `--json`: NDJSON events, then a last-line envelope).
  Leaving it (Ctrl-C, SIGTERM, SIGHUP) stops **nothing**.
* On a terminal (human mode) both open the dashboard instead of the plain
  stream (deliverable 27, [tui.md](tui.md)): attached `up` asks "Stop
  everything? [y/N/d(etach)]" on `q`/Ctrl-C (`y` = the `down --all`
  teardown above, `d` = leave the daemon running); `attach` just leaves.
  `STEMS_TUI=0`, `--json` or a non-terminal stdout keep the plain stream.
* The daemon's own exit path (`stems daemon stop`, SIGTERM/SIGINT/SIGHUP to
  the daemon) stops every running stem in reverse order before it removes
  its socket and lock, so no stem outlives an orderly daemon exit.

## Extension points

`crates/stems-daemon/src/supervisor/`: `RuntimeRegistry` (docker 14,
compose 15), `OutputSink` (logs 12), `Waiter` + `probes` (health 21), the actor's exit
handler (restart policies 22, `supervisor/restart.rs`; watchdogs 24,
`supervisor/watch.rs`, call `StemCell::restart_bypassing_policy`), `Host` (state store 11).
