# stems examples

This folder is both a worked example for new users and the fixture set the
test suite validates against (REQUIREMENTS.md §7.1).

```
examples/
  repos/            # stand-in code repositories (never modified by stems)
    shop-api/       # Python stdlib HTTP service with /__chaos/* endpoints
    shop-web/       # static site + tiny dev server
    shop-worker/    # queue consumer with a chaos control port
  workspaces/
    minimal/        # one process stem; the smallest valid workspace
    shop-lite/      # api + web + worker, no containers  ← start here without Docker
    hello-shop/     # the full example: docker, compose, external, every feature
    broken/         # one deliberately invalid workspace per error class
```

Only Python 3 is needed for `minimal` and `shop-lite`. `hello-shop` also
needs Docker (Docker Desktop, Colima or OrbStack) with the compose v2 plugin.

## 0. Build the binary

There is no published release yet, so build from source (Rust 1.96):

```sh
git clone <this-repo> && cd stems
cargo build --release -p stems-cli
export PATH="$PWD/target/release:$PATH"   # or copy target/release/stems somewhere on PATH
stems --version
```

## 1. Ten-minute tour with `shop-lite` (no Docker)

```sh
cd examples/workspaces/shop-lite
stems validate           # schema, paths, cycles, ports; prints the start order
stems doctor             # tools, ports, codebases, scripts, orphans
stems up                 # starts api + worker, then web; opens the dashboard
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
stems down            # stops everything in reverse order, removes overlays, stops the daemon
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

```sh
cd examples/workspaces/hello-shop
cp stems.local.yaml.example stems.local.yaml    # optional per-developer overrides
stems doctor
stems up --profile backend --detach             # postgres, redis, shop-api, shop-worker
stems up                                        # everything, attached
stems run postgres seed-large -- --rows 100000
stems down --volumes --yes                      # also removes the hello-shop_pgdata volume
```

| Stem | Type | Demonstrates |
|---|---|---|
| `postgres` | docker | image, ports, volumes, env, `command` health check, `reset` and `seed`/`seed-large` scripts |
| `redis` | compose | wrapping an existing `compose/redis.yml` file |
| `shop-api` | process | `setup` with stamps and `inputs`, `env_files`, an overlay, `http` health, a watchdog restart, custom scripts with args, depends on postgres + redis |
| `shop-worker` | process | `on-failure` restart policy with backoff, depends on redis (healthy) + postgres (seeded), a chaos control HTTP port |
| `shop-web` | process | `port: auto`, `${stem.shop-api.port}` substitution, depends on shop-api |
| `httpbin` | external | monitor-only; `unknown` when unreachable, never started or stopped |

The docker and compose paths were developed on a machine without Docker and are
verified by the `@docker` scenarios (`make e2e-docker`); see `docs/docker.md`
and `docs/compose.md` for the verification checklist.

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
