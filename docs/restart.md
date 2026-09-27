# Restart policies and backoff

A stem whose process (or container) exits on its own is handled by its
`restart:` settings (FR-LC-6, deliverable 22). The decision is a pure state
machine (`stems_core::restart::RestartTracker`, unit-tested); the stem's
actor applies it (`crates/stems-daemon/src/supervisor/restart.rs`).

```yaml
stems:
  shop-worker:
    restart:
      policy: on-failure      # never | on-failure | always
      max: 5                  # restarts allowed within `window`
      window: 10m             # sliding window for `max` (and the degraded threshold)
      backoff: { initial: 500ms, max: 30s, factor: 2 }
      on_unhealthy: false     # also restart a stem that stays unhealthy
      unhealthy_grace: 10s    # ... for this long
      cascade: false          # also restart its dependants (see below)
```

Everything shown is the default; `stems show --effective` lists the defaults.

## Policies

What happens when a running stem exits and nobody asked it to stop:

| policy | exit code 0 | anything else (non-zero, signal) |
|---|---|---|
| `never` | `stopped` | `failed` (reason `exited with code N`) |
| `on-failure` (default) | `stopped` | restart |
| `always` | restart | restart |

"Restart" means: unless `max` restarts already happened within `window`,
the stem goes `starting` with reason `restarting in <delay> (attempt n)`
(no pid while it waits), the daemon emits

```json
{"kind": "stem.restarting", "stem": "api", "reason": "restarting in 500ms (attempt 1)",
 "data": {"attempt": 1, "delay_ms": 500, "reason": "exit", "exit_code": 3, "signal": null, "counted": true}}
```

and after the delay the stem is started again. Otherwise the stem is
`failed` with `MAX_RESTARTS` (details `{stem, attempts, max, window_ms,
exit_code, signal}`) and the daemon emits `stem.gave_up` with the same
data. Nothing of the stem is left running either way.

A process that exits before its **first** start is ready is not a crash
but a failed start: `failed` with `START_FAILED`, reported by `up` (exit
3). A stem that crashes again while a *restart* is starting goes through
the policy again (crash loops back off and give up).

## Backoff

The delay of consecutive attempt `n` (1-based) is
`initial × factor^(n − 1)`, capped at `backoff.max`: 500 ms, 1 s, 2 s, 4 s,
… 30 s with the defaults. The attempt counter starts over when

- the stem had been `healthy` for at least `window` when it exited,
- every earlier restart has aged out of the window, or
- you start it yourself (`up`, `start`, `restart`).

## What a restart keeps and reruns

A policy restart uses the configuration, environment (`--pass-env`
included) and ports of the stem's last start by you — `port: auto` ports
are sticky, and overlays stay in place. It runs `pre_start` and
`post_start` again but **never `setup` or `seed`**: their stamps are
intact, the data they produced is still there, and `seeded` keeps its
value. A policy restart uses the latest *applied* config (33): a config
change that restarts the stem waits for `stems config apply` (or your next
`stems restart`), and once hot changes (e.g. `restart:` itself, scripts,
`stop_grace`) are applied, the next policy restart uses them (see
[config.md](config.md#reload-on-change-fr-wd-3)). Docker stems restart the
same container.

## The window, `restarts` and the degraded threshold

`status` shows two counters per stem:

- `restarts` — policy restarts since the daemon started (lifetime, never
  reset by you; watchdog restarts are not counted);
- `restarts_in_window` — policy restarts within `restart.window`.

With 3 or more restarts in the window a `healthy` stem is **degraded**
(glyph `!`, reason `restarts (N recently)`; FR-HS-4, see
[health.md](health.md)). It becomes plainly healthy again once they age
out of the window.

## `on_unhealthy`

With `restart.on_unhealthy: true` a stem that stays `unhealthy`
([health.md](health.md)) for `unhealthy_grace` is stopped (stop hooks and
`stop` script included; overlays, ports and the container kept) and started
again through the same path: `stem.restarting` with `data.reason:
"unhealthy"`, backoff, and it **counts against `max`** like a crash (a stem
that never recovers ends `failed` with `MAX_RESTARTS`). Becoming healthy
again within the grace cancels it. With `policy: never` the option has no
effect.

## Your actions win

- `stems stop`, `down`, `restart` and daemon shutdown are never undone by
  the policy, whatever it is: the actor knows the exit was asked for (the
  start generation changes before the process is signalled, so its exit is
  not seen as a crash).
- A stop while a restart is pending (`starting`, "restarting in …")
  cancels the backoff timer and the stem is `stopped` at once.
- Your own start (`up`, `start`, `restart`) of a stem resets its window and
  backoff; `up` after `MAX_RESTARTS` gives it a fresh budget.
- `up` while a restart is pending does not start a second process; it
  waits for the pending restart like any start.

## Watchdogs (24)

A watchdog restart (a file changed, deliverable 24) goes through
`StemCell::restart_bypassing_policy`: the stem is stopped and started
again immediately, `stem.restarting` has `attempt: 0`, `delay_ms: 0`,
`counted: false` and `reason` naming the trigger, and neither `restarts` nor
the window changes. It is refused (`USAGE`) when the stem does not run.

What runs on a watchdog restart: `pre_stop`, the stop (custom `stop`
script or SIGTERM → grace → SIGKILL), `post_stop`, then `pre_start`, the
start and `post_start` — never `setup`/`seed` (their stamps are intact, as
on a policy restart); ports and overlays are kept, a docker container is
`docker restart`ed. The actor of the events is `watchdog` and the reason is
`watch: <path> changed` / `watch: N files changed`. `action: rebuild` runs
the `build` script first (a docker/compose stem is stopped and started
instead, which rebuilds its image). A crash after a watchdog restart is
handled by the policy as usual. See [watchdogs.md](watchdogs.md).

## Cascading restarts (FR-LC-9)

Restarting a stem can also restart the stems that depend on it: after a
code change in `api`, `web` (which caches `api`'s responses, or holds
connections to it) usually needs a restart too.

```console
$ stems restart api --cascade      # api, then everything that depends on it
$ stems restart api --no-cascade   # only api, even with restart.cascade: true
```

```yaml
stems:
  api:
    restart: { cascade: true }     # every restart of api cascades
    watch:
      - paths: ["*.py"]
        cascade: true              # this rule cascades (overrides restart.cascade)
```

**What is restarted.** The *dependants* of the restarted stem(s): every
enabled stem that depends on it through **hard** `depends_on` edges,
transitively. Soft edges (`soft: true`) never cascade. Only dependants
that run (`starting`, `healthy`, `unhealthy`) are restarted; the others are
reported as `skipped` (the stems below them are still restarted), and
external stems are never touched. With several stems (`stems restart a b
--cascade`) the dependants are the union of their dependants, ordered once;
a stem you named is never restarted again as a dependant.

**Order.** The restarted stem (the *origin*) restarts first, through its
usual path, and must become healthy again (bounded by its
`health.start_timeout`). Then the dependants restart **layer by layer**, in
`start_order` restricted to them: a layer's stems restart in parallel, and
the next layer starts once they are all healthy again (their `post_start`
done), so every dependency edge's condition holds as it does for `up`. In a
diamond (`b` and `c` depend on `a`, `d` on `b` and `c`) that is `a`, then
`b` and `c`, then `d` **once**.

**How a dependant restarts.** Like a watchdog restart (the policy
*bypass*): `pre_stop`, the stop, `post_stop`, `pre_start`, the start,
`post_start`; never `setup`/`seed`; ports, overlays and a docker container
are kept; `restarts` and `restart.max` do not count it. Its
`stem.restarting` has `reason: "cascade from <origin>"`, `counted: false`
and `cascade: <id>`; the actor is whoever caused the cascade (`cli:<user>`,
`mcp:<client>`, `watchdog`, or `daemon` for a policy restart).

**Failures.** If the origin does not become healthy the cascade stops
(`cascade.aborted {id, origin, stem, error}`): no dependant is touched and
`restart` fails with the origin's error. A dependant that fails to come
back is listed in `failed` (exit 3 for `restart`), and the stems depending
on it are `skipped`; the others continue.

**Who cascades.**

| trigger | cascades when |
|---|---|
| `stems restart` / MCP `restart` / TUI | `--cascade` (`cascade: true`); else the stem's `restart.cascade`; `--no-cascade` never |
| a watch rule (`restart`, `rebuild`) | the rule's `cascade`, else the stem's `restart.cascade` |
| the restart policy (a crash, `on_unhealthy`) | `restart.cascade: true`, once the origin is healthy again |
| `stems config apply` | never: it restarts changed stems in its own dependency order ([config.md](config.md#reload-on-change-fr-wd-3)) |

**Loop guards.** Hard dependency cycles are validation errors, so the
closure is finite; on top of that, a cascade can never feed itself:

1. A restart caused by a cascade never starts one: a dependant that
   crashes during the cascade is restarted by its policy, but that restart
   does not cascade even with `restart.cascade: true`.
2. No stem is restarted twice within one cascade (`visited`), whatever the
   shape of the graph.
3. The origin is never one of its own dependants (a soft edge back to it
   is ignored).
4. Watchdog triggers of the stems a cascade is restarting (origin and
   dependants) are **dropped**, not queued: a change that restarts `api`
   with a cascade also fires `web`'s rule on the same files, and `web` is
   restarted by the cascade anyway. The daemon log records each dropped
   trigger (`watch trigger dropped`).

**One at a time.** A daemon runs one cascade at a time, under the same
lock as `up`, `down` and `restart`. A cascade asked for while another runs
waits (`cascade.queued {id, origin, behind}`) and starts after the first
finished: two cascades never nest. A policy-triggered cascade whose origin
is not running when its turn comes (it crashed again while queued) is
aborted (`cascade.aborted`); its next policy restart asks for a new one.

**Output.** `stems restart --cascade --json` adds `data.cascade: {id,
origin, origins, restarted: [[layer 1…], [layer 2…]], failed, skipped,
aborted}`; the human output prints one line per dependant (`restarted b
(layer 1)`). While a cascade runs, `stems status --json` shows `cascade:
{id, origin}` on each of its stems.

## Events

| kind | when | data |
|---|---|---|
| `stem.restarting` | a restart is scheduled (after the stem went `starting`) | `attempt`, `delay_ms`, `reason` (`exit`, `unhealthy`, a watchdog's reason, or `cascade from <origin>`), `exit_code`/`signal` (exits), `counted`, `cascade` (the cascade's id, for a dependant's restart) |
| `cascade.started` | a cascading restart begins (before the origin's own restart) | `id`, `origin`, `origins`, `reason` (`restart`, `policy`, or the watchdog's reason), `stems` (the dependants' layers) |
| `cascade.queued` | a cascade waits for the running one | `id`, `origin`, `origins`, `reason`, `behind` (the running cascade's id) |
| `cascade.finished` | every layer was handled | `id`, `origin`, `restarted` (names), `layers`, `failed`, `skipped`, `ok` |
| `cascade.aborted` | the origin did not become healthy; dependants untouched | `id`, `origin`, `stem`, `error` |
| `stem.gave_up` | `max` restarts in the window were used up | `stem`, `attempts`, `max`, `window_ms`, `reason`, `exit_code`/`signal` |
| `process.exited` | before either, for an exit | `pid`, `code`, `signal` |

## Not covered

- A restart that never becomes healthy fails like any start
  (`START_TIMEOUT`); it is not retried by the policy.
- Pending restarts are not persisted: if the daemon itself dies during a
  backoff, the stem is simply not running afterwards (crash recovery,
  [recovery.md](recovery.md), adopts only live units).
