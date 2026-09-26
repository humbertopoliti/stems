import json
import os
import signal
import sys
import tempfile
import time
import unittest

from api_support import APP, REPO, Service, pid_alive, poll

sys.path.insert(0, REPO)
import app  # noqa: E402  (pure helpers only; main() is not run on import)


def read(path):
    with open(path) as f:
        return f.read()


class ServiceTest(unittest.TestCase):
    env = {"SHOP_CHAOS": "1"}

    def start(self, env=None, cwd=None, listen=True):
        e = dict(self.env)
        e.update(env or {})
        svc = Service(APP, env=e, cwd=cwd)
        self.addCleanup(svc.kill)
        if listen:
            svc.wait_listening()
        return svc


class TestRoutes(ServiceTest):
    def test_healthz_and_products(self):
        svc = self.start()
        self.assertEqual(svc.request("/healthz")[0], 200)
        status, body = svc.request("/products")
        self.assertEqual(status, 200)
        self.assertEqual([p["id"] for p in json.loads(body)], [1, 2, 3])
        self.assertEqual(svc.request("/nope")[0], 404)

    def test_create_user_logs_and_returns_201(self):
        svc = self.start()
        status, body = svc.request("/users", "POST", json.dumps({"email": "a@b.test", "role": "admin"}).encode(),
                                   {"Content-Type": "application/json"})
        self.assertEqual(status, 201)
        self.assertEqual(json.loads(body)["email"], "a@b.test")
        svc.wait_line(r"^INFO created user a@b\.test role admin request_id=\w+")
        self.assertEqual(svc.request("/users", "POST", b"{}")[0], 400)

    def test_requests_are_logged_with_request_id(self):
        svc = self.start()
        svc.request("/healthz", headers={"X-Request-Id": "req-42"})
        svc.wait_line(r"^INFO GET /healthz 200 request_id=req-42$")

    def test_unreachable_database_falls_back_to_memory(self):
        svc = self.start({"DATABASE_URL": "postgres://nobody@127.0.0.1:1/none?connect_timeout=1"})
        status, body = svc.request("/products")
        self.assertEqual(status, 200)
        self.assertEqual(len(json.loads(body)), 3)
        svc.wait_line(r"^WARN .*in-memory products")
        self.assertTrue(svc.alive())

    def test_chaos_disabled_is_404(self):
        svc = self.start({"SHOP_CHAOS": "0"})
        self.assertEqual(svc.request("/__chaos/crash?code=3")[0], 404)
        self.assertTrue(svc.alive())


class TestStartup(ServiceTest):
    def test_startup_is_fast(self):
        t0 = time.monotonic()
        self.start()
        elapsed = time.monotonic() - t0
        # 200 ms budget for the service itself; allow slack for a loaded CI box.
        self.assertLess(elapsed, 1.0, "startup took %.3fs" % elapsed)

    def test_crash_on_start(self):
        svc = self.start({"SHOP_CRASH_ON_START": "3"}, listen=False)
        self.assertEqual(svc.proc.wait(5), 3)
        self.assertFalse(any("listening on" in line for line in svc.lines))

    def test_sleep_start_delays_bind(self):
        t0 = time.monotonic()
        self.start({"SHOP_SLEEP_START": "0.3"})
        self.assertGreaterEqual(time.monotonic() - t0, 0.3)

    def test_config_is_logged(self):
        with tempfile.NamedTemporaryFile("w", suffix=".ini", delete=False) as f:
            f.write("[db]\npool = 5\n")
        self.addCleanup(os.unlink, f.name)
        svc = self.start({"SHOP_CONFIG": f.name})
        svc.wait_line(r"^INFO config: \[db\]$")
        svc.wait_line(r"^INFO config: pool = 5$")

    def test_missing_config_warns(self):
        svc = self.start({"SHOP_CONFIG": "/nonexistent/shop.ini"})
        svc.wait_line(r"^WARN config file missing")

    def test_json_logs(self):
        svc = Service(APP, env={"SHOP_LOG_JSON": "1", "SHOP_CHAOS": "1"})
        self.addCleanup(svc.kill)
        svc.wait_line(r'"msg": "listening on')
        svc.port = int(json.loads(svc.lines[0])["msg"].rsplit(":", 1)[1])
        svc.request("/healthz")
        svc.wait_line(r'"request_id"')
        for line in svc.lines:
            obj = json.loads(line)
            self.assertIn("ts", obj)
            self.assertIn("level", obj)
            self.assertIn("msg", obj)

    def test_sigterm_default_terminates(self):
        svc = self.start()
        svc.proc.send_signal(signal.SIGTERM)
        self.assertEqual(svc.proc.wait(5), -signal.SIGTERM)


class TestChaos(ServiceTest):
    def test_unhealthy_then_healthy_again(self):
        svc = self.start()
        self.assertEqual(svc.request("/healthz")[0], 200)
        status, body = svc.request("/__chaos/unhealthy?for=1s")
        self.assertEqual(status, 200)
        self.assertTrue(json.loads(body)["ok"])
        self.assertEqual(svc.request("/healthz")[0], 503)
        self.assertTrue(poll(lambda: svc.request("/healthz")[0] == 200, timeout=3))

    def test_crash_exits_with_code(self):
        svc = self.start()
        status, body = svc.request("/__chaos/crash?code=3")
        self.assertEqual(status, 200)
        self.assertTrue(json.loads(body)["ok"])
        self.assertEqual(svc.proc.wait(5), 3)

    def test_fork_children_share_group_and_die_with_it(self):
        svc = self.start()
        status, body = svc.request("/__chaos/fork?n=2")
        pids = json.loads(body)["effect"]["pids"]
        self.assertEqual(len(pids), 2)
        for pid in pids:
            self.assertTrue(pid_alive(pid))
            self.assertEqual(os.getpgid(pid), svc.proc.pid)
        os.killpg(svc.proc.pid, signal.SIGKILL)
        svc.proc.wait(5)
        for pid in pids:
            self.assertTrue(poll(lambda: not pid_alive(pid), timeout=5), "child %d survived" % pid)

    def test_hang_on_stop_ignores_sigterm(self):
        svc = self.start()
        self.assertEqual(svc.request("/__chaos/hang-on-stop")[0], 200)
        svc.proc.send_signal(signal.SIGTERM)
        svc.wait_line(r"^WARN ignoring SIGTERM$")
        time.sleep(0.5)  # the requirement is literally "still alive 500 ms later"
        self.assertTrue(svc.alive())
        self.assertEqual(svc.request("/healthz")[0], 200)
        svc.proc.send_signal(signal.SIGKILL)
        self.assertEqual(svc.proc.wait(5), -signal.SIGKILL)

    def test_logs_emits_n_lines_at_level(self):
        svc = self.start()
        status, _ = svc.request("/__chaos/logs?n=50&level=error")
        self.assertEqual(status, 200)
        svc.wait_line(r"^ERROR chaos log line 49$")
        self.assertEqual(sum(1 for l in svc.lines if l.startswith("ERROR chaos log line ")), 50)
        svc.request("/__chaos/logs?n=1")
        svc.wait_line(r"^INFO chaos log line 0$")

    def test_alloc_and_free(self):
        svc = self.start()
        status, body = svc.request("/__chaos/alloc?mb=20")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["effect"], "holding 20 MB")
        self.assertEqual(svc.request("/__chaos/alloc?mb=0")[0], 200)

    def test_touch_modifies_file_relative_to_cwd(self):
        with tempfile.TemporaryDirectory() as tmp:
            svc = self.start(cwd=tmp)
            path = os.path.join(tmp, "src", "x.py")
            self.assertEqual(svc.request("/__chaos/touch?file=src/x.py")[0], 200)
            first = read(path)
            self.assertIn("touched", first)
            self.assertEqual(svc.request("/__chaos/touch?file=src/x.py")[0], 200)
            self.assertNotEqual(read(path), first)
            svc.kill()

    def test_spin_returns_immediately(self):
        svc = self.start()
        t0 = time.monotonic()
        status, body = svc.request("/__chaos/spin?ms=500")
        self.assertEqual(status, 200)
        self.assertLess(time.monotonic() - t0, 0.4)
        self.assertEqual(svc.request("/healthz")[0], 200)

    def test_unknown_chaos_action_is_404(self):
        svc = self.start()
        self.assertEqual(svc.request("/__chaos/nope")[0], 404)


class TestDurations(unittest.TestCase):
    def test_parse_duration(self):
        self.assertEqual(app.parse_duration("500ms"), 0.5)
        self.assertEqual(app.parse_duration("5s"), 5.0)
        self.assertEqual(app.parse_duration("2m"), 120.0)
        self.assertEqual(app.parse_duration("7"), 7.0)
        self.assertEqual(app.parse_duration("1.5"), 1.5)
        with self.assertRaises(ValueError):
            app.parse_duration("")


if __name__ == "__main__":
    unittest.main()
