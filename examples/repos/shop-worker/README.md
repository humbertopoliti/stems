# shop-worker

A queue consumer written against the Python 3 standard library only. It `BLPOP`s jobs from a redis
list through a small hand-written RESP client, so there is no `redis` package to install. It is a
stand-in code repository for the stems `hello-shop` example and a chaos fixture.

## Run it

```sh
python3 worker.py                                            # redis://127.0.0.1:6379/0, list shop:jobs
REDIS_URL=redis://127.0.0.1:6380/0 SHOP_CHAOS=1 python3 worker.py
redis-cli RPUSH shop:jobs hello                              # -> INFO processed job hello
```

The worker logs `INFO heartbeat` every second. If redis is down it logs `WARN redis unavailable, retrying`
and tries again every second. It never crashes because redis is missing.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `REDIS_URL` | `redis://127.0.0.1:6379/0` | `redis://[user:password@]host:port/db` |
| `SHOP_QUEUE` | `shop:jobs` | List to `BLPOP` (1 s timeout) |
| `SHOP_CHAOS` | unset | `1` enables the chaos commands below |
| `SHOP_CONTROL_PORT` | unset | With `SHOP_CHAOS=1`, also serve an HTTP control plane on this port (`0` = any free port. It logs `INFO control listening on <host>:<port>`) |
| `HOST` | `127.0.0.1` | Bind address for the control plane |
| `SHOP_SLEEP_START` | `0` | Seconds (float) to sleep before starting |
| `SHOP_CRASH_ON_START` | unset | Exit immediately with this code |
| `SHOP_LOG_JSON` | unset | `1` makes the worker write one JSON object per line (`{"ts","level","msg",...}`) |

## Chaos (`SHOP_CHAOS=1`)

Through redis, push a job to the queue:

| Job | Effect |
|---|---|
| `crash:<code>` | Exits immediately with the code |
| `hang-on-stop` | SIGTERM is ignored from now on (it logs `WARN ignoring SIGTERM`) |

Any other job is logged as `INFO processed job <job>`. When `SHOP_CHAOS` is unset, `crash:5` is an
ordinary job.

Through HTTP, which only runs when `SHOP_CONTROL_PORT` is set. It lets process-only scenarios with no
redis drive the worker:

| Endpoint | Effect |
|---|---|
| `/healthz` | `200 {"status":"ok"}` |
| `/__chaos/crash?code=5` | Responds `{"ok": true, ...}`, then exits with the code |
| `/__chaos/hang-on-stop` | Same as the redis command |

## Tests

```sh
python3 -m unittest discover -s examples/repos/shop-worker/tests   # uses an in-process fake RESP server
```
