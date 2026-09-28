# Metrics

stems samples CPU, memory and process counts of every running stem, keeps
a history for the session, turns `limits:` into `degraded` warnings and
shows it all in `stems metrics`, `stems status --json` and the TUI.

```sh
stems metrics                      # table with sparklines and a TOTAL row
stems metrics --sort mem           # what is eating my laptop
stems metrics api --history 5m --json
stems metrics --disk               # build outputs and docker volumes (slow)
stems metrics --watch 1s           # redraw until Ctrl-C
```

## Configuration

```yaml
metrics:
  interval: 2s       # sampling interval (default 2s; tests use 200-500ms)
  persist: false     # also append samples to disk (see Persistence)

stems:
  api:
    limits:
      memory: 2GB            # or "2GB for 30s", or bytes as an integer
      cpu: "80% for 60s"     # or "150%", or cores as a number (1.5)
```

`limits.cpu` is a percentage of **one core** (`150%` = 1.5 cores; a bare
number is cores, as before). `for <duration>` makes the threshold count only
once the value has stayed above it that long. Both can live in
`stems.local.yaml` like any stem setting.

## The sample

One sample per running stem per interval:

```json
{ "ts": "2026-09-26T10:00:00Z", "cpu_pct": 12.5, "rss_bytes": 104857600,
  "children": 3, "uptime_s": 42, "restarts": 0 }
```

| Field | Process stems | Docker / compose stems |
|---|---|---|
| `cpu_pct` | CPU time of the process tree over wall time (below) | Docker's formula (below) |
| `rss_bytes` | resident memory summed over the process group | memory usage minus page cache (`inactive_file` on cgroup v2, `total_inactive_file`/`cache` on v1), as `docker stats` |
| `children` | processes in the group, the leader included | `pids_stats.current` |
| `uptime_s` | seconds since the current process started | same |
| `restarts` | policy restarts so far | same |

External stems are not sampled (`latest: null`). A stopped or failed stem
has `latest: null`; its history is kept for the session.

### Formulas

- **Process CPU %** — the tree is read with `os::process_tree(pgid)`
  (libproc on macOS, `/proc` on Linux). For every process present now, its
  CPU-time growth since the previous sample (all of its CPU time if it is
  new) is summed; `cpu_pct = Σ Δcpu_ms / Δwall_ms × 100`. One fully busy core
  is 100, so multi-threaded stems can exceed 100. A process that exited
  between two samples loses its last slice (the value never goes
  negative). The first sample after a start is 0.
- **Docker CPU %** — one `GET /containers/<id>/stats?stream=false` per
  interval (Docker takes two readings ~1 s apart itself):
  `cpu_delta = cpu_stats.cpu_usage.total_usage − precpu_stats.cpu_usage.total_usage`,
  `system_delta = cpu_stats.system_cpu_usage − precpu_stats.system_cpu_usage`,
  `cpu_pct = cpu_delta / system_delta × online_cpus × 100` (`online_cpus`
  falls back to the length of `percpu_usage`, then 1).

`cpu_pct` is rounded to one decimal.

## Sampling

One sampler task per daemon wakes every `metrics.interval` (at least
100 ms). It reads the stems' snapshots (it never talks to, or waits for, a
stem's actor), then:

- process stems: all process trees in **one** blocking call;
- docker/compose stems: one stats call per container, each in its own task
  with a 5 s timeout, never two at once for a stem;
- open ports: from the runtime's `describe`, refreshed every 10 s (reported
  as `open_ports` by the RPC).

A restart starts a fresh CPU baseline and resets the thresholds. Overhead:
20 process stems at 2 s keep the daemon under 3 % of one core
(`tests/features/metrics/overhead.feature`, `@slow`).

## History

Per stem, a ring of `30 min / interval` samples (900 at 2 s) in memory for
the daemon's lifetime. `stems metrics --history 5m` (RPC `history_ms`)
returns the samples of that window, oldest first; the human table always
draws sparklines of the last 30 samples (RPC `last: 30`).

## Persistence

With `metrics.persist: true` every sample is appended as one NDJSON line
(the sample above) to

```
$STEMS_HOME/<ws-hash>/metrics/<stem>.ndjson
```

At the first write of a new UTC day the file is renamed
`<stem>-YYYY-MM-DD.ndjson` (the day it holds) and a new one started; the 7
newest rotated files per stem are kept.

## Thresholds

Each `limits:` entry becomes a tracker (`stems_core::metrics::ThresholdTracker`):

- **crossed** — the value is strictly above the limit on a sample, and has
  stayed above it for the `for` duration (a sample at or below the limit
  restarts the wait). stems emits `stem.threshold` with `state: crossed`
  and the stem, if `healthy`, shows as **degraded** (`!`, `degraded: true`)
  with the reason `memory > 100MB` / `cpu > 80% for 1m`
  ([health.md](health.md#degraded)).
- **cleared** — the value drops below 90 % of the limit (10 % hysteresis,
  so a value hovering at the limit does not flap); `stem.threshold` with
  `state: cleared`, the degraded reason goes away.

```json
{"kind":"stem.threshold","stem":"api","reason":"memory > 100MB crossed",
 "data":{"metric":"memory","value":157286400.0,"limit":104857600.0,"for_s":0,"state":"crossed"}}
```

A stem that stops forgets its crossed thresholds. `stems metrics --json`
lists each stem's `limits: [{metric, limit, for_s, crossed}]`.

## Disk (`--disk`)

Measured only on request, since it walks directories (FR-MT-3):

- `codebase_build_bytes` — apparent size of the files under the build-output
  directories at the codebase root: `target`, `dist`, `build`,
  `node_modules`, `.venv` (symlinks not followed, hard links counted once,
  at most 200 000 entries per stem; `truncated: true` when the walk was cut
  short); `dirs` lists each directory measured;
- `volumes_bytes` — the stem's named docker volumes from `docker system df`
  (`null` without volumes or when Docker is unreachable).

Results are cached for 60 s per stem. `totals.disk_bytes` sums both.

## `stems metrics`

```
STEM  CPU%  MEM     CHILDREN  UPTIME  RESTARTS  CPU-SPARK   MEM-SPARK
a     0.3   17.8MB  1         3s      0         ▁▁▂▁▁▁▁▁▁   ▅██████▇▇
d     1.0   17.5MB  1         1s      0         ▁▂▁▁        ▃███
TOTAL 1.3   35.3MB  2         -       -         -           -
```

- `CPU%` percent of one core; `MEM` resident memory (1KB = 1024 B);
  stopped stems show `-`.
- Sparklines: CPU scaled 0..max(100, peak), memory auto-scaled; ASCII
  (`_.,-~=+#`) with `--no-color`, `STEMS_ASCII=1` or a non-UTF-8 locale.
- `--sort cpu|mem` orders the stems highest first (in `--json` too; stems
  without a sample last).
- `--watch [<interval>]` redraws (default 2 s; NDJSON frames in JSON mode),
  like `stems status --watch`.
- `--disk` adds a `DISK` column.

JSON (`--json`): `data` is the `metrics` RPC result
(see [protocol.md](protocol.md#methods)):

```json
{"interval_ms":2000,
 "stems":[{"name":"api","type":"process","state":"healthy",
           "latest":{"ts":"…","cpu_pct":1.2,"rss_bytes":18612224,"children":1,"uptime_s":42,"restarts":0},
           "open_ports":[8080],"limits":[{"metric":"memory","limit":104857600.0,"for_s":0,"crossed":false}]}],
 "totals":{"cpu_pct":1.2,"rss_bytes":18612224,"children":1}}
```

`stems status --json` carries the latest numbers of a running stem as
`metrics: {ts, cpu_pct, rss_bytes, children}`; the TUI builds its CPU/MEM
sparkline columns from it (last 30 samples).
