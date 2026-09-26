import json
import signal
import socket
import time
import unittest

from fake_redis import FakeRedis
from worker_support import WORKER, Service


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class WorkerTest(unittest.TestCase):
    def start(self, env):
        svc = Service(WORKER, env=env)
        self.addCleanup(svc.kill)
        return svc

    def with_redis(self, **extra):
        redis = FakeRedis()
        self.addCleanup(redis.close)
        env = {"REDIS_URL": redis.url, "SHOP_CHAOS": "1"}
        env.update(extra)
        svc = self.start(env)
        svc.wait_line(r"^INFO connected to redis$")
        return redis, svc


class TestWorker(WorkerTest):
    def test_processes_jobs_and_heartbeats(self):
        redis, svc = self.with_redis()
        redis.push("hello")
        svc.wait_line(r"^INFO processed job hello$")
        svc.wait_line(r"^INFO heartbeat$")
        self.assertIn(["BLPOP", "shop:jobs", "1"], redis.commands)

    def test_queue_name_override(self):
        redis, svc = self.with_redis(SHOP_QUEUE="other:q")
        redis.push("x")
        svc.wait_line(r"^INFO processed job x$")
        self.assertEqual(redis.commands[0], ["BLPOP", "other:q", "1"])

    def test_crash_command_exits_with_code(self):
        redis, svc = self.with_redis()
        redis.push("crash:5")
        self.assertEqual(svc.proc.wait(5), 5)

    def test_crash_command_ignored_without_chaos(self):
        redis, svc = self.with_redis(SHOP_CHAOS="0")
        redis.push("crash:5")
        svc.wait_line(r"^INFO processed job crash:5$")
        self.assertTrue(svc.alive())

    def test_hang_on_stop_command(self):
        redis, svc = self.with_redis()
        redis.push("hang-on-stop")
        svc.wait_line(r"^WARN ignoring SIGTERM$")
        svc.proc.send_signal(signal.SIGTERM)
        time.sleep(0.5)
        self.assertTrue(svc.alive())
        svc.proc.send_signal(signal.SIGKILL)
        self.assertEqual(svc.proc.wait(5), -signal.SIGKILL)

    def test_redis_unavailable_does_not_crash(self):
        svc = self.start({"REDIS_URL": "redis://127.0.0.1:%d/0" % free_port()})
        svc.wait_line(r"^WARN redis unavailable, retrying")
        svc.wait_line(r"^INFO heartbeat$")
        self.assertTrue(svc.alive())

    def test_crash_on_start(self):
        svc = self.start({"SHOP_CRASH_ON_START": "4"})
        self.assertEqual(svc.proc.wait(5), 4)

    def test_json_logs(self):
        redis = FakeRedis()
        self.addCleanup(redis.close)
        svc = self.start({"REDIS_URL": redis.url, "SHOP_LOG_JSON": "1"})
        svc.wait_line(r'"msg": "heartbeat"')
        for line in list(svc.lines):
            self.assertIn("level", json.loads(line))


class TestControlPort(WorkerTest):
    def test_control_port_crash_without_redis(self):
        svc = self.start({"REDIS_URL": "redis://127.0.0.1:%d/0" % free_port(),
                          "SHOP_CHAOS": "1", "SHOP_CONTROL_PORT": "0"})
        m = svc.wait_line(r"^INFO control listening on [\d.]+:(\d+)$")
        svc.port = int(m.group(1))
        self.assertEqual(svc.request("/healthz")[0], 200)
        status, body = svc.request("/__chaos/crash?code=5")
        self.assertEqual(status, 200)
        self.assertTrue(json.loads(body)["ok"])
        self.assertEqual(svc.proc.wait(5), 5)

    def test_control_port_hang_on_stop(self):
        svc = self.start({"REDIS_URL": "redis://127.0.0.1:%d/0" % free_port(),
                          "SHOP_CHAOS": "1", "SHOP_CONTROL_PORT": "0"})
        svc.port = int(svc.wait_line(r"control listening on [\d.]+:(\d+)").group(1))
        self.assertEqual(svc.request("/__chaos/hang-on-stop")[0], 200)
        svc.proc.send_signal(signal.SIGTERM)
        time.sleep(0.5)
        self.assertTrue(svc.alive())

    def test_control_port_requires_chaos(self):
        svc = self.start({"REDIS_URL": "redis://127.0.0.1:%d/0" % free_port(), "SHOP_CONTROL_PORT": "0"})
        svc.wait_line(r"^INFO worker started")
        self.assertFalse(any("control listening" in l for l in svc.lines))


if __name__ == "__main__":
    unittest.main()
