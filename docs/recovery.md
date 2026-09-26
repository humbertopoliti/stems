# Crash recovery and orphans

The daemon remembers what it runs, so a crash (or `kill -9`) of the daemon
never strands stems: the next daemon **adopts** whatever is still alive, and
`stems down` always cleans up (FR-CR-1..4, FR-CR-6, NFR-2, NFR-3;
deliverable 11). Processes nobody recorded but that sit on the workspace's
ports are **orphans**: stems reports them and only kills them with consent.

## The state file

`<STEMS_HOME>/<ws-hash>/state.json` (next to the socket and lock, see
[protocol.md](protocol.md#where-the-daemon-lives)):

```json
{
  "version": 1,
  "run_id": "01J8ZQ7Y6M5X4W3V2T1S0R9Q8P",
  "daemon": { "pid": 4242, "start_time": 1790000000000 },
  "stems": {
    "echo-svc": {
      "pid": 5001,
      "pgid": 5001,
      "start_time": 1790000000500,
      "container_id": null,
      "ports": [{ "name": "http", "port": 18090, "auto": false }],
      "overlays": [],
      "state": "healthy",
      "started_at": "2026-09-26T12:00:00Z",
      "log_file": null
    }
  },
  "stamps": { "echo-svc": { "setup": { "hash": "…", "computed_at": "…", "inputs": [] } } }
}
```

| Field | Meaning |
|---|---|
| `version` | schema version (1) |
| `run_id` | ULID of the daemon run that wrote the file (`STEMS_RUN_ID` of its stems) |
| `daemon` | pid and start time of that daemon |
| `stems.<name>` | one entry per stem with a live unit: leader `pid`, process group `pgid`, leader `start_time` (opaque, OS-specific; see [process-model.md](process-model.md#start-time-verification-pid-reuse-defence)), `container_id` (docker/compose, 14), host `ports` (`auto` = allocated for `port: auto`), materialised `overlays` (18), last known `state`, `started_at`, `log_file` (12) |
| `stamps` | script stamps (16), carried over from run to run |

**When it is written.** On every transition of a stem with a live unit
(started, ready, stopping, stopped/failed — the entry is removed when the
unit is gone), when ports are allocated (they are part of the entry), and when
stamps or overlays change. A new daemon rewrites it once at startup, after
recovery.

**How.** Atomically: the document goes to `state.json.tmp-<pid>` in the same
directory, is `fsync`ed, renamed over `state.json`, and the directory is
`fsync`ed. A reader — or the daemon started after a `kill -9` — sees either
the previous or the next complete document, never a torn one; a leftover temp
file is ignored. A file that does not parse (disk trouble, a hand edit) is
renamed to `state.json.corrupt-<timestamp>` with a warning in `stemsd.log`
and the daemon starts empty (the orphan scan then finds what was running).

Only the daemon holding the workspace lock writes the file; the CLI only
reads it (hints, `daemon status`, the orphan scan).

## Verification and adoption (daemon start)

Before the daemon serves its socket (so no request can race recovery), after
loading the workspace:

1. For every entry: `Runtime::adopt(AdoptRecord { pid, pgid, start_time,
   container_id })`. A process is adopted only if the pid is alive, is not a
   zombie, **has the recorded start time** (a recycled pid has a different
   one) and still leads the recorded process group. One start-time read per
   entry; containers are verified by their runtime (14).
2. **Alive** → the stem's actor gets a `Handle::Adopted` and moves
   `stopped → healthy` with reason `adopted`; events `stem.adopted {pid,
   pgid, started_at}` and `stem.state`. Its `auto` ports go back into the
   port book, so dependants started later get the same `PORT` /
   `${stem.x.port}` values.
3. **Dead or unverifiable** → the entry is dropped with `stem.recovered_dead
   {pid, pgid, start_time, container_id}`. The process behind a recycled pid
   is never signalled.
4. The file is rewritten (new `run_id`, the adopted entries, the stamps).

Adopted stems behave like any other for `status` (original pid, `uptime_s`
from the recorded start), `stop`, `restart`, `down` and daemon shutdown:
signals go to the process group; exit is detected by polling liveness. They
have **no output stream** — the old daemon owned the pipes — so `logs` shows
their file history only ([logs.md](logs.md)); the exit code of an adopted
process is unknown (it ends `failed` with reason `exited`).

## `stems down` without a daemon

If no daemon answers but `state.json` lists stems (a crashed daemon), `stems
down` starts a daemon (which adopts what is alive), runs `down --all` (or
`down <stems>` when stems are named) and waits for it to exit. The result
has `data.recovered: true`. Without state entries it still exits 4
`DAEMON_NOT_RUNNING`. Only `down` recovers; other commands just explain:

```text
DAEMON_NOT_RUNNING: no stems daemon is running for this workspace
hint: stems from a previous run are still alive (echo-svc) (the daemon, pid 4242,
      crashed or was killed); run `stems down` to stop them
```

(`details.state = {path, alive: [stem]}`.) `stems daemon status --json` has
`data.state = {run_id, stems, alive, path}` (or `null`) whether or not a
daemon runs.

## Orphans

An **orphan** is a process listening on a port declared by an enabled
`process` stem (fixed ports, plus `auto` ports recorded in state) that is not
part of a recorded stem's process group and is not the daemon. Typical
causes: a service started by hand, a stem of a run whose state was lost, an
unrelated program on the same port. Containers labelled for the workspace
but missing from state are the docker runtime's part (14:
`Runtime::scan_orphans`).

Each orphan (JSON):

```json
{ "kind": "process", "port": 18090, "pid": 7001, "pgid": 7001,
  "command": "python3 app.py", "container_id": null, "stem": "echo-svc",
  "matches_start_command": true, "action": "killed" }
```

### The start-command heuristic

`matches_start_command` says the orphan looks like the stem's own start
command. The command (`command`, else `scripts.start`) is reduced to its
program and first argument, skipping leading `VAR=value` and `exec`/`env`
(`python3 app.py` → program `python`, argument `app.py`). A listener matches
when its argv[0] basename has the same normalised name (lowercase, trailing
version digits and dots removed, so `python3`, `python3.13` and macOS's
framework `…/Python` agree) and, if there is a first argument, some later
argument equals it or ends with `/<it>`. Wrappers that exec something else
(`npm start` → `node`, `uv run` → `python`) do not match — the heuristic errs
on the side of *not* killing.

### Policy

| | orphan matching its start command | any other orphan |
|---|---|---|
| no flag, `--json` or no terminal | reported: `ORPHANS_FOUND`, exit 3 | reported |
| no flag, human on a terminal | prompt `adopt / kill / ignore` | prompt `kill / ignore` |
| `--yes` / `--kill-orphans` | killed | ignored (left running) |
| `--adopt-orphans` (`up`) | adopted as its stem | ignored |
| `--kill-foreign` | killed (adopted with `--adopt-orphans`) | killed |

stems never kills a process it did not start without one of these flags or
an answer at the prompt (NFR-3). Killing: SIGTERM to the process group when
the orphan leads one (as stems-started processes do), else to the process
alone; SIGKILL after 2 s. Adopting registers the process as the stem
(`stem.adopted`, state `healthy`, reason `adopted`), exactly like crash
recovery, through the `adopt_orphans` RPC.

### `stems up`

Scans automatically once the daemon is running (after it adopted what the
state file lists). With orphans and no consent, **nothing is started**: exit 3
`ORPHANS_FOUND`, `details.orphans` lists them (each with `action: "none"`),
and a daemon auto-started by this `up` shuts down again. With consent the
orphans are handled first, `up` proceeds, and `data.orphans` lists them with
their `action`; a stem whose port is still taken by an ignored orphan fails
with `PORT_IN_USE`.

### `stems doctor --orphans [--yes] [--kill-foreign] [--json]`

The first real `doctor` check (the rest of `doctor` is deliverable 19).
`data: {orphans: [Orphan + {action, error?}], remaining}`. Exit 0 when no
orphan remains (none found, or all killed/adopted); exit 3 `ORPHANS_FOUND`
(`details.orphans`: the remaining ones) otherwise. `action` is `killed`,
`adopted` (interactive only, needs a running daemon), `ignored`,
`kill_failed` or `adopt_failed` (with `error`).

## Limitations

* A stem spawned in the instant between `fork` and the state write, when the
  daemon is killed exactly then, is not recorded. It is an orphan matching
  its start command (if it declares a port), so `stems doctor --orphans
  --yes` removes it.
* Stems without ports are invisible to the orphan scan.
* Adopted processes lose their stdout/stderr pipe; a program that exits on
  `EPIPE` may die on its next write (it then shows as `failed`).
