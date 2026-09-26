#!/usr/bin/env python3
"""shop-api: a tiny stdlib-only HTTP service used as a stems example and chaos fixture.

Run standalone:  PORT=8080 SHOP_CHAOS=1 python3 app.py
See README.md for routes, environment variables and the chaos endpoint table.
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

LOG_JSON = os.environ.get("SHOP_LOG_JSON") == "1"
CHAOS = os.environ.get("SHOP_CHAOS") == "1"
_log_lock = threading.Lock()


def log(level, msg, **fields):
    """Write one log line to stdout: `LEVEL msg k=v` or one JSON object."""
    if LOG_JSON:
        obj = {"ts": datetime.now(timezone.utc).isoformat(timespec="milliseconds"), "level": level.lower(), "msg": msg}
        obj.update(fields)
        line = json.dumps(obj)
    else:
        extra = "".join(" %s=%s" % (k, v) for k, v in fields.items())
        line = "%s %s%s" % (level.upper(), msg, extra)
    with _log_lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def parse_duration(text):
    """Parse `500ms`, `5s`, `2m` or plain seconds into float seconds."""
    text = (text or "").strip().lower()
    if not text:
        raise ValueError("empty duration")
    if text.endswith("ms"):
        return float(text[:-2]) / 1000.0
    if text.endswith("s"):
        return float(text[:-1])
    if text.endswith("m"):
        return float(text[:-1]) * 60.0
    return float(text)


# --------------------------------------------------------------------------- state

PRODUCTS = [
    {"id": 1, "name": "Espresso cup", "price_cents": 900},
    {"id": 2, "name": "Pour-over kettle", "price_cents": 4500},
    {"id": 3, "name": "Burr grinder", "price_cents": 12900},
]
_users = []
_users_lock = threading.Lock()
_unhealthy_until = 0.0
_hang_on_stop = False
_allocated = None  # holds the chaos `alloc` bytearray
_children = []  # chaos `fork` children (kept so they are not garbage collected)


def _on_sigterm(signum, frame):
    if _hang_on_stop:
        log("WARN", "ignoring SIGTERM")
        return
    # Behave exactly like the default disposition: die by the signal.
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    os.kill(os.getpid(), signal.SIGTERM)


def products_from_db(url):
    """Read products through `psql` if available; None means "use the in-memory list"."""
    psql = shutil.which("psql")
    if not psql:
        log("WARN", "DATABASE_URL set but psql not found, using in-memory products")
        return None
    try:
        out = subprocess.run(
            [psql, url, "-At", "-F", "\t", "-c", "SELECT id, name, price_cents FROM products ORDER BY id"],
            capture_output=True, text=True, timeout=3,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        log("WARN", "database unreachable, using in-memory products", error=repr(str(exc)))
        return None
    if out.returncode != 0:
        log("WARN", "database unreachable, using in-memory products", error=repr(out.stderr.strip()[:200]))
        return None
    rows = []
    for line in out.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) == 3:
            rows.append({"id": int(parts[0]), "name": parts[1], "price_cents": int(parts[2])})
    return rows


# --------------------------------------------------------------------------- chaos


def _spin(seconds):
    deadline = time.monotonic() + seconds
    x = 0
    while time.monotonic() < deadline:
        x += 1


def chaos(handler, action, q):
    """Execute a chaos action. Returns (status, body) or None for unknown actions."""
    global _unhealthy_until, _hang_on_stop, _allocated

    def arg(name, default=None):
        return q.get(name, [default])[0]

    if action == "crash":
        code = int(arg("code", "1"))
        log("ERROR", "chaos crash requested", code=code)
        handler.send_json(200, {"ok": True, "effect": "exiting with code %d" % code})
        handler.wfile.flush()
        sys.stdout.flush()
        sys.stderr.flush()
        os._exit(code)
    if action == "unhealthy":
        secs = parse_duration(arg("for", "5s"))
        _unhealthy_until = time.monotonic() + secs
        log("WARN", "chaos unhealthy", seconds=secs)
        return 200, {"ok": True, "effect": "unhealthy for %gs" % secs}
    if action == "hang-on-stop":
        _hang_on_stop = True
        log("WARN", "ignoring SIGTERM")
        return 200, {"ok": True, "effect": "SIGTERM will be ignored"}
    if action == "fork":
        n = int(arg("n", "1"))
        pids = []
        for _ in range(n):
            # No setsid / new session: children stay in our process group on purpose.
            p = subprocess.Popen(
                [sys.executable, "-c", "import time\nwhile True: time.sleep(3600)"],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            _children.append(p)
            pids.append(p.pid)
        log("INFO", "chaos forked children", pids=",".join(map(str, pids)))
        return 200, {"ok": True, "effect": {"pids": pids}}
    if action == "logs":
        n = int(arg("n", "100"))
        level = arg("level", "info").upper()
        if level == "WARNING":
            level = "WARN"
        for i in range(n):
            log(level, "chaos log line %d" % i)
        return 200, {"ok": True, "effect": "emitted %d %s lines" % (n, level.lower())}
    if action == "alloc":
        mb = int(arg("mb", "100"))
        # Filled (not zeroed) so the pages are really resident.
        _allocated = bytearray(b"\x01") * (mb * 1024 * 1024) if mb > 0 else None
        log("INFO", "chaos alloc", mb=mb)
        return 200, {"ok": True, "effect": "holding %d MB" % mb}
    if action == "touch":
        rel = arg("file", "src/x.py")
        path = os.path.join(os.getcwd(), rel)
        parent = os.path.dirname(path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(path, "a") as f:
            f.write("# touched %s\n" % datetime.now(timezone.utc).isoformat())
        log("INFO", "chaos touched file", file=rel)
        return 200, {"ok": True, "effect": "touched %s" % rel}
    if action == "spin":
        ms = int(arg("ms", "3000"))
        threading.Thread(target=_spin, args=(ms / 1000.0,), daemon=True).start()
        log("INFO", "chaos spin", ms=ms)
        return 200, {"ok": True, "effect": "spinning for %d ms" % ms}
    return None


# --------------------------------------------------------------------------- http


class Handler(BaseHTTPRequestHandler):
    server_version = "shop-api/1.0"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # silence http.server's stderr logging
        pass

    def send_json(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Request-Id", self.request_id)
        self.send_header("Access-Control-Allow-Origin", "*")  # shop-web fetches from another port
        self.end_headers()
        self.wfile.write(data)
        self.status = status

    def _dispatch(self, method):
        self.request_id = self.headers.get("X-Request-Id") or uuid.uuid4().hex[:12]
        self.status = 0
        url = urlsplit(self.path)
        try:
            self._route(method, url.path, parse_qs(url.query))
        except Exception as exc:  # never let a handler kill the server
            log("ERROR", "request failed", error=repr(str(exc)), request_id=self.request_id)
            if not self.status:
                self.send_json(500, {"ok": False, "error": str(exc)})
        log("INFO", "%s %s %d" % (method, url.path, self.status), request_id=self.request_id)

    def _route(self, method, path, q):
        if method == "GET" and path == "/healthz":
            if time.monotonic() < _unhealthy_until:
                return self.send_json(503, {"status": "unhealthy"})
            return self.send_json(200, {"status": "ok"})
        if method == "GET" and path == "/products":
            rows = None
            if os.environ.get("DATABASE_URL"):
                rows = products_from_db(os.environ["DATABASE_URL"])
            return self.send_json(200, PRODUCTS if rows is None else rows)
        if method == "POST" and path == "/users":
            length = int(self.headers.get("Content-Length") or 0)
            try:
                body = json.loads(self.rfile.read(length) or b"{}")
            except ValueError:
                return self.send_json(400, {"ok": False, "error": "invalid JSON"})
            email = body.get("email") if isinstance(body, dict) else None
            if not email:
                return self.send_json(400, {"ok": False, "error": "email is required"})
            role = body.get("role") or "user"
            with _users_lock:
                user = {"id": len(_users) + 1, "email": email, "role": role}
                _users.append(user)
            log("INFO", "created user %s role %s" % (email, role), request_id=self.request_id)
            return self.send_json(201, user)
        if CHAOS and path.startswith("/__chaos/"):
            result = chaos(self, path[len("/__chaos/"):], q)
            if result is not None:
                return self.send_json(*result)
        return self.send_json(404, {"ok": False, "error": "not found"})

    def do_GET(self):
        self._dispatch("GET")

    def do_POST(self):
        self._dispatch("POST")


def main():
    crash = os.environ.get("SHOP_CRASH_ON_START")
    if crash:
        log("ERROR", "SHOP_CRASH_ON_START set, exiting", code=int(crash))
        sys.exit(int(crash))
    delay = float(os.environ.get("SHOP_SLEEP_START") or 0)
    if delay > 0:
        log("INFO", "sleeping before bind", seconds=delay)
        time.sleep(delay)

    cfg = os.environ.get("SHOP_CONFIG")
    if cfg:
        if os.path.isfile(cfg):
            with open(cfg) as f:
                for line in f.read().splitlines():
                    log("INFO", "config: %s" % line)
        else:
            log("WARN", "config file missing", path=cfg)

    signal.signal(signal.SIGTERM, _on_sigterm)
    host = os.environ.get("HOST") or "127.0.0.1"
    port = int(os.environ.get("PORT") or 8080)
    ThreadingHTTPServer.daemon_threads = True
    ThreadingHTTPServer.allow_reuse_address = True
    server = ThreadingHTTPServer((host, port), Handler)
    log("INFO", "listening on %s:%d" % (host, server.server_address[1]))
    if CHAOS:
        log("WARN", "chaos endpoints enabled under /__chaos/")
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        log("INFO", "shutting down")


if __name__ == "__main__":
    main()
