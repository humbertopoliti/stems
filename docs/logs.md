# Logs

Every line a stem (or a script run for it) prints is captured by the daemon,
timestamped, parsed, kept in memory and appended to a per-stem rotating file
(REQUIREMENTS §4.8, FR-LG-1/2/4/5). `stems logs` queries and follows it;
`stems logs --export` bundles it for a bug report. Wire shapes of the RPCs:
[protocol.md](protocol.md).

## Record schema

Every captured line becomes one record, serialised as one JSON object — the
same shape in `stems logs --json` (NDJSON, one per line), in the RPCs and in
the files on disk:

```json
{
  "ts": "2026-09-26T10:00:00.123456Z",
  "stem": "shop-api",
  "stream": "err",
  "tag": null,
  "level": "error",
  "text": "db connection refused",
  "fields": { "request_id": "r-42" }
}
```

| field    | type | meaning |
|----------|------|---------|
| `ts`     | RFC 3339 UTC string | daemon receive time (not the time the program printed) |
| `stem`   | string | stem that produced the line |
| `stream` | `"out"` \| `"err"` \| `"script"` | stdout, stderr, or output of a lifecycle/custom script |
| `tag`    | string \| `null` | script name for `stream: script` lines, else `null` |
| `level`  | `"trace"` \| `"debug"` \| `"info"` \| `"warn"` \| `"error"` \| `null` | detected level, `null` when unknown |
| `text`   | string | the message: `msg`/`message` of a JSON line, otherwise the raw line |
| `fields` | object \| `null` | remaining keys of a JSON line (level and message keys removed) |

All keys are always present (absent values are `null`).

### Level and structure detection (FR-LG-4)

- **JSON lines** (a line that is a JSON object): the first of `level`, `lvl`,
  `severity` (case-insensitive key; a name such as `warning`/`fatal` or a
  syslog/pino number) gives `level`; the first of `msg`, `message` gives
  `text`; both keys are removed and the rest becomes `fields`.
- **Plain lines**: a leading `ERROR`, `WARN`/`WARNING`, `INFO`, `DEBUG`,
  `TRACE` (also `ERR`, `FATAL`, `CRITICAL`), case-insensitive, optionally
  bracketed (`[error]`, `(warn)`, `<info>`) and optionally after an ISO-8601
  or `HH:MM:SS` timestamp; else a logfmt `level=…` pair; else `null`.

The stream (`out`/`err`) is recorded before any parsing, so a program that
logs errors to stdout (like the example `shop-api`) yields
`stream: out, level: error`.

### Records written by stems itself

Text starting with `[stems]`, `level: warn`:

- `[stems] dropped N lines (output storm)` (`stream: err`) — the stem printed
  faster than the daemon could keep up; N lines were skipped instead of the
  daemon buffering without bound (FR-LC-5).
- `[stems] adopted: live output unavailable, showing file log` (`stream: err`)
  — after a daemon crash the stem was adopted by the new daemon (FR-CR-2); its
  output pipes died with the old daemon, so only the lines already on disk
  exist.

## Where logs live

```text
$STEMS_HOME/<ws-hash>/logs/<stem>/current.log      newest
$STEMS_HOME/<ws-hash>/logs/<stem>/current.1.log
$STEMS_HOME/<ws-hash>/logs/<stem>/current.2.log    oldest (keep = 2)
```

`<ws-hash>` is the daemon directory of the workspace (next to `stemsd.sock`,
see [protocol.md](protocol.md)). Files hold one record JSON object per line,
so replaying history from them is exact.

The daemon writes the files whether or not a client is attached: one writer
task per stem owns the file and the in-memory ring (so both hold records in
the same order), writes everything queued in one batch and flushes once the
stem has been idle for 100 ms (and at least every 100 ms under continuous
output). The in-memory ring keeps the last `logs.ring` records per stem for
fast queries; older history is read from the files.

## Rotation and retention

Workspace `logs:` (defaults shown):

```yaml
logs:
  max_size: 50MB   # rotate current.log before a write would exceed this (0: never)
  keep: 3          # rotated files kept: current.1.log … current.<keep>.log
  ring: 10000      # records per stem kept in the daemon's memory
```

Before a write would push `current.log` past `max_size`, the files shift by
one (`current.<keep>.log` is deleted, `current.N.log` → `current.N+1.log`,
`current.log` → `current.1.log`) and a fresh `current.log` starts, so at most
`keep + 1` files exist per stem. A single line longer than `max_size` is still
written whole. Rotation settings apply at the next write after the workspace
is (re)loaded; a new `ring` size applies to stems whose sink is created
afterwards (i.e. after a daemon restart).

## `stems logs`

```text
stems logs [stem…] [-f] [--since 10m|<ts>] [--until …] [--grep re]
           [--level warn+] [--script name] [--tail n] [--json]
```

- No stems: every stem that has logs. Several stems interleave by `ts`.
- `--since`/`--until`: a duration before now (`10m`, `2h30m`, `1.5s`) or an
  RFC 3339 instant; inclusive.
- `--grep`: a regex matched against `text`.
- `--level error` matches exactly that level; `--level warn+` warn and above.
  Records without a detected level never match a level filter.
- `--script seed`: only output of that script (`tag`).
- `--tail n`: the last n matching records.
- At most 10 000 records per call (the newest); human output says so when
  more matched.
- Human output: `HH:MM:SS.mmm <stem> | [tag] text` (local time), the stem
  prefix coloured per stem (not with `--no-color`, `STEMS_NO_COLOR` or when
  stdout is not a terminal).
- `--json`: NDJSON, one record per line, no envelope (the same exception as
  `stems events`); errors still print the envelope. Exit 2 for an unknown
  stem (`UNKNOWN_STEM`) or a bad filter (`USAGE`), 4 without a daemon.
- `-f`: replays the last 10 lines (or `--since`/`--tail`), then streams new
  lines until Ctrl-C/SIGTERM (exit 0) or the daemon stops. The follower
  blocks on the socket: no polling.

## Export bundle

`stems logs --export [--since 1h] [-o bundle.tar.gz]` asks the daemon to write
a gzip-compressed tarball (default name `stems-logs-<date>-<time>.tar.gz` in
the current directory):

| entry | content |
|-------|---------|
| `status.json` | the `status` result |
| `events.ndjson` | the daemon's buffered events (last 10 000), one per line |
| `config.json` | `{sources, workspace}`: the resolved workspace, redacted |
| `logs/<stem>/current*.log` | every stem's log files (one record per line) |

With `--since`, older events and log records are left out.

**Redaction.** In every environment map of the config (`env`, `local_env`,
any `*_env`, `environment`), values whose key matches
`(?i)(secret|token|password|passwd|key)` are replaced by `"<redacted>"`
(`stems_daemon::logs::export::redact` takes extra patterns: the hook for
later secrets work). Log lines themselves are not redacted: review a bundle
before sharing it.

## Scripts (deliverable 16; 17 for custom scripts)

`LogHub::script_writer(stem, script)` returns a `ScriptWriter` whose lines are
recorded with `stream: script, tag: <script>` in the stem's log (FR-SC-4),
so `stems logs <stem> --script seed` shows a script run's output. The
script runner (`stems_daemon::scripts::ScriptRunner`, [scripts.md](scripts.md))
forwards stdout and stderr of every lifecycle script this way. Workspace
scripts (`bootstrap`, `teardown`) log under the pseudo-stem `_workspace`:
`stems logs _workspace --script bootstrap`.
