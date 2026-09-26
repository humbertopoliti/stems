"""Helpers for the shop-web tests: spawn the service, capture its log, poll with bounded timeouts."""

import os
import re
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SERVE = os.path.join(REPO, "serve.py")


class Service:
    """A running script whose stdout lines are collected in the background."""

    def __init__(self, script, env=None, cwd=None):
        full_env = {k: v for k, v in os.environ.items() if not k.startswith("SHOP_")}
        full_env.update({"PORT": "0", "HOST": "127.0.0.1"})
        full_env.update(env or {})
        self.lines = []
        self.proc = subprocess.Popen(
            [sys.executable, script], env=full_env, cwd=cwd or os.path.dirname(script),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            start_new_session=True,  # own process group, like stems does
        )
        self._reader = threading.Thread(target=self._read, daemon=True)
        self._reader.start()
        self.port = None

    def _read(self):
        for line in self.proc.stdout:
            self.lines.append(line.rstrip("\n"))

    def wait_line(self, pattern, timeout=5.0):
        rx = re.compile(pattern)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for line in list(self.lines):
                m = rx.search(line)
                if m:
                    return m
            if self.proc.poll() is not None and not self._reader.is_alive():
                break
            time.sleep(0.01)
        raise AssertionError("no log line matching %r within %ss; got:\n%s" % (pattern, timeout, "\n".join(self.lines)))

    def wait_listening(self, timeout=5.0):
        m = self.wait_line(r"listening on [\d.]+:(\d+)", timeout)
        self.port = int(m.group(1))
        return self.port

    def url(self, path):
        return "http://127.0.0.1:%d%s" % (self.port, path)

    def request(self, path, method="GET", data=None, headers=None):
        """Return (status, body_bytes); HTTP errors are returned, not raised."""
        req = urllib.request.Request(self.url(path), method=method, data=data, headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, r.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    def alive(self):
        return self.proc.poll() is None

    def kill(self):
        if self.proc.poll() is None:
            try:
                os.killpg(self.proc.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.proc.wait(5)
        self.proc.stdout.close()


def poll(predicate, timeout=5.0, interval=0.02):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
