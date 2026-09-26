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
| `healthy` | ready (see *Readiness*) | `✓` |
| `unhealthy` | running, probe failing (deliverable 21) | `✗` |
| `stopping` | SIGTERM sent, waiting for the group to exit | `↻` |
| `failed` | could not start, exited, or failed to become ready | `✗` |
| `unknown` | external stem (no probe until 21) | `?` |
| `setup`, `seeding` | scripts (deliverable 16) | `↻` |

The legal transitions are the table in
`crates/stems-daemon/src/supervisor/state.rs` (unit-tested exhaustively).
Every transition emits a `stem.state` event with `from`, `to`, `reason` and
`actor` (`cli:<user>` for what a command caused, `daemon` for what the daemon
observed, e.g. `starting → healthy` or a crash).

A process that exits while `starting` is `failed` with `START_FAILED` (exit
code and signal in `details`). One that exits later is `stopped` if its exit
code was 0 and `failed` otherwise (restart policies arrive with 22).

## Readiness (until probes land)

Deliverable 21 adds health probes. Until then one component, the `Waiter`,
decides when a started stem satisfies an edge condition:

* `started` — the process is alive;
* `healthy` — the process stayed alive for `health.start_period` and, for
  `tcp`/`http`/`grpc` checks, its port accepts TCP connections; bounded by
  `health.start_timeout` (`HEALTH_TIMEOUT`, the process is then stopped);
* `seeded` — same as `healthy` (seed scripts arrive with 16).

## Commands

### `stems up [stems…] [--detach] [--timeout D] [--no-fail-fast] [--max-parallel N] [--pass-env VARS] [--yes|--kill-orphans|--adopt-orphans] [--kill-foreign]`

Before anything starts, `up` scans the workspace's declared ports for
orphans (processes not started by stems); with orphans and no consent flag
it exits 3 `ORPHANS_FOUND` and starts nothing. A new daemon first adopts the
stems a crashed one left running. See [recovery.md](recovery.md).

1. Starts the workspace daemon if none runs (detached, own session) and
   remembers that `up` started it.
2. Re-loads and validates the workspace (config errors exit 2).
3. Plans the **closure**: the requested stems (all enabled stems if none)
   plus their hard dependencies, transitively. Unknown or disabled names are
   `UNKNOWN_STEM` (exit 2). `--profile` is deliverable 26 (`NOT_IMPLEMENTED`).
4. Starts the closure layer by layer (`start_order`). Each stem waits for
   each hard dependency's edge condition (`started`, `healthy`, `seeded`),
   then takes one of `--max-parallel` (default 4) slots, starts, and holds
   the slot until it is ready or failed. Independent stems start in parallel.
5. A stem whose dependency failed is **skipped**. With fail-fast (the
   default) the first failure also skips every stem that has not started
   yet; stems already running stay up. `--no-fail-fast` only skips the failed
   stem's dependants.
6. Already running stems are left alone (so `up` twice is a no-op).
7. External stems move to `unknown` and count as ready.

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
result envelope as the **last line** (compact). Events: `up.started {requested,
stems, layers, detach}`, `stem.state…`, `up.finished {requested, started,
failed, skipped, ok}`.

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
`--volumes` is deliverable 14.

### `stems start <stems…> [--no-deps]`

Starts the stems and their unstarted hard dependencies (`--no-deps`: only the
named stems), with the same scheduler and result as `up`. External stems:
`NOT_MANAGED` (exit 1).

### `stems stop <stems…> [--cascade]`

Stops the stems. If running stems depend on them (hard edges, transitively)
the command refuses with `HAS_DEPENDANTS` (exit 1, `details.dependants`)
unless `--cascade`, which stops those dependants first. External stems:
`NOT_MANAGED`.

### `stems restart <stems…> [--no-deps]`

Stop, then start (starting missing dependencies unless `--no-deps`); the
stem gets a new process on the same ports. The workspace is re-read first,
so config changes apply. `--build` is deliverable 16 (`NOT_IMPLEMENTED`).

### `stems status [stems…] [-v]`

`data: { stems: [ { name, type, state, glyph, reason, pid, pgid, ports:
[{name, port, auto}], uptime_s, started_at, restarts, health, error } ],
summary: { healthy, degraded, failed, stopped, unknown, starting } }` —
enabled stems in declaration order. `health` is `null` until 21; `restarts`
is 0 until 22. With `-v`/`--verbose` every running stem also has `env`: the
variables stems set for its process (config env, env files, local
overrides, `--pass-env`, `PORT`, `STEMS_*`), never the inherited daemon
environment. The human table is minimal (13 finishes it; `--watch` too).

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
  Leaving it (Ctrl-C, SIGTERM, SIGHUP) stops **nothing**; the TUI and its
  "stop everything?" prompt come with deliverable 27.
* The daemon's own exit path (`stems daemon stop`, SIGTERM/SIGINT/SIGHUP to
  the daemon) stops every running stem in reverse order before it removes
  its socket and lock, so no stem outlives an orderly daemon exit.

## Extension points

`crates/stems-daemon/src/supervisor/`: `RuntimeRegistry` (docker 14,
compose 15), `OutputSink` (logs 12), `Waiter` (probes 21), the actor's exit
handler (restart policies 22, watchdog triggers 24), `Host` (state store 11).
