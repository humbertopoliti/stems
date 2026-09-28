#!/usr/bin/env python3
"""shop-web: stdlib-only static dev server for public/, with API_URL injected into index.html.

Run standalone:  PORT=3000 API_URL=http://127.0.0.1:8080 python3 serve.py
"""

import json
import mimetypes
import os
import signal
import socketserver
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlsplit

LOG_JSON = os.environ.get("SHOP_LOG_JSON") == "1"
PUBLIC = os.path.join(os.path.dirname(os.path.abspath(__file__)), "public")
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


def api_url():
    return os.environ.get("API_URL") or "http://127.0.0.1:8080"


class Handler(BaseHTTPRequestHandler):
    server_version = "shop-web/1.0"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _send(self, status, data, ctype):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        self.status = status

    def do_GET(self):
        request_id = self.headers.get("X-Request-Id") or uuid.uuid4().hex[:12]
        self.status = 0
        path = unquote(urlsplit(self.path).path)
        try:
            self._serve(path)
        except Exception as exc:
            log("ERROR", "request failed", error=repr(str(exc)), request_id=request_id)
            if not self.status:
                self._send(500, b"internal error\n", "text/plain")
        log("INFO", "GET %s %d" % (path, self.status), request_id=request_id)

    do_HEAD = do_GET

    def _serve(self, path):
        if path == "/healthz":
            return self._send(200, b'{"status": "ok"}', "application/json")
        if path.endswith("/"):
            path += "index.html"
        full = os.path.realpath(os.path.join(PUBLIC, path.lstrip("/")))
        if not full.startswith(os.path.realpath(PUBLIC) + os.sep) or not os.path.isfile(full):
            return self._send(404, b"not found\n", "text/plain")
        with open(full, "rb") as f:
            data = f.read()
        ctype = mimetypes.guess_type(full)[0] or "application/octet-stream"
        if full.endswith(".html"):
            data = data.replace(b"__API_URL__", api_url().encode())
            ctype = "text/html; charset=utf-8"
        return self._send(200, data, ctype)


def main():
    crash = os.environ.get("SHOP_CRASH_ON_START")
    if crash:
        log("ERROR", "SHOP_CRASH_ON_START set, exiting", code=int(crash))
        sys.exit(int(crash))
    delay = float(os.environ.get("SHOP_SLEEP_START") or 0)
    if delay > 0:
        log("INFO", "sleeping before bind", seconds=delay)
        time.sleep(delay)

    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    host = os.environ.get("HOST") or "127.0.0.1"
    port = int(os.environ.get("PORT") or 3000)
    class Server(ThreadingHTTPServer):
        daemon_threads = True
        allow_reuse_address = True

        def server_bind(self):
            # Skip HTTPServer's reverse-DNS lookup of the bind address
            # (socket.getfqdn): some resolvers stall it for 30+ s.
            socketserver.TCPServer.server_bind(self)
            self.server_name, self.server_port = self.server_address[:2]

    server = Server((host, port), Handler)
    log("INFO", "listening on %s:%d" % (host, server.server_address[1]))
    log("INFO", "api_url=%s" % api_url())
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        log("INFO", "shutting down")


if __name__ == "__main__":
    main()
