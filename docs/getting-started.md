# Getting started: onboard your system

This guide takes a real system from "a README and some aliases" to a
`stems.yaml` your team can clone and run with one command. It builds one
example step by step:

- **postgres** in a container;
- **redis** from a `docker-compose.yml` you already have;
- **api**, a backend service run from its code repository;
- **worker**, a queue consumer that needs redis and a migrated database;
- **web**, a frontend dev server that talks to the api.

Along the way it covers health checks, the dependency graph, scripts, ports
and environment wiring, variants, profiles, per-developer overrides and
handing the environment to a teammate. Each section links to the reference
page for the details.

If you have not installed stems yet, see [install.md](install.md). The
[examples](../examples/README.md) are runnable versions of everything below.

## 1. Create an integration repo

stems configuration lives in its own repository, the **integration repo**,
next to your code repositories. It holds `stems.yaml`, the scripts that set
up and seed things, env files and templates. Your code repositories stay
untouched.

```text
~/work/
  acme-env/        ← the integration repo (new)
  acme-api/        ← existing code repos
  acme-worker/
  acme-web/
```

```sh
mkdir ~/work/acme-env && cd ~/work/acme-env
git init
stems init --name acme
```

`stems init` creates:

| Path | What for |
|---|---|
| `stems.yaml` | the environment; the first line points editors at the JSON schema, so fields autocomplete and are checked as you type |
| `scripts/` | setup, seed and custom scripts |
| `env/` | env files (`env_files:`) |
| `overlays/` | templates rendered into code repos at start (see [overlays.md](overlays.md)) |
| `.gitignore` | ignores `.stems/` (clones, state) and `stems.local.yaml` (per-developer overrides) |

`stems init --from hello-shop` scaffolds a copy of the full example instead,
if you would rather start from something complete and delete what you don't
need.

## 2. Inventory what you run today

Before writing config, list every running thing and, for each one:

| Question | Becomes |
|---|---|
| Is it your code, a container, a compose service, or something hosted you only connect to? | `type: process`, `docker`, `compose` or `external` |
| Where is its code, and how do you start it? | `codebase:` and `scripts.start` |
| Which ports does it listen on? | `ports:` |
| How do you know it is up (an HTTP endpoint, an open port, a CLI check)? | `health:` |
| What must be running first, and must it be *ready* or just *started*? | `depends_on:` |
| Which env vars does it need, and which point at other services? | `env:`, `env_files:`, `${stem.<name>.port}` |
| One-off steps: install dependencies, run migrations, load data? | `scripts.setup`, `scripts.seed` |
| Handy commands people keep in their shell history? | custom scripts |

For the example:

| Stem | Type | Port | Healthy when | Needs |
|---|---|---|---|---|
| postgres | docker `postgres:16` | 15432 | `pg_isready` passes | — |
| redis | compose, `compose/redis.yml` | 16379 | `redis-cli ping` passes | — |
| api | process, `../acme-api` | 18080 | `GET /healthz` is 2xx | postgres healthy, redis healthy |
| worker | process, `../acme-worker` | 18081 | its control port is open | redis healthy, api seeded (migrations applied) |
| web | process, `../acme-web` | any free port | `GET /` is 2xx | api healthy |

Pick fixed host ports that don't clash with what developers already run
(hence `15432` rather than `5432`). Use `port: auto` when nobody needs to
remember the port.

## 3. Add infrastructure stems

Start with the things everything else depends on. A **docker** stem runs a
single container:

```yaml
schema_version: 1
name: acme

vars:
  pg_user: acme

stems:
  postgres:
    type: docker
    description: Postgres 16 for the api and worker
    image: postgres:16
    ports: [{ name: pg, port: 15432, container_port: 5432 }]
    env:
      POSTGRES_USER: "${var.pg_user}"
      POSTGRES_PASSWORD: local
      POSTGRES_DB: acme
    volumes: ["acme-pgdata:/var/lib/postgresql/data"]
    health:
      type: command
      command: "pg_isready -h 127.0.0.1 -U ${var.pg_user} -d acme"
      interval: 500ms
      retries: 5
      start_period: 1s
      start_timeout: 120s
```

- `vars:` are named values you reuse as `${var.<name>}`.
- A `command` health check on a docker stem runs **inside the container**, so
  `pg_isready` comes from the image and the host needs no client tools.
- `start_timeout` bounds how long the first start may take (image pulls).
- Named volumes are prefixed with the workspace name, so two workspaces never
  share data by accident.

If a service already lives in a compose file, wrap it with a **compose**
stem instead of rewriting it:

```yaml
  redis:
    type: compose
    description: Redis 7, the worker's queue and the api's cache
    file: compose/redis.yml
    service: redis
    project_name: acme
    env:
      REDIS_PORT: "${stem.self.port}"   # compose interpolates ${REDIS_PORT} in the file
    ports: [{ name: redis, port: 16379, container_port: 6379 }]
    health: { type: command, command: "redis-cli ping", interval: 500ms, retries: 5 }
```

A stem's `env` is compose's interpolation input, so the compose file can keep
publishing `${REDIS_PORT:-16379}` while stems stays the source of truth for
the port. One compose stem is one service: add a stem per service you want
stems to track. Details: [docker.md](docker.md), [compose.md](compose.md).

Check it:

```sh
stems validate
stems up postgres redis
stems status
```

## 4. Add your services

A **process** stem runs your code from its repository:

```yaml
  api:
    type: process
    description: The backend API
    codebase: ../acme-api
    depends_on:
      - { stem: postgres, condition: healthy }
      - { stem: redis, condition: healthy }
    env_files: [env/api.env]
    env:
      PORT: "18080"
      DATABASE_URL: "postgres://${var.pg_user}:local@localhost:${stem.postgres.port}/acme"
      REDIS_URL: "redis://localhost:${stem.redis.port}"
    ports: [{ name: http, port: 18080 }]
    health:
      type: http
      url: "http://localhost:${stem.self.port}/healthz"
      interval: 500ms
      timeout: 400ms
      retries: 3
      start_period: 2s
    stop_grace: 5s
    restart: { policy: on-failure, max: 3 }
    scripts:
      setup:
        file: scripts/api/setup.sh          # e.g. create a venv, install dependencies
        inputs: ["requirements.txt"]        # rerun only when these files change
      seed:
        file: scripts/api/seed.sh           # e.g. run migrations, load sample data
        inputs: ["migrations/**"]
      start: python3 -m acme_api
```

- `codebase` is relative to `stems.yaml`. Scripts run in the codebase, and
  each stem gets a private `$STEMS_STATE_DIR` for venvs, caches and build
  output, so nothing is written into the code repository.
- `${stem.postgres.port}` and `${stem.self.port}` keep ports in one place:
  change a port once and everything that uses it follows.
- `env_files` load `KEY=value` files from the integration repo.
- `stop_grace` is how long a stop waits after SIGTERM before SIGKILL, and
  `restart` restarts the stem when it crashes (`never`, `on-failure` or
  `always`, with backoff). See [restart.md](restart.md).

The `start` script runs your service in the foreground. stems runs it in its
own process group and captures its output; `stems logs api -f` shows it.

The other two services follow the same shape:

```yaml
  worker:
    type: process
    description: Queue consumer
    codebase: ../acme-worker
    depends_on:
      - { stem: redis, condition: healthy }
      - { stem: api, condition: seeded }      # api's seed runs the migrations
    env:
      REDIS_URL: "redis://localhost:${stem.redis.port}"
      CONTROL_PORT: "18081"
    ports: [{ name: control, port: 18081 }]
    health: { type: tcp, port: 18081, interval: 500ms, retries: 3 }
    restart: { policy: on-failure, max: 5, backoff: { initial: 500ms, max: 5s, factor: 2 } }
    scripts:
      start: python3 worker.py

  web:
    type: process
    description: Frontend dev server
    codebase: ../acme-web
    depends_on:
      - { stem: api, protocol: http }
    env:
      API_URL: "http://localhost:${stem.api.port}"
    ports: [{ name: http, port: auto }]
    health: { type: http, url: "http://localhost:${stem.self.port}/" }
    scripts:
      setup: { command: "npm ci", inputs: ["package-lock.json"] }
      start: npm run dev -- --port "$PORT"
```

`port: auto` picks a free port on first start and keeps it (it's sticky), and
stems puts it in the stem's environment as `PORT`. `stems status` shows it.

A service your team uses but doesn't run locally, such as a hosted staging
API, is an **external** stem: stems only health-checks it, and dependants can
wait for it.

```yaml
  payments-sandbox:
    type: external
    description: The payment provider's sandbox
    health: { type: http, url: "https://sandbox.payments.example.com/health", timeout: 2s }
```

## 5. Wire the dependency graph

`depends_on` is what turns a list of services into an environment. Each edge
names a stem and a **condition**:

| Condition | The dependant starts when the dependency… |
|---|---|
| `healthy` (the default) | passes its health check |
| `started` | is running, healthy or not |
| `seeded` | is healthy **and** its `seed` script has finished (the dependency must have one) |

- **Soft edges.** `soft: true` draws the edge and orders the start, but the
  dependant starts even if the dependency never becomes healthy. Use it for
  things a service copes without (a cache, a queue it retries). A soft
  dependency is also not pulled in automatically, so a profile can leave it
  out.
- **Pulling in dependencies.** `stems up web` starts `api`, `postgres` and
  `redis` first, in order, because `web` needs them.
- **Metadata.** `protocol:` (`http`, `tcp`, ...) is only a label for the
  graph.
- **Restart cascades.** `stems restart api --cascade` also restarts
  everything that depends on `api`, in order. `restart: { cascade: true }`
  makes that the default for that stem.

See the graph before you start anything:

```sh
stems graph                  # boxes and arrows, soft edges dashed
stems graph --format mermaid # paste into a PR description or a wiki
stems validate               # also prints the start order
```

`stems validate` rejects cycles, edges to stems that don't exist and other
graph errors, each with a hint. Once the daemon runs, `stems graph` shows
live status on each box, and the dashboard's graph view (`stems attach`,
then `1`) does the same.

Health checks are what make the graph trustworthy: a dependant only starts
when its dependency really answers. Probe what "ready" means for the
service, not merely that the process exists: an HTTP endpoint that touches
the database beats an open port. [health.md](health.md) has every probe type
and option.

## 6. Scripts: setup, seed and everyday commands

Lifecycle scripts run in a fixed order on every start:

```text
setup → pre_start → start → (healthy) → post_start → seed
```

`setup` and `seed` are skipped when their **stamp** is current: a hash of the
script, its `inputs` files and chosen env vars. So `npm ci` or migrations run
the first time and whenever `package-lock.json` or `migrations/**` change,
not on every `up`. `stems up --fresh` resets and re-runs everything, and
`stems stamps` shows the stamps.

Other lifecycle scripts are run on demand: `build` (`stems build`,
`stems restart --build`) and `reset` (`stems reset`: stop, wipe local data,
clear stamps).

Turn the commands people keep in their shell history into **custom scripts**
with typed arguments:

```yaml
  api:
    scripts:
      create-user:
        file: scripts/api/create-user.sh
        description: Create a user with a known password
        args:
          - { name: email, type: string, required: true }
          - { name: role, type: enum, values: [admin, user], default: user }
```

```sh
stems scripts api                                  # list them
stems run api create-user -- --email me@acme.test --role admin
```

Workspace-level scripts sit under a top-level `scripts:` and run with
`stems run --ws <name>`. All the details are in [scripts.md](scripts.md).

## 7. Variants: run a stem another way

Some stems need to run in more than one form: your API as a local process
while you work on it, or as a container built from its Dockerfile when you
only need it running. A **variant** is a partial stem definition merged over
the base, so you describe only what differs:

```yaml
  api:
    type: process
    # … the definition from step 4 …
    variants:
      docker:
        type: docker
        build: { context: "${codebase}" }            # the repo's own Dockerfile
        ports: [{ name: http, port: 18080, container_port: 8080 }]
        env:
          PORT: "8080"
          DATABASE_URL: "postgres://${var.pg_user}:local@host.docker.internal:${stem.postgres.port}/acme"
          REDIS_URL: "redis://host.docker.internal:${stem.redis.port}"
        health: { start_timeout: 3m }                # the first build takes a while
      debug:
        env: { LOG_LEVEL: debug }
```

Rules worth knowing:

- Maps merge (`env` keys add up), lists replace (`ports`, `depends_on`),
  and `scripts` merge per key.
- A variant with a different `type` drops the base's type-specific fields
  (`command`, `image`, `file`, ...) and keeps the shared ones: `codebase`,
  `env`, `ports`, `depends_on`, `health`, `scripts`, and so on. Custom
  scripts keep working.
- **Inside a container, `localhost` is the container itself**, so reach the
  host's services through `host.docker.internal`.
- `local` and `base` are reserved names that mean the base definition.

Switch while everything runs:

```sh
stems switch api            # list the variants; * marks the active one
stems switch api docker     # stop the process, build the image, start the container
stems switch api local      # back to the process
```

`stems switch` writes your choice to `stems.local.yaml`, so it applies to your
machine only, and restarts just that stem; its dependants keep running. To
make a variant the team default, set `variant: docker` on the stem in
`stems.yaml`. Details: [config.md#variants-fr-st-8](config.md#variants-fr-st-8).

A variant can also run an image your CI already publishes instead of
building one, from any registry (Docker Hub, GHCR, Artifact Registry,
ECR, …) with the credentials `docker pull` uses:

```yaml
      published:
        type: docker
        image: europe-west2-docker.pkg.dev/acme/images/api:main
        ports: [{ name: http, port: 18080, container_port: 8080 }]
```

`stems pull api --restart` fetches the latest build of that tag and restarts
the stem onto it; `pull: always` on the variant re-pulls at every start.
`stems doctor` shows where each registry's credentials come from. Details:
[config.md](config.md#a-published-image-instead-of-a-local-build).

## 8. Profiles: named subsets

Not everyone needs everything. Profiles are named starting sets:

```yaml
profiles:
  default: [postgres, redis, api, worker, web]
  backend: [postgres, redis, api, worker]
  frontend: [api, web]
```

```sh
stems up --profile backend
stems profiles              # list them
```

A profile is only a *starting set*: hard dependencies are pulled in, so
`frontend` still starts `postgres` and `redis` for `api`. Set
`strict_profiles: true` to refuse a profile that is missing a dependency
instead. See [config.md#profiles-fr-ws-8](config.md#profiles-fr-ws-8).

## 9. Per-developer overrides

`stems.local.yaml` is gitignored and merged over `stems.yaml` for one
machine. Commit a `stems.local.yaml.example` that shows the useful overrides:

```yaml
# stems.local.yaml.example: copy to stems.local.yaml and edit
stems:
  api:
    codebase: ~/src/acme-api     # your checkout lives somewhere else
    variant: docker              # you don't work on the api
    env:
      LOG_LEVEL: debug
  web:
    enabled: false               # you run the frontend yourself
profiles:
  default: backend
```

`stems config set`, `stems config get` and `stems config unset` edit these
values from the command line and keep comments. A running environment picks
up config changes: `stems config diff` shows what would change, and
`stems config apply` restarts only the affected stems.

Keep secrets out of `stems.yaml`: put them in `stems.local.yaml` or a
gitignored env file, or pass them from your shell with `${env.NAME}`.

## 10. Code from git

Instead of asking everyone to clone each repository into the right place, a
stem's `codebase` can be a git repository that stems clones for you:

```yaml
  api:
    codebase:
      git: git@github.com:acme/acme-api.git
      ref: main
```

Clones go to `.stems/repos/<stem>` and use your normal git credentials.
`stems up --sync` or `stems repos sync` fetch and fast-forward them, and
never touch a clone with local changes or unpushed commits. A developer who
works on the api points `codebase` at their own checkout in
`stems.local.yaml`. See [repos.md](repos.md).

## 11. Restart on code changes

A `watch` rule restarts, rebuilds or runs a script when files in the codebase
change:

```yaml
  api:
    watch:
      - { paths: ["**/*.py"], action: restart, debounce: 500ms }
      - { paths: ["requirements.txt"], action: rebuild }
```

`stems watch pause` and `stems watch resume` turn them off and on. Skip this
for services whose dev server already reloads itself. See
[watchdogs.md](watchdogs.md).

## 12. Try it end to end

```sh
stems doctor          # tools, Docker, ports in use, config problems
stems validate
stems up              # starts in dependency order and opens the dashboard
```

In the dashboard, `1` to `5` switch views (graph, table, detail, logs,
events), the action bar names the keys for the selected stem, and `?` shows
help. Quitting asks whether to stop everything or leave it running
(`stems up --detach` starts without the dashboard). Then:

```sh
stems status
stems logs api -f
stems restart api --cascade
stems down            # stop everything; `stems down --volumes` also drops data
```

When something doesn't come up:

| Symptom | Look at |
|---|---|
| a stem stays `starting` | `stems health api`: each probe result and why it failed |
| `PORT_IN_USE` | `stems doctor`: its `ports.<stem>.<name>` check names the process holding the port |
| a dependant never starts | `stems graph`: its edge condition, and whether the dependency is healthy |
| setup or seed didn't run again | `stems stamps`, or `stems up --fresh` |
| the config change didn't take | `stems config diff`, then `stems config apply` |

## 13. Hand it to the team

Commit the integration repo: `stems.yaml`, `scripts/`, `env/` (without
secrets), `overlays/`, `compose/` and `stems.local.yaml.example`. A teammate
then onboards with:

```sh
brew install humbertopoliti/tap/stems
git clone git@github.com:acme/acme-env.git && cd acme-env
cp stems.local.yaml.example stems.local.yaml    # optional
stems doctor
stems up
```

Put a `requires:` block in `stems.yaml` so `stems doctor` and
`stems validate` check tool versions (`python3: ">=3.11"`, `node: ">=20"`)
before anything starts. Document the profiles in the integration repo's
README.

## 14. Let agents operate it

Every command takes `--json`, and `stems mcp` serves the workspace to MCP
clients such as Claude Code and Cursor: status, logs, start and stop, and one
tool per custom script, all recorded in the event log. Destructive actions
(`down --volumes`, `reset`) are refused unless `agent.allow_destructive` is
enabled. See [mcp.md](mcp.md).

## Onboarding checklist

- [ ] Integration repo created with `stems init`, `stems.yaml` committed
- [ ] Every service has a stem with a `description`
- [ ] Every stem has a health check that means "ready", not just "running"
- [ ] Every edge has the right condition (`healthy`, `started`, `seeded`), with `soft: true` where a service copes without the dependency
- [ ] Ports are declared once and referenced with `${stem.<name>.port}`
- [ ] `setup` and `seed` have `inputs`, so they rerun only when something changed
- [ ] The commands people repeat are custom scripts with `description`s
- [ ] Variants cover the alternative ways to run a stem (process or container)
- [ ] Profiles cover the common subsets
- [ ] `stems.local.yaml.example` shows the useful overrides; no secrets are committed
- [ ] `requires:` lists the tools and versions
- [ ] `stems validate` and `stems doctor` pass on a clean machine, and `stems graph` looks right
