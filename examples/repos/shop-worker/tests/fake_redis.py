"""A tiny in-process RESP server that understands just enough for shop-worker: AUTH, SELECT, PING, BLPOP."""

import queue
import socket
import threading


class FakeRedis:
    def __init__(self):
        self.jobs = queue.Queue()
        self.commands = []
        self.sock = socket.socket()
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(8)
        self.port = self.sock.getsockname()[1]
        self.url = "redis://127.0.0.1:%d/0" % self.port
        self._stop = False
        threading.Thread(target=self._accept, daemon=True).start()

    def push(self, job):
        self.jobs.put(job)

    def close(self):
        self._stop = True
        self.sock.close()

    def _accept(self):
        while not self._stop:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn):
        f = conn.makefile("rb")
        try:
            while True:
                line = f.readline()
                if not line:
                    return
                n = int(line[1:-2])
                args = []
                for _ in range(n):
                    size = int(f.readline()[1:-2])
                    args.append(f.read(size + 2)[:-2].decode())
                self.commands.append(args)
                cmd = args[0].upper()
                if cmd == "BLPOP":
                    try:
                        job = self.jobs.get(timeout=float(args[-1]))
                    except queue.Empty:
                        conn.sendall(b"*-1\r\n")
                        continue
                    key, val = args[1].encode(), job.encode()
                    conn.sendall(b"*2\r\n$%d\r\n%s\r\n$%d\r\n%s\r\n" % (len(key), key, len(val), val))
                else:
                    conn.sendall(b"+OK\r\n")
        except (OSError, ValueError):
            return
        finally:
            conn.close()
