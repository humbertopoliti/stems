"""Every example repo must import only the Python standard library (plus its own local modules)."""

import ast
import os
import sys
import unittest

REPOS = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SERVICES = ("shop-api", "shop-worker", "shop-web")


def python_files():
    for svc in SERVICES:
        for root, dirs, files in os.walk(os.path.join(REPOS, svc)):
            dirs[:] = [d for d in dirs if not d.startswith(".") and d not in ("__pycache__", "venv")]
            for name in files:
                if name.endswith(".py"):
                    yield os.path.join(root, name)


class TestStdlibOnly(unittest.TestCase):
    def test_only_stdlib_imports(self):
        files = list(python_files())
        self.assertGreaterEqual(len(files), 6)
        local = {os.path.splitext(os.path.basename(f))[0] for f in files}
        offenders = []
        for path in files:
            with open(path) as f:
                tree = ast.parse(f.read(), path)
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [a.name for a in node.names]
                elif isinstance(node, ast.ImportFrom) and node.level == 0:
                    names = [node.module]
                else:
                    continue
                for name in names:
                    top = name.split(".")[0]
                    if top not in sys.stdlib_module_names and top not in local:
                        offenders.append("%s: %s" % (os.path.relpath(path, REPOS), name))
        self.assertEqual(offenders, [], "third-party imports found")


if __name__ == "__main__":
    unittest.main()
