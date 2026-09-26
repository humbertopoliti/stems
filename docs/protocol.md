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
  state.json    state store (deliverable 11)
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
| `events` | `{since_seq?, limit?}` | `{events: [Event], last_seq}` — buffered events with `seq > since_seq`, oldest first |
| `subscribe_events` | `{since_seq?}` | ack `{subscribed: true, last_seq}`, then `event` notifications (see below) |
| `load_workspace` | `{path}` | `{root, name, stems: [name], sources: [path]}`; emits `workspace.loaded`; config errors are returned as the first error with all of them in `details.errors` |
| `shutdown` | `{}` | `{stopping: true}`, then the orderly shutdown path runs |
| `_debug.start_raw` | `{spec: ProcessSpec}` | runtime `Handle` (only with `STEMS_DEBUG_RPC=1`) |
| `_debug.stop_raw` | `{handle, grace_ms?}` | `"graceful" \| "killed" \| "already_dead"` |
| `_debug.describe` | `{handle}` | `RuntimeFacts` |

Reserved for later deliverables (answer `NOT_IMPLEMENTED` until then): `up`,
`down`, `start`, `stop`, `restart`, `status` (10), `logs`, `subscribe_logs` (12),
`run_script` (13). `Method` is an open string type: new methods never change
the frame format.

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
`stem.health`, `process.exited`, `process.output`, `up.started`, `up.finished`,
`down.started`, `down.finished`, `script.queued`, `script.started`,
`script.finished`, `watch.triggered`, `watch.paused`, `watch.reconfigured`,
`config.changed`, `config.invalid`.

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
