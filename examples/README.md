# stems examples

This folder is both a worked example for new users and the fixture set the
test suite validates against.

```
examples/
  repos/            # stand-in code repositories (never modified by stems)
    shop-api/       # Python stdlib HTTP service with /__chaos/* endpoints
    shop-web/       # static site + tiny dev server
    shop-worker/    # queue consumer with a chaos control port
  workspaces/
    minimal/        # one process stem; the smallest valid workspace
    shop-lite/      # api + web + worker + one redis container  ← start here
                    #   (`--profile local`: the three Python services, no Docker)
    hello-shop/     # the full example: docker, compose, external, every feature
    broken/         # one deliberately invalid workspace per error class
```

Only Python 3 is needed for `minimal` and for `shop-lite --profile local`.
`shop-lite`'s default profile adds one container (redis), and `hello-shop`
needs Docker (Docker Desktop, Colima or OrbStack) with the compose v2 plugin.

## 0. Build the binary

There is no published release yet, so build from source (Rust 1.96):

```sh
git clone <this-repo> && cd stems
cargo build --release -p stems-cli
export PATH="$PWD/target/release:$PATH"   # or copy target/release/stems somewhere on PATH
stems --version
```

## 1. Ten-minute tour with `shop-lite`

`shop-lite` runs the three Python services as local processes plus `redis`
as a docker stem (`redis:7`, host port 16380 → container 6379), the
worker's queue. The worker's edge to redis is `soft`, so it starts (and
keeps retrying) even while redis is down. No Docker? Use the `local`
profile, which leaves redis out: `stems up --profile local`.

```sh
cd examples/workspaces/shop-lite
stems validate           # schema, paths, cycles, ports; prints the start order
stems doctor             # tools, ports, codebases, scripts, orphans
stems up                 # starts redis, api and worker, then web; opens the dashboard
                         # (no Docker: `stems up --profile local`)
```

The dashboard is a full-screen TUI. Useful keys: `Tab` cycles Graph → Table →
Detail → Logs → Events, `j/k` move, `Enter` opens the detail of a stem,
`:` opens the script menu, `Ctrl-P` is a command palette, `?` shows all keys,
`q` asks whether to stop everything, detach, or cancel. Choose **d** to detach
and keep things running; the commands below assume the daemon is still up
(or use `stems up --detach` from the start).

```sh
stems status                      # table with glyphs, pids, ports, uptime, restarts
stems status --json               # the same data for scripts and agents
stems graph --edges               # dependency graph with live status
stems logs shop-api -f            # follow one stem; Ctrl-C to stop
stems logs -f                     # all stems interleaved
stems logs shop-api --level error --since 5m
stems metrics                     # CPU, memory, children, sparklines
stems health shop-api             # last probe results with latency
stems events -f --json            # NDJSON event stream, the hook for automation

# run a custom script declared in stems.yaml (arguments are validated)
stems run shop-api create-test-user -- --email a@b.c --role admin
stems logs shop-api --script create-test-user

# see the overlay stems rendered into the code repo (removed again on down)
stems overlays
cat ../../repos/shop-api/config/local.ini
```

### Break things on purpose

`shop-api` and `shop-worker` expose chaos endpoints because the workspace sets
`SHOP_CHAOS=1`. Ports are the declared ones (18080 for the api, 18081 for the
worker's control port); `shop-web` gets a free port shown in `stems status`.

```sh
curl "localhost:18080/__chaos/unhealthy?for=5s"   # /healthz returns 503 → ✗ unhealthy, then ✓ again
curl "localhost:18081/__chaos/crash?code=3"       # worker exits 3 → restarted with backoff (see RESTARTS)
curl "localhost:18080/__chaos/fork?n=3"           # child processes → shown in `stems metrics` CHILDREN
curl "localhost:18080/__chaos/logs?n=200&level=error"
curl "localhost:18080/__chaos/alloc?mb=200"       # memory grows in `stems metrics`; alloc?mb=0 frees it
curl "localhost:18080/__chaos/spin?ms=3000"       # CPU spike
curl "localhost:18080/__chaos/hang-on-stop"       # ignores SIGTERM → SIGKILL after stop_grace on down
touch ../../repos/shop-api/app.py                  # watchdog restarts shop-api (see `stems events`)
stems watch pause shop-api                        # ...and stop it doing that
```

Kill the daemon to see crash recovery:

```sh
kill -9 "$(stems daemon status --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["data"]["pid"])')"
stems status          # DAEMON_NOT_RUNNING, with a hint that stems are still alive
stems up --detach     # adopts the running processes (events show stem.adopted), nothing started twice
stems down            # cleans everything up, including the overlay
```

### Change config while running

```sh
stems config set stems.shop-api.env.GREETING hello   # writes stems.local.yaml, keeps comments
stems config diff                                    # shop-api: restart_required (env)
stems config apply --yes                             # restarts only shop-api
stems config unset stems.shop-api.env.GREETING
```

### Tear down

```sh
stems down            # stops everything in reverse order (removes the redis container and the overlays), stops the daemon
stems doctor          # confirms nothing is left behind
```

## 2. `minimal`: one stem

```sh
cd examples/workspaces/minimal
stems up --detach && stems status && stems run echo-svc ping && stems down --all
```

Used by the fast test suite; a good template for your own first workspace
(`stems init --from minimal` scaffolds it into a new directory).

## 3. `hello-shop`: the full example (needs Docker)

Adds `postgres` (docker stem: image, volume, command health check, seed
scripts), `redis` (compose stem wrapping `compose/redis.yml`) and `httpbin`
(external stem: monitored, never started). Both process stems depend on the
containers, so nothing starts without Docker; `stems validate` still works and
`stems doctor` tells you what is missing.

The host needs only Docker (Desktop, Colima, OrbStack, ...) with compose v2
and `python3`: health checks of the containers run inside them, and the seed
scripts and shop-api's `/products` (`SHOP_PSQL`) use `docker exec ...
psql`, so no host `psql`, `pg_isready` or `redis-cli` is needed. stems finds
the `docker` CLI even when Docker Desktop did not put it on `PATH`.

```sh
cd examples/workspaces/hello-shop
cp stems.local.yaml.example stems.local.yaml    # optional per-developer overrides
stems doctor
stems up --detach                               # all six; ~15 s from a warm image cache
curl localhost:18080/products                   # the rows postgres's `seed` inserted
stems up --profile backend --detach             # or: postgres, redis, shop-api, shop-worker
stems run postgres seed-large -- --rows 100000
stems logs postgres --grep ready
stems down --volumes --yes                      # also removes the hello-shop_pgdata volume
```

| Stem | Type | Demonstrates |
|---|---|---|
| `postgres` | docker | image, ports, volumes, env, `command` health check (run inside the container), `reset` and `seed`/`seed-large` scripts |
| `redis` | compose | wrapping an existing `compose/redis.yml` file; `command` health check inside the service's container |
| `shop-api` | process | `setup` with stamps and `inputs`, `env_files`, an overlay, `http` health, a watchdog restart, custom scripts with args, depends on postgres + redis; a `docker` variant (`stems switch shop-api docker`) runs it as a container built from its Dockerfile |
| `shop-worker` | process | `on-failure` restart policy with backoff, depends on redis (healthy) + postgres (seeded), a chaos control HTTP port |
| `shop-web` | process | `port: auto`, `${stem.shop-api.port}` substitution, depends on shop-api |
| `httpbin` | external | monitor-only; `unknown` offline (`unhealthy` if it answers slower than its 2 s timeout), never started or stopped |

Verified zero-to-ready on macOS (Intel, Docker Desktop with Engine 29.8.0
and compose v5.5.1) by hand and by the `@docker` scenario
`tests/features/scripts/hello-shop-zero-to-ready.feature` (`make
e2e-docker`); `down --volumes --yes` leaves no container, volume or network
behind. See `docs/docker.md` and `docs/compose.md`.

### Switch a component between local and Docker

`shop-api` declares a `docker` variant (FR-ST-8, `docs/config.md` →
Variants): the same codebase built from `repos/shop-api/Dockerfile` and run
as the container `hello-shop-shop-api`, published on 18080 → 8080, with
the database URL pointing at `host.docker.internal` and the overlay's
`config/local.ini` mounted read-only. `stems switch` writes the choice into
`stems.local.yaml` and restarts only shop-api; shop-web, the worker and
the containers keep running.

```sh
stems up --detach
stems switch shop-api              # lists the choices: * local (process), docker (docker)
stems switch shop-api docker       # stops the process, builds the image, runs the container
docker ps --filter name=hello-shop-shop-api
curl localhost:18080/products      # served by the container (see below)
stems status                       # shop-api: TYPE docker, no pid
stems switch shop-api local        # back to the process; the container is removed
curl localhost:18080/products      # the seeded rows again
stems down --volumes --yes
```

The image has no `psql` and no docker CLI, so the container's `/products`
serves shop-api's built-in in-memory list (`WARN DATABASE_URL set but psql
not found` in `stems logs shop-api`); the process form reads the seeded rows
through `SHOP_PSQL`. The first switch on a machine builds the image
(`start_timeout: 3m` covers a cold `python:3-slim` pull); later switches
take a few seconds. Scenario: `tests/features/variants/hello-shop-switch.feature`.

### Git codebases

Instead of a relative path, a stem's `codebase` can be a git URL; stems clones
it under `.stems/repos/<stem>` on first `up` and never touches a dirty tree:

```yaml
  shop-api:
    codebase: { git: git@github.com:acme/shop-api.git, ref: main }
```

`stems repos status` / `stems repos sync` manage the checkouts, and a
developer can repoint a stem at a local checkout in `stems.local.yaml`.

## 4. `broken/*`: the error catalogue

Each directory has a `stems.yaml` with exactly one problem and an
`EXPECTED.json` naming the error code. Try them:

```sh
cd examples/workspaces/broken/cycle && stems validate; echo "exit $?"
cd ../port-conflict && stems validate --json | python3 -m json.tool
```

## 5. Agents: the MCP server

```sh
stems mcp --workspace examples/workspaces/shop-lite --auto-start
```

Point Claude Code, Cursor or any MCP client at it (`docs/mcp.md` has the
config snippet). Every command above is a tool; custom scripts become tools
automatically; destructive tools need `confirm: true` and
`agent.allow_destructive: true` in `stems.yaml`.

## Where state lives

Nothing is written into `examples/repos` except declared overlays. The daemon
socket, lock, `state.json`, logs and metrics live under
`~/Library/Application Support/stems/<workspace-hash>/` (macOS) or
`$XDG_STATE_HOME/stems/…` (Linux); `STEMS_HOME` overrides it. `stems doctor`
finds and fixes stale files there.
