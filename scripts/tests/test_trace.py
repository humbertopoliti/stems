"""Unit tests for scripts/trace.py against a tiny fixture repository."""

import importlib.util
import io
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURE = HERE / "fixtures" / "trace"
_spec = importlib.util.spec_from_file_location("trace_mod", HERE.parent / "trace.py")
trace = importlib.util.module_from_spec(_spec)
sys.modules["trace_mod"] = trace
_spec.loader.exec_module(trace)


class ParseTests(unittest.TestCase):
    def test_requirements_and_priorities(self):
        reqs = trace.parse_requirements((FIXTURE / "REQUIREMENTS.md").read_text())
        self.assertEqual(reqs["FR-WS-1"], "P0")
        self.assertEqual(reqs["FR-WS-2"], "P1")
        self.assertEqual(reqs["NFR-2"], "P0")
        self.assertEqual(len(reqs), 6)

    def test_status_table_and_claims(self):
        status = trace.parse_status_table((FIXTURE / "plan" / "README.md").read_text())
        self.assertEqual(status, {"01": "done", "02": "todo"})
        noted = trace.parse_status_table("| 32 | [Release](32-r.md) | 5 | 19 | done (pipeline unexercised: no remote) |\n")
        self.assertEqual(noted, {"32": "done"})
        claims = trace.parse_plan_requirements(FIXTURE / "plan")
        self.assertEqual(claims["FR-LC-6"], {"01"})
        self.assertEqual(claims["FR-WS-2"], {"02"})
        self.assertNotIn("§7.3", claims)

    def test_feature_tags_are_inherited(self):
        scs = trace.parse_feature("f.feature", (FIXTURE / "tests/features/a/one.feature").read_text())
        by_name = {s.name: s for s in scs}
        self.assertEqual(by_name["plain"].tags, {"@FR-LC-5", "@recovery"})
        self.assertEqual(trace.tagged_ids(by_name["tagged"]), {"NFR-2", "FR-LC-5", "FR-XX-99"})
        self.assertEqual(trace.tagged_ids(by_name["outline <n>"]), {"FR-LC-5", "FR-WS-1", "FR-WS-2"})
        self.assertEqual(by_name["plain"].line, 4)

    def test_pending(self):
        entries = trace.parse_pending("x/\na.feature:3 # c\n# only comment\n")
        self.assertTrue(trace.is_pending(entries, "x/y.feature", 1))
        self.assertTrue(trace.is_pending(entries, "a.feature", 3))
        self.assertFalse(trace.is_pending(entries, "a.feature", 4))


class TraceTests(unittest.TestCase):
    def run_trace(self, root, **kw):
        out = io.StringIO()
        code = trace.trace(root, out=out, **kw)
        return code, out.getvalue()

    def test_gap_in_done_deliverable_fails(self):
        code, text = self.run_trace(FIXTURE)
        self.assertEqual(code, 1, text)
        self.assertIn("FR-LC-6", text.split("FAIL:")[1])
        self.assertNotIn("FR-GR-9", text.split("FAIL:")[1])
        self.assertIn("UNKNOWN", text)
        self.assertIn("FR-XX-99", text)
        self.assertIn("(pending)", text)

    def test_passes_once_gap_is_traced_and_strict_flags_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            shutil.copytree(FIXTURE, root)
            (root / "tests/features/a/two.feature").write_text(
                "@FR-LC-6\nFeature: two\n  Scenario: s\n    Given x\n"
            )
            code, text = self.run_trace(root)
            self.assertEqual(code, 0, text)
            code, text = self.run_trace(root, strict=True)
            self.assertEqual(code, 1, text)


if __name__ == "__main__":
    unittest.main()
