# stems

**Your whole local environment as one versioned file.**

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

Everything the dashboard shows is also available as JSON, over a socket
protocol and as an MCP server, so AI agents can operate the environment the
same way you do.

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
- **Agent-ready:** `--json` on every command, a daemon protocol, and
  `stems mcp`, which exposes the workspace to Claude Code, Cursor and other
  MCP clients.

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

- [Getting started: onboard your system](docs/getting-started.md)
- [Configuration](docs/config.md)
- [Lifecycle](docs/lifecycle.md), [process model](docs/process-model.md) and
  [recovery](docs/recovery.md)
- [Health checks](docs/health.md), [restart policies](docs/restart.md) and
  [watchdogs](docs/watchdogs.md)
- [Scripts](docs/scripts.md), [overlays](docs/overlays.md) and
  [repositories](docs/repos.md)
- [Docker](docs/docker.md) and [Compose](docs/compose.md)
- [Logs](docs/logs.md) and [metrics](docs/metrics.md)
- [The dashboard](docs/tui.md)
- [MCP server](docs/mcp.md) and [daemon protocol](docs/protocol.md)
- [Doctor](docs/doctor.md)

## Status

Early: 0.x releases, and the config format may still change between minor
versions (`schema_version` guards it). Runs on macOS and Linux; Windows is out
of scope.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). In short:

```sh
make ci      # what CI runs
make check   # the full local suite, including process and e2e tests
```

Maintainers cutting a release: [`release/RELEASING.md`](release/RELEASING.md).

## License

[MIT](LICENSE)
