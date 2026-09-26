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
— `sh -c <command>` in its own process group, cwd = workspace, with
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
(`SIGKILL`, `TERM`, `9` all accepted).
```gherkin
When the daemon is killed with SIGKILL
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
or NDJSON returned as an array). Paths are RFC 9535 JSONPath
(`serde_json_path`); quote keys with dashes: `$.data.stems['shop-api']`.
Expected values are JSON; anything that is not valid JSON is taken as a bare
string.

**`Then the JSON at "<jsonpath>" equals <json>`** — exactly one node, equal.
```gherkin
Then the JSON at "$.data.stems['shop-web'].enabled" equals false
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
compares stdout with `tests/features/goldens/<name>.txt` token by token; the
first line is a header, named columns are masked. A missing golden is
written as `<name>.txt.new` and the step fails; `STEMS_E2E_BLESS=1` writes it.
Prefer `insta` goldens in the crates; this is for whole-binary output.
```gherkin
Then the output matches golden "status-table" ignoring columns PID,UPTIME
```

### Then: daemon-backed polling (need 08/10+)

These steps call real commands; while the binary does not know the command
they fail with `not implemented until deliverable NN`.

Assumed JSON shapes (align with these in 08/10/13, or extend the helpers in
`src/world.rs`): `stems status --json` has `$.data.stems` as a map keyed by
stem name **or** an array of objects with `name`; a stem has `state` (or
`status`) and `port` or `ports` (array of numbers/objects with `port`, or a
map). `stems events --json --since 0` prints NDJSON events or an envelope
whose `data` is an array of events. Any `pgid` field in any `--json` output is
recorded for the leak check.

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

**`Then the chaos response status is <code>`**
```gherkin
Then the chaos response status is 200
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
