<div align="center">

# stems

**Your whole local environment as one versioned file, operable by you and by your AI agents.**

[![CI](https://github.com/humbertopoliti/stems/actions/workflows/ci.yml/badge.svg)](https://github.com/humbertopoliti/stems/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/humbertopoliti/stems?sort=semver)](https://github.com/humbertopoliti/stems/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![MCP server](https://img.shields.io/badge/MCP-server-8A2BE2)](docs/mcp.md)
![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)
![Written in Rust](https://img.shields.io/badge/written%20in-Rust-orange)

[Install](#install) · [Quick start](#quick-start) · [MCP for agents](#built-for-ai-agents-stems-mcp) · [Commands](#everyday-commands) · [Docs](#documentation)

</div>

---

Working on a large system usually means running five to twenty things at
once: databases in containers, a message broker, several backend services, a
frontend or two, a worker. How to start them usually lives in READMEs, shell
aliases, half-maintained compose files and people's heads.

stems replaces that with a `stems.yaml` you commit next to your code. It
describes every running thing (a *stem*): how it is built, configured, seeded,
started, health-checked and connected to the others. Then:

```sh
stems up      # start everything, in dependency order, and open the dashboard
stems down    # stop everything
```

```
 stems · shop                     1 Graph  2 [Table]  3 Detail  4 Logs  5 Events
  STEM      TYPE     STATUS     REASON  PID    PORTS  UPTIME  RESTARTS  CPU   MEM
  postgres  docker   ✓ healthy  -       -      15432  3m12s   0
› api       process  ✓ healthy  -       41822  8080   3m04s   0

 ■ x stop  ↻ r restart  : scripts (0)  o editor  ? more
 shop · profile - · daemon pid 4242 · ✓2 !0 ✗0 ↯0 ·0 ?0 ↻0         ? help · q quit
```

## Built for AI agents: `stems mcp`

> [!TIP]
> **stems ships a first-class [Model Context Protocol](https://modelcontextprotocol.io) server.**
> Claude Code, Cursor, Claude Desktop and any other MCP client can start,
> stop, inspect, debug and script your environment through the same runtime
> you use, with guardrails you control.

One command connects Claude Code to a workspace:

```sh
claude mcp add stems -- stems mcp --workspace "$PWD" --auto-start
```

Or, for any MCP client (`.mcp.json`, `.cursor/mcp.json`, Claude Desktop):

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

Then ask your agent things like *"bring up the backend profile and tell me
what's unhealthy"* or *"the api keeps restarting, find out why"*.

What the agent gets:

| | |
|---|---|
| **Runtime control** | `up`, `down`, `start`, `stop`, `restart` (with cascade), `watch_pause` / `watch_resume` |
| **Observability** | `get_status`, `get_health`, `get_logs` (filtered, paginated), `get_metrics`, `get_events`, `get_graph` |
| **Your scripts as tools** | every custom script becomes a typed tool, e.g. `shop_api__create_test_user`, with its arguments as the input schema |
| **Resources** | `stems://workspace`, `stems://graph`, `stems://<stem>/config`, `stems://<stem>/logs` |
| **Prompts** | `diagnose_stem` and `bring_up_and_report`, ready-made investigation flows |

And what keeps it safe:

- **Destructive actions are double-gated.** `reset`, `up --fresh`, `down`
  with volumes and `doctor --fix` need both `confirm: true` from the agent
  *and* `agent.allow_destructive: true` in the workspace, which is off by
  default.
- **You choose the toolset.** `agent.allowed_tools` / `agent.denied_tools`
  globs hide tools from agents entirely.
- **Every action is audited.** Agent requests carry the actor
  `mcp:<client>` (`mcp:claude-code`, `mcp:cursor`, …) into the event log, so
  `stems events` and the dashboard show exactly what an agent did.
- **Secrets never leave.** Config, outputs and connection strings are
  redacted before an agent sees them.

The full tool catalogue, schemas and limits: [`docs/mcp.md`](docs/mcp.md).

## Features

- **Four stem types:** local `process`es, `docker` containers, `compose`
  services, and `external` endpoints that stems only health-checks.
- **Dependency graph:** `depends_on` with readiness conditions. Stems start
  in order, wait for their dependencies to be healthy, and a restart can
  cascade to dependants. `stems graph` draws it.
- **Health checks:** HTTP, TCP and command probes, with retries, start
  periods and flapping detection.
- **Supervision:** restart policies with backoff, and watchdogs: file
  watchers that restart or rebuild a stem, run a script or signal it on
  change.
- **Scripts:** per-stem and workspace scripts (`setup`, `seed`, `reset`,
  custom ones with typed arguments), run with `stems run`.
- **Logs, events and metrics:** merged, searchable logs; a structured event
  log; CPU and memory per stem, with thresholds.
- **Profiles, variants and overlays:** named subsets (`up --profile
  backend`), per-stem alternatives, and personal overrides in
  `stems.local.yaml`.
- **A terminal dashboard:** status, graph, logs and events, with every
  action one key away.
- **Automation-friendly:** `--json` on every command, a documented
  [daemon protocol](docs/protocol.md), and the [MCP server](#built-for-ai-agents-stems-mcp).

## Install

macOS (Apple Silicon and Intel), with Homebrew:

```sh
brew install humbertopoliti/tap/stems
```

This installs the binary, shell completions and man pages. On macOS or Linux
without Homebrew:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/humbertopoliti/stems/releases/latest/download/stems-cli-installer.sh | sh
```

`stems upgrade` updates either install. Tarballs, uninstalling and supported
targets: [`docs/install.md`](docs/install.md).

## Quick start

```sh
mkdir my-env && cd my-env
stems init            # scaffold stems.yaml, scripts/, env/, overlays/
$EDITOR stems.yaml
stems validate        # check the config and the graph
stems up              # start everything and open the dashboard
```

A small `stems.yaml`:

```yaml
schema_version: 1
name: shop

stems:
  postgres:
    type: docker
    image: postgres:16
    ports: [{ name: pg, port: 15432, container_port: 5432 }]
    env: { POSTGRES_PASSWORD: local, POSTGRES_DB: shop }
    health:
      type: command
      command: pg_isready -h 127.0.0.1 -d shop

  api:
    type: process
    codebase: ../shop-api
    depends_on: [postgres]
    env:
      DATABASE_URL: postgres://postgres:local@localhost:15432/shop
    ports: [{ name: http, port: 8080 }]
    health: { type: http, url: "http://localhost:8080/health" }
    scripts:
      start: python3 app.py
      seed: python3 seed.py
```

To onboard a real system step by step (infrastructure, services, the
dependency graph, scripts, variants, profiles and handing it to your team),
follow [Getting started](docs/getting-started.md). Worked examples to try,
from the smallest to one that uses every feature, are in
[`examples/`](examples/README.md).

## Everyday commands

| Command | What it does |
|---|---|
| `stems up [stems…]` / `stems down` | Start or stop the environment (or some stems and their dependencies) |
| `stems start` / `stop` / `restart <stem>` | Control single stems |
| `stems attach` | Reopen the dashboard |
| `stems status` | What is running and healthy |
| `stems logs [stem] -f` | Follow logs, merged or per stem |
| `stems events` | The structured event log |
| `stems graph` | The dependency graph |
| `stems run <stem> <script>` | Run a stem's script (`--ws` for a workspace script) |
| `stems exec` / `stems shell <stem>` | Run a command or open a shell in a stem's environment |
| `stems doctor` | Diagnose the machine and the workspace |
| `stems mcp` | Serve the workspace to MCP clients |

Every command takes `--json`. The full reference is
[`docs/cli.md`](docs/cli.md).

## Documentation

| Topic | Pages |
|---|---|
| **Start here** | [Getting started: onboard your system](docs/getting-started.md) · [Install](docs/install.md) · [Configuration](docs/config.md) |
| **Running** | [Lifecycle](docs/lifecycle.md) · [Process model](docs/process-model.md) · [Recovery](docs/recovery.md) |
| **Supervision** | [Health checks](docs/health.md) · [Restart policies](docs/restart.md) · [Watchdogs](docs/watchdogs.md) |
| **Workflows** | [Scripts](docs/scripts.md) · [Overlays](docs/overlays.md) · [Repositories](docs/repos.md) |
| **Containers** | [Docker](docs/docker.md) · [Compose](docs/compose.md) |
| **Observability** | [Logs](docs/logs.md) · [Metrics](docs/metrics.md) · [The dashboard](docs/tui.md) · [Doctor](docs/doctor.md) |
| **Agents & integration** | [MCP server](docs/mcp.md) · [Daemon protocol](docs/protocol.md) · [CLI reference](docs/cli.md) |

## Status

Early: 0.x releases, and the config format may still change between minor
versions (`schema_version` guards it). Runs on macOS and Linux; Windows is out
of scope. Changes are tracked in [`CHANGELOG.md`](CHANGELOG.md).

## Contributing

Contributions are welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md) and the
[code of conduct](CODE_OF_CONDUCT.md). In short:

```sh
make ci      # what CI runs
make check   # the full local suite, including process and e2e tests
```

Security issues: please follow [`SECURITY.md`](SECURITY.md) rather than
opening a public issue. Maintainers cutting a release:
[`release/RELEASING.md`](release/RELEASING.md).

## License

[MIT](LICENSE)
