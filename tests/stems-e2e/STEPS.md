# stems e2e step library

The cucumber-rs harness in `tests/stems-e2e` runs every
`tests/features/**/*.feature` scenario against the real `stems` binary.
This file documents every step with an example. **Extend the vocabulary; do
not rename steps** (deliverable 04, "Interfaces delivered").

- Steps are registered for Given, When and Then alike, so `And`/`But` work
  after any keyword. Keep the Given/When/Then convention anyway.
- Step regexes live in `src/steps.rs` (`STEPS` table). Adding a step = one
  `step!` function + one table row + one entry here + a sample in the
  `step_regexes_are_unambiguous` unit test.
- A step with no matching definition fails the scenario ("not defined in the
  step library"); it is never silently skipped.

## Running

| Command | What runs |
|---|---|
| `make e2e` | everything except `@docker` and `@harness-selftest` |
| `make e2e FEATURE=tests/features/config/load.feature` | one file (or a directory) |
| `make e2e TAGS='@FR-WS-4 and not @slow'` | a cucumber tag expression |
| `make e2e-docker` | also `@docker` (run serially; needs Docker) |
| `make e2e-selftest` | only `@harness-selftest`; passes iff the After hook reported `LEAK:` |

All targets build the binary first (`cargo build -p stems-cli`) and then run
`cargo test -p stems-e2e --test e2e`. The e2e target has `test = false`, so a
plain `cargo test --workspace` only runs the harness's own unit tests.

The binary is located at `target/<profile>/stems`, next to the test
executable's `deps/` dir (Cargo's `CARGO_BIN_EXE_*` only works within one
package). Override with `STEMS_E2E_BIN=/path/to/stems`.

Other environment knobs: `STEMS_E2E_SCENARIO_TIMEOUT` (seconds, default 60),
`STEMS_E2E_CONCURRENCY` (default 4), `STEMS_E2E_TMPDIR` (default `/tmp`, kept
short so `$STEMS_HOME/<hash>/stemsd.sock` stays under the 104-byte socket
path limit on macOS), `STEMS_E2E_BLESS=1` (write goldens).

## Isolation (what every scenario gets)

- A fresh temp root `/tmp/stems-e2e-XXXXXX` (canonical path) containing:
  - `examples/workspaces/<name>/`: a copy of the chosen workspace (or
    `tests/fixtures/workspaces/<name>/` for fixture workspaces). Developer
    files (`stems.local.yaml`, `.stems/`, caches) are not copied.
  - `examples/repos`: a symlink to the real `examples/repos`, so
    `codebase: ../../repos/...` keeps resolving. Use
    `Given the workspace has a private copy of the repos` before anything
    writes into a codebase (watchdog tests).
  - `home/`: `STEMS_HOME` for every command.
  - `outside/`: an empty dir that is not inside any workspace.
- A unique block of 20 ports (`20000 + n*20`, each port checked free). Every
  numeric port declared in the workspace (`stems.*.ports[*].port`) is
  remapped through a generated `stems.local.yaml`: the `ports` arrays, `env`
  values equal to a declared port (`PORT: "18090"`, `SHOP_CONTROL_PORT`) or
  containing `:<port>`, `health.port` and `health.url`. `port: auto` and
  `container_port` are untouched. `broken/*` workspaces are copied verbatim
  (they are validation fixtures), and `... with its original ports` opts out.
- Commands run with cwd = the workspace (or `outside/` if no workspace was
  chosen), env = the harness's env minus every `STEMS_*` variable, plus
  `STEMS_HOME=<root>/home` and `STEMS_NO_COLOR=1`. `STEMS_WORKSPACE` is only
  set when a step sets it. Each command runs in its own process group, which
  is recorded for the leak check.
- stdin is `/dev/null`; stdout/stderr are captured. A command that exits
  while a descendant still holds its stdout does not hang the step (2 s grace).

### Placeholders

Command lines, env values, expected JSON values, `contains` texts and file
paths expand:

| Placeholder | Value |
|---|---|
| `${ws}` | the copied workspace dir |
| `${tmp}` | the scenario temp root |
| `${home}` | `STEMS_HOME` |
| `${outside}` | `<tmp>/outside` |
| `${repo}` | the real repository root |
| `${port:18090}` | the remapped value of declared port 18090 (18090 if not remapped) |
| `${var:name}` | a value saved with `When I save the JSON at "<path>" as "<name>"` (strings unquoted, anything else as JSON text) |

Anything else (e.g. `${stem.shop-web.port}`) is left as is.

## Timeouts

Every scenario has a 60 s budget (`STEMS_E2E_SCENARIO_TIMEOUT`), set when its
World is created. Each step checks the budget first; every command and poll
is bounded by the remaining budget (a command still running at the deadline
has its process group killed). Exceeding it fails the step with `TIMEOUT:`.
Polling steps take their own explicit bound (`within 5s ...`), capped by the
budget. There are no fixed sleeps: polls retry every 100 ms.

## The After hook (leak check)

Runs after every scenario, whatever the outcome:

1. If a step may have started a daemon, or any `*.sock`/`*.lock` exists under
   `STEMS_HOME`: `stems daemon stop --json` (10 s bound, errors ignored).
2. Waits up to 3 s for every recorded process group (commands, strays, and
   any `pgid` seen in `--json` output or `state.json`) to disappear.
3. Reports as leaks: live process groups; any process whose command line or
   cwd (Linux: also environment) mentions the scenario dir (macOS hides other
   processes' environments); `*.sock`/`*.lock` under `STEMS_HOME`; for
   `@docker` scenarios with Docker present, containers labelled
   `stems.workspace=<workspace name>`.
4. Kills what leaked, copies the scenario dir to
   `target/e2e-failures/<feature>__L<line>__<scenario>/` (with a
   `harness-report.txt`) if the scenario failed or leaked, deletes the temp
   dir, and fails the scenario with a message starting `LEAK:`.

A LEAK is never excused by PENDING.txt.

## PENDING.txt (instead of `@ignore`)

`tests/features/PENDING.txt` lists scenarios that are expected to fail today
because the feature is not implemented yet. They are **always run**:

- fails -> `PENDING (expected)`; the run stays green;
- passes -> `UNEXPECTED PASS ... remove it from tests/features/PENDING.txt`;
  the run fails. The list can only shrink.

Entries: `tests/features/x.feature` (whole file), `tests/features/config/`
(a directory) or `tests/features/x.feature:12` (the scenario on line 12).
Comment every entry with the deliverable that removes it. The summary at the
end of each run lists every scenario as PASSED / PENDING (expected) / FAILED /
FAILED (LEAK) / UNEXPECTED PASS with the first failure lines.

## Tags

| Tag | Effect |
|---|---|
| `@FR-*`, `@NFR-*` | requirement trace (`scripts/trace.py`) |
| `@docker` | skipped unless `STEMS_E2E_DOCKER=1`; runs serially |
| `@harness-selftest` | only under `make e2e-selftest` |
| `@slow`, `@error`, `@recovery` | informational; filter with `TAGS=` |

Tags on a Feature or Rule apply to all its scenarios.

## Steps

### Given: workspaces

**`Given the "<name>" workspace`** — copy `examples/workspaces/<name>`, remap
its ports.
```gherkin
Given the "minimal" workspace
```

**`Given the broken workspace "<name>"`** — same as
`Given the "broken/<name>" workspace` (copied verbatim, no port remap).
```gherkin
Given the broken workspace "cycle"
```

**`Given the fixture workspace "<name>"`** — copy
`tests/fixtures/workspaces/<name>` (harness-only fixtures), remap ports.
```gherkin
Given the fixture workspace "include-demo"
```

**`Given the "<name>" workspace with its original ports`** — no remapping, no
generated `stems.local.yaml` (use only when asserting on the committed ports;
such scenarios must not bind them).
```gherkin
Given the "minimal" workspace with its original ports
```

**`Given the fixture workspace "<name>" with its original ports`** — the
fixture without port remapping (for validation-only scenarios whose error
locations must all be in the fixture's own `stems.yaml`).
```gherkin
Given the fixture workspace "two-errors" with its original ports
```

**`Given the "<name>" workspace with a local override setting <dotted.path>=<value>`**
— workspace plus a value merged into the generated `stems.local.yaml`. The
value is parsed as YAML (`false`, `18080`, `"0"`, `[a, b]`).
```gherkin
Given the "hello-shop" workspace with a local override setting stems.shop-web.enabled=false
```

**`Given a local override setting <dotted.path>=<value>`** — another override
on the current workspace (merged).
```gherkin
And a local override setting profiles.default=backend
```

**`Given the workspace has a private copy of the repos`** — replace the
`examples/repos` symlink with a copy (after the workspace step).
```gherkin
And the workspace has a private copy of the repos
```

**`Given an empty directory`** — a new empty dir `<tmp>/new-ws` becomes the
workspace dir (cwd for commands, `${ws}`, `the file ... exists`), for
`stems init`. No `stems.local.yaml`, no port block.
```gherkin
Given an empty directory
When I run "stems init --from minimal --json"
Then the file "stems.yaml" exists
```

**`Given the "<name>" workspace is up [in detached mode] [with profile "<p>"]`**
— workspace, then `stems up --detach --json [--profile <p>]`, which must
succeed. Marks the daemon as started for the After hook. Fails with
`not implemented until deliverable 08` while the binary lacks `up`.
```gherkin
Given the "hello-shop" workspace is up in detached mode with profile "process-only"
```

**`Given a stray process "<shell command>" is running in a new process group`**
— `sh -c <command>` (placeholders expanded) in its own process group, cwd = workspace, with
`STEMS_HOME` set; its pgid is recorded, so if it is still alive at the end
the After hook reports `LEAK:` (this is the selftest). Stop it explicitly
with `When the stray processes are stopped` if the scenario needs a
long-lived unrelated process (e.g. pid-reuse tests).
```gherkin
Given a stray process "sleep 300" is running in a new process group
```

### When: running stems

The command text cannot contain `"`; use single quotes inside it
(`stems run api seed -- 'a b'`). It must start with `stems`. `--json` is not
added implicitly: write it in the scenario.

**`When I run "stems <args>"`** — cwd = workspace (or `outside/`).
```gherkin
When I run "stems validate --json"
```

**`When I run "stems <args>" from "<subdir>"`** — cwd = `<workspace>/<subdir>`
(created if missing).
```gherkin
When I run "stems show --json" from "scripts/nested/deeper"
```

**`When I run "stems <args>" with env KEY=value [KEY2=value2 ...]`** — extra
environment (placeholders expanded).
```gherkin
When I run "stems show --json" with env STEMS_WORKSPACE=${ws}
```

**`When I run "stems <args>" from outside any workspace`** — cwd = `<tmp>/outside`.
```gherkin
When I run "stems show --json" from outside any workspace
```

**`When the chaos endpoint "<path>" is called on "<stem>"`** — reads the
stem's port from `stems status --json` and sends
`GET http://127.0.0.1:<port>/__chaos/<path>` (HTTP/1.0). A connection refused
fails the step; a connection dropped mid-response (e.g. `crash`) is recorded
as status 0.
```gherkin
When the chaos endpoint "fork?n=3" is called on "echo-svc"
```

**`When the daemon is killed with <SIGNAL>`** — pid from
`stems daemon status --json` (`$.data.pid`), then `kill(pid, SIGNAL)`
(`SIGKILL`, `TERM`, `9` all accepted). For `SIGKILL` the step also waits
(≤ 2 s) until the pid is gone, so the next step sees the crashed state.
```gherkin
When the daemon is killed with SIGKILL
```

**`When the daemon is killed with SIGKILL during "stems <args>" once it prints "<kind>", <n> times`**
— crash-recovery stress (11): `n` times, runs the command in the background
(replacing the previous background command), polls its NDJSON stdout (≤ 20 s)
for an event of that `kind`, waits a pseudo-random 0–400 ms (so the kills land
at different points), SIGKILLs the daemon (pid from the `stemsd.lock` under
`STEMS_HOME`), waits for the command to exit (≤ 10 s) and asserts that every
`state.json` still parses. If the command exits before printing the marker
(e.g. `ORPHANS_FOUND`) that iteration kills nothing.
```gherkin
When the daemon is killed with SIGKILL during "stems up --detach --json" once it prints "up.started", 5 times
```

**`Given the state file records stem "<stem>" at the stray process with a wrong start time`**
— writes `state.json` (path: the lock's directory from `stems daemon status
--json`) with one entry for `<stem>` whose `pid`/`pgid` are the first stray
process's and whose `start_time` is `1`: a recycled pid as far as stems can
tell (pid-reuse tests, 11). Start the stray first; no daemon may run.
```gherkin
Given a stray process "sleep 300" is running in a new process group
And the state file records stem "echo-svc" at the stray process with a wrong start time
```

**`Then the stray processes are still running`** — every stray started by
this scenario is alive (e.g. stems did not kill a foreign process holding a
port).
```gherkin
Then the stray processes are still running
```

**`When the stray processes are stopped`** — SIGKILL the process groups of
strays started by this scenario and reap them.
```gherkin
When the stray processes are stopped
```

### Then: exit codes and output

**`Then the exit code is <n>`**
```gherkin
Then the exit code is 2
```

**`Then the command succeeds`** — exit 0 and, if the output is a JSON
envelope, `ok == true`.
```gherkin
Then the command succeeds
```

**`Then the command fails`** — non-zero exit.
```gherkin
Then the command fails
```

JSON steps use the last command's stdout parsed as JSON (a single document,
or NDJSON returned as an array; NDJSON whose last line is an envelope, as
printed by `stems up --json`, parses as that envelope). Paths are RFC 9535 JSONPath
(`serde_json_path`); quote keys with dashes: `$.data.stems['shop-api']`.
Expected values are JSON; anything that is not valid JSON is taken as a bare
string.

**`Then the JSON at "<jsonpath>" equals <json>`** — exactly one node, equal.
```gherkin
Then the JSON at "$.data.stems['shop-web'].enabled" equals false
```

**`Then the JSON at "<jsonpath>" does not equal <json>`** — exactly one
node, different from the value (e.g. a pid after `restart`).
```gherkin
Then the JSON at "$.data.stems[0].pid" does not equal ${var:pid}
```

**`Then the JSON at "<jsonpath>" contains <json>`** — some node contains the
value: substring for strings, element (object-subset) for arrays, subset for
objects.
```gherkin
Then the JSON at "$.data.stems['shop-api'].env.DATABASE_URL" contains "${port:15432}"
```

**`Then the JSON at "<jsonpath>" matches semver`**
```gherkin
Then the JSON at "$.version" matches semver
```

**`Then the JSON at "<jsonpath>" exists`** / **`does not exist`**
```gherkin
Then the JSON at "$.data.stems['echo-svc']" exists
And the JSON at "$.data.stems['shop-web']" does not exist
```

**`Then the JSON error has code "<CODE>"[ and path "<path>"]`** — some element
of `$.errors[*]` (envelope on stdout, else stderr) has that `code` (and
`path`).
```gherkin
Then the JSON error has code "CYCLE" and path "stems.shop-api.depends_on"
```

**`Then the error message contains "<text>"`** — some `$.errors[*].message`
contains the text (JSON only; never human stderr).
```gherkin
Then the error message contains "shop-api -> shop-worker -> shop-api"
```

**`Then stdout contains "<text>"`** / **`Then stderr contains "<text>"`** —
raw text; prefer JSON assertions (DECISIONS.md), use these for NDJSON logs or
completions.
```gherkin
Then stdout contains "pong"
```

**`Then stdout is not JSON`** — stdout does not parse as JSON and does not
start with `{` (e.g. `--human` forced text on a pipe).
```gherkin
Then stdout is not JSON
```

**`Then the output matches golden "<name>"[ ignoring columns <A,B>]`** —
compares stdout with `tests/features/goldens/<name>.txt` cell by cell; the
first line is a header, named columns are masked. An aligned table (every
row has a space before each header column) is cut at the header's column
positions, so cells may contain spaces (`OK healthy`, a reason); otherwise
rows are split on whitespace. Lines after the first blank line are compared
token by token, unmasked. Remapped ports in stdout are mapped back to the
declared ones first, so goldens hold the workspace's ports. A missing golden is
written as `<name>.txt.new` and the step fails; `STEMS_E2E_BLESS=1` writes it.
Prefer `insta` goldens in the crates; this is for whole-binary output.
```gherkin
Then the output matches golden "status-table" ignoring columns PID,UPTIME
```

### Then: daemon-backed polling (need 08/10+)

These steps call real commands; while the binary does not know the command
they fail with `not implemented until deliverable NN` (`events` exists since
08; `status` lands in 10/13).

Assumed JSON shapes (10 settled `status` on an array of objects with `name`,
`state` and `ports: [{name, port, auto}]`; the helpers in `src/world.rs`
accept these alternatives): `stems status --json` has `$.data.stems` as a map keyed by
stem name **or** an array of objects with `name`; a stem has `state` (or
`status`) and `port` or `ports` (array of numbers/objects with `port`, or a
map). `stems events --json --since 0` prints NDJSON events or an envelope
whose `data` is an array of events. Any `pgid` field in any `--json` output is
recorded for the leak check. (08: `stems events --json` is NDJSON; a single
event line parses as one object, which the events steps also accept.)

**`Then within <n>s the stem "<stem>" is "<status>"`** — polls
`stems status --json` every 100 ms.
```gherkin
Then within 10s the stem "echo-svc" is "healthy"
```

**`Then within <n>s the events stream contains <json-subset>`** — polls
`stems events --json --since 0` until some event is a superset.
```gherkin
Then within 5s the events stream contains {"kind": "stem.state", "stem": "echo-svc", "to": "healthy"}
```

**`Then the events stream contains <json-subset> before <json-subset>`** —
one `stems events --json --since 0`: the first event matching the first
subset has a lower `seq` than the first event matching the second (both
must exist). Ordering proofs without sleeps.
```gherkin
Then the events stream contains {"stem": "a", "to": "healthy"} before {"stem": "b", "to": "starting"}
```

**`Then the chaos response status is <code>`**
```gherkin
Then the chaos response status is 200
```

### Daemon, RPCs and background commands (08/09)

**`Given the daemon is started[ with debug RPCs]`** — `stems daemon start
--json` (with `STEMS_DEBUG_RPC=1` in its environment, inherited by the
detached daemon, for the `_debug.*` RPCs); must succeed. Marks the daemon as
started so the After hook stops it.
```gherkin
Given the daemon is started with debug RPCs
```

**`Given a stale daemon lock from a dead process[ and its socket]`** — reads
`$.data.lock` / `$.data.socket` from `stems daemon status --json` (no daemon
may be running), spawns and reaps `true` to get a dead pid, and writes
`{"pid", "start_time": 1, "version": "0.0.1", "created_at"}` to the lock
file; `and its socket` also leaves a socket file nobody listens on.
```gherkin
Given a stale daemon lock from a dead process and its socket
```

**`When I call the daemon RPC "<method>" with <json>`** — connects to the
single `stemsd.sock` under `STEMS_HOME` with the real `stems_api` client
(actor `cli:e2e`, version check on), calls the method with the (expanded)
JSON params and stores the outcome as the **last command**, so the JSON and
exit-code steps apply:
`{"ok": true, "result": <result>, "errors": []}` (exit code 0) or
`{"ok": false, "result": null, "errors": [<stems error>]}` (exit code =
the error's exit code). Any `pgid` in the result is recorded for the leak
check. `_debug.*` methods need `the daemon is started with debug RPCs`.
```gherkin
When I call the daemon RPC "_debug.describe" with {"handle": ${var:h}}
Then the JSON at "$.result.children[3]" exists
```

**`When I save the JSON at "<jsonpath>" as "<name>"`** — saves the node
(several nodes: a JSON array of them) of the last command's JSON for
`${var:name}`.
```gherkin
When I save the JSON at "$.result.children[*].pid" as "pids"
```

**`When I run "stems <args>" in the background`** — starts the command in its
own process group (recorded for the leak check) and keeps collecting its
stdout/stderr. One per scenario.
```gherkin
When I run "stems events -f --json" in the background
```

**`When the background command is stopped`** — SIGINT to its process group,
then waits ≤ 5 s (SIGKILL and fail if it ignores SIGINT). A no-op if it
already exited.
```gherkin
When the background command is stopped
```

**`Then within <n>s the background command's output contains <json array> in order`**
— polls its stdout, parsed as NDJSON, until the array's elements match (as
JSON subsets) lines in that order (other lines may come between).
```gherkin
Then within 5s the background command's output contains [{"kind": "daemon.started"}, {"kind": "daemon.stopping"}] in order
```

**`Then within <n>s the background command's stdout contains "<text>"`** —
polls the background command's raw stdout (human text, e.g. `status
--watch --human`) until it contains the (expanded) text.
```gherkin
Then within 1s the background command's stdout contains "- stopped"
```

**`Then within <n>s the background command exits with code <n>`**
```gherkin
Then within 5s the background command exits with code 0
```

**`When the chaos endpoint "<path>" is called on port <port>`** — like the
stem variant but with an explicit (expanded) port, for processes started
through `_debug.start_raw`.
```gherkin
When the chaos endpoint "fork?n=3" is called on port ${port:18090}
```

**`Then within <n>s port <port> is listening`** — polls a TCP connect to
`127.0.0.1:<port>` (readiness without sleeps).
```gherkin
Then within 10s port ${port:18090} is listening
```

**`Then within <n>s none of the pids <json> is alive`** — a pid or a JSON
array of pids (usually `${var:...}`); zombies count as dead.
```gherkin
Then within 2s none of the pids ${var:pids} is alive
```

**`Then the daemon RSS is below <n> MB`** — pid from `stems daemon status
--json`, RSS from `ps -o rss= -p <pid>`.
```gherkin
Then the daemon RSS is below 20 MB
```

**`Then the last command took less than <n> ms`** / **`took at least <n> ms`**
— wall-clock time of the last command (spawn to exit), or of the last RPC.
```gherkin
Then the last command took less than 100 ms
```

**`Then the socket has mode <octal>`** — permission bits of the single
`stemsd.sock` under `STEMS_HOME`.
```gherkin
Then the socket has mode 0600
```

**`Then within <n>s the daemon log contains "<text>"`** — polls every
`stemsd.log` under `STEMS_HOME`.
```gherkin
Then within 5s the daemon log contains "stale lock reclaimed"
```

**`Then within <n>s the lock file and socket do not exist`** — polling
variant of the step below.
```gherkin
Then within 2s the lock file and socket do not exist
```

**`Then the JSON at "<jsonpath>" is greater than <n>`** — exactly one numeric
node.
```gherkin
Then the JSON at "$.result.dropped_lines" is greater than 0
```

### Logs (12)

`stems logs --json` prints NDJSON: several records parse as an array
(`$[0].text`, `$[*].ts`), a single record as one object (`$.text`).

**`Then the JSON nodes at "<jsonpath>" equal <json array>`** — all nodes the
path selects, in document order, equal the array (e.g. every `text` of a
result, in order).
```gherkin
Then the JSON nodes at "$[*].text" equal ["ERROR chaos log line 0", "ERROR chaos log line 1"]
```

**`Then the JSON at "<jsonpath>" is in ascending order`** — at least two
nodes, non-decreasing; RFC 3339 strings compare as instants, numbers
numerically, other strings lexically.
```gherkin
Then the JSON at "$[*].ts" is in ascending order
```

**`Then within <n>s the output of "stems <args>" has no JSON at "<jsonpath>"`**
— reruns the command (every 100 ms) until it exits 0 and the path selects
nothing in its stdout parsed as NDJSON — always an array here, even for one
line; empty output is `[]`. For time windows (`--since 1s`) without sleeps.
```gherkin
Then within 5s the output of "stems logs --json --since 1s" has no JSON at "$[?@.level == 'warn']"
```

**`Then within <n>s the stem log file "<stem>/<file>" contains "<text>"`** —
polls `$STEMS_HOME/<hash>/logs/<stem>/<file>` (the daemon flushes after
100 ms idle).
```gherkin
Then within 2s the stem log file "echo-svc/current.log" contains "ERROR chaos log line 49"
```

**`Then the stem log directory "<stem>" holds at most <n> files`** — at least
one and at most `n` files in `$STEMS_HOME/<hash>/logs/<stem>/` (rotation).
```gherkin
Then the stem log directory "echo-svc" holds at most 3 files
```

**`Then the archive "<path>" contains "<entry>"`** — a `.tar.gz` (path relative
to the workspace) has that entry; an entry ending in `/` matches any entry
under that directory.
```gherkin
Then the archive "out/bundle.tar.gz" contains "logs/echo-svc/"
```

**`Then the archive "<path>" entry "<entry>" contains "<text>"`** /
**`... does not contain "<text>"`**
```gherkin
Then the archive "out/bundle.tar.gz" entry "config.json" contains "<redacted>"
And the archive "out/bundle.tar.gz" entry "config.json" does not contain "hunter2"
```

**`Then the background command's CPU is below <n> %`** — CPU time the
background command used (`ps -o time=`) over a 2 s sampling window, as a
percentage of wall time (the window is the measurement, not a wait for a
condition). For "no busy loop" checks; tag such scenarios `@slow`.
```gherkin
Then the background command's CPU is below 2 %
```

### Then: nothing left behind

**`Then no process from the workspace's process groups is alive`** — every
recorded pgid (commands, strays, `pgid`s from `--json` output and
`state.json`) is gone and no process mentions the scenario dir (2 s bound).
```gherkin
Then no process from the workspace's process groups is alive
```

**`Then no container with label stems.workspace=<ws> exists`** — passes
trivially when Docker is absent (use only in `@docker` scenarios).
```gherkin
Then no container with label stems.workspace=hello-shop exists
```

**`Then the state file contains no stems`** — every `state.json` under
`STEMS_HOME` has no/empty `stems` (a missing file passes).
```gherkin
Then the state file contains no stems
```

**`Then the state file is valid JSON`** — every `state.json` under
`STEMS_HOME` parses (a missing file passes; 11's atomic-write check).
```gherkin
Then the state file is valid JSON
```

**`Then the lock file and socket do not exist`** — no `*.lock`/`*.sock` under
`STEMS_HOME`.
```gherkin
Then the lock file and socket do not exist
```

**`Then the file "<path>" exists`** / **`does not exist`** — relative to the
workspace.
```gherkin
Then the file "stems.local.yaml" exists
And the file ".stems/state.json" does not exist
```

### Scripts (16)

**`Given the file "<path>" is written with "<text>"`** — writes the
(expanded) text plus a newline to the file (relative to the workspace, or
absolute after expansion), creating directories. Use after `Given the
workspace has a private copy of the repos` to change a codebase file (stamp
inputs).
```gherkin
Given the file "${tmp}/examples/repos/shop-api/VERSION" is written with "1.0.1"
```

**`Then the file "<path>" contains "<text>"`** — the file exists and
contains the (expanded) text.
```gherkin
Then the file "hooks.log" contains "post_start port=${port:18521}"
```

**`Then the events stream does not contain <json-subset>`** — one `stems
events --json --since 0` (must succeed): no event is a superset. Absence
proofs (a stamp that skipped a script) after the command that would have
emitted it has returned.
```gherkin
Then the events stream does not contain {"kind": "script.started", "data": {"script": "setup"}}
```

**`When I run the shell command "<command>"`** — `sh -c <command>`
(placeholders expanded) with the same cwd, environment and process-group
rules as `I run "stems ..."`; becomes the last command (exit code, stdout).
For checks stems has no command for (`docker exec … psql`, `curl`).
```gherkin
When I run the shell command "curl -fsS http://127.0.0.1:${port:18080}/products"
```
