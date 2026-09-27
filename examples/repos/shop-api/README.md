# shop-api

A tiny HTTP service written against the Python 3 standard library only (no `pip install`).
It is a stand-in code repository for the stems `hello-shop` example, and the chaos fixture the
stems test suite uses to provoke failures on demand.

## Run it

```sh
python3 app.py                               # http://127.0.0.1:8080
PORT=9000 SHOP_CHAOS=1 python3 app.py        # with the /__chaos/* endpoints
docker build -t shop-api . && docker run -p 8080:8080 shop-api
```

Stop it with Ctrl-C or SIGTERM.

## Routes

| Route | Result |
|---|---|
| `GET /healthz` | `200 {"status":"ok"}`, or `503` while chaos-unhealthy |
| `GET /products` | JSON list. Reads the `products` table through `$SHOP_PSQL`, or through `psql` when `DATABASE_URL` is set and `psql` is on `PATH`. Otherwise, or if the database is unreachable, it logs a `WARN` and serves the in-memory list |
| `POST /users` | body `{"email": "...", "role": "..."}`. Returns `201` with the user and logs `INFO created user <email> role <role>` |

`migrations/001_init.sql` creates and seeds the `products` table.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `8080` | Port to listen on (`0` means pick any free port. The chosen port is logged) |
| `HOST` | `127.0.0.1` | Address to bind |
| `DATABASE_URL` | unset | Postgres URL. Optional |
| `SHOP_PSQL` | unset | psql command line used instead of `psql $DATABASE_URL`, carrying its own connection (hello-shop: `docker exec hello-shop-postgres psql -U shop -d shop`, so no host `psql` is needed). Optional |
| `REDIS_URL` | unset | Accepted for parity with the workspace. Not used by the API itself |
| `SHOP_CHAOS` | unset | `1` enables the `/__chaos/*` endpoints (404 otherwise) |
| `SHOP_SLEEP_START` | `0` | Seconds (float) to sleep **before** binding the port |
| `SHOP_CRASH_ON_START` | unset | Exit immediately with this code, before binding |
| `SHOP_CONFIG` | unset | Path to an ini file. Each line is logged as `INFO config: <line>`. Logs `WARN config file missing` if the file is absent |
| `SHOP_LOG_JSON` | unset | `1` switches to one JSON object per line: `{"ts","level","msg","request_id",...}` |

Logs go to stdout. The plain format is `LEVEL message key=value`, with `INFO`, `WARN`, `ERROR` or `DEBUG`
as the level. Right after it binds, the service logs `INFO listening on <host>:<port>`. Every request is
logged as `INFO <METHOD> <path> <status> request_id=<id>`. The id comes from the `X-Request-Id`
request header when one is sent. Otherwise it is generated.

## Chaos endpoints (`SHOP_CHAOS=1`)

GET or POST. Each one returns `{"ok": true, "effect": ...}`.

| Endpoint | Effect |
|---|---|
| `/__chaos/crash?code=3` | Sends the response, then exits with the code at once (`os._exit`) |
| `/__chaos/unhealthy?for=5s` | `/healthz` returns 503 for that long. Accepts `500ms`, `5s`, `2m` or plain seconds |
| `/__chaos/hang-on-stop` | SIGTERM is ignored from now on (it logs `WARN ignoring SIGTERM`). Only SIGKILL stops the process |
| `/__chaos/fork?n=3` | Spawns n child processes that sleep forever, **in the same process group**. `effect.pids` lists them |
| `/__chaos/logs?n=1000&level=error` | Emits n lines `chaos log line <i>` at that level (default `info`) |
| `/__chaos/alloc?mb=300` | Allocates that many MB and keeps them resident. `mb=0` frees them |
| `/__chaos/touch?file=src/x.py` | Appends a timestamp to that path, relative to the working directory. Parent directories are created |
| `/__chaos/spin?ms=3000` | Busy-loops one core in a background thread for that long. Returns at once |

These two environment variables drive failures at start time: `SHOP_SLEEP_START=20` delays readiness,
and `SHOP_CRASH_ON_START=3` fails the start.

## Tests

```sh
python3 -m unittest discover -s examples/repos/shop-api/tests    # from the stems repo root
```
