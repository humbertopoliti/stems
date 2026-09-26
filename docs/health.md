# Health checks and the status model

Every stem has a health check (`health:` in `stems.yaml`). The daemon probes
it for as long as the stem runs, and the result is the stem's state:
`starting → healthy` on the first passing probe, `healthy ⇄ unhealthy` after
that. `degraded` (`!`) is derived on top of it for every `status` snapshot.
Requirements: FR-HS-1, FR-HS-2, FR-GR-1, FR-GR-6, FR-ST-3. Code:
`crates/stems-daemon/src/supervisor/probes.rs` (probes, transitions),
`crates/stems-core/src/health.rs` (degraded derivation).

## Probe types

| `type` | Passes when | Parameters |
|---|---|---|
| `tcp` | a TCP connect to `host:port` succeeds within `timeout` | `host` (default `localhost`), `port` (default: the stem's first port) |
| `http` | a `GET url` answers `status` (any 2xx when unset) and, with `body_contains`, a body containing it | `url` (default `http://<host>:<port>/`), `status`, `body_contains`, `headers` (map), `insecure` (accept invalid TLS certificates) |
| `command` | the shell command exits 0 within `timeout`; for docker/compose stems it runs inside the container (`docker exec <id> sh -c '<command>'`, so `pg_isready`/`redis-cli` come from the image) | `command`, `cwd` (process stems: relative to the stem's codebase; default the codebase) |
| `docker` | Docker reports the container `healthy` (its own healthcheck); `starting`/`unhealthy` fail; a container without a healthcheck passes while it runs | the stem's `healthcheck:` override (docker stems) |
| `process` | the process (group leader) or container is alive | — |
| `grpc` | not in this build: a stem with a `grpc` check fails to start with `NOT_IMPLEMENTED` (hint: `type: tcp` on the gRPC port, or a `command` probe running `grpc_health_probe`) | `host`, `port` |

When a stem has no `health:` at all: process stems get `process`, docker
and compose stems `docker`, external stems none. `type` is inferred from
`url` (http), `command` (command) or `port` (tcp) when omitted. `url` and
`port` may reference auto ports (`${stem.api.port}`); they are rendered when
the stem starts.

Common parameters:

| Parameter | Default | Meaning |
|---|---|---|
| `interval` | `2s` | time between probes (test and example workspaces use ≤ 500 ms) |
| `timeout` | `1s` | one probe's bound (connect/request/command; a command is killed) |
| `retries` | `3` | failed probes in a row that make a healthy stem `unhealthy` (0 counts as 1) |
| `start_period` | `0s` | failures in this period after the start do not count |
| `start_timeout` | `60s` | a stem still `starting` after this is `failed` with `START_TIMEOUT` |

Probes of one stem never overlap: they run one after the other, and a tick
that comes while a probe still runs is skipped. `http` probes use `reqwest`
with rustls (no native TLS) and a fresh connection each time; `command`
probes run through the script runner with tag `health` in the stem's
environment, without `script.*` events or run records.

**Command probe output** goes to the stem's log tagged `health` (`stems
logs <stem> --script health`) only when the probe fails or its outcome
changes (pass → fail or fail → pass), followed by a `[stems] health check
passed|failed (...)` line — a passing check every 2 s does not flood the log.

## Transitions

The probe task starts with the process and feeds the stem's actor, which is
the only writer of its state:

| State | Probe results | New state | Event |
|---|---|---|---|
| `starting` | one passes | `healthy` (then `post_start`/`seed`, see [scripts.md](scripts.md)) | `stem.state` |
| `starting` | none passed within `start_timeout` | `failed`, `START_TIMEOUT` (message names the last probe error); the process is stopped | `stem.state` |
| `healthy` | `retries` in a row fail (failures within `start_period` of the start do not count) | `unhealthy`, `reason` = the last probe's detail | `stem.state` + `stem.health` |
| `unhealthy` | one passes | `healthy`, `reason` cleared | `stem.state` + `stem.health` |
| `setup`, `seeding`, `stopping` | any | unchanged (results are recorded) | — |

A process that exits is handled as before ([lifecycle.md](lifecycle.md)):
`starting` → `failed` with `START_FAILED`, later → the restart policy
([restart.md](restart.md)). A stem that stays `unhealthy` is left alone
unless `restart.on_unhealthy: true`, which restarts it after
`restart.unhealthy_grace` (event `stem.restarting {reason: "unhealthy"}`).

`stem.health` is emitted once per health transition, never per probe:
`{kind: "stem.health", stem, from, to, reason, data: {probe, detail,
latency_ms, outcome, consecutive_failures}}`. Probe results themselves are
stored (the last 50 per stem), not streamed: `stems health` shows them.

Edges wait on the real state ([lifecycle.md](lifecycle.md#readiness)):
`condition: started` = the process is alive, `healthy` = the probe passed,
`seeded` = healthy and the seed finished. `up --timeout D` bounds the whole
run (`START_TIMEOUT` for what was not ready by then).

## External stems

An external stem is never started, only monitored. With a health check its
probe drives `unknown ⇄ healthy/unhealthy` from `up` until `down` (which
ends the monitoring: state `stopped`). There is no `START_TIMEOUT`.

| Probe outcome | Examples | Managed stem | External stem |
|---|---|---|---|
| `ok` | connected, HTTP 2xx, exit 0 | pass | → `healthy` |
| `fail` — it ran and failed | connection refused, HTTP 503, exit 1, timeout, body mismatch | failure | `retries` in a row → `unhealthy` |
| `unknown` — it could not run | host name does not resolve, invalid URL, command cannot be spawned | failure | `retries` in a row → `unknown` |

A `condition: healthy` edge to an external stem waits until its probe
passes, bounded by the external's `start_timeout`: past it `up` reports the
external `failed` with `HEALTH_TIMEOUT` (its state stays `unknown`/
`unhealthy`) and its dependants are skipped. An external stem without a
health check stays `unknown` and satisfies every edge immediately.

## Degraded

Degraded (glyph `!`, colour yellow, `summary.degraded`) is derived for every
`status` snapshot and never stored. Only a `healthy` stem can be degraded;
`state` stays `healthy`, `degraded: true`, and `reason` lists the reasons
joined with `; `:

| Reason | When | Deliverable |
|---|---|---|
| `dependency <name> unhealthy` / `dependency <name> failed` | a hard (`soft: false`) dependency is `unhealthy` or `failed` | 21 |
| `flapping` | ≥ 3 health transitions (`stem.health`) in the last 60 s | 21 |
| `restarts (N recently)` | the restart tracker's threshold (`is_restart_degraded`) | 22 |
| a metric threshold, e.g. `memory > 2GB` | a threshold of `limits` is crossed ([metrics.md](metrics.md#thresholds)) | 25 |

Not degrading: a dependency that is `unknown` (an external stem stems cannot
reach — it cannot tell the service is down), or `stopped`/`starting`/
`seeding` (deliberate or transient). A plain healthy stem has an empty
`reason`.

## Status fields

`status` (`StemStatus`, see [protocol.md](protocol.md)) adds:

* `degraded: bool` and `reason` as above;
* `health: {type, last: {ts, ok, outcome, latency_ms, detail},
  consecutive_failures, transitions_60s, container?}` — `null` when the stem
  has no health check; `container` is Docker's own health status for
  container stems.

## `stems health [stem] [--last N] [--json]`

The last probe results per stem (the daemon keeps 50), oldest first: without
a stem each stem's latest result, with one its last 10 (`--last` changes
both). Human output has one row per probe:

```
STEM      TYPE  OK   LATENCY  DETAIL               TS
echo-svc  http  yes  2ms      HTTP 200             12:00:01.200
echo-svc  http  no   3ms      HTTP 503 (want 2xx)  12:00:01.400
```

`OK` is `yes`, `no` or `?` (the probe could not run). JSON: `data =
{stems: [{name, type, state, consecutive_failures, transitions_60s,
results: [{ts, ok, outcome, latency_ms, detail}]}]}` (RPC `health {stems?,
last?}`).

## Overhead

Twenty process stems probed over tcp every 200 ms keep the daemon under 10 %
of one CPU (measured ~3 %; `tests/features/health/overhead.feature`, `@slow`); at the
default 2 s interval the probe overhead is well under 1 %.
