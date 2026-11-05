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
| `warn` | works, but deserves attention (a foreign process on a port, an orphan, a stale overlay, low disk, hot-reload overlap, no notification backend) |
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
| `docker.credentials.<registry>` | each registry an enabled docker stem's `image` comes from (`details {registry, stems, source, helper, path}`) | the pull's first credential source: a `credHelpers` helper or the `credsStore` found (on the caller's `PATH`, then next to the docker CLI), inline `auths`, or anonymous | the `credsStore` helper is missing (pulls fall back to anonymous: private images fail) | the `credHelpers` helper for that registry is missing, an unusable `auths` entry, or an unreadable docker config (id `docker.credentials`) (`IMAGE_PULL_FAILED`) | no |
| `compose.version` | an enabled compose stem and Docker reachable | `docker compose` v2 | | missing, or Compose v1 (`DOCKER_UNAVAILABLE`) | no |
| `image.<stem>` | each enabled compose stem whose service has `build:` in the compose file (an `image:` service is skipped), and each docker `build:` stem (the image of the current daemon run, `stems/<ws>/<stem>:<run_id>`); Docker reachable | `<image> built <when>, inputs unchanged since`; not built yet (the first `up` builds it); cannot compare (more than 50 000 entries in the context) | **stale**: `<image> is older than the checkout: <path> modified <when>, image built <when>` (`IMAGE_STALE`; `details {stem, image, image_created, newest_input, newest_input_path}`; hint `stems restart <stem> --build` or `build: true`); `compose config` failed | | no |
| `requires.<tool>`, `requires.<stem>.<tool>` | each workspace-level `requires:` entry, and each entry of an enabled stem's `requires:` ([below](#requires)) | installed version satisfies the range (`(from <file>)` when read from the codebase) | a `from:` entry of a git codebase that is not cloned yet: "codebase not cloned yet" (`REQUIRES_NOT_CHECKED`) | too old/new, not installed ("not found"), unparseable output, no version derivable from the `from:` file (`TOOL_VERSION`, `details {tool, required, found, from?, stem?}`) | no |
| `check.<name>`, `check.<stem>.<name>` | each `checks:` entry with `doctor` in `when` ([below](#custom-checks)) | the command exits 0 | `severity: warn` and it failed | it failed or timed out (`CHECK_FAILED`, `details {command, exit, tail}`) | when `fix:` is set |
| `ports.<stem>.<name>` | each fixed port of a non-external stem, plus `auto` ports recorded in state | free, or held by that stem (per state) | held by a process stems did not start (`details.pid`, `command`), by another stem, or by the daemon (`PORT_IN_USE`) | | no |
| `codebase.<stem>` | each stem with a codebase | exists (git: branch in `details`) | git codebase not cloned yet or not a git repo; dirty tree while the stem has a `build` script | local codebase missing (`CODEBASE_NOT_FOUND`) | no |
| `scripts.<stem>.<name>`, `scripts.workspace.<name>` | each `file:` script | exists | not executable | missing / not a file (`SCRIPT_NOT_FOUND`) | no |
| `env.<stem>` | each stem with `env_files` | every file readable; absent `required: false` files listed as `<path> optional, absent` (`details {stem, files: [{path, required, exists}]}`) | `ENV_SHADOWED`: a file sets a key `env:` or an earlier file sets to a different value (`details {stem, key, file, shadows, keys, files}`) | a required file is missing (`ENV_FILE_NOT_FOUND`) or a file cannot be read (`CONFIG_READ_FAILED`); `details {stem, file, files, errors}` | no |
| `overlays.<stem>` | each stem in the state file's overlay ledger | recorded and the stem runs, or `keep: true` | **stale**: recorded but the stem is not running; **modified** since stems wrote it | | stale |
| `orphans` | always | none | processes on declared ports not in state, stems-labelled containers not in state (containers only when Docker is needed and reachable) (`ORPHANS_FOUND`) | | when one looks like its stem's start command, or is a container |
| `disk.home` | always | ≥ 1 GB free under `$STEMS_HOME` | < 1 GB | | no |
| `disk.codebase.<stem>` | each distinct codebase | ≥ 1 GB free | < 1 GB | | no |
| `notifications` | the workspace loads | the backend desktop notifications would use (`osascript (/usr/bin/osascript)`, `notify-send`, a custom `notifications.command`), or `disabled` ([notifications.md](notifications.md)) | no backend on `PATH` (hint: install `terminal-notifier`/libnotify or set `notifications.command`) | | no |
| `watch.hot-reload.<stem>` | each stem with `watch` | no overlap | the start command hot-reloads (`vite`, `next dev`, `nodemon`, `cargo watch`, `air`, `uvicorn --reload`, `flask run --reload`, `webpack --watch`) **and** a watch covers `src/**` or `**` (FR-WD-4) | | no |

## `requires`

`requires:` declares the tools a workspace (or one stem) needs. It is checked
by `stems validate` (skip with `--skip-requires`), `stems doctor` and
`stems config apply`. Errors are `TOOL_VERSION` at `requires.<tool>` or
`stems.<stem>.requires.<tool>`.

```yaml
requires:                       # workspace level; files are read from the workspace root
  python3: ">=3.11"             # a range: npm-like, space = AND
  go: { version: "^1.22", command: "go version", regex: 'go(\d+\.\d+)' }

stems:
  web:
    codebase: ../web
    requires:                   # per stem; checked only while the stem is enabled
      node: { from: .nvmrc }
      pnpm: { from: "package.json#packageManager" }
      python3: { from: [.python-version, .tool-versions] }   # first file that exists
```

A bare version (`20`, `20.11.1`) means `20.x` and exactly `20.11.1`. The long
form takes `version` **or** `from` (never both, never neither), plus the
optional `command` (default `<tool> --version`, `go version` for go) and
`regex` (first capture group, else the whole match) that read the installed
version.

A binary with no version constraint is `jq: "*"` (any version). For anything
`requires:` cannot express (a registry login, a reachable cluster), see
[Custom checks](#custom-checks).

### `from:` files

Relative to the stem's codebase (the workspace root for a workspace-level
entry). A list uses the first file that exists. A stem whose codebase is a git
repository that is not cloned yet has no files to read: `validate` skips
its `from:` entries with a `REQUIRES_NOT_CHECKED` warning (exit 0),
`doctor` reports them `warn` ("codebase not cloned yet"), and `up` clones
the codebase and goes on; check again once it is cloned. Its `version:`
entries are probed from the workspace root meanwhile.

| file | rule |
|---|---|
| `.nvmrc`, `.node-version`, `.python-version`, `.ruby-version`, `.go-version`, `.java-version`, `.terraform-version` | the first non-comment line; a leading `v` is dropped; `lts/*`, `system`, `latest` and the like fail with "cannot derive a version from `lts/*` in .nvmrc" |
| `.tool-versions` (asdf, mise) | the line for the tool (aliases `node` = `nodejs`, `go` = `golang`, `python3` = `python`); the first version on it; `system`, `ref:` and `path:` fail as above |
| `<file>.json#<pointer>` | `#a.b` or `#/a/b`; a `name@version` value drops `name@` (and a `+sha...` suffix): `packageManager: pnpm@9.1.0`; `engines.node: ">=18"` is already a range |
| `<file>.toml#<pointer>` | e.g. `rust-toolchain.toml#toolchain.channel` |
| `go.mod` | the `go 1.22.1` directive |
| any other file | the first non-comment line |

None of the listed files existing fails with "none of ... exists under
<codebase>" (hint: add one, or set `version:`).

### Version managers (nvm, asdf, mise)

A stem's version probe runs with its codebase as the working directory, so
a shim from asdf or mise reports the version the project pins
(`.tool-versions`). nvm only affects shells that sourced it: make sure the
shell that runs `stems` has the right node first on `PATH`.

## Custom checks

`checks:` (workspace level, and per stem) lists your own shell checks: exit 0
is ok. They show up in `stems doctor` like any check and run before `stems up`.

```yaml
checks:
  registry-login:
    command: docker login ghcr.io --get-login >/dev/null   # exit 0 = ok
    message: "not logged in to ghcr.io"                    # shown on failure
    hint: "run `docker login ghcr.io`"
    severity: fail          # fail (default) | warn
    timeout: 5s             # default 2s, max 30s
    fix: "echo $GH_TOKEN | docker login ghcr.io -u x --password-stdin"   # optional
    when: [doctor, up]      # default [doctor, up]; `validate` is opt-in

stems:
  api:
    codebase: ../api
    checks:
      kube-context: { command: 'test "$(kubectl config current-context)" = dev' }
```

* The command runs through `sh -c` in its own process group (killed on
  timeout). The working directory is the workspace root (a stem's check: its
  codebase); the environment is the workspace `env:` (a stem's check: the
  stem's `env:`, which includes the workspace's) over the caller's. A stem's
  checks run only while it is enabled.
* `doctor`: ids `check.<name>` and `check.<stem>.<name>`; `fail` or `warn`
  from `severity`; `details {command, exit, tail}` (`tail` = last 20 lines of
  output). A check with `fix:` is fixable: `stems doctor --fix` runs the
  command (fix outcome action `run_fix`) and the report is re-run.
* `stems up` runs the `up` checks before anything starts, after the Docker
  preflight (a stem's checks only when it is in the selection). A failing
  `fail` check aborts with `CHECK_FAILED` (exit 1, `details {check, exit,
  tail}`); a failing `warn` check is printed.
* `stems validate` runs only checks whose `when` includes `validate`
  (`CHECK_FAILED` at `checks.<name>`; `warn` ones are warnings);
  `--skip-requires` skips them too. Each runs once, and only once the
  config is otherwise valid; they never run when the daemon (re)loads the
  config (`up`, `start`, the file watcher, …). Shape errors (`timeout` over 30s, empty
  `when` or `command`) are `SCHEMA_INVALID` and always reported.
* Checks are not scripts: `stems scripts` does not list them.

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

See also: [Repos](repos.md) · [Recovery](recovery.md) · [Config](config.md) · [What stems can do](features.md)
