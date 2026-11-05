# Scripts: lifecycle, custom, hooks and stamps

A stem's `scripts:` map holds its **lifecycle scripts** (fixed names, run by
stems at fixed points) and any number of **custom scripts** (run with `stems
run`, FR-SC-2). The workspace has two lifecycle scripts of its own,
`bootstrap` and `teardown`, and named workspace scripts (FR-SC-1, FR-SC-6).
This page covers when each one runs, the environment it gets, stamps,
custom scripts and their arguments, timeouts, retries and failures. States and commands:
[lifecycle.md](lifecycle.md); logs: [logs.md](logs.md).

```yaml
scripts:                       # workspace scripts
  bootstrap: scripts/bootstrap.sh
  teardown: scripts/teardown.sh
stems:
  shop-api:
    scripts:
      setup:     { file: scripts/shop-api/setup.sh, inputs: ["requirements.txt"], stamp_env: [PYTHON] }
      pre_start: echo about to start
      start:     python3 app.py
      post_start: ./warm-cache.sh
      seed:      { command: "python3 seed.py", timeout: 60s }
      pre_stop:  echo stopping
      stop:      { command: "curl -s -X POST localhost:$PORT/shutdown" }
      post_stop: echo stopped
      build:     make
      reset:     rm -rf "$STEMS_STATE_DIR/venv"
```

A script is an inline shell `command:` (or a bare string), or a `file:` in
the **integration repo** (FR-WS-2). Per script: `inputs` (stamp globs,
relative to the codebase), `stamp_env` (variables hashed into the stamp),
`timeout` (default none), `retries` (default 0), `soft_fail` (default
false; [Partial installs](#partial-installs-soft_fail)), `when` (`build`
only, default `on_demand`; [`build` in the start sequence](#build-in-the-start-sequence-when)),
`cwd`; and for scripts run
with `stems run`: `description`, `args`, `requires`, `requires_stopped`,
`concurrent` (see
[Custom scripts](#custom-scripts-and-stems-run)).

## The lifecycle order

```text
stems up
│
├─ workspace bootstrap                      once per `up`, before any stem
├─ (--fresh) stop + reset + clear stamps    for every planned stem
│
└─ per stem, in dependency order (layers in parallel):
   │
   ├─ [build images] (build.before_setup) docker/compose images built
   │               first; `setup` gets STEMS_IMAGE (compose: STEMS_IMAGES
   │               too; docs/docker.md, docs/compose.md)
   ├─ setup        state `setup`    only if its stamp is missing or changed
   ├─ build        only with `when: changed` (stamp missing or changed)
   │               or `when: always`; setup installs what build needs
   ├─ pre_start
   ├─ start        state `starting` the process (`command` / `scripts.start`)
   ├─ wait         → `healthy`       condition: healthy dependants may start
   ├─ post_start
   ├─ seed         state `seeding`  only if its stamp is missing or changed
   └─ ready        `healthy`, `seeded: true`
                                     condition: seeded dependants may start

stems stop / down / restart (per stem, reverse dependency order)
│
├─ state `stopping`
├─ pre_stop
├─ stop script if the stem has one, else SIGTERM to the process group;
│  SIGKILL for whatever is left after `stop_grace`
├─ post_stop
└─ state `stopped`

stems down --all → … → workspace teardown → the daemon exits
```

`restart --build` runs `build` between the stop and the start; `stems build`
runs it without restarting. `stems reset` stops the stems, runs `reset` and
clears their stamps. `health` is reserved for the `health: {type: command}`
probe (its output is logged under that name; [health.md](health.md)).

**`stems scripts --lifecycle` prints this** as a table — every hook in this
order, `stem` or `workspace`, when it runs and which stems (or the
workspace) define it, set or not — and `--json` gives `data: {lifecycle:
[{name, scope, when, defined_by}]}`. The same names are under `stems
--help`, in the long help of `stems run`/`scripts`/`build`/`reset`, in
`stems scripts --json` (`data.lifecycle_hooks`) and in the MCP `list_stems`
result (`lifecycle_hooks`), all generated from one table
(`stems_config::lifecycle::HOOKS`), so they cannot drift. Every other name
under `scripts:` is a custom script.

### `build` in the start sequence (`when`)

`build` runs on demand by default (`stems build`, `restart --build`, watch
`rebuild`). `when:` on a `build` script (and nowhere else) changes that:

| `when` | `build` also runs |
|---|---|
| `on_demand` (default) | never in a start |
| `changed` | in every start, after `setup` and before `pre_start`, when its stamp (text, `inputs`, `stamp_env`; key `build`, listed by `stems stamps`) is missing or changed |
| `always` | in every start, after `setup` and before `pre_start` |

Never on a policy respawn. A failure is `SCRIPT_FAILED` and the stem does not
start, unless the script is `soft_fail`. `stems build` and `restart --build`
run it regardless of `when` (a successful `stems build` of a `changed` script
records its stamp).

## Environment (FR-SC-3)

A stem script gets the stem's resolved environment
([lifecycle.md](lifecycle.md#a-stems-environment-fr-st-4): the environment
of the `stems` command that ran it, minus `STEMS_*`, then `PORT`, workspace
and stem `env`, `env_files`, `stems.local.yaml`, `--pass-env`) plus:

| Variable | Value |
|---|---|
| `STEMS_STEM` | the stem's name |
| `STEMS_CODEBASE` | the codebase directory (absolute), when the stem has one |
| `STEMS_WORKSPACE` | the integration repo root (the directory of `stems.yaml`) |
| `STEMS_RUN_ID` | the daemon run's id (a ULID) |
| `STEMS_STATE_DIR` | `$STEMS_HOME/<ws-hash>/stems/<stem>/`, created before the script runs: the place for venvs, caches, build output — never the codebase |
| `STEMS_SCRIPT` | the script's name (`setup`, `seed`, …) |
| `STEMS_<DEP>_PORT` | the primary host port of each dependency (`shop-api` → `STEMS_SHOP_API_PORT`) |
| `PORT_<NAME>` | each explicitly named port of the stem itself (`{name: metrics, port: 9100}` → `PORT_METRICS`); `PORT` stays the primary |
| `STEMS_<DEP>_PORT_<NAME>` | each explicitly named port of each dependency (`shop-api`'s `http` → `STEMS_SHOP_API_PORT_HTTP`); see [ports.md](ports.md) |
| `STEMS_<DEP>_CODEBASE` | the codebase directory (absolute) of each dependency that has one (`shop-api` → `STEMS_SHOP_API_CODEBASE`): no need to read `stems show --json` for a sibling repo's path |
| `STEMS_<DEP>_OUTPUT_<X>` | each evaluated output `X` of each dependency (`shop-api`'s `TOKEN` → `STEMS_SHOP_API_OUTPUT_TOKEN`; see [config.md](config.md#outputs-fr-st-6)) |
| `PORT` | the stem's own primary host port (when it declares ports) |

Workspace scripts get the caller's environment (minus `STEMS_*`), workspace
`env`, `--pass-env`, `STEMS_WORKSPACE`, `STEMS_RUN_ID`, `STEMS_SCRIPT` and
`STEMS_STATE_DIR` = `$STEMS_HOME/<ws-hash>/workspace/`.

**Whose environment.** The base layer is the environment of the `stems`
command that asked: `FOO=1 stems run api seed` sees `FOO=1` even if the
daemon was started from another shell. The scripts of a start (`setup`,
`pre_start`, `post_start`, `seed`, the stop hooks) get the environment of
the `stems up`/`start`/`restart` that started the stem; a restart stems
performs on its own keeps that one. `stems run` on a running stem gets your
environment under what stems set at the stem's start. Without one (TUI,
MCP) it is the daemon's.

**`inherit_env: false`.** A script with

```yaml
scripts:
  setup:
    command: ./scripts/setup.sh
    inherit_env: false
```

gets only `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TERM`,
`LANG` and `LC_*` of that base layer, then everything stems sets (`PORT`,
`env`, `env_files`, local env, `--pass-env`, `STEMS_*`): a reproducible
`setup`/`build` that does not depend on what happens to be exported in your
shell. `stems scripts --json` shows `inherit_env` per script; `stems exec
--fresh` is the same for an ad-hoc command.

Arguments given with `stems run` are the positional parameters `$1…`; for
inline commands `$0` is the script's name. Scripts with an `args` schema
also get `STEMS_ARG_<NAME>` variables (see below).

## Working directory

* Stem scripts run in the **codebase** (FR-WS-2: sourced from the
  integration repo, executed in the code repo, which they must not modify —
  write to `$STEMS_STATE_DIR`).
* `cwd: <path>` overrides it (relative to the codebase); `cwd: workspace`
  runs the script in the integration repo root.
* A stem without a codebase (docker stems) runs its scripts in
  `$STEMS_STATE_DIR` unless `cwd:` is set: scripts never run inside the
  integration repo unless asked to.
* Workspace scripts run in the integration repo root.

## How a script is run

* Inline: `<shell> -c <command> <name> <args…>` (`shell` of a process stem,
  default `/bin/sh`).
* `file:`: resolved relative to the integration repo; it must exist inside
  it — `SCRIPT_NOT_FOUND` / `SCRIPT_OUTSIDE_WORKSPACE` at `validate` time
  (exit 2) and again before every run (`..`, absolute paths and symlinks
  pointing out are refused). Executable files are executed directly
  (shebang), others through `/bin/sh`.
* Through the process runtime: own session and process group, stdin
  `/dev/null`. When the script exits, anything it left running in its group
  is stopped (SIGTERM, then SIGKILL after 0.5 s) — scripts cannot leak
  daemons.
* Output (stdout and stderr) goes to the stem's log with `stream: script`
  and `tag: <script>` (FR-SC-4): `stems logs <stem> --script setup`.
  Workspace scripts log under the pseudo-stem `_workspace`
  (`stems logs _workspace --script bootstrap`).
* `retries: N` re-runs a failed script (non-zero exit or timeout) up to N
  more times, waiting 500 ms, 1 s, 2 s, … (doubling, capped at 10 s) in
  between (FR-SC-8). A cancellation (a stop) is never retried.

## Stamps (FR-SC-5)

`setup` and `seed` are idempotent by **stamp**:

```text
stamp = sha256( "stems-stamp-v1"
              ‖ script text (inline command, or the file's contents)
              ‖ for each file matched by `inputs` (sorted): path ‖ sha256(contents)
              ‖ for each `stamp_env` variable (sorted): name ‖ value )
```

(every field length-prefixed; `crates/stems-core/src/stamps.rs`). Only
contents count — touching a file (mtime) changes nothing; editing it, adding
or renaming a matched file does. `inputs` globs are relative to the codebase
(`*` stays within a directory, `**` crosses; a directory name matches
everything below it); `.git`, `node_modules`, `target` and `.venv` are never
walked.

Before `setup`/`seed` would run, stems computes the stamp and compares it
with the one recorded after the last **successful** run (state file,
`stamps.<stem>.<script>`; carried over across daemon restarts). Equal: the
script is skipped (a skipped `seed` still counts as seeded). Different or
missing: the script runs, and on success the new stamp is recorded. A failed
run records nothing, so the next `up` retries.

* `stems stamps [stem] [--json]` lists them: `data: { stamps: [{stem,
  script, hash, computed_at, inputs}], cleared }` (from the daemon, or from
  `state.json` when none runs). `--clear` forgets them.
* `stems reset [stems…] --yes` stops the stems, runs their `reset` scripts
  and clears their stamps (all enabled stems when none are named). `data:
  { ok, stopped, reset: [{stem, script, exit, duration_ms}], cleared,
  failed }`. Without `--yes`: `DESTRUCTIVE_NOT_CONFIRMED` (exit 2).
* `stems up --fresh` does the same for the planned stems (after
  `bootstrap`), then starts them: `setup` and `seed` run again.

## Custom scripts and `stems run`

Any script name that is not a lifecycle name is a **custom script**
(FR-SC-2): `seed-large`, `create-test-user`, `flush-cache`, … Workspace
scripts other than `bootstrap`/`teardown` are custom too.

```yaml
scripts:                                   # workspace
  nuke-databases:
    file: scripts/nuke.sh
    description: Drop and recreate all local databases
    requires: [postgres]                   # must be healthy first
  restore-snapshot:
    file: scripts/restore.sh
    requires_stopped: [shop-api]           # must not run meanwhile
stems:
  shop-api:
    scripts:
      create-test-user:
        file: scripts/shop-api/create-test-user.sh
        description: Create a user with a known password for manual testing
        args:
          - { name: email, type: string, required: true, description: "Email address" }
          - { name: role, type: enum, values: [admin, user], default: admin }
        timeout: 30s
        retries: 2
        concurrent: false                  # the default
```

```text
stems run shop-api create-test-user -- --email a@b.c --role user
stems run shop-api create-test-user --json -- --email a@b.c
stems run --ws nuke-databases --start-deps
stems run --ws restore-snapshot --stop-deps
stems scripts [stem] [--json]
```

`stems run <stem> <script> [-- args…]` runs a stem's script,
`stems run --ws <script> [-- args…]` a workspace script (`--workspace-script`
is an alias; plain `--workspace` is the global flag that picks the workspace).
Any script can be run this way, lifecycle ones included (`stems run shop-api
seed`). The daemon is started if none runs (and stopped again afterwards
when nothing is running).

### Arguments (FR-SC-7)

`args` declares the script's arguments: `name`, `type` (`string`, `int`,
`float`, `bool`, `enum` with `values`, `path`), `default`, `required`,
`description`. What follows `--` is parsed against the schema like a
command line (`--email a@b.c`, `--email=a@b.c`, `--verbose` for a bool,
`--verbose false`). Anything that does not fit — a missing required
argument, an unknown flag, a stray positional, `--rows many` for an int, a
value outside an enum — is `SCRIPT_ARGS_INVALID` (exit 2) with `details:
{arg, reason, stem, script}` and a hint listing the declared arguments;
nothing runs.

The script then gets the validated values, defaults applied:

* as argv: `--<name> <value>` per argument, sorted by name (bools as `true` /
  `false`): `--email a@b.c --role admin`;
* as environment: `STEMS_ARG_<NAME>` (upper-cased, other characters → `_`):
  `STEMS_ARG_EMAIL=a@b.c`, `STEMS_ARG_ROLE=admin`.

A script **without** `args` gets everything after `--` untouched
(passthrough), and no `STEMS_ARG_*`.

Over the API (`run_script`) and from MCP, `args` may instead be a JSON object
(`{"email": "a@b.c", "role": "user"}`), validated the same way with type
coercion (`"42"` for an int, `42` for a string).

### `requires` (FR-SC-6)

`requires: [postgres, redis]` lists stems that must be `healthy` before the
script runs. If one is not: `SCRIPT_REQUIRES_UNMET` (exit 1) with `details:
{requires, unmet: [{stem, state}]}` — or, with `--start-deps`, stems starts
them first (like `stems start`, dependencies included) and runs the script
once they are healthy.

### `requires_stopped`

`requires_stopped: [shop-api, worker]` is the opposite: stems that must
**not** run while the script does (a restore, a schema rewrite, a cleanup of
files a running stem holds open). `stopped`, `failed` and `disabled` count
as not running. If one runs: `SCRIPT_REQUIRES_UNMET` (exit 1) with
`details: {requires_stopped, unmet_stopped: [{stem, state}]}` — or, with
`--stop-deps`, stems stops them first (like `stems stop`: a stem with
running dependants is refused with `HAS_DEPENDANTS`, reported in
`details.stop_error`; stop them too, or `stems stop --cascade`). Both keys
can be combined; the stops happen first, and `--start-deps` only starts
`requires` once every `requires_stopped` stem is down. A script checking a
state itself can use `stems status --expect stopped shop-api` (exit 0 or 1,
see [lifecycle.md](lifecycle.md#checking-states-in-scripts-status---expect)).

### When the stem is starting, and concurrency

* A stem script run while its stem is in its start sequence (`setup`,
  `starting`, `seeding`, or healthy with `post_start`/`seed` still to run)
  waits until it is done, up to `--wait <duration>` (default 30 s), then
  fails with `START_TIMEOUT`. A stopped stem does not block: the script just
  runs (it may of course need the stem, like `create-test-user`).
* **One script at a time per stem** (and one workspace script at a time): a
  second `stems run` waits for the first and emits `script.queued`; its
  result says `queued: true`. A script with `concurrent: true` never waits.

### Output and result

Human output streams the script's output live (its tagged log lines) and
ends with `✓ create-test-user finished in 0.8s` or `✗ create-test-user
failed: exit 3 (0.1s)`. The exit code is 0 when the script succeeded, else
1 with `SCRIPT_FAILED`. `--json` prints the envelope with `data:
RunScriptResult`:

```json
{"run_id": "01J…", "stem": "shop-api", "script": "create-test-user", "ok": true,
 "exit": 0, "signal": null, "duration_ms": 812, "timed_out": false,
 "attempts": 1, "argv": ["--email", "a@b.c", "--role", "admin"],
 "tail": ["create-test-user: ok (email=a@b.c, role=admin)"], "queued": false}
```

`--no-wait` returns `{run_id, stem, script}` at once; follow the run with
`stems events -f` (every `script.*` event of the run carries `data.run_id`)
and `stems logs <stem> --script <name> -f`.

### The catalogue (`stems scripts`)

`stems scripts [stem] [--json]` lists every runnable script — workspace
scripts first, then each enabled stem's, sorted by name — as a table
(`STEM SCRIPT KIND DESCRIPTION ARGS`, with a `lifecycle hooks: …` footer)
or as `data: {scripts: [{stem, name, description, args, requires,
requires_stopped, kind:
lifecycle|custom, timeout, retries, concurrent, mcp_tool, input_schema}],
lifecycle_hooks: [names]}`. `stems scripts --lifecycle` lists the hooks
themselves ([above](#the-lifecycle-order)). It reads the config locally and
needs no daemon. `mcp_tool` (`shop_api__create_test_user`,
`workspace__nuke_databases`) and `input_schema` (a JSON Schema of `args`)
are what the MCP server publishes (FR-AI-2); the dashboard's `:` menu and
its Scripts view ([tui.md](tui.md#the-scripts-view), every script, the
workspace-level ones included) use the same data. The daemon serves it as the `script_catalog` RPC
([protocol.md](protocol.md#methods)).

## Timeouts and failures

`timeout: 30s` bounds a script; past it the whole process group is killed
(SIGTERM, SIGKILL after 0.5 s) and the run fails with `reason: timeout`.
Default: no timeout. A custom `stop` script is always bounded by the stem's
`stop_grace`.

| Script | On failure |
|---|---|
| `bootstrap` | `up` aborts with `SETUP_FAILED` (exit 1); no stem starts |
| `setup` | the stem is `failed` with `SETUP_FAILED`; its process never starts |
| `pre_start` | the stem is `failed` with `SCRIPT_FAILED` |
| `post_start`, `seed` | the process is stopped, the stem is `failed` with `SCRIPT_FAILED`; `condition: seeded` dependants are skipped |
| `pre_stop`, `post_stop`, `stop` | logged; the stop goes on (SIGTERM/SIGKILL) |
| `build` | `SCRIPT_FAILED`; `restart --build` then starts nothing |
| `reset` | `SCRIPT_FAILED` for that stem; its stamps are kept |
| `teardown` | reported in `down`'s `failed` as stem `_workspace`; the daemon still stops |
| any of `setup`, `seed`, `pre_start`, `post_start`, `build`, `bootstrap` with `soft_fail: true` | nothing fails: the sequence goes on, nothing is stamped, the stem is degraded ([below](#partial-installs-soft_fail)) |

Every `SETUP_FAILED` / `SCRIPT_FAILED` has `details: { stem, script, exit,
signal, duration_ms, timed_out, reason: "exit" | "timeout" | "cancelled",
tail }`, `tail` being the script's last 20 output lines.

A stop that arrives while `setup`, `pre_start`, `post_start` or `seed` runs
kills the script and the stop proceeds at once.

### Partial installs (`soft_fail`)

The contract is **exit 0 = done** (a `setup`/`seed` is then stamped) and
**non-zero = not done**. A script that decides to skip work it cannot do
(a missing tool, an optional download) should say so with a non-zero exit,
never `exit 0` — otherwise the skipped install is stamped as complete and
not retried until its inputs change. `soft_fail: true` decides what "not
done" costs:

```sh
# scripts/api/setup.sh
if ! command -v poetry >/dev/null; then echo "poetry missing; skipping"; exit 1; fi
poetry install
```

```yaml
stems:
  api:
    scripts:
      setup: { file: scripts/api/setup.sh, soft_fail: true }
```

A soft failure (non-zero exit or timeout, after `retries`):

* does not stop anything: `setup → pre_start → start…` goes on (a soft
  `bootstrap` lets `up` go on);
* records **no stamp**, so the script runs again on the stem's next start
  or restart (`stems start`/`restart`, or an `up` after it was stopped; an
  `up` that finds the stem already running leaves it alone);
* leaves the stem `healthy` but **degraded** (`!`, reason `setup exited 1
  (soft_fail)`) with `status.warnings: [{script, exit, signal, timed_out,
  tail (≤ 10 lines), at}]`; `status -v` prints the warning and its tail.
  The warning lasts for this run of the stem: a stop clears it, a restart
  re-runs the script and decides again;
* emits `stem.degraded {script, exit, signal, timed_out}` and
  `script.finished` with `soft: true`; no desktop notification (degraded is
  not failed);
* is listed in `up`/`start`/`restart`'s `data.degraded: [{stem, script,
  exit, timed_out?}]` (`_workspace` for `bootstrap`); the exit code stays
  **0** when every stem is ready. Human `up` prints `! api: setup exited 1
  (soft_fail, retried on the next start or restart)` (`timed out` instead
  of `exited N` for a timeout).

A soft-failed `seed` leaves `seeded: false`: `condition: seeded` dependants
(and their own dependants) are not started and appear in `skipped` with
`skipped_reasons: {<stem>: "seed_degraded"}`; that `up` is partial (exit
3). A soft `build` lets `restart --build` start the stem anyway (`stems
build` itself still reports it in `failed`). A `build` warning belongs to
the `restart --build` that ran it: the next start without a build clears
it, and so does a later successful `stems build`.

`soft_fail` is accepted on `setup`, `seed`, `pre_start`, `post_start`,
`build` and the workspace `bootstrap`; anywhere else it is `SCHEMA_INVALID`
(stop hooks never block a stop anyway). `stems run api setup` of a
`soft_fail` script that fails still fails (`ok: false`, `soft: true`, exit
1): the softness is for the lifecycle, not for an explicit run.

## Events and state

* `script.started {script, actor, args}` and `script.finished {script, exit,
  signal, duration_ms, timed_out, cancelled, ok, soft?}`, with `stem` set for stem
  scripts; `stems run` adds `run_id` and `attempt` (one pair per attempt).
* `script.queued {script, run_id, actor, reason}`: a `stems run` waits for
  another script of the same stem.
* `stem.state` `stopped → setup → starting`, `healthy → seeding → healthy`
  (reason `seeded`).
* `state.json` keeps the stamps and the last 100 runs in `script_runs:
  [{stem, script, started, ended, exit, timed_out}]`.
* `status` shows `seeded: true` once the stem's `seed` ran or was current
  since it last started (always `false` for stems without a `seed`).

## Limitations

* Hooks and a custom `stop` script run for stems started by the current
  daemon; a stem adopted after a daemon crash is stopped with SIGTERM /
  SIGKILL and without hooks.
* `health` scripts are deliverable 21.
* The retry backoff is fixed (500 ms × 2, up to 10 s); a per-script
  `backoff:` is not configurable yet.
* The TUI menu (30) and the MCP tools (31) build on `script_catalog` and
  `run_script`.

See also: [Lifecycle](lifecycle.md) · [Overlays](overlays.md) · [Exec and env](exec.md) · [What stems can do](features.md)
