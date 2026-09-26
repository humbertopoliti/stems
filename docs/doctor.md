# `stems doctor`

`stems doctor` explains why a machine is not ready to run a workspace and,
where it is safe, fixes it (FR-CL-3, FR-WD-4, FR-CR-4; deliverable 19).

```
stems doctor [--json] [--strict] [--fix [--yes] [--kill-foreign]]
stems doctor --orphans [--yes] [--kill-foreign] [--json]
```

It needs no daemon: the CLI loads the workspace and runs every check
itself. When a daemon runs, it is asked for its health (`daemon_status`)
and the stems it runs count as the owners of their ports.

Each check runs with a **2 s timeout** (a wedged Docker never hangs
`doctor`); a check that runs out reports `fail` with "timed out". Checks
run concurrently, so `doctor` takes at most about 2 s.

## Output

Human mode prints a table, then the hints of everything that is not ok,
what `--fix` did, and a summary:

```
CHECK                STATUS  MESSAGE
config               ✓ ok    stems.yaml is valid (1 stem)
daemon               ✗ fail  stale lock: the daemon (pid 42) exited without cleaning up (fixable)
requires.node        ✗ fail  `node` 20.1.0 does not satisfy the required range >=99
ports.echo-svc.http  ! warn  port 18090 is held by pid 7 (python3 -m http.server)
disk.home            ✓ ok    12.0 GB free under /tmp

hints:
  daemon: `stems doctor --fix` removes it
  requires.node: install node >=99

2 ok, 1 warn, 2 fail
```

`--json` prints the envelope with `data` = `DoctorReport`:

```json
{
  "ok": false,
  "checks": [
    { "id": "requires.node", "status": "fail", "message": "...", "hint": "...",
      "fixable": false, "details": { "tool": "node", "required": ">=99", "found": "20.1.0" },
      "code": "TOOL_VERSION" }
  ],
  "fixed": [ { "id": "daemon", "action": "remove_stale_lock", "ok": true, "message": "..." } ],
  "summary": { "ok": 2, "warn": 1, "fail": 2 }
}
```

`hint`, `details` and `code` are omitted when empty. Every failing check is
also an entry of the envelope's `errors` (its `code`, `details.check` = the
check id); with `--strict`, so is every warning that has a code.

## Status semantics and exit codes

| status | meaning |
|---|---|
| `ok` | fine |
| `warn` | works, but deserves attention (a foreign process on a port, an orphan, a stale overlay, low disk, hot-reload overlap) |
| `fail` | will not work (a tool version, Docker, a missing codebase or script, a stale lock) |

Exit **0** when nothing fails (warnings included), **1** when anything
fails. `--strict` makes warnings exit 1 too (and `data.ok` false).

## Checks

Ids are stable; `<stem>`, `<name>` and `<tool>` come from `stems.yaml`.
Only enabled stems are checked.

| id | when | ok | warn | fail | fixable |
|---|---|---|---|---|---|
| `config` | always | the workspace loads and validates (without probing tools or overlays) | | load/validation error (first one, all in `details.errors`); workspace checks are skipped if it does not load | no |
| `daemon` | always | not running (and no leftovers), or running and compatible (`details.running`, pid, version, uptime) | | stale lock (dead pid, `details.lock_state: stale`), stale socket nobody listens on, a daemon that holds the lock but does not answer, a daemon of another stems/API version (`DAEMON_VERSION_MISMATCH`) | stale lock / socket |
| `docker.reachable` | an enabled docker or compose stem | the engine answers `_ping` | | unreachable (`DOCKER_UNAVAILABLE`, hint "start Docker Desktop…") | no |
| `docker.version` | Docker reachable | engine version in `details.version` | | | no |
| `compose.version` | an enabled compose stem and Docker reachable | `docker compose` v2 | | missing, or Compose v1 (`DOCKER_UNAVAILABLE`) | no |
| `requires.<tool>` | each `requires:` entry | installed version satisfies the range | | too old/new, not installed ("not found"), unparseable output (`TOOL_VERSION`, `details {tool, required, found}`) | no |
| `ports.<stem>.<name>` | each fixed port of a non-external stem, plus `auto` ports recorded in state | free, or held by that stem (per state) | held by a process stems did not start (`details.pid`, `command`), by another stem, or by the daemon (`PORT_IN_USE`) | | no |
| `codebase.<stem>` | each stem with a codebase | exists (git: branch in `details`) | git codebase not cloned yet or not a git repo; dirty tree while the stem has a `build` script | local codebase missing (`CODEBASE_NOT_FOUND`) | no |
| `scripts.<stem>.<name>`, `scripts.workspace.<name>` | each `file:` script | exists | not executable | missing / not a file (`SCRIPT_NOT_FOUND`) | no |
| `overlays.<stem>` | each stem in the state file's overlay ledger | recorded and the stem runs, or `keep: true` | **stale**: recorded but the stem is not running; **modified** since stems wrote it | | stale |
| `orphans` | always | none | processes on declared ports not in state, stems-labelled containers not in state (containers only when Docker is needed and reachable) (`ORPHANS_FOUND`) | | when one looks like its stem's start command, or is a container |
| `disk.home` | always | ≥ 1 GB free under `$STEMS_HOME` | < 1 GB | | no |
| `disk.codebase.<stem>` | each distinct codebase | ≥ 1 GB free | < 1 GB | | no |
| `watch.hot-reload.<stem>` | each stem with `watch` | no overlap | the start command hot-reloads (`vite`, `next dev`, `nodemon`, `cargo watch`, `air`, `uvicorn --reload`, `flask run --reload`, `webpack --watch`) **and** a watch covers `src/**` or `**` (FR-WD-4) | | no |

## `--fix`

`--fix` applies the fixable items after a confirmation prompt (human mode
on a terminal) or `--yes`; without consent nothing is touched and
`data.fix_skipped` says why. After fixing, the checks run again: the report
shows the state after the fixes and `fixed` lists what was done.

| check | action(s) | what happens |
|---|---|---|
| `daemon` | `remove_stale_lock`, `remove_stale_socket` | the lock file is removed while it is still stale; a socket is removed when no live daemon holds the lock |
| `overlays.<stem>` | `remove_overlay`, `leave_modified_overlay`, `forget_overlay` | under the workspace lock (so no daemon starts meanwhile; a running daemon refuses it — stop it first): an unchanged file is deleted, a modified one is left in place, a missing one is forgotten; the ledger entries are dropped (`keep: true` ones stay) |
| `orphans` | `kill_orphan`, `remove_container` | processes that look like their stem's start command are killed (SIGTERM, 2 s, SIGKILL; `--kill-foreign`: every process orphan); stems-labelled containers not in state (stopped ones included) are removed |

Never fixed: ports held by foreign processes (unless `--kill-foreign`),
tool versions, Docker, codebases, scripts.

## `--orphans`

`stems doctor --orphans` only runs the orphan scan and decides per orphan
(adopt / kill / ignore), exactly as in deliverable 11: see
`docs/recovery.md`. Exit 3 `ORPHANS_FOUND` while some remain.

## `stems up` runs the fast subset

Before starting anything (even the daemon), `stems up` checks that Docker
is reachable when the selection (with its hard dependencies) contains a
docker or compose stem: otherwise it fails with `DOCKER_UNAVAILABLE` (exit
1, hint "start Docker Desktop…", `details.stems`). Process-only selections
never need Docker. Ports and orphans are `up`'s orphan scan
(`docs/recovery.md`); a stale lock is reclaimed by the daemon it starts.

## For later deliverables

The registry is `stems_daemon::doctor::registry`: a check implements
`stems_daemon::doctor::Check` (`id()`, `async run(ctx) -> Vec<CheckResult>`)
and runs under the same timeout. Metrics thresholds and config reload add
their checks there.
