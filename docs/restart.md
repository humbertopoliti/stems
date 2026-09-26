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

## Events

| kind | when | data |
|---|---|---|
| `stem.restarting` | a restart is scheduled (after the stem went `starting`) | `attempt`, `delay_ms`, `reason` (`exit`, `unhealthy`, or a watchdog's reason), `exit_code`/`signal` (exits), `counted` |
| `stem.gave_up` | `max` restarts in the window were used up | `stem`, `attempts`, `max`, `window_ms`, `reason`, `exit_code`/`signal` |
| `process.exited` | before either, for an exit | `pid`, `code`, `signal` |

## Not covered

- A restart that never becomes healthy fails like any start
  (`START_TIMEOUT`); it is not retried by the policy.
- Pending restarts are not persisted: if the daemon itself dies during a
  backoff, the stem is simply not running afterwards (crash recovery,
  [recovery.md](recovery.md), adopts only live units).
