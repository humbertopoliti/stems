# Config: profiles, local edits, include / extends

Deliverable 26 (part A). The file format itself is described in
`crates/stems-config/README.md` (merge rules, substitution, defaults) and
`schema/stems.schema.json`. This page covers what 26 adds: stem selection
and profiles (FR-WS-8), `stems config get|set|unset` (FR-CL-5), the
include / extends error cases (FR-WS-9), and stem outputs (FR-ST-6, part B,
[below](#outputs-fr-st-6)).

## Profiles (FR-WS-8)

```yaml
profiles:
  default: [postgres, redis, shop-api, shop-worker, shop-web]
  backend: [postgres, redis, shop-api, shop-worker]
  web-only: [shop-web]
  everything: default        # an alias of another profile
default_profile: backend     # committed default
strict_profiles: false       # true: refuse profiles that miss a hard dependency
```

and, in `stems.local.yaml` only for your machine:

```yaml
profile: web-only            # wins over default_profile
stems:
  shop-worker:
    enabled: false           # removed from every profile and from the graph
```

### Selection rules (`stems_core::selection::Selection::resolve`)

`up`, `start`, `restart`, `down <names>` and `status <names>` all compute
their stem set the same way:

1. **Stem names win.** `stems up shop-web` starts `shop-web` (and its hard
   dependencies); no profile applies. `--profile` together with names is a
   `USAGE` error; `STEMS_PROFILE` is simply ignored when names are given.
2. Otherwise (`up` only) the profile is the first of: `--profile <p>`,
   the `STEMS_PROFILE` environment variable of the client, the local
   override `profile:`, `default_profile:`, a profile literally named
   `default`. If none applies, every enabled stem is selected.
3. A profile is a list of stems or an **alias** (`everything: default`),
   followed for at most 8 hops. An alias loop or an alias to an undefined
   profile is `UNKNOWN_PROFILE` (also reported by `stems validate`, as are
   a `default_profile:` / `profile:` naming an undefined profile).
4. Disabled members (`enabled: false`) are dropped from the profile
   silently (`stems profiles` lists them under `disabled`). Listing a stem
   that does not exist is `UNKNOWN_STEM` (validation).
5. The **closure** adds hard `depends_on` targets transitively. Soft edges
   never pull a stem in. A hard edge to a disabled stem is
   `DEPENDENCY_DISABLED` at validation (so `up` never gets that far); a soft
   edge to one is fine.
6. The stems the closure added to a profile's own list are `expanded`: `up`
   emits `profile.expanded {profile, added, requested}` before `up.started`
   (whose data now carries `profile`) so the addition is visible.
   `--no-deps` (`start`, `restart`) disables the closure.
7. With `strict_profiles: true` (usually set in `stems.local.yaml`), a
   profile whose closure would add anything is refused with
   `PROFILE_MISSING_DEPENDENCY` (exit 2); `details.missing` lists the
   stems, `details.dependencies` which listed stems need each one.

Unknown profile: `UNKNOWN_PROFILE` (exit 2) with a "did you mean" hint and
`details.known`.

The daemon has no access to the client's environment: the CLI resolves
`--profile` / `STEMS_PROFILE` into `UpParams.profile`; `None` makes the
daemon apply rules 2–7 with the workspace default.

### `stems profiles [--json]`

Reads the config locally (no daemon). `data`:

```json
{
  "default": "web",
  "env_profile": null,
  "profiles": [
    { "name": "web", "alias_of": null, "stems": ["web"],
      "closure": ["db", "api", "web"], "expanded": ["db", "api"],
      "disabled": [], "default": true, "default_source": "default_profile" },
    { "name": "all", "alias_of": "everything", "stems": ["db", "api", "web", "tool"],
      "closure": ["db", "api", "web", "tool"], "expanded": [], "disabled": [],
      "default": false }
  ]
}
```

`closure` and `expanded` are in declaration order; `default_source` is
`profile` (local override), `default_profile` or `named` (a profile called
`default`); a profile that cannot be resolved (strict mode, bad alias) has an
`error` object instead of failing the command. `env_profile` echoes
`STEMS_PROFILE` when set.

## `stems config get|set|unset` (FR-CL-5)

Paths are dotted with `[n]` indices: `stems.shop-api.env.PORT`,
`stems.shop-api.ports[0].port`, `profile`, `strict_profiles`.

- **`config get <path>`** prints the *resolved* value (the model
  `stems show --json` prints: defaults applied, variables substituted) and
  its source: `data: {path, value, source, file}` with `source` one of
  `stems.yaml`, `stems.local.yaml`, `include:<file relative to the
  workspace>` (an included or `extends` file) or `default` (written in no
  file: a default or a derived value, e.g. a stem's env inherited from the
  workspace `env:`). A path with no value is `USAGE` (exit 2) with the keys
  available at its parent in the hint.
- **`config set <path> <value>`** writes `value` into `stems.local.yaml`
  (created if missing). The value is YAML: a scalar (`false`, `18080`,
  `"0"`, `abc`) or a flow collection (`[a, b]`, `{ stem: db, soft: true }`).
  Before keeping the change the workspace is re-loaded and validated
  (`requires:` and overlay checks skipped); if the edit introduces an error
  that was not already there, the file is restored byte for byte and the
  errors are returned (exit 2). `data: {path, value, file, changed, diff}`
  where `diff` holds `-old` / `+new` lines.
- **`config unset <path>`** removes the key (and parents left empty: an
  empty `stems.web:` would *clear* the stem when merged), validated the same
  way.
- **`config diff`** / **`config apply`** show and apply what a running
  daemon would change: see [Reload on change](#reload-on-change-fr-wd-3).

A running daemon notices the edit within about a second (its config
watcher) and waits for `stems config apply` before restarting anything.

### How the file is edited (DECISIONS.md: targeted line-level editing)

`stems_config::edit` works on the text, not on a YAML model:

- an existing `key: value` line gets the new value in place; its
  indentation and trailing comment (`port: 1  # why`) are kept;
- a key whose value is a nested block gets the new value on its line and the
  nested lines are removed;
- missing keys are appended at the end of their parent block, one
  indentation step per level (the file's own step, else 2 spaces);
- every other line (comments, blank lines, other keys) is untouched.

When a path goes through a value that is not a block mapping (a flow
mapping `{a: 1}`, a list, an index such as `ports[0]`), that one value is
edited as data and rewritten in flow style on its key line (comments inside
it are lost). A list the local file does not have yet is copied from the
committed config first, so `config set stems.api.ports[0].port 18091`
produces `ports: [{ name: http, port: 18091 }]` in `stems.local.yaml`
(lists replace, they never merge).

## Reload on change (FR-WD-3)

The daemon watches the integration repo's config files: `stems.yaml`,
`stems.local.yaml` (also before it exists) and every `include:` / `extends:`
file (the loaded config's `sources`). A change is picked up after 300 ms
without further writes, loaded and validated (`requires:` tools are not
probed here; overlay conflicts are decided at start as always), and
compared with the **applied** config. Nothing is applied yet.

- **Invalid** (a cycle, a schema error, an unknown dependency, ...): event
  `config.invalid {errors, codes}`; the applied config stays in force
  (`status`, restarts and watchdogs are unaffected); `config diff` reports
  `pending: false` and the errors in `last_error`. Fixing the file emits
  `config.changed` again (with an empty plan when the fix restores the
  applied config).
- **Valid and different**: event `config.changed {plan, sources,
  auto_apply}` with the **reload plan**; the change is *pending* until
  `stems config apply` (or `config.reload.auto_apply`).
- **Valid and equal** (a comment, a reordering): nothing.

### The plan

`ReloadPlan { stems: [{name, action, changes, fields, hot, running}],
workspace: [...], catalog_changed }` lists every stem of the old and the new
config (declaration order, removed stems last). Each stem's resolved config
is serialised to JSON and compared field by field; `changes` has one action
per changed field group, `action` is the strongest, `hot` is true when none
of them restarts the stem, `running` says whether the stem runs now. A
running stem is compared with the config it *runs with* (so a stem left
out of a partial apply stays pending until it restarts).

| Changed field(s) | Action | Hot | What `apply` does |
|---|---|---|---|
| `env` (incl. local env, workspace `env`/`vars` folded in), `env_files`, `ports`, `command`, `cwd`, `shell`, `stdin`, `codebase`, `overlays`, `type`, docker/compose fields (`image`, `build`, `volumes`, `entrypoint`, `network`, `labels`, `healthcheck`, ...), `scripts.start`, any other field | `restart_required` (`fields` names them) | no | restart in dependency order with the new config |
| a `${stem.x.port}` target's `ports` (`port: auto` references) | `restart_required` (`stem.x.ports`) | no | restart |
| `outputs` | `outputs_changed` | no | restart (outputs are evaluated at start) |
| new stem, or `enabled: true` again | `added` | no | start it if the last `up` covers it (every stem, or its profile/stems); otherwise just register it |
| stem gone, or `enabled: false` | `removed` | no | stop it, clean its overlays, drop its state entry; it leaves `status` |
| `health` | `health_changed` | yes | restart the prober (a stem still `starting` keeps its readiness probe until its next start; an external stem is re-monitored) |
| `watch` | `watch_changed` | yes | reconfigure the watchdog in place (`watch.reconfigured {rules}`) |
| `scripts` (not `start`) | `scripts_changed` | yes | swap the definitions (the next run uses them) |
| `limits` | `limits_changed` | yes | thresholds apply from the next sample |
| `restart` | `restart_policy_changed` | yes | the next crash uses the new policy (the restart window starts over) |
| `description`, `tags`, `depends_on`, `stop_grace` | `metadata_changed` | yes | swap in place |
| nothing | `unchanged` | yes | nothing |

`workspace` lists workspace-level changes, always applied in place:
`profiles_changed` (`profiles`, `default_profile`, `profile`,
`strict_profiles`), `agent_changed`, `logs_changed` (retention applies at
once), `metrics_changed`, `scripts_changed` (workspace scripts),
`requires_changed`, `config_changed`, ... `catalog_changed` is true when the
custom scripts (the MCP tools, `stems scripts`) change; once applied the
daemon emits `tools.changed {added, removed, changed}` and `stems mcp`
sends `notifications/tools/list_changed`.

### `stems config diff [--json]`

Reads the config on disk *now* and prints the plan from the applied config
(`data = {plan, pending, loaded_at, detected_at, sources, last_error}`).
Human: a `STEM ACTION FIELDS HOT` table of the affected stems, then the
workspace changes. Needs a running daemon (exit 4 otherwise).

### `stems config apply [stems…] [--yes] [--json]`

Asks for confirmation on a terminal; without a terminal (or with `--json`)
it needs `--yes` (`DESTRUCTIVE_NOT_CONFIRMED`, exit 2). The config on disk is
loaded and validated again (with `requires:`), then per stem,
transactionally:

1. stems that restart or were removed are stopped, in reverse dependency
   order (`stem.state` reason `config reload`);
2. removed stems are forgotten (overlays cleaned, state entry dropped);
3. the new config becomes the applied one; hot changes are applied in
   place; every running stem that is up to date is rebased onto it;
4. restarted and added stems start in dependency order (a stem waits for
   its restarted dependencies' edge condition).

Stems whose config did not change keep running, *dependants included*: an
edge condition only gates a start. A failure (a port now in use, a start
that fails) is reported in `failed`, the other stems proceed, and the
command exits 3. `data = {applied: [{stem, action, result}], failed:
[{stem, action, error}], skipped: [{stem, action, reason}], workspace,
pending, ok}` with `result` one of `restarted`, `started`, `registered`,
`stopped`, `removed`, `reconfigured`, `swapped` (a stopped stem: its next
start uses the new config). Event `config.applied`.

With stem names only their changes are applied (plus the removal of any
running stem that left the config: it cannot keep running unlisted); the
new config still becomes the applied one, so the stems left out keep
running their old config and stay pending.

### `config.reload.auto_apply`

```yaml
config:
  reload:
    auto_apply: true   # false (default) | true | all
```

- `false`: every change waits for `stems config apply`.
- `true`: the daemon applies what restarts nothing on its own: hot changes
  of running stems and any change of a stopped stem (`config.applied
  {auto: true}`); restarts stay pending for `stems config apply`. When a
  *running* stem was removed, nothing is applied automatically.
- `all`: the daemon applies everything, restarts included.

The setting of the *new* config counts (turning it on applies that edit).

### Interplay with `up`, `start`, `restart`, `run`

These always use the latest **applied** config. Before they run, the daemon
compares the config on disk with it: a difference that restarts no running
stem (a stopped stem's env, a new stem, a hot change) is applied in place
first (`config.applied {implicit: true}`), so editing and then running `up`
works as before; a difference that would restart a running stem stays
pending: the command runs with the applied config and emits
`config.pending {stems}` (run `stems config apply`). A restart policy (22)
respawns a crashed stem with the config it runs with, which `config apply`
rebases onto the latest applied config: *policy restarts use the latest
applied config*.

## include / extends (FR-WS-9)

Merge order and rules are in `crates/stems-config/README.md`. Paths in
`include:` / `extends:` are relative to the file that names them, at every
level (`teams/payments.yaml` including `../shared/extra.yaml` resolves from
`teams/`). Precedence, lowest first: `extends` base, `include`s in list
order, the including file, `stems.local.yaml`.

| Case | Result |
|---|---|
| a target is missing / unreadable | `INCLUDE_NOT_FOUND` / `CONFIG_READ_FAILED` |
| files include each other in a loop | `INCLUDE_CYCLE` (the chain in the message) |
| `extends` chain deeper than 5 files | `INCLUDE_CYCLE` with `details.max_depth: 5` and a hint to flatten it |
| the same stem defined by two included files, or by an included file and the file including it | `DUPLICATE_STEM` at `stems.<name>`, `details: {stem, files: [first, second]}` |
| a stem of an `extends` base redefined by the child | allowed: merged (bases exist to be overridden) |
| a stem redefined in `stems.local.yaml` | allowed: merged (the local overlay) |
| the same file included twice | allowed (not a duplicate) |

## Outputs (FR-ST-6)

A stem can publish values for its dependants:

```yaml
stems:
  api:
    type: process
    ports: [8080]
    outputs:
      API_URL: "http://localhost:${stem.self.port}"          # a template
      TOKEN: { command: "cat $STEMS_STATE_DIR/token", secret: true }
  web:
    type: process
    depends_on: [api]                  # condition: healthy (the default)
    env:
      API_URL: "${stem.api.outputs.API_URL}"
```

Output names are identifiers (`[A-Za-z_][A-Za-z0-9_]*`; anything else is
`SCHEMA_INVALID` at `stems.<stem>.outputs.<name>`). A value is either

* a **template string** (numbers and booleans are taken as text), rendered
  with the stem's own context: `vars`, `${env.X}`, fixed ports and
  `${workspace.root}` are substituted when the config is loaded; `auto`
  ports (`${stem.self.port}`) and outputs of other running stems are filled
  in when it is evaluated; or
* **`{command, secret}`**: a shell command (the stem's `shell`, default
  `/bin/sh`) run in the stem's environment and working directory (its
  codebase, else `$STEMS_STATE_DIR`), bounded by 30 s. Its trimmed stdout
  (the last 20 lines) is the value. It runs through the script runner as a
  script named `outputs` (log tag `outputs`, events `script.started` /
  `script.finished` with `data.output: <NAME>`), except when
  `secret: true`: then it runs quietly — no log lines, no `script.*`
  events, no output tail in errors.

### When they are evaluated

Once per start, when the health check first passes and **before** the stem
enters `healthy`: `starting → [outputs] → healthy → [post_start, seed]`. So
`condition: healthy` (and `seeded`) edges are only satisfied once the
outputs exist, and a dependant renders its env, scripts and overlays at
*its* start with the values in place. The event `stem.outputs {names,
secret}` is emitted (names only, never values). A command that fails (non-zero
exit, timeout) stops the process and fails the stem: `SCRIPT_FAILED` with
`details.output: <NAME>` (and `details.script: "outputs"`).

Values live in the daemon's memory only (never in `state.json` or logs).
They are dropped when the stem stops, exits or fails, and re-evaluated on
every start (`restart` included). A dependant keeps the values it was
started with until it is restarted itself. After a daemon restart, adopted
stems re-evaluate their outputs in the background (a failure is only
logged). External stems evaluate theirs when monitoring starts.

### Consuming them

| Where | How |
|---|---|
| env values (config env, `env_files`, `stems.local.yaml`, `--pass-env`) | `${stem.<name>.outputs.<X>}` |
| inline stem scripts (`command:` text) | `${stem.<name>.outputs.<X>}` (rendered before the script runs) |
| overlay templates | `${stem.<name>.outputs.<X>}` |
| env of every direct dependant | `STEMS_<NAME>_OUTPUT_<X>` (`shop-api`, `TOKEN` → `STEMS_SHOP_API_OUTPUT_TOKEN`) |

A reference without a value when the dependant starts — the dependency
does not declare it, is not running, or is only a `condition: started` /
soft dependency that has not become healthy yet — is `UNRESOLVED_VARIABLE`
(`details: {stem, reference, key}`) with the hint "outputs are available
only from dependencies with condition: healthy"; the dependant fails
without being spawned. `build`, `reset` and custom scripts keep such a
reference literally instead.

### Showing them

* `stems outputs [stem] [--json] [--reveal]` (RPC `outputs {stems?,
  reveal?}` → `{stems: [{name, outputs: [{name, value, secret}]}]}`): every
  stem that declares outputs (or the named one); `value` is `null` until the
  stem is healthy, `"<redacted>"` for a secret. `--reveal` shows secret
  values **only** in human output on a terminal; the JSON envelope never
  contains them.
* `stems status --json`: `stems[].outputs` (`{NAME: value}`, secrets
  `"<redacted>"`; omitted when empty). With `--verbose`, a dependant's `env`
  shows `STEMS_<DEP>_OUTPUT_<X>` of a secret output — and any variable whose
  value contains a secret — as `"<redacted>"`.
* `stems show --json` prints the declarations as written (`{command,
  secret: true}`); there is no value at config time, so nothing leaks.
