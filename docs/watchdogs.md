# Watchdogs

A stem's `watch:` rules turn file changes in its codebase into an action:
restart it, rebuild and restart it, run one of its scripts, or send it a
signal (FR-WD-1). Watchdogs can be paused and resumed, per stem or all at
once, and every firing is in the event log (FR-WD-2).

```yaml
stems:
  api:
    type: process
    codebase: ../api
    command: python3 app.py
    watch:
      - paths: ["*.py", "templates/**"]
        action: restart            # default
        debounce: 300ms            # default 500ms
      - paths: ["requirements.txt"]
        action: rebuild            # `build` script, then restart
        settle: 2s                 # wait for 2s of quiet first
      - paths: ["proto/**"]
        root: workspace            # relative to the integration repo
        action: "script:codegen"
      - paths: ["config/*.toml"]
        action: "signal:SIGHUP"    # reload in place
```

## Rules

| key | default | meaning |
|---|---|---|
| `paths` | (required) | globs relative to the root; a change must match one |
| `ignore` | built-in list | more globs to ignore, **merged** with the built-in ones |
| `debounce` | `500ms` | coalescing window: changes within it fire once |
| `settle` | `0s` | additionally wait until nothing changed for this long |
| `action` | `restart` | `restart`, `rebuild`, `script:<name>`, `signal:<SIG>` |
| `root` | `codebase` | `codebase` (the stem's codebase; the integration repo for a stem without one) or `workspace` (the integration repo) |

Matching is done on the changed path **relative to the root**, with `/`
separators, using [globset](https://docs.rs/globset) syntax:

* `*` also crosses directories: `*.py` matches `app.py` and `src/db/x.py`
  (like a `.gitignore` basename pattern). Use `src/**/*.py` or `**` freely.
* A trailing `/` means "everything below": `templates/` = `templates/**`.
* **Ignore wins over `paths`.** An ignore pattern also ignores everything
  below a matching directory, and a bare name (`tmp`, `*.bak`) matches at
  any depth.

Built-in ignores, always applied: `.git`, `node_modules`, `target`, `dist`,
`build`, `__pycache__` (as `**/<name>/**`), `.venv`, `.stems` and `*.log`.
Your `ignore:` entries are added to them; you cannot remove a built-in one
(`stems watch status --json` lists the merged set per rule).

Reads never count: only creations, modifications (content or metadata) and
removals do.

## Timing: debounce, settle, one pending action

```text
first change ──┬── more changes ──┐
               │  (same batch)    │
               └── debounce ──────┴── settle (quiet) ──▶ fire
                                       while the action runs:
                                       changes collect in ONE pending batch
                                       ──▶ fires once the action finished
```

* A batch fires at `max(first change + debounce, last change + settle)`.
  With the default `settle: 0s` that is `debounce` after the first change,
  however many files changed (a `git checkout` of 500 files fires once).
* One action runs at a time per stem. Changes during an action are not
  dropped and not queued N times: they collect into **one pending batch per
  rule**, which fires when the action finished (at once if its window has
  already passed). Editing during a restart therefore gives at most one
  more restart.
* `settle` is for tools that write in bursts (formatters, code generators,
  `npm install`): the action waits until the tree has been quiet that long.
* A restart action is finished when the stem is `healthy` again (with its
  `post_start` done), or `unhealthy`/`failed`.

## Actions

| action | what happens | events |
|---|---|---|
| `restart` | stop + start through the restart-policy **bypass** ([restart.md](restart.md#watchdogs-24)): `pre_stop`/`stop`/`post_stop`, `pre_start`/start/`post_start`; never `setup`/`seed`; same ports and overlays; not counted in `restarts` or `restart.max` | `stem.restarting {counted: false}` |
| `rebuild` | the stem's `build` script ([scripts.md](scripts.md)), then `restart`; a failing build leaves the stem running (`ok: false`). Without a `build` script it is a plain restart. Docker/compose stems are stopped (container removed) and started again, which rebuilds a `build:` image (`docker.build` events) and creates a new container | `script.started/finished {script: build}` then the restart |
| `script:<name>` | runs the stem's script `<name>` like `stems run <stem> <name>` (17), actor `watchdog`; its output is tagged `<name>` in the stem's log; the stem keeps running | `script.*` |
| `signal:<SIG>` | sends the signal to the stem's process group (`SIGHUP`, `HUP`, `USR1`, `15` all work). Process stems only: docker/compose stems answer `NOT_IMPLEMENTED` (use `restart`/`rebuild`) | — |

Every firing emits `watch.triggered` (actor `watchdog`, `reason` `watch:
app.py changed` or `watch: 12 files changed`, `data: {paths (the first 10,
relative to the root), path_count, action, rule_index}`) and, when the
action is done, `watch.action_finished {action, rule_index, ok, error?}`.

## When watchers run

* A stem's watcher starts when its start begins (after a missing git clone
  is cloned, before `setup`), so edits made during a slow start count; it
  stops when the stem becomes `stopped` or `failed`. Policy restarts and
  watchdog restarts keep it running.
* `stems up --no-watch` starts no watchers (and stops running ones) until
  the next `up` without the flag; `stems watch status` then says `disabled`.
* Stems re-attached after a daemon crash (`stem.adopted`) are not watched
  until their next start.

## Pause and resume

```console
$ stems watch pause api          # just api's rules
$ stems watch pause              # everything (global)
$ stems watch resume api
$ stems watch resume             # global resume: also clears per-stem pauses
$ stems watch status [--json]
```

Changes made while paused are dropped, not replayed on resume. `watch.paused`
/ `watch.resumed` are emitted with the stem, or with `data: {global: true}`.
`stems status --json` shows `watch: {paused, rules}` for stems with rules.

`stems watch status --json`:

```json
{"stems": [{"name": "api",
            "rules": [{"paths": ["*.py"], "ignore": ["**/.git/**", "…"],
                       "action": "restart", "debounce_ms": 300,
                       "settle_ms": 0, "root": "codebase",
                       "dir": "/home/me/src/api"}],
            "paused": false, "active": true,
            "last_triggered": "2026-09-26T12:00:03Z",
            "pending": false, "busy": false}],
 "global_paused": false, "disabled": false}
```

`active` is true while a watcher runs; `busy` while an action runs;
`pending` when changes wait to fire.

## Hot reload: usually one or the other

Many dev servers already reload themselves (`vite`, `next dev`, `nodemon`,
`cargo watch`, `air`, `uvicorn --reload`, `flask run --reload`, `webpack
--watch`). Adding a `restart` watchdog on the same sources restarts the
whole process on every save, which throws away the dev server's fast
in-process reload and races with it. Guidance:

* A hot-reloading dev server: **no watchdog** on its sources. Keep watch
  rules for what the dev server does not see: dependency manifests
  (`package.json` → `rebuild` with `npm install` as `build`), `.env` files,
  generated code (`root: workspace` for a shared `proto/`).
* A server that can reload its config on a signal: `signal:SIGHUP` instead
  of a restart.
* Everything else (plain `python3 app.py`, a Go binary, a JVM): `restart`,
  or `rebuild` when a compile step is needed.

`stems doctor` warns (`watch.hot-reload.<stem>`, FR-WD-4,
[doctor.md](doctor.md)) when a stem's start command hot-reloads and a watch
rule covers `src/**` or `**`.

## Platform notes

* macOS uses FSEvents, which delivers changes about 0.5–1 s after they
  happen and may coalesce them; the debounce comes on top. Linux uses
  inotify (near-instant). Expect a restart to begin roughly `debounce` +
  up to 1 s after a save on macOS.
* Very large trees: inotify has a per-user watch limit
  (`fs.inotify.max_user_watches`); keep build outputs and dependencies in
  the ignore list (they are by default) and prefer narrow `paths`.
* The watch roots are resolved to real paths too, so a codebase reached
  through a symlink (`/tmp` → `/private/tmp` on macOS) matches.

## Implementation

`crates/stems-daemon/src/supervisor/watch.rs`: `Matcher` (globs relative
to a root), `Coalescer`/`Scheduler` (the pure timing state machine,
unit-tested with a fake clock), `spawn_fs_watcher` (notify +
notify-debouncer-full; generic, reused by the config watcher of deliverable
33), the per-stem watcher task and the actions, plus the `watch_pause`,
`watch_resume` and `watch_status` RPCs ([protocol.md](protocol.md)).
Scenarios: `tests/features/watch/`.
