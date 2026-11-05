# Ports

Where host ports are declared, how `auto` works, which variables and
templates address them, and how files outside stems can read the numbers
without repeating them.

## Declaring ports

```yaml
stems:
  shop-api:
    ports:
      - 8080                                   # host 8080 (container 8080)
      - "15432:5432"                           # docker/compose: host:container
      - { name: metrics, port: 9100 }          # named
      - { name: http, port: auto }             # allocated when the stem starts
      - { name: grpc, port: 9000, container_port: 9000 }
```

The first port is the **primary** one. Unnamed ports are called `port0`,
`port1`, ... by position. `container_port` (docker/compose) defaults to the
fixed host port.

## `auto`

An `auto` port is a free ephemeral port picked when the stem first starts,
kept for the daemon's lifetime and across restarts, and announced with the
`stem.port_allocated` event. Until then it has no number.

## Conflicts

- `PORT_CONFLICT`: two stems declare the same fixed port (found by
  `stems validate`, before anything starts).
- `PORT_IN_USE`: something else already listens on a fixed port when the stem
  starts.

## Variables and templates

| In | Form | Meaning |
|---|---|---|
| env of a stem | `PORT` | its own primary port |
| env of a stem | `PORT_<NAME>` | its own explicitly named ports |
| env of a dependant | `STEMS_<DEP>_PORT` | the dependency's primary port |
| env of a dependant | `STEMS_<DEP>_PORT_<NAME>` | the dependency's explicitly named ports |
| any config value | `${stem.<n>.port}` | primary port of stem `<n>` (`stem.self` for itself) |
| any config value | `${stem.<n>.ports.<p>}` | port `<p>` of stem `<n>` |

Names are upper-cased and every non-alphanumeric character becomes `_`
(`shop-api` -> `SHOP_API`). Default `port<i>` names get no `_<NAME>` variable.

## `stems ports`

```
stems ports [stems…] [--format table|json|dotenv|shell] [--no-status]
```

Lists the ports of every enabled stem (or the named ones, enabled or not)
from the config, plus the live `auto` allocations when a daemon runs. It
never starts the daemon; `--no-status` ignores a running one. `--json`:

```json
{ "ports": [ { "stem": "api", "name": "http", "port": 18080, "auto": true,
               "container_port": null, "primary": true, "var": "STEMS_API_PORT",
               "template": "${stem.api.port}", "running": true } ],
  "vars": { "STEMS_API_PORT": 18080, "STEMS_API_PORT_HTTP": 18080 } }
```

`port` is `null` for an `auto` port that is not allocated; `vars` holds only
resolved ports, in both the primary and the named form. `running` is true
while the stem is `starting`, `healthy`, `unhealthy` or `seeding` (false
without a daemon or with `--no-status`).

## Feeding non-stem consumers

Keep the numbers in `stems.yaml` only:

```sh
stems ports --format dotenv > .env.ports     # STEMS_API_PORT=18080 …
eval "$(stems ports --format shell)"         # export STEMS_API_PORT=18080 …
stems ports --json | jq -r '.data.vars.STEMS_API_PORT'   # in a docs build
```

`dotenv` and `shell` print sorted lines and skip unresolved `auto` ports
(with a note on stderr in human mode); start the stems first to include them.

See also: [Config](config.md) · [Compose](compose.md) · [Exec and env](exec.md) · [What stems can do](features.md)
