# stems examples

> **Phase 0 banner:** `stems` itself does not exist yet — this deliverable
> (03) ships the config format and fixtures ahead of the tool. None of the
> commands below will actually run until deliverables 05 (config loading)
> and 06 (validation) land, and `stems up` needs the daemon/supervisor
> work in later phases. The YAML and scripts here are real and
> hand-checked, but treat every `stems ...` invocation below as a preview
> of the intended UX, not something you can run today.

This folder is both a worked example for new users and the fixture set the
test suite validates against (REQUIREMENTS.md §7.1).

```
examples/
  repos/            # stand-in code repositories (never modified by stems)
  workspaces/
    hello-shop/     # the full example: every stem type, every graph feature
    minimal/        # smallest valid workspace, used by the fast E2E suite
    broken/         # one deliberately invalid workspace per error class
```

## Walkthrough: clone to `stems up`

```sh
# 1. Install stems (once it ships)
brew install <org>/tap/stems

# 2. Clone this repo and go to the example integration repo
git clone <this-repo>
cd stems/examples/workspaces/hello-shop

# 3. (optional) copy the local override template and edit it
cp stems.local.yaml.example stems.local.yaml

# 4. Bring the whole workspace up, attached, with a live TUI
stems up

# Or bring up just the process stems + their declared deps, detached:
stems up --profile backend --detach
stems attach

# 5. Poke around
stems status --json
stems logs shop-api -f
stems graph

# 6. Run a custom script
stems run shop-api create-test-user -- --email test@example.com --role admin

# 7. Tear everything down
stems down
```

## Per-stem "what it demonstrates" table

| Stem | Type | Demonstrates |
|---|---|---|
| `postgres` | docker | image, ports, volumes, env, `command` health check, `reset` and `seed`/`seed-large` scripts |
| `redis` | compose | wrapping an existing `compose/redis.yml` file |
| `shop-api` | process | local (git-free) codebase path, `setup` with stamps and `inputs`, `env_files`, an overlay, `http` health, a watchdog restart, custom scripts with args (`create-test-user`), depends on postgres (healthy) + redis (healthy) |
| `shop-worker` | process | `on-failure` restart policy with backoff, depends on redis (healthy) + postgres (seeded), a chaos control HTTP port |
| `shop-web` | process | `port: auto`, `${stem.shop-api.port}` substitution, depends on shop-api |
| `httpbin` | external | monitor-only; shows `unknown` when offline, never started or stopped by stems |

### Docker and compose stems (`[postgres]`, `[redis]`)

`postgres` (docker, deliverable 14) and `redis` (compose, deliverable 15)
are **implemented, verified on Linux CI only**: the development machine
has no Docker, so the `@docker` scenarios (`make e2e-docker`) and the
checklists in `docs/docker.md` / `docs/compose.md` are the verification
path. What to expect:

- `stems up postgres` runs the container `hello-shop-postgres` (labels
  `stems.workspace=hello-shop`, `stems.stem=postgres`) on host port 15432,
  with the named volume `hello-shop_pgdata` (the config's
  `hello-shop-pgdata`, prefixed `<ws>_` without doubling the name).
- `stems up redis` runs `docker compose -f compose/redis.yml -p hello-shop
  up -d --no-deps redis`; the stem passes `REDIS_PORT` so a port override
  in `stems.local.yaml` reaches the compose file.
- `stems down` removes both containers and keeps the volume;
  `stems down --volumes --yes` also deletes `hello-shop_pgdata`.
- Without Docker, `stems up postgres` fails fast with `DOCKER_UNAVAILABLE`
  (hint "start Docker Desktop") before anything starts; a selection
  whose closure has no docker/compose stem never contacts Docker.

### Codebases from git (alternative)

Every example stem uses a local path (`codebase: ../../repos/shop-api`), so
the examples work offline. A codebase can instead be a git repository that
stems clones into `.stems/repos/<stem>` on the first `up` and keeps at a ref
with `stems repos sync` (see `docs/repos.md`):

```yaml
stems:
  shop-api:
    codebase: { git: git@github.com:acme/shop-api.git, ref: main }
```

A developer who already has it checked out points at their copy in
`stems.local.yaml` (`stems: { shop-api: { codebase: ~/work/shop-api } }`),
which replaces the git form entirely.

The `minimal` workspace is a single `echo-svc` process stem (shop-api run
standalone, no DB) with a 200 ms `tcp` health check — the workhorse for the
fast E2E suite, and small enough to read in one sitting.

The `broken/*` workspaces are intentionally invalid, one error class per
directory, each with an `EXPECTED.json` describing the error `code`,
config `path`, exit code and a `message_contains` substring (see
REQUIREMENTS.md §7.4/§7.5). They are the table-driven fixtures for 06's
validation scenarios, not something you should try to bring up.

## Chaos by hand

With `SHOP_CHAOS=1` (set in `stems.yaml` for `shop-api` and `shop-worker`),
the example services expose control endpoints so you can provoke failure
modes yourself, without waiting for a test suite:

```sh
# shop-api, once it is up on its declared port (18080 in hello-shop):
curl "http://localhost:18080/__chaos/crash?code=3"          # exits with code 3
curl "http://localhost:18080/__chaos/unhealthy?for=5s"       # /healthz -> 503 for 5s
curl "http://localhost:18080/__chaos/hang-on-stop"           # ignores SIGTERM
curl "http://localhost:18080/__chaos/fork?n=3"               # spawns 3 children
curl "http://localhost:18080/__chaos/logs?n=1000&level=error" # emits 1000 error lines
curl "http://localhost:18080/__chaos/alloc?mb=300"           # allocates 300MB
curl "http://localhost:18080/__chaos/touch?file=src/x.py"    # touches a watched file
curl "http://localhost:18080/__chaos/spin?ms=3000"           # busy-loops one core

# Env-driven, set before starting the stem:
SHOP_SLEEP_START=20      # delay before binding the port
SHOP_CRASH_ON_START=3    # exit immediately with this code on boot
```

See REQUIREMENTS.md §7.2 for the full table and what each endpoint is used
to verify.
