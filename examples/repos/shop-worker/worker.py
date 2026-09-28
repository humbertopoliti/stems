#!/usr/bin/env python3
"""shop-worker: a stdlib-only redis queue consumer used as a stems example and chaos fixture.

Run standalone:  REDIS_URL=redis://127.0.0.1:6379/0 SHOP_CHAOS=1 python3 worker.py
Push jobs with:  redis-cli RPUSH shop:jobs hello      (or `crash:5`, `hang-on-stop`)
See README.md for environment variables and chaos commands.
"""

import json
import os
import signal
import socket
import socketserver
import sys
import threading
import time
from datetime import datetime, timezone
from urllib.parse import unquote, urlsplit

LOG_JSON = os.environ.get("SHOP_LOG_JSON") == "1"
CHAOS = os.environ.get("SHOP_CHAOS") == "1"
_log_lock = threading.Lock()
_hang_on_stop = False


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


# --------------------------------------------------------------------------- RESP client


class RespError(Exception):
    pass


class Redis:
    """Just enough RESP2 to AUTH, SELECT and BLPOP."""

    def __init__(self, url, timeout=5.0):
        u = urlsplit(url)
        self.host = u.hostname or "127.0.0.1"
        self.port = u.port or 6379
        self.password = unquote(u.password) if u.password else None
        self.username = unquote(u.username) if u.username else None
        path = (u.path or "/").lstrip("/")
        self.db = int(path) if path else 0
        self.sock = socket.create_connection((self.host, self.port), timeout=timeout)
        self.buf = self.sock.makefile("rb")
        if self.password:
            args = ["AUTH", self.username, self.password] if self.username else ["AUTH", self.password]
            self.command(*args)
        if self.db:
            self.command("SELECT", str(self.db))

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass

    def command(self, *args, timeout=None):
        out = [b"*%d\r\n" % len(args)]
        for a in args:
            b = a if isinstance(a, bytes) else str(a).encode()
            out.append(b"$%d\r\n%s\r\n" % (len(b), b))
        self.sock.settimeout(timeout if timeout is not None else 5.0)
        self.sock.sendall(b"".join(out))
        return self._read()

    def _read(self):
        line = self.buf.readline()
        if not line:
            raise ConnectionError("redis closed the connection")
        kind, rest = line[:1], line[1:-2]
        if kind == b"+":
            return rest.decode()
        if kind == b"-":
            raise RespError(rest.decode())
        if kind == b":":
            return int(rest)
        if kind == b"$":
            n = int(rest)
            if n < 0:
                return None
            data = self.buf.read(n + 2)
            return data[:-2]
        if kind == b"*":
            n = int(rest)
            if n < 0:
                return None
            return [self._read() for _ in range(n)]
        raise RespError("unexpected RESP reply %r" % line)


# --------------------------------------------------------------------------- chaos


def _on_sigterm(signum, frame):
    if _hang_on_stop:
        log("WARN", "ignoring SIGTERM")
        return
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    os.kill(os.getpid(), signal.SIGTERM)


def chaos_crash(code):
    log("ERROR", "chaos crash requested", code=code)
    sys.stdout.flush()
    os._exit(code)


def chaos_hang():
    global _hang_on_stop
    _hang_on_stop = True
    log("WARN", "ignoring SIGTERM")


def handle_job(job):
    if CHAOS and job.startswith("crash:"):
        chaos_crash(int(job.split(":", 1)[1] or 1))
    if CHAOS and job == "hang-on-stop":
        chaos_hang()
        return
    log("INFO", "processed job %s" % job)


def start_control_server(host, port):
    """Optional HTTP control plane (SHOP_CHAOS=1 + SHOP_CONTROL_PORT) for scenarios without redis."""
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from urllib.parse import parse_qs

    class Control(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):
            pass

        def _send(self, status, body):
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            self.wfile.flush()

        def do_GET(self):
            u = urlsplit(self.path)
            q = parse_qs(u.query)
            if u.path == "/healthz":
                return self._send(200, {"status": "ok"})
            if u.path == "/__chaos/crash":
                code = int(q.get("code", ["1"])[0])
                self._send(200, {"ok": True, "effect": "exiting with code %d" % code})
                chaos_crash(code)
            if u.path == "/__chaos/hang-on-stop":
                chaos_hang()
                return self._send(200, {"ok": True, "effect": "SIGTERM will be ignored"})
            return self._send(404, {"ok": False, "error": "not found"})

        do_POST = do_GET

    class Server(ThreadingHTTPServer):
        daemon_threads = True
        allow_reuse_address = True

        def server_bind(self):
            # Skip HTTPServer's reverse-DNS lookup of the bind address
            # (socket.getfqdn): some resolvers stall it for 30+ s.
            socketserver.TCPServer.server_bind(self)
            self.server_name, self.server_port = self.server_address[:2]

    server = Server((host, port), Control)
    threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.2}, daemon=True).start()
    log("INFO", "control listening on %s:%d" % (host, server.server_address[1]))


def heartbeat():
    while True:
        log("INFO", "heartbeat")
        time.sleep(1.0)


def main():
    crash = os.environ.get("SHOP_CRASH_ON_START")
    if crash:
        log("ERROR", "SHOP_CRASH_ON_START set, exiting", code=int(crash))
        sys.exit(int(crash))
    delay = float(os.environ.get("SHOP_SLEEP_START") or 0)
    if delay > 0:
        log("INFO", "sleeping before start", seconds=delay)
        time.sleep(delay)

    signal.signal(signal.SIGTERM, _on_sigterm)
    url = os.environ.get("REDIS_URL") or "redis://127.0.0.1:6379/0"
    queue = os.environ.get("SHOP_QUEUE") or "shop:jobs"
    control = os.environ.get("SHOP_CONTROL_PORT")
    if CHAOS and control:
        start_control_server(os.environ.get("HOST") or "127.0.0.1", int(control))
    log("INFO", "worker started queue=%s redis=%s" % (queue, url))
    threading.Thread(target=heartbeat, daemon=True).start()

    conn = None
    try:
        while True:
            if conn is None:
                try:
                    conn = Redis(url)
                    log("INFO", "connected to redis")
                except (OSError, RespError, ValueError) as exc:
                    log("WARN", "redis unavailable, retrying", error=repr(str(exc)))
                    time.sleep(1.0)
                    continue
            try:
                reply = conn.command("BLPOP", queue, "1", timeout=5.0)
            except (OSError, RespError, ConnectionError, ValueError) as exc:
                log("WARN", "redis unavailable, retrying", error=repr(str(exc)))
                conn.close()
                conn = None
                time.sleep(1.0)
                continue
            if reply:
                job = reply[1].decode("utf-8", "replace")
                handle_job(job)
    except KeyboardInterrupt:
        log("INFO", "shutting down")


if __name__ == "__main__":
    main()
