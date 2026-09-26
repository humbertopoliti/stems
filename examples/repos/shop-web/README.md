# shop-web

A static site with a tiny standard-library-only dev server. `public/index.html` fetches `${API_URL}/products`
from shop-api and renders the list. `serve.py` replaces the `__API_URL__` placeholder in HTML files
with the `API_URL` environment variable each time a page is requested.

## Run it

```sh
PORT=3000 API_URL=http://127.0.0.1:8080 python3 serve.py    # open http://127.0.0.1:3000
```

At start it logs `INFO listening on <host>:<port>` and then `INFO api_url=<API_URL>`.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `PORT` | `3000` | Port to listen on (`0` = any free port. The chosen port is logged) |
| `HOST` | `127.0.0.1` | Address to bind |
| `API_URL` | `http://127.0.0.1:8080` | Base URL of shop-api, injected into the page |
| `SHOP_SLEEP_START` | `0` | Seconds (float) to sleep before binding |
| `SHOP_CRASH_ON_START` | unset | Exit immediately with this code |
| `SHOP_LOG_JSON` | unset | `1` writes one JSON object per line |

`GET /healthz` returns 200. The server has no chaos endpoints. Use shop-api for those.

## Tests

```sh
python3 -m unittest discover -s examples/repos/shop-web/tests
```
