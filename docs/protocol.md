# Local API protocol

How the CLI (and later the TUI and MCP server) talk to `stemsd`, the per-workspace
daemon (REQUIREMENTS FR-CR-5/6, FR-CL-4, FR-DS-2, NFR-4). Code:
`crates/stems-api` (wire types + client), `crates/stems-daemon` (server).

## Where the daemon lives

```
$STEMS_HOME/<ws-hash>/
  stemsd.sock   Unix socket, mode 0600 (directory 0700)
  stemsd.lock   {"pid", "start_time", "version", "created_at"}
  stemsd.log    daemon log (STEMS_DAEMON_LOG overrides the path)
  state.json    running stems + stamps, written atomically (recovery.md)
```

* `$STEMS_HOME` defaults to `~/Library/Application Support/stems` (macOS) or
  `$XDG_STATE_HOME/stems` / `~/.local/state/stems` (Linux).
* `<ws-hash>` is the first 12 hex characters of the SHA-256 of the canonical
  workspace root directory path (`stems_daemon::workspace_hash`; a path to
  `stems.yaml` hashes like its directory, symlinks are resolved).
* One daemon per workspace: the lock is created exclusively. A lock whose pid is
  dead, or alive with a different process start time (pid reuse), is **stale**
  and reclaimed on the next start (logged as `stale lock reclaimed`). A live
  holder makes a second daemon fail with `LOCK_HELD`. The socket file is only
  removed/re-bound after the lock is held.

## Frames

JSON-RPC 2.0, **one JSON object per line** (`\n`-terminated, UTF-8, max 4 MiB
per frame) in both directions. Requests on one connection are answered in order.

Request (every request **must** carry `meta`):

```json
{"jsonrpc":"2.0","id":1,"method":"ping","params":{},
 "meta":{"actor":"cli:alice","client_version":"0.1.0","api_version":1}}
```

* `id`: number or string, echoed in the response.
* `params`: an object (omitted = `{}`).
* `meta.actor`: `cli:<user>`, `tui:<user>`, `mcp:<client>`; recorded on every
  event the request causes.
* `meta.api_version`: must equal the daemon's `API_VERSION` (currently `1`).

Response: exactly one of `result` / `error`:

```json
{"jsonrpc":"2.0","id":1,"result":{"pong":true}}
{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"method `up` is not implemented",
  "data":{"code":"NOT_IMPLEMENTED","message":"method `up` is not implemented","path":null,
          "location":null,"hint":"…","details":{"method":"up"}}}}
```

`error.data` is always the full stems error (the same object `--json` prints in
`errors[]`), so clients map it back 1:1 to a `stems_core::Error`.

Notification (subscription frames, no `id`):

```json
{"jsonrpc":"2.0","method":"event","params":{ …Event… }}
```

## From the CLI

```sh
stems daemon start --json        # spawn detached, wait ≤ 5 s for the socket
# {"ok":true,"data":{"already_running":false,"pid":4242,"version":"0.1.0","api_version":1,
#  "uptime_s":0,"workspace":"/path/ws","socket":"…/stemsd.sock","lock":"…/stemsd.lock",
#  "log":"…/stemsd.log"},"errors":[],"version":"0.1.0"}
stems daemon start --json        # again: same data with "already_running": true, exit 0

stems daemon status --json       # `daemon_status` + paths
# data: {running: true, pid, version, api_version, workspace, workspace_name, uptime_s,
#        started_at, start_time, socket, lock, lock_state: "held", log, stem_count,
#        last_seq, subscribers, debug_rpc, client_version, compatible, state}
# not running: exit 4 DAEMON_NOT_RUNNING, data: {running: false, lock_state: "free"|"stale",
#        workspace, socket, lock, log, state}; a stale lock puts "stale lock: …" in the hint,
#        live stems of a crashed run "stems from a previous run are still alive; run `stems down`"
# state: {run_id, stems, alive, path} from state.json, or null (recovery.md)

stems events --json --since 0    # NDJSON: one Event per line, no envelope
# {"ts":"…","seq":1,"kind":"daemon.started","stem":null,"from":null,"to":null,"reason":null,"actor":"daemon","data":{…}}
# {"ts":"…","seq":2,"kind":"workspace.loaded",…}
stems events -f --json           # the same replay, then live events until the daemon
                                 # stops (after daemon.stopped) or Ctrl-C; exit 0

stems daemon stop --json         # `shutdown`, then wait ≤ 5 s for socket + lock to go
# data: {stopped: true, pid, via: "rpc"|"signal"}; stale lock: {stopped: false,
#        stale_lock_removed: true, pid}; nothing at all: exit 4 DAEMON_NOT_RUNNING
```

`stems daemon status` and `stems daemon stop` accept a daemon of another stems
version (same API version) so that the `DAEMON_VERSION_MISMATCH` hint
(`stems daemon stop && stems daemon start`) always works; `status` then reports
`compatible: false` with the mismatch error (exit 4). If even the API version
differs, `stop` falls back to SIGTERM on the lock's pid. Every other command
refuses a mismatched daemon. `_debug.*` methods have no CLI subcommand; the e2e
harness calls them over the socket (`When I call the daemon RPC ...`).

## Try it by hand

```sh
sock="$STEMS_HOME/$(…ws-hash…)/stemsd.sock"   # `stems daemon status --json` prints it
nc -U "$sock"
{"jsonrpc":"2.0","id":1,"method":"ping","params":{},"meta":{"actor":"cli:me","client_version":"0.1.0","api_version":1}}
```

The daemon answers `{"jsonrpc":"2.0","id":1,"result":{"pong":true}}`. Type
another line to send another request; a malformed line gets a `-32700` error and
the connection stays usable. For a stream:

```
{"jsonrpc":"2.0","id":2,"method":"subscribe_events","params":{"since_seq":0},"meta":{"actor":"cli:me","client_version":"0.1.0","api_version":1}}
```

## Methods

| Method | Params | Result |
|---|---|---|
| `ping` | `{}` | `{"pong": true}` |
| `info` | `{}` | `DaemonInfo {version, api_version, workspace, pid, start_time, uptime_s, started_at}` |
| `daemon_status` | `{}` | `DaemonInfo` fields + `{workspace_name, stem_count, last_seq, subscribers, debug_rpc}` |
| `events` | `{since_seq?, limit?, kinds?, actor?}` | `{events: [Event], last_seq}` — buffered events with `seq > since_seq`, oldest first; `kinds` (exact, or a prefix ending in `*`) and `actor` filter before `limit` applies (31) |
| `subscribe_events` | `{since_seq?}` | ack `{subscribed: true, last_seq}`, then `event` notifications (see below) |
| `load_workspace` | `{path}` | `{root, name, stems: [name], sources: [path]}`; emits `workspace.loaded`; config errors are returned as the first error with all of them in `details.errors` |
| `shutdown` | `{}` | `{stopping: true}`, then the orderly shutdown path runs |
| `up` | `UpParams {stems?, profile?, detach?, timeout_ms?, fail_fast? (true), max_parallel? (4), pass_env? {K: V}, daemon_auto_started?, fresh?, force_overlays?, sync?, no_watch?}` | `UpResult {ok, requested, ready: [stem], failed: [{stem, error}], skipped: [stem]}` — long-running; progress is the event stream (see [lifecycle.md](lifecycle.md)) |
| `down` | `DownParams {stems?, all?, timeout_ms?, volumes?}` | `DownResult {ok, stopped, skipped, failed: [{stem, error}], daemon_stopping, volumes_removed?}`; with `daemon_stopping` the daemon shuts down right after replying; `volumes` removes the selected docker stems' named volumes (14) |
| `start` | `StartParams {stems, no_deps?, timeout_ms?}` | `UpResult`; external stems: `NOT_MANAGED` |
| `stop` | `StopParams {stems, cascade?, timeout_ms?}` | `DownResult`; running dependants without `cascade`: `HAS_DEPENDANTS` (`details.dependants`) |
| `restart` | `RestartParams {stems, no_deps?, build?, timeout_ms?}` | `UpResult` (ports kept); `build: true` runs each stem's `build` script between stop and start ([scripts.md](scripts.md)) |
| `status` | `StatusParams {stems?, verbose?}` | `StatusResult {stems: [StemStatus], summary: {healthy, degraded, failed, unhealthy, stopped, unknown, starting}}` (`unhealthy`, a running or external stem whose probe fails, is counted apart from `failed`); `StemStatus` has `degraded` and `health {type, last: {ts, ok, outcome, latency_ms, detail}, consecutive_failures, transitions_60s, container?}` ([health.md](health.md)) |
| `health` | `HealthParams {stems?, last? (10, max 50)}` | `HealthResult {stems: [{name, type, state, consecutive_failures, transitions_60s, results: [{ts, ok, outcome: ok\|fail\|unknown, latency_ms, detail}]}]}` — the last probe results per stem, oldest first ([health.md](health.md)) |
| `metrics` | `MetricsParams {stems?, history_ms?, last?, sort?: cpu\|mem, disk?}` | `MetricsResult {interval_ms, stems: [{name, type, state, latest: Sample \| null, history?: [Sample], open_ports, limits: [{metric, limit, for_s, crossed}], disk?: {codebase_build_bytes, dirs: [{path, bytes}], volumes_bytes, truncated}}], totals: {cpu_pct, rss_bytes, children, disk_bytes?}}`; `Sample = {ts, cpu_pct, rss_bytes, children, uptime_s, restarts}`. `history_ms` returns that window, `last` the last N samples; `sort` orders highest first; `disk` measures build outputs and volumes (cached 60 s). Unknown stem: `UNKNOWN_STEM` ([metrics.md](metrics.md)) |
| `outputs` | `OutputsParams {stems?, reveal?}` | `OutputsResult {stems: [{name, outputs: [{name, value, secret}]}]}` — evaluated stem outputs (26); `value` `null` until healthy, `"<redacted>"` for secrets unless `reveal` ([config.md](config.md#outputs-fr-st-6)) |
| `watch_pause` | `WatchPauseParams {stems?}` | `WatchPauseResult {stems, global_paused}` — pause watchdogs (24): no stems = global; event `watch.paused` ([watchdogs.md](watchdogs.md)) |
| `watch_resume` | `WatchPauseParams {stems?}` | `WatchPauseResult` — resume; no stems = global, which also clears per-stem pauses; event `watch.resumed` |
| `watch_status` | `WatchStatusParams {stems?}` | `WatchStatusResult {stems: [{name, rules: [{paths, ignore, action, debounce_ms, settle_ms, root, dir}], paused, active, last_triggered, pending, busy}], global_paused, disabled}` — stems with `watch:` rules; `disabled` after `up --no-watch` |
| `config_diff` | `{}` | `ConfigDiffResult {plan: ReloadPlan {stems: [{name, action, changes, fields, hot, running}], workspace, catalog_changed}, pending, loaded_at, detected_at?, sources, last_error?}` — the plan from the applied config to the one on disk, read now; `last_error` when it is invalid (33, [config.md](config.md#reload-on-change-fr-wd-3)) |
| `config_apply` | `ConfigApplyParams {stems?, yes}` | `ConfigApplyResult {applied: [{stem, action, result}], failed: [{stem, action, error}], skipped: [{stem, action, reason}], workspace, pending, ok}` — stop, install, hot-apply, start; `yes: true` required (`DESTRUCTIVE_NOT_CONFIRMED`); invalid config on disk: its errors; event `config.applied` |
| `switch_variant` | `SwitchVariantParams {stem, variant?}` | `SwitchVariantResult {stem, from, to, type, variants: [{name, type, active}], path, file, changed, diff, applied}` — without `variant`: the stem's choices (`local` first; nothing written). With one: writes `stems.<stem>.variant` to `stems.local.yaml` (comment-preserving; `local` removes the key), re-validates (the file is restored on errors), then `config_apply {stems: [stem], yes: true}` (`applied`). `stems switch` and the TUI's `v` use it; `UNKNOWN_STEM`, `UNKNOWN_VARIANT` (FR-ST-8, [config.md](config.md#variants-fr-st-8)) |
| `query_logs` | `{stems?, since?, until?, grep?, level?, script?, tail?, from_files?}` | `{records: [LogRecord], truncated}` — oldest first, interleaved by `ts`, at most 10 000 (the newest; `truncated` says more matched). Times: `10m`, `1.5s` (before the daemon's clock) or RFC 3339; `level`: `error` (exact) or `warn+` (and above). Unknown stem: `UNKNOWN_STEM`; bad filter: `USAGE` (see [logs.md](logs.md)) |
| `subscribe_logs` | `{stems?, since?, grep?, level?, script?, tail?}` | ack `{subscribed: true, replay: n}`, then `log` notifications (see below) |
| `export_logs` | `{path, since?}` (`path` absolute) | `{path, entries: [name], bytes}` — writes the `.tar.gz` bundle ([logs.md](logs.md#export-bundle)) |
| `adopt_orphans` | `{orphans: [{stem, pid}]}` | `{adopted: [{stem, pid, pgid}], failed: [{stem, pid, error}]}` — registers running processes found by the orphan scan as their stems (`stem.adopted`); see [recovery.md](recovery.md) |
| `build` | `BuildParams {stems?}` | `BuildResult {ok, built: [{stem, script, exit, duration_ms}], skipped, failed: [{stem, error}]}` — runs `build` scripts ([scripts.md](scripts.md)) |
| `reset` | `ResetParams {stems?}` | `ResetResult {ok, stopped, reset: [{stem, script, exit, duration_ms}], cleared, failed}` — stops the stems, runs `reset`, clears their stamps |
| `stamps` | `StampsParams {stem?, clear?}` | `StampsResult {stamps: [{stem, script, hash, computed_at, inputs}], cleared}` |
| `run_script` | `RunScriptParams {stem? (null: workspace script), name, args? ([argv] \| {name: value}), wait? (true), start_deps?, ready_timeout_ms? (30000)}` | `RunScriptResult {run_id, stem, script, ok, exit, signal, duration_ms, timed_out, attempts, argv, tail, queued, error?}`; with `wait: false` at once `RunScriptAccepted {run_id, stem, script}` (follow the `script.*` events with that `data.run_id`). A failed script is a successful call with `ok: false` and `error` (`SCRIPT_FAILED`); bad args: `SCRIPT_ARGS_INVALID` (`details {arg, reason, stem, script}`); unhealthy `requires`: `SCRIPT_REQUIRES_UNMET` (`details {requires, unmet: [{stem, state}], start_error}`); stem still starting past the bound: `START_TIMEOUT` ([scripts.md](scripts.md#custom-scripts-and-stems-run)) |
| `script_catalog` | `ScriptCatalogParams {stem?}` | `ScriptCatalogResult {scripts: [{stem, name, description, args: [{name, type, default, required, description, values}], requires, kind: lifecycle\|custom, timeout, retries, concurrent, mcp_tool, input_schema}]}` — what the TUI (30) and MCP (31) consume |
| `stem_config` | `{stem}` | the stem's resolved config (`stems_config::Stem` JSON: `type`, `env`, `ports`, `scripts`, `health`, ...; `UNKNOWN_STEM` otherwise) — the TUI detail view (27) |
| `overlays` | `OverlaysParams {stem?}` | `OverlaysResult {overlays: [{stem, dest, status: present\|modified\|missing, keep, sha256, run_id}]}` ([overlays.md](overlays.md)) |
| `repos_sync` | `ReposSyncParams {stems?, force_fetch? (true), recurse_submodules?}` | `ReposSyncResult {ok, repos: [{stem, path, action, ref, sha, message, error?}]}` — clone / fetch / check out git codebases ([repos.md](repos.md)) |
| `repos_status` | `ReposStatusParams {stems?}` | `ReposStatusResult {repos: [{stem, source, path, url, ref, branch, sha, dirty, ahead, behind, exists}]}` |
| `_debug.start_raw` | `{spec: ProcessSpec}` | runtime `Handle` (only with `STEMS_DEBUG_RPC=1`) |
| `_debug.stop_raw` | `{handle, grace_ms?}` | `"graceful" \| "killed" \| "already_dead"` |
| `_debug.describe` | `{handle}` | `RuntimeFacts` |

`StemStatus` is `{name, type, state, glyph, reason, pid, pgid, ports: [{name,
port, auto}], uptime_s, started_at, restarts, restarts_in_window, seeded, health, error}` plus `metrics`
(`{ts, cpu_pct, rss_bytes, children}`, the latest sample of a running stem; 25) and `env` (what
stems set for the process) with `verbose`, and `outputs` (`{NAME: value}`,
secrets `"<redacted>"`, omitted when empty; 26) and `watch` (`{paused, rules}`,
only for stems with `watch:` rules; 24). Semantics of the lifecycle methods:
[lifecycle.md](lifecycle.md).

Methods a daemon does not know answer `NOT_IMPLEMENTED`. `Method` is an
open string type: new methods never change the frame format.

`_debug.*` `handle` is either the `Handle` object returned by `start_raw` or its
numeric `id`. Debug processes emit `process.output` (`data: {handle, pid,
stream: "stdout"|"stderr", text}`) and `process.exited` (`data: {handle, pid,
code, signal}`) events and are stopped on daemon shutdown.

### Subscriptions

`subscribe_events` is answered with a normal response (the ack), after which the
same connection carries only `event` notifications: first the buffered events
with `seq > since_seq` (none when `since_seq` is omitted), then live ones, with
no gap and no duplicate. Anything the client writes afterwards is ignored; the
stream ends when the client closes the connection or after the daemon's
`daemon.stopped` event. Use a dedicated connection for each subscription. The
daemon buffers the last 10 000 events; a subscriber that falls behind the live
channel is caught up from that buffer.

`subscribe_logs` works the same way with `log` notifications whose `params`
is one `LogRecord` (`{ts, stem, stream, tag, level, text, fields}`, see
[logs.md](logs.md)). With `since` or `tail` the matching history (ring, then
rotated files) is replayed first — the ack's `replay` says how many — then live
records follow with no duplicate; without either only live records are sent.
The filters apply to live records too (except the time window). A subscriber
that falls behind the live channel (4096 records) skips records rather than
slowing the daemon down.

## Events

```json
{"ts":"2026-09-26T12:00:00Z","seq":7,"kind":"stem.state","stem":"api",
 "from":"starting","to":"healthy","reason":"health check passed",
 "actor":"cli:alice","data":{"pid":4242}}
```

* `seq` starts at 1 per daemon run and increases by 1.
* `stem`, `from`, `to`, `reason` are always present (`null` when not relevant);
  `data` is always an object.
* `actor` is the requesting client's `meta.actor`, `daemon` for things the
  daemon does on its own, or `signal` for a signal-initiated shutdown.

Kinds defined so far (`stems_api::EventKind`; later deliverables add more, so
consumers must ignore unknown kinds): `daemon.started`, `daemon.stopping`,
`daemon.stopped`, `workspace.loaded`, `stem.state`, `stem.port_allocated`,
`stem.adopted`, `stem.recovered_dead`, `stem.restarting`, `stem.gave_up`,
`cascade.started`, `cascade.queued`, `cascade.finished`, `cascade.aborted`,
`stem.health`, `stem.threshold`, `stem.outputs`, `process.exited`, `process.output`, `profile.expanded`, `up.started`, `up.finished`,
`down.started`, `down.finished`, `script.queued`, `script.started`,
`script.finished`, `watch.triggered`, `watch.action_finished`, `watch.paused`,
`watch.resumed`, `watch.reconfigured`,
`config.changed`, `config.invalid`, `config.applied`, `config.pending`,
`tools.changed`, `overlay.materialised` (`data: {dest,
keep, backup}`), `overlay.removed`, `overlay.kept`,
`overlay.modified_left_in_place` (`data: {dest}`; see
[overlays.md](overlays.md)), `repo.cloned`, `repo.fetched`,
`repo.checked_out`, `repo.skipped_dirty`, `repo.failed` (see
[repos.md](repos.md#logs-and-events)), `docker.pull` (`data: {stem, image,
layer, status}`, one per layer status change) and `docker.build` (`data:
{stem, line}`) (see [docker.md](docker.md)).

Config reload payloads (deliverable 33, [config.md](config.md#reload-on-change-fr-wd-3)):
`config.changed` `data: {plan, sources, auto_apply}` (reason `config
changed: N stems affected`); `config.invalid` `data: {errors, codes}`;
`config.applied` `data: {applied, failed: [{stem, action, code}], skipped,
workspace, auto, pending}` (or `{applied, workspace, implicit: true}` when a
command applied a change that restarts nothing); `config.pending` `data:
{stems}` (a command used the applied config while these running stems wait
for `config apply`); `watch.reconfigured` (stem) `data: {rules: [{paths,
action, debounce_ms, settle_ms}], running}`; `tools.changed` `data: {added,
removed, changed, agent_changed}` (MCP tool names; `stems mcp` sends
`notifications/tools/list_changed`). Their actor is `daemon` when the
config watcher acted.

Recovery payloads (deliverable 11): `stem.adopted` `data: {pid, pgid,
started_at, container_id}` (`container_id` for docker/compose stems, whose
`pid`/`pgid` are 0; followed by `stem.state` `stopped → healthy`, reason
`adopted`); `stem.recovered_dead` `data: {pid, pgid, start_time,
container_id}` (the state entry was cleared). See [recovery.md](recovery.md).

Script payloads (deliverables 16, 17): `script.started` `data: {script,
actor, args, run_id?, attempt?}`; `script.finished` `data: {script, exit,
signal, duration_ms, timed_out, cancelled, ok, run_id?, attempt?}`;
`script.queued` `data: {script, run_id, actor, reason}` (a `run_script` waiting
for another script of the same stem). `run_id` and `attempt` (1-based) are
set for `run_script` runs. See [scripts.md](scripts.md).

Cascading restart payloads (FR-LC-9, actor = whoever caused the cascade:
the client, `watchdog`, or `daemon` for a policy restart; `stem` = the
origin): `cascade.started` `reason` and `data: {id, origin, origins,
reason, stems}` (`stems` = the dependants' layers, `[["b", "c"], ["d"]]`);
`cascade.queued` `data: {id, origin, origins, reason, behind}` (the id of
the running cascade); `cascade.finished` `data: {id, origin, restarted
(names in restart order), layers, failed, skipped, ok}`; `cascade.aborted`
`data: {id, origin, stem, error}` (the origin did not become healthy;
nothing else was restarted). Each dependant's `stem.restarting` has `data:
{reason: "cascade from <origin>", cascade: <id>, counted: false, attempt:
0, delay_ms: 0}`. See [restart.md](restart.md#cascading-restarts-fr-lc-9).

Watchdog payloads (deliverable 24, actor `watchdog` unless a client paused/resumed):
`watch.triggered` `reason` (`watch: app.py changed`) and `data: {paths (≤ 10,
relative to the rule's root), path_count, action, rule_index}`;
`watch.action_finished` `data: {action, rule_index, ok, error?}`;
`watch.paused` / `watch.resumed` with `stem` (per-stem) or `data: {global:
true}`. See [watchdogs.md](watchdogs.md).

Output payloads (deliverable 26): `stem.outputs` `data: {names, secret}`
(the names of the outputs just evaluated and of the secret ones; never
values), emitted before the stem's `stem.state → healthy`. See
[config.md](config.md#outputs-fr-st-6).

Health payloads (deliverable 21): `stem.health` — one per health
transition of a running or monitored stem (`healthy ⇄ unhealthy`, and
`unknown ⇄ healthy/unhealthy` for external stems; never per probe, and not
for `starting → healthy`, which is a `stem.state`), with `from`/`to`,
`reason` (the last probe's detail) and `data: {probe, detail, latency_ms,
outcome, consecutive_failures}`. The matching `stem.state` event is emitted
too. See [health.md](health.md).

Metrics payloads (deliverable 25): `stem.threshold` — a `limits:`
threshold was crossed or cleared: `reason` (`memory > 100MB crossed`) and
`data: {metric: memory|cpu, value, limit, for_s, state: crossed|cleared}`
(`limit` in bytes or percent of one core). While crossed, a healthy stem is
`degraded` with that reason. See [metrics.md](metrics.md).

Lifecycle payloads (deliverable 10): `stem.state` has `from`/`to`/`reason`
and `data: {pid, error?, outcome?, exit_code?, signal?}`;
`stem.port_allocated` `data: {name, port}`; `process.exited` (supervised
stems) `data: {pid, code, signal}`; `profile.expanded` (26) `data: {profile,
added, requested}`; `up.started` `data: {profile, requested, stems,
layers, detach}`; `up.finished` `data: {requested, started, failed, skipped,
ok}`; `down.started` `data: {requested, all}`; `down.finished` `data:
{stopped, skipped, failed, daemon_stopping}`.

## Errors

| JSON-RPC `code` | When | `data.code` |
|---|---|---|
| `-32700` | the line is not JSON / longer than 4 MiB | `USAGE` |
| `-32600` | JSON but not a valid request (missing `meta`, `jsonrpc` ≠ `"2.0"`, …) | `USAGE` |
| `-32601` | unknown method | `NOT_IMPLEMENTED` (hint names the method) |
| `-32602` | bad params | `USAGE` |
| `-32001` | `meta.api_version` ≠ daemon's | `DAEMON_VERSION_MISMATCH` |
| `-32000` | any other stems error | the stems code (`LOCK_HELD`, `SCHEMA_INVALID`, …) |

Parse errors have `"id": null`. Client-side (no frame involved):
`DAEMON_NOT_RUNNING` (exit 4) when the socket is missing, refuses connections,
the 2 s connect timeout or the per-call timeout (30 s default) expires, or the
connection drops mid-call.

## Versioning

* `API_VERSION` (currently `1`) changes only on an incompatible change to the
  frame format or to an existing method's params/result. Adding methods, event
  kinds, optional params or result fields is compatible and does not bump it.
* The daemon rejects any request whose `meta.api_version` differs
  (`DAEMON_VERSION_MISMATCH`).
* The client additionally calls `info` right after connecting and refuses to
  talk to a daemon of a different **stems version** (`DAEMON_VERSION_MISMATCH`,
  exit 4, hint `restart the daemon: stems daemon stop && stems daemon start`),
  so an upgraded CLI never drives an old daemon. `STEMS_FAKE_VERSION` overrides
  the client's version for tests.

## Lifecycle

`stems daemon --home … --workspace … [--foreground]` (hidden; auto-spawned
detached in its own session with stdio appended to the log): take the lock →
log to `stemsd.log` → bind the socket (0600) → `daemon.started` → load the
workspace (`workspace.loaded`) → serve. A `shutdown` RPC or SIGTERM/SIGINT/SIGHUP
all run the same path: `daemon.stopping` → supervisor shutdown hook (`down`,
deliverable 10) → stop debug processes → close the listener → remove socket
and lock → `daemon.stopped` (best effort to subscribers) → exit 0. After a
`kill -9` the lock stays behind with a dead pid: clients see
`DAEMON_NOT_RUNNING` and `lock::probe` reports it as stale; the next start
reclaims it.
