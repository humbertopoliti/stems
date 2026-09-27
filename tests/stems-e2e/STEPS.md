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
`STEMS_E2E_CONCURRENCY` (default 8), `STEMS_E2E_TMPDIR` (default `/tmp`, kept
short so `$STEMS_HOME/<hash>/stemsd.sock` stays under the 104-byte socket
path limit on macOS), `STEMS_E2E_BLESS=1` (write goldens).

## Isolation (what every scenario gets)

- A fresh temp root `/tmp/stems-e2e-s<slot>-XXXXXX` (canonical path;
  `/tmp/stems-e2e-XXXXXX` outside `scripts/e2e_slot.sh`) containing:
  - `examples/workspaces/<name>/`: a copy of the chosen workspace (or
    `tests/fixtures/workspaces/<name>/` for fixture workspaces). Developer
    files (`stems.local.yaml`, `.stems/`, caches) are not copied.
  - `examples/repos`: a symlink to the real `examples/repos`, so
    `codebase: ../../repos/...` keeps resolving. Use
    `Given the workspace has a private copy of the repos` before anything
    writes into a codebase (watchdog tests).
  - `home/`: `STEMS_HOME` for every command.
  - `outside/`: an empty dir that is not inside any workspace.
- A unique block of 20 ports (`STEMS_E2E_PORT_START + n*20`, each port
  checked free, wrapping inside the slot's 4000-port range). Every
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

### Concurrent runs (slots)

`make e2e*` runs the harness through `scripts/e2e_slot.sh`, which claims
the lowest free slot `i` of `STEMS_E2E_SLOTS` (default 4, max 10) as
`/tmp/stems-e2e-slots/<i>/` (mkdir lock; a dead owner pid is reclaimed) and
exports `STEMS_E2E_SLOT=i` and `STEMS_E2E_PORT_START=20000 + i*4000`. Two
runs in different slots therefore never share:

- ports: each run allocates its blocks in `[start, start+4000)` (200 blocks,
  reused round-robin once free), and the daemon's orphan scan only inspects
  listeners on the workspace's *declared* (remapped) ports;
- scenario dirs: the `stems-e2e-s<i>-` prefix, and the leak hook matches
  processes by the scenario's own dir/cwd;
- failure artefacts: `target/e2e-failures/` is shared, each entry records
  its writer's slot in `.e2e-slot`, and a run's start-up cleanup keeps
  entries of slots held by another live run (two slots failing the same
  scenario at once would still overwrite one entry).

The temp root stays under `/tmp` because of the 104-byte `sun_path` limit:
the daemon socket is `/private/tmp/stems-e2e-s<i>-XXXXXX/home/<12 hex>/stemsd.sock`,
62 bytes for a one-digit slot.

`@docker` scenarios are Serial within a run, and `make e2e-docker` claims
every slot (`--exclusive`) because compose projects, containers and
networks are machine-global. `@slow` scenarios are queued after the others
(ordering only). `STEMS_E2E_*` variables are stripped from the commands the
steps run, like every `STEMS_*` variable.

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
2. Waits up to 15 s (`STEMS_E2E_LEAK_GRACE_SECS`) for every recorded process group (commands, strays, and
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

**Docker checks (14/15, `@docker` only).** These run the `docker` CLI
(`src/docker.rs`; bounded by 120 s and the scenario budget) and fail
without Docker. The pure parts (label/state checks on `docker inspect`
JSON, `docker compose ps` parsing in both the array and the NDJSON format,
`docker run` arguments) are unit-tested in the harness.

**`Then the container "<name>" is running with label "<key>=<value>"`** —
`docker inspect <name>`: `State.Running` and `Config.Labels[key] == value`.
```gherkin
Then the container "docker-pg-db" is running with label "stems.workspace=docker-pg"
```

**`Given a container "<name>" is running with labels "<k>=<v>,..."`** —
`docker run -d --name <name> --label k=v ... alpine:3 sleep 300` (orphan
scenarios). With `stems.workspace=<ws>` among the labels the After hook
reports it as a leak unless the scenario removes it (e.g. `stems doctor
--orphans --yes`).
```gherkin
Given a container "docker-pg-stray" is running with labels "stems.workspace=docker-pg,stems.stem=db"
```

**`Then the docker volume "<name>" exists`** / **`does not exist`** —
`docker volume inspect <name>` succeeds / fails.
```gherkin
Then the docker volume "docker-pg_pgdata" exists
```

**`Then the compose project "<project>" has service "<service>" running`**
— `docker compose -p <project> ps --format json -a` lists the service in
state `running`.
```gherkin
Then the compose project "stems-compose-redis" has service "redis" running
```

Other Docker facts go through `When I run the shell command "docker ..."`
(`docker inspect -f '{{json .Id}}' <name>` prints JSON, so `I save the
JSON at "$" as "cid"` works on it). Compose containers carry no `stems.*`
labels, so the After hook does not see them: compose scenarios end with
`docker compose -p <project> down`.

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

### Config (26)

**`Given the local override file contains:`** + a docstring — writes
`stems.local.yaml` with the docstring verbatim (placeholders expanded,
common indentation removed), replacing the harness-generated file. Use it
with a workspace `with its original ports` (the generated port overrides are
not merged back); a later `a local override setting ...` step rewrites the
file from the harness overrides. For comment-preservation checks of
`stems config set`.
```gherkin
Given the "minimal" workspace with its original ports
And the local override file contains:
  """
  # my machine only
  stems:
    echo-svc:
      env:
        SHOP_CHAOS: "0"  # quieter locally
  """
```

### Config reload (33)

**`Given the local override file is extended with:`** + a docstring — the
docstring (a YAML mapping, placeholders expanded, common indentation
removed) is deep-merged into the harness-generated `stems.local.yaml` (the
port remapping and earlier `a local override setting ...` values are kept:
mappings merge key by key, anything else replaces), which is rewritten.
Adds a whole stem while the daemon watches the file.
```gherkin
Given the local override file is extended with:
  """
  stems:
    extra:
      type: process
      command: sleep 60
      ports: [{ name: http, port: auto }]
  """
```

**`Given the local override "<dotted.path>" is removed`** — removes the key
(and mappings left empty above it) from the generated `stems.local.yaml`
and rewrites it; fails if the key is not there.
```gherkin
Given the local override "stems.extra" is removed
```

### Overlays (18)

**`Given the repo copy is a git repository with "<repo>/<path>" committed`**
— in the private copy of the repos (`Given the workspace has a private copy
of the repos` first), `git init`s `<tmp>/examples/repos/<repo>` and commits
`<path>` (created with placeholder text if it does not exist yet), with a
throwaway identity and no signing. For `OVERLAY_TRACKED_FILE`.
```gherkin
Given the repo copy is a git repository with "shop-api/config/local.ini" committed
```

### Git codebases (20)

**`Given a bare git repository "<name>" made from "<dir>"`** — copies
`<repo>/<dir>` (without `__pycache__` etc.) to `<tmp>/git/<name>-src`, makes
it a git repo on `main` with one commit (neutral config, fixed identity,
signing off), creates the bare repo `<tmp>/git/<name>.git` and pushes `main`
to it (`origin` of the source repo). Saves `${var:<name>_url}` (the bare
repo's `file://` URL, for a `codebase.git` override) and `${var:<name>_src}`
(the source working copy: commit, tag and `git push -q origin ...` there
with a shell step to change the remote).
```gherkin
Given a bare git repository "shop" made from "examples/repos/shop-api"
And a local override setting stems.shop-api.codebase.git=${var:shop_url}
When I run the shell command "cd ${var:shop_src} && git tag v2 && git push -q origin v2"
```

### Doctor (19)

**`Given a fake tool "<name>" on PATH that runs "<command>"`** — writes the
executable script `<tmp>/bin/<name>` (`#!/bin/sh` + the expanded command);
from then on every `stems` command (and the daemon it starts) gets
`<tmp>/bin` first on `PATH`. For deterministic `requires:` versions and
dev-server start commands.
```gherkin
Given a fake tool "node" on PATH that runs "echo v20.11.1"
```

**`Given the state file records an overlay for stem "<stem>" at "<path>"`**
— creates the file (relative to the workspace, or absolute after expansion)
with placeholder text if it does not exist, then adds
`{dest, sha256 of its bytes, run_id, keep: false}` to `overlays.<stem>` of
`state.json` (path from `stems daemon status --json`; created if missing).
No daemon may be running. For stale-overlay checks.
```gherkin
Given the state file records an overlay for stem "shop-api" at "${tmp}/examples/repos/shop-api/config/local.ini"
```

### Health (21)

**`Then there are exactly <n> events matching <json-subset>`** — one `stems
events --json --since 0`: exactly `n` events are supersets (e.g. one
`stem.health` per transition, none per probe).
```gherkin
Then there are exactly 2 events matching {"kind": "stem.health", "stem": "echo-svc"}
```

**`Then within <n>s there are at least <k> events matching <json-subset>`** —
polls the events stream until at least `k` events match.
```gherkin
Then within 3s there are at least 4 events matching {"kind": "stem.health", "stem": "api"}
```

**`Then within <n>s the JSON at "<jsonpath>" of "stems <args>" equals <json>`**
— reruns the command every 100 ms until the path selects exactly one node
equal to the value (state that is derived, like `degraded`, without sleeps).
The command becomes the last command.
```gherkin
Then within 2s the JSON at "$.data.stems[?@.name=='web'].degraded" of "stems status --json" equals false
```

**`Then the daemon's CPU is below <n> %`** — daemon pid from `stems daemon
status --json`, CPU time (`ps -o time=`) over a 3 s sampling window as a
percentage of wall time. Tag such scenarios `@slow`.
```gherkin
Then the daemon's CPU is below 5 %
```

**`Given a workspace with <n> process stems using tcp health every <ms>ms`**
— an empty workspace dir (as `Given an empty directory`) with a generated
`stems.yaml`: `n` (≤ 20) stems `s00`… each running `python3 -m http.server`
on a port of the scenario's port block, with a `tcp` probe at that interval.
```gherkin
Given a workspace with 20 process stems using tcp health every 200ms
```

### Restart policies (22)

**`Then during <n>s the events stream never contains <json-subset>`** —
polls `stems events --json --since 0` every 200 ms for `n` seconds and
fails as soon as an event is a superset. A bounded "nothing happens" check
(a stop is not undone by the restart policy), not a wait for a condition.
```gherkin
Then during 3s the events stream never contains {"kind": "stem.restarting", "stem": "api"}
```

**`Then the first event matching <json-subset> is followed by one matching <json-subset> after <a> to <b> ms`**
— one `stems events --json --since 0`: takes the first event matching the
first subset, then the first *later* (by `seq`) event matching the second,
and asserts that their `ts` differ by `a..=b` milliseconds. For measured
delays (backoff) with a tolerant window.
```gherkin
Then the first event matching {"kind": "stem.restarting", "data": {"attempt": 2}} is followed by one matching {"kind": "stem.state", "to": "healthy"} after 800 to 11000 ms
```

### Watchdogs (24)

**`Then during <n>s there are at most <k> events matching <json-subset>`** —
polls `stems events --json --since 0` every 200 ms for `n` seconds and
fails as soon as more than `k` events are supersets. A bounded coalescing
proof (a burst of changes fires once), not a wait for a condition.
Files are changed with `Given the file "<path>" is written with "<text>"`
(replaces the file) or a shell step (`echo '# x' >> app.py`, loops) in the
private copy of the repos.
```gherkin
Then during 2s there are at most 1 events matching {"kind": "watch.triggered", "stem": "burst"}
```

### TUI frames (27)

Headless `stems attach --headless --script ...` (and attached `stems up`
with `STEMS_TUI_SCRIPT`) prints each `frame` token as text after a
`--- frame N ---` line. These steps read the **last command's** stdout,
split it into frames (the text after each delimiter up to the next) and map
remapped ports back to the declared ones first. Script tokens:
`docs/tui.md`.

**`Then the frame matches golden "<name>"[ masking <COL,COL>]`** — the last
frame against `tests/features/goldens/<name>.txt`, line by line (trailing
spaces trimmed). Masking uses the table header (the first line with `STEM`
and `STATUS`): each named column spans from its header's start to the next
header's; in the rows below it (up to the first blank line) every masked
cell becomes `*`, blank ones too (a sparkline may not have its first
sample yet). `daemon pid N` is always masked, and the status bar is compared
only up to its seven glyph counts (`✓ ! ✗ ↯ · ? ↻`; the right-aligned hints move with the
pid's width). `TIME` (29) masks the digits
of every clock time in the frame (`12:00:01`, `12:00:01.234`: log
timestamps, the events table). A missing golden is
written as `<name>.txt.new` (the step fails); `STEMS_E2E_BLESS=1` writes it.
```gherkin
Then the frame matches golden "tui-table-minimal" masking PID,UPTIME,CPU,MEM
Then the frame matches golden "tui-logs-warn-only" masking TIME
```

**`Then the last frame contains "<text>"`** / **`does not contain "<text>"`**
```gherkin
Then the last frame contains "Detail: b"
```

**`Then frame <n> contains "<text>"`** / **`Then frame <n> does not contain "<text>"`**
— frame `n` (from 1).
```gherkin
Then frame 2 contains "[Detail]"
```

**`Then stdout contains the terminal restore sequence`** — the raw stdout
contains `ESC[?1049l` (leave the alternate screen; the panic-restore test).
```gherkin
Then stdout contains the terminal restore sequence
```

**`Then stdout does not contain "<text>"`** — raw text absence.
```gherkin
Then stdout does not contain "--- frame 1 ---"
```

**`Then stdout contains the OSC 52 sequence for "<text>"`** (29) — the raw
stdout contains `ESC ] 52 ; c ; <base64 of text> BEL`, what headless mode
prints for a copy (`y`). `\n` in the text is a newline (a copied range).
```gherkin
Then stdout contains the OSC 52 sequence for "INFO chaos log line 2"
```

Headless script tokens added by 29 (see `docs/tui.md`): `wait:lines>=<n>`,
`wait:log=<text>` (the log pane) and `chaos:<path>` (the selected stem's
chaos endpoint, from inside the TUI run).

Added by 30: `type:<text>` (the characters verbatim, e.g. `type:rest api`
into the palette or a form field) and `wait:event=<kind>[:<stem>]` (an event
after the last key, e.g. `wait:event=script.finished:shop-api`). Actions run
synchronously in headless mode. Actor checks use the generic JSON steps on
`stems events --json --since 0` (a string `contains` is a substring match):
```gherkin
Then the JSON at "$[?@.kind=='watch.paused'].actor" contains "tui:"
```

### Metrics (25)

**`Then within <n>s the JSON at "<jsonpath>" of "stems <args>" is greater than <number>`**
— reruns the command every 100 ms until the path selects exactly one number
above the bound. The bound may be a sum after placeholder expansion
(`${var:rss}+104857600`: "grew by more than 100 MB").
```gherkin
Then within 5s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than ${var:rss}+104857600
```

**`Then some sample in the JSON at "<jsonpath>" has "<field>" greater than <number>`**
— the path selects objects or arrays of objects (flattened one level); at
least one has the numeric `field` above the bound.
```gherkin
Then some sample in the JSON at "$.data.stems[0].history" has "cpu_pct" greater than 50
```

**`Then within <n>s some sample in the JSON at "<jsonpath>" of "stems <args>" has "<field>" greater than <number>`**
— the same, polling the command every 100 ms (it becomes the last command).
```gherkin
Then within 5s some sample in the JSON at "$.data.stems[0].history" of "stems metrics --history 10s --json" has "cpu_pct" greater than 50
```

**`Then the JSON at "<jsonpath>" is in descending order by "<field>"`** — the
selected objects (an array is flattened) are ordered by the dotted `field`
(`latest.rss_bytes`), highest first; objects without it count as lowest. At
least two objects.
```gherkin
Then the JSON at "$.data.stems" is in descending order by "latest.rss_bytes"
```

**`Then the JSON at "<jsonpath>" has between <a> and <b> elements`** — the
selected nodes (an array is flattened) number `a..=b`.
```gherkin
Then the JSON at "$.data.stems[0].history" has between 1 and 40 elements
```

**`Then within <n>s the metrics file of "<stem>" has at least <k> lines`** —
polls `$STEMS_HOME/*/metrics/<stem>.ndjson` (`metrics.persist: true`) until
it holds `k` sample lines (JSON objects with `rss_bytes`).
```gherkin
Then within 3s the metrics file of "echo-svc" has at least 2 lines
```

### MCP (31)

An in-process rmcp client named `stems-e2e` (so its actor is
`mcp:stems-e2e`) talks over stdio to a `stems mcp` child started with the
scenario's cwd and environment (`src/mcp.rs`). Every tool call carries a
progress token. A tool result (the JSON text of its first content block)
also becomes the last command (`json` = the result, exit code 1 when
`isError`), so `Then the JSON at "<path>" ...` steps apply to it as well.
The server's stderr is kept in `<scenario>/mcp-server.stderr`. The After hook
closes the client and kills the server's process group before the leak
check, which then stops any daemon it auto-started.

**`Given an MCP client is connected[ with auto-start]`** — starts `stems mcp`
(`--auto-start`: also marks the daemon as possibly started) and initialises.
```gherkin
Given an MCP client is connected with auto-start
```

**`When the MCP client calls tool "<name>" with <json object>`** — placeholders
are expanded (`${var:c1}`).
```gherkin
When the MCP client calls tool "get_logs" with {"cursor": "${var:c1}"}
```

**`Then the tool result is success|error`** — `isError` of the last call.
```gherkin
Then the tool result is error
```

**`Then the tool result JSON at "<jsonpath>" equals <json>`** /
**`... contains <json>`** — like the generic JSON steps, on the last tool
result.
```gherkin
Then the tool result JSON at "$.code" equals "DESTRUCTIVE_NOT_CONFIRMED"
And the tool result JSON at "$.events" contains {"actor": "mcp:stems-e2e"}
```

**`When I save the tool result JSON at "<jsonpath>" as "<name>"`** — one node,
saved for `${var:<name>}` (strings unquoted).
```gherkin
When I save the tool result JSON at "$.next_cursor" as "c1"
```

**`Then across the last <n> tool results the values at "<jsonpath>" are <d> distinct of <t>`**
— collects the selected values of the last `n` tool results: `t` in total,
`d` distinct (pagination: every record exactly once).
```gherkin
Then across the last 3 tool results the values at "$.records[*].text" are 1200 distinct of 1200
```

**`Then the tools list contains|does not contain "<name>"`** — a fresh
`tools/list`.
```gherkin
Then the tools list contains "echo_svc__ping"
```

**`Then the tools list matches "<repo file>"`** — a fresh `tools/list` has the
same tools (name, description, inputSchema, in order) as the golden file.
```gherkin
Then the tools list matches "schema/mcp-tools.json"
```

**`When the MCP client reads resource "<uri>"`** / **`Then the resource content contains "<text>"`**
```gherkin
When the MCP client reads resource "stems://echo-svc/logs?tail=5"
Then the resource content contains "chaos log line 2"
```

**`When the MCP client gets prompt "<name>" with <json object>`** / **`Then the prompt text contains "<text>"`**
```gherkin
When the MCP client gets prompt "diagnose_stem" with {"stem": "echo-svc"}
Then the prompt text contains "state: healthy"
```

**`Then the MCP client received at least <n> progress notification(s)`** —
polls up to 2 s (notifications may trail the result).
```gherkin
Then the MCP client received at least 2 progress notifications
```

**`When the MCP client disconnects`** — closes the client (stdin EOF for the
server) and waits up to 15 s for `stems mcp` to exit (after stopping a daemon
it auto-started when nothing runs).
```gherkin
When the MCP client disconnects
```
