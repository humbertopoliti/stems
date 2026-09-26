import json
import signal
import time
import unittest

from web_support import SERVE, Service


class TestWeb(unittest.TestCase):
    def start(self, env=None, listen=True):
        svc = Service(SERVE, env=env)
        self.addCleanup(svc.kill)
        if listen:
            svc.wait_listening()
        return svc

    def test_healthz(self):
        svc = self.start()
        self.assertEqual(svc.request("/healthz")[0], 200)

    def test_index_injects_api_url(self):
        svc = self.start({"API_URL": "http://127.0.0.1:9999"})
        svc.wait_line(r"^INFO api_url=http://127\.0\.0\.1:9999$")
        status, body = svc.request("/")
        self.assertEqual(status, 200)
        self.assertIn(b'"http://127.0.0.1:9999"', body)
        self.assertNotIn(b"__API_URL__", body)
        self.assertEqual(svc.request("/index.html")[1], body)

    def test_not_found_and_no_traversal(self):
        svc = self.start()
        self.assertEqual(svc.request("/missing.js")[0], 404)
        self.assertEqual(svc.request("/../serve.py")[0], 404)
        self.assertEqual(svc.request("/%2e%2e/serve.py")[0], 404)

    def test_requests_logged_with_request_id(self):
        svc = self.start()
        svc.request("/healthz", headers={"X-Request-Id": "abc"})
        svc.wait_line(r"^INFO GET /healthz 200 request_id=abc$")

    def test_crash_on_start(self):
        svc = self.start({"SHOP_CRASH_ON_START": "3"}, listen=False)
        self.assertEqual(svc.proc.wait(5), 3)

    def test_sleep_start(self):
        t0 = time.monotonic()
        self.start({"SHOP_SLEEP_START": "0.3"})
        self.assertGreaterEqual(time.monotonic() - t0, 0.3)

    def test_json_logs(self):
        svc = self.start({"SHOP_LOG_JSON": "1"}, listen=False)
        svc.wait_line(r'"msg": "api_url=')
        for line in list(svc.lines):
            self.assertIn("msg", json.loads(line))

    def test_sigterm_terminates(self):
        svc = self.start()
        svc.proc.send_signal(signal.SIGTERM)
        self.assertEqual(svc.proc.wait(5), -signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
