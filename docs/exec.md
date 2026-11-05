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

1. your environment, minus `STEMS_*` (with `--fresh` only `PATH`, `HOME`,
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
the environment a running stem was started with (including the
`--pass-env` values of its last `up`, which `stems restart` keeps unless
given `--pass-env` again), its allocated
`port: auto` ports and its dependencies' evaluated outputs. Without one,
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
| `json` (or `--json`) | `data: {stem, env, cwd, container, unresolved}` |

The dotenv and shell forms are printed as they are even when stdout is not
a terminal (no JSON envelope unless `--json`). `--all` puts your own
environment (minus `STEMS_*`) underneath. Secret outputs of dependencies
(and any value containing one) are `<redacted>`, exactly like `stems status
-v`, unless `--reveal` in human mode on a terminal (like `stems outputs`).

## Protocol

All three use the `stem_env` method ([protocol.md](protocol.md)); without
a daemon the CLI computes the same result from the config.

See also: [Scripts](scripts.md) · [Compose](compose.md) · [Config](config.md) · [What stems can do](features.md)
