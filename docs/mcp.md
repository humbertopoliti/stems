# MCP server (`stems mcp`)

`stems mcp` exposes a workspace to any [Model Context Protocol](https://modelcontextprotocol.io)
client (Claude Code, Cursor, Claude Desktop, ...): tools to read and change the
environment, one tool per custom script, resources and prompts. It is a thin
proxy to the workspace daemon (`docs/protocol.md`); every action it takes is
recorded in the event log with the actor `mcp:<client name>` (FR-AI-1..4).

```
stems mcp [--transport stdio|http] [--port 7070] [--auto-start] [--workspace <dir>]
```

| Flag | Meaning |
|---|---|
| `--transport stdio` | (default) MCP over stdin/stdout; the client starts `stems mcp` itself |
| `--transport http` | Streamable HTTP on `http://127.0.0.1:<port>/mcp`; local only, no auth, off by default |
| `--port` | HTTP port (default 7070) |
| `--auto-start` | start the workspace daemon when a tool needs it (see below) |
| `--workspace` | the workspace (default: `STEMS_WORKSPACE`, else walk up from the cwd) |

Nothing but the protocol is written to stdout. The server returns when the
client disconnects (stdio) or on SIGINT/SIGTERM (http).

## Configuring a client

Claude Code (`.mcp.json` in the integration repo, or `claude mcp add`), Cursor
(`.cursor/mcp.json`) and Claude Desktop all take the same shape:

```json
{
  "mcpServers": {
    "stems": {
      "command": "stems",
      "args": ["mcp", "--workspace", "/path/to/integration-repo", "--auto-start"]
    }
  }
}
```

With Claude Code: `claude mcp add stems -- stems mcp --workspace "$PWD" --auto-start`.
Drop `--auto-start` if you prefer to start the environment yourself (`stems up`).

## The daemon and `--auto-start`

A fresh daemon connection is opened for every tool call, so a daemon started or
restarted behind the server's back is picked up.

* **Without `--auto-start`** and without a running daemon, tools that need it
  fail with `DAEMON_NOT_RUNNING` (hint: `stems up`, `stems daemon start` or
  `--auto-start`). Read-only tools that can work from the config on disk still
  do: `list_stems`, `get_config`, `get_graph` (config only, every stem
  `stopped`), `doctor`, and `tools/list` (custom scripts from the config).
* **With `--auto-start`**, the first tool that needs the daemon starts it
  detached (`stems daemon --home … --workspace …`, which loads the workspace).
  `tools/list` never starts it. When the MCP client disconnects, a daemon *this
  server started* is shut down **if no stem is running**; stems an agent left
  running keep it alive (stop them with the `down` tool, or `stems down`).
  A daemon that was already running is never stopped by the MCP server.

## Tools

Every result is JSON text (one text content block, pretty-printed). A failure
is a tool result with `isError: true` whose text is the stems error:
`{code, message, hint, path, details}`.
Calls that ran but failed (`ok: false`: a script that exited non-zero, an `up`
where a stem failed) are also `isError: true`, with the full result as text.

| Tool | Arguments | Daemon | What it does |
|---|---|---|---|
| `list_stems` | – | optional | stems from the config (type, description, enabled, dependencies, ports, custom scripts) plus live state |
| `get_status` | `stems?` | yes | the `status` RPC (never the env) |
| `get_graph` | `focus?`, `profile?` | optional | graph JSON `{nodes, edges, live}` with live glyphs when the daemon runs |
| `start` | `stems`, `no_deps?`, `timeout_ms?` | yes | start and wait until ready |
| `stop` | `stems`, `cascade?`, `timeout_ms?` | yes | stop (`HAS_DEPENDANTS` without `cascade`) |
| `restart` | `stems`, `no_deps?`, `build?`, `timeout_ms?`, `cascade?` | yes | restart keeping ports; `cascade` also restarts the running hard dependants ([restart.md](restart.md#cascading-restarts-fr-lc-9)) |
| `up` | `stems?`, `profile?`, `fresh?`, `confirm?`, `timeout_ms?` | yes | bring up; progress notifications; **`fresh` is destructive** |
| `down` | `stems?`, `all?`, `volumes?`, `confirm?` | yes | stop; progress notifications; **`all` / `volumes` are destructive** |
| `run_script` | `stem?`, `name`, `args` (object), `start_deps?` | yes | run a script and wait; progress notifications |
| `get_logs` | `stems?`, `since?`, `level?`, `grep?`, `tail?`, `cursor?`, `limit?` | yes | paginated log records (below) |
| `get_metrics` | `stems?`, `history?` (`5m`) | yes | CPU/RSS/process samples and totals |
| `get_events` | `since_seq?`, `kinds?`, `actor?`, `limit?` | yes | the event log, filterable by kind and actor |
| `get_config` | `stem?`, `effective?` | no | the resolved config, secrets redacted |
| `doctor` | `fix?`, `confirm?` | optional | runs `stems doctor` in-process; **`fix` is destructive** |
| `reset` | `stems`, `confirm` | yes | **destructive**: stop, run `reset`, clear stamps |
| `watch_pause` / `watch_resume` | `stems?` | yes | pause / resume watchdogs |
| `get_health` | `stems?`, `last?` | yes | last probe results per stem |
| `get_outputs` | `stems?` | yes | stem outputs; secret ones are always `<redacted>` |
| `<stem>__<script>` | the script's `args` | yes | one tool per custom script (below) |

The input schemas come from the argument types (schemars; field docs become the
descriptions agents read). The whole catalogue for `examples/workspaces/minimal`
is the golden `schema/mcp-tools.json`: tool names and schemas are a public
contract (`UPDATE_SCHEMA=1 cargo test -p stems-mcp --test golden` regenerates it
after a deliberate change).

### Custom scripts as tools (FR-AI-2)

Every **custom** script (not the lifecycle ones: `setup`, `start`, ...) becomes a
tool named `<stem>__<script>`, or `workspace__<script>` for workspace-level
scripts, with every character outside `[A-Za-z0-9_]` replaced by `_`
(`shop-api` / `create-test-user` → `shop_api__create_test_user`). Its
description is the script's `description`; its input schema is built from the
script's `args` (types, enums, defaults, required). Calling it is `run_script`
with the arguments as a JSON object: values are validated and coerced like
`stems run` arguments, and invalid ones fail with `SCRIPT_ARGS_INVALID`.

The list is rebuilt on every `tools/list` (from the running daemon's
`script_catalog`, else the config on disk), so config changes show up on the
next listing. The server advertises `tools.listChanged`; deliverable 33 sends
`notifications/tools/list_changed` on config reload through
`StemsMcp::notify_tools_changed()`.

### `get_logs` pagination and limits

* At most **500 records per call** (`limit`, default 500), oldest first.
* Without `cursor`: with `since` or `tail`, the matching window from its
  oldest record; with neither, the newest 500 records.
* Every result has `records`, `returned`, `truncated` and `next_cursor`.
  `truncated: true` means the result does not hold every matching record (more
  after this page, or older records left out). Pass `next_cursor` back as
  `cursor` to continue; the cursor is opaque and carries the position and the
  filters (`stems`, `level`, `grep`), so the other arguments are ignored with
  it. A call past the end returns no records and the same cursor, so an agent
  can poll for new lines.
* `get_events` returns at most 500 events (`limit`, default 100) with
  `truncated` and `next_since_seq`.

## Destructive actions (FR-AI-3)

A call is destructive when it can lose data or tear the environment down:
`down` with `all` or `volumes`, `up` with `fresh`, `reset`, `doctor` with `fix`.
It runs only when **both** hold:

1. the call passes `confirm: true` (an agent should ask the user first), else
   `DESTRUCTIVE_NOT_CONFIRMED`;
2. the workspace allows it, else `DESTRUCTIVE_NOT_ALLOWED`, whose hint names the
   setting:

```yaml
# stems.yaml (or stems.local.yaml for just your machine)
agent:
  allow_destructive: true      # default false
```

or `stems config set agent.allow_destructive true`. The setting is read from
disk on every call, so a human can flip it while the server runs.

`agent.allowed_tools` (if non-empty, only matching tools exist) and
`agent.denied_tools` (matching tools never exist) are glob lists over tool
names, custom script tools included (`denied_tools: [reset, "workspace__*"]`).
A filtered tool is absent from `tools/list`, and calling it anyway is a `USAGE`
error ("disabled for agents in this workspace").

## Actors and auditing (FR-AI-4)

Every daemon request carries the actor `mcp:<client name>`, the `name` of the
client's `clientInfo` from `initialize` (`mcp:claude-code`, `mcp:cursor`, ...;
`mcp:unknown` if the client sent none). The events the request causes (state
changes, script runs, `up`/`down`) carry the same actor, so `stems events`,
`stems events -f --json` (FR-CL-4) and the TUI show what an agent did, and
`get_events {"actor": "mcp:claude-code"}` lists it.

## Resources

| URI | Content |
|---|---|
| `stems://workspace` | the resolved workspace config (JSON, secrets redacted) |
| `stems://graph` | the dependency graph (JSON, live glyphs when the daemon runs) |
| `stems://<stem>/config` | one stem's resolved config (JSON, redacted) |
| `stems://<stem>/logs?tail=200` | the stem's last `tail` log lines as text (`<ts> <stream> <level> [<script>] <text>`; default 200, at most 500) |

`resources/list` enumerates the workspace, the graph and each stem's config and
logs; `resources/templates/list` has `stems://{stem}/config` and
`stems://{stem}/logs{?tail}`.

## Prompts

* `diagnose_stem {stem}`: the stem's status (state, reason, pid, restarts,
  last error), its last 10 health probes, its last 20 error-level log lines and
  its recent events, followed by instructions to find the cause with the tools
  and to ask before anything destructive.
* `bring_up_and_report {profile?}`: the current status and the steps to bring
  the environment up with `up`, check it and report per stem.

## Secrets

The MCP server never reveals secrets: `get_outputs` never asks the daemon for
secret values, `get_status` never includes the environment, and config JSON
(`get_config`, `stems://…/config`, `doctor`) is redacted: string values whose key
looks like a secret (`*PASSWORD*`, `*SECRET*`, `*TOKEN*`, `*API_KEY*`,
`*PRIVATE_KEY*`, `*CREDENTIAL*`, `*ACCESS_KEY*`, `*PASSWD*`) and URL passwords
(`postgres://user:<redacted>@host`) become `<redacted>`.

## Limits and non-goals

* HTTP transport: `127.0.0.1` only, no authentication, off unless asked for.
* Long calls (`up`, `down`, `run_script`, `start`, ...) wait up to 15 minutes.
* No embedded assistant (`stems ask`, FR-AI-5) in v1.
