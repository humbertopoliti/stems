# Running commands in a stem's context: `exec`, `shell`, `env`

Three commands give you what stems gives a stem — its environment, its
working directory, its container — without writing a script for it:

```sh
stems exec shop-api -- npm test          # a command, with the stem's env, in its codebase
stems exec db -- psql -U shop            # inside the running postgres container
stems shell shop-api                     # an interactive shell in that context
eval "$(stems env shop-api --format shell)"   # the stem's env in your shell
```

All three work **with or without a daemon** and never start one.

## `stems exec <stem> [--host] [--cwd <dir>] [--fresh] -- <cmd…>` (FR-LC-7)

The `stems` CLI runs the command itself, so it gets your terminal (an
interactive `psql`, `node` or `python` works), your stdin and stdout, and
`stems exec` exits with the command's exit code (128 + the signal number
when the command was killed).

**Process and external stems** (and any stem with `--host`): the command
runs on your machine with

1. your environment, minus the `STEMS_*` variables stems injects (`STEMS_HOME`, `STEMS_PROFILE`, `STEMS_LOG`, `STEMS_UI_CONFIG`, `STEMS_DOCKER_CLI` and the other CLI settings listed in [scripts.md](scripts.md) pass through, so a nested `stems` uses the same home and profile; with `--fresh` only `PATH`, `HOME`,
   `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `TERM`, `LANG` and `LC_*` of it —
   like a script's `inherit_env: false`);
2. what stems sets for the stem: `PORT`, workspace and stem `env`,
   `env_files`, `stems.local.yaml` env, `STEMS_STEM`, `STEMS_WORKSPACE`,
   `STEMS_CODEBASE`, `STEMS_STATE_DIR`, `STEMS_RUN_ID` (with a daemon),
   `STEMS_<DEP>_PORT`, named ports and dependencies' outputs (layers 2–7 of
   [lifecycle.md](lifecycle.md#a-stems-environment-fr-st-4)).

It runs in `--cwd <dir>` (relative to your current directory), else the
stem's codebase, else its state directory. It is not supervised: it is
your command in your terminal, not part of the stem.

**Docker and compose stems**: `docker exec -i [-t] [-w <dir>] <container>
<cmd…>` into the stem's running container (`-t` when your terminal is
one; `--cwd` becomes `-w`). The container already has the stem's
environment. A stopped stem is `STEM_NOT_RUNNING` (exit 1): start it with
`stems start <stem>`, or run on the host with `--host`.

**Where the values come from.** With a daemon running, from the daemon:
the stem's config and env files as they are **now** (re-read on every
call), the `--pass-env` values of its last `up` (which `stems restart`
keeps unless given `--pass-env` again), its allocated `port: auto` ports
and its dependencies' evaluated outputs. A key removed from an env file is
gone, even while the stem runs. For a running stem, a variable that has no
value now keeps the one the stem got at its start, per key: a variable
whose `${stem...}` reference is unresolved, and a dependency's
`STEMS_<DEP>_PORT…`/`STEMS_<DEP>_OUTPUT_<X>` variable that is absent,
because the dependency is restarting or down (its outputs are cleared
until it is running again). A running stem keeps the environment it
was started with until it is restarted; when that differs, `stems env`
warns `ENV_STALE` with the keys (see below). Without one,
from the config: fixed ports are known, but an auto port that is not
allocated (and an output that is not evaluated) stays unresolved — a
variable referring to it keeps its `${stem...}` text, its port variables
(`PORT`, `PORT_<NAME>`, `STEMS_<DEP>_PORT`, `STEMS_<DEP>_PORT_<NAME>`) are
not set, all of them are listed in `unresolved`, and `exec` prints a
one-line warning on stderr.

**Output.** Human: only the command's own output. `--json` prints
`data: {stem, container, cwd, argv, exit}` **after** the command ran (its
output streams first, so `--json` is only useful with a quiet command).
Errors before the command runs (`UNKNOWN_STEM`, `STEM_NOT_RUNNING`, a
command that cannot be started, exit 127) are reported like any other
stems error.

## `stems shell <stem> [--host]` (FR-LC-8)

`stems exec <stem> -- $SHELL -i` (your `SHELL`, else `/bin/sh`). In a
running container: `bash` when the image has it, else `sh`.

## `stems env <stem> [--format dotenv|shell|json] [--all] [--reveal]`

Prints what stems sets for the stem (the variables `exec` gets, without
your environment; unlike `exec`, secret outputs are redacted, see below):

| format | output |
|---|---|
| `dotenv` (default) | `KEY='value'` lines, quoted like the compose env file stems writes |
| `shell` | `export KEY='value'` lines — `eval "$(stems env api --format shell)"` |
| `json` (or `--json`) | `data: {stem, env, cwd, container, unresolved, warnings}` |

The dotenv and shell forms are printed as they are even when stdout is not
a terminal (no JSON envelope unless `--json`). `--all` puts your own
environment (minus the injected `STEMS_*`) underneath. Secret outputs of dependencies
(and any value containing one) are `<redacted>`, exactly like `stems status
-v`, unless `--reveal` in human mode on a terminal (like `stems outputs`).

When the stem runs with an environment that differs from what its config
and env files yield now (a key changed, added or removed since it
started), the printed values are the config's and stems warns on stderr
(and in `data.warnings[]`):

```text
warning: ENV_STALE: `api` runs with an environment that differs from its config now: LEGACY_FLAG (changed, added or removed since it started)
```

`stems restart api` applies it. `stems status` shows the same keys
(`env_stale`).

Only what you configure counts as a change: `env:`, `env_files`,
`stems.local.yaml` env and the `--pass-env` of the stem's last start
(applied the same way as at the start, `STEMS_*` names included). The
variables stems sets itself (`STEMS_STEM`, `STEMS_RUN_ID`, …) and a
dependency's port, codebase and output variables never do: a dependency
that restarted with a new output, or is down right now, is not
`ENV_STALE`. A configured value that refers to a dependency
(`${stem.auth.outputs.token}`, `http://localhost:${stem.auth.port}`) is
compared as a template: only its literal text changing counts, never what
the reference resolves to.

## Protocol

All three use the `stem_env` method ([protocol.md](protocol.md)); without
a daemon the CLI computes the same result from the config.

See also: [Scripts](scripts.md) · [Compose](compose.md) · [Config](config.md) · [What stems can do](features.md)
