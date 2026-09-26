"""The Examples table of tests/features/validate/broken.feature must mirror
examples/workspaces/broken/*/EXPECTED.json exactly.

Gherkin Examples must be literal, so the table is generated from the
EXPECTED.json files. Adding a broken workspace therefore needs no Rust
change: add the directory with its EXPECTED.json, then regenerate the table:

    python3 scripts/tests/test_broken_examples_in_sync.py --write
"""

import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BROKEN = ROOT / "examples" / "workspaces" / "broken"
FEATURE = ROOT / "tests" / "features" / "validate" / "broken.feature"
COLUMNS = ["name", "code", "path", "exit", "message_contains"]
REQUIRED_KEYS = {"code", "path", "exit", "message_contains"}


def expected_rows():
    """One row per broken workspace, sorted by name."""
    rows = []
    for d in sorted(p for p in BROKEN.iterdir() if p.is_dir()):
        f = d / "EXPECTED.json"
        if not f.is_file():
            raise AssertionError(f"{d.relative_to(ROOT)} has no EXPECTED.json")
        data = json.loads(f.read_text())
        missing = REQUIRED_KEYS - data.keys()
        if missing:
            raise AssertionError(f"{f.relative_to(ROOT)} lacks {sorted(missing)}")
        for k in ("code", "path", "message_contains"):
            v = str(data[k])
            if "|" in v or '"' in v or "\n" in v:
                raise AssertionError(f"{f.relative_to(ROOT)}: {k} cannot contain `|`, `\"` or newlines")
        rows.append([d.name, data["code"], data["path"], str(data["exit"]), data["message_contains"]])
    return rows


def format_table(rows, indent="      "):
    table = [COLUMNS] + rows
    widths = [max(len(r[i]) for r in table) for i in range(len(COLUMNS))]
    return "\n".join(
        indent + "| " + " | ".join(c.ljust(w) for c, w in zip(r, widths)) + " |" for r in table
    )


def split_feature(text):
    """(before, table lines, after) around the first Examples table."""
    lines = text.splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if l.strip().startswith("Examples:")) + 1
    except StopIteration:
        raise AssertionError(f"{FEATURE.relative_to(ROOT)} has no Examples: block") from None
    end = start
    while end < len(lines) and lines[end].strip().startswith("|"):
        end += 1
    return lines[:start], lines[start:end], lines[end:]


def parse_table(table_lines):
    rows = [[c.strip() for c in l.strip().strip("|").split("|")] for l in table_lines]
    return rows[0], rows[1:]


def write():
    before, _, after = split_feature(FEATURE.read_text())
    text = "\n".join(before + [format_table(expected_rows())] + after) + "\n"
    FEATURE.write_text(text)


class BrokenExamplesInSync(unittest.TestCase):
    def test_every_broken_workspace_has_a_valid_expected_json(self):
        rows = expected_rows()
        self.assertGreaterEqual(len(rows), 10)
        for r in rows:
            self.assertEqual(r[3], "2", f"{r[0]}: validation errors exit 2")

    def test_feature_table_matches_expected_json(self):
        header, rows = parse_table(split_feature(FEATURE.read_text())[1])
        self.assertEqual(header, COLUMNS)
        want = expected_rows()
        self.assertEqual(
            sorted(rows),
            want,
            "tests/features/validate/broken.feature is out of sync with "
            "examples/workspaces/broken/*/EXPECTED.json; run "
            "`python3 scripts/tests/test_broken_examples_in_sync.py --write`",
        )

    def test_table_round_trips(self):
        rows = [["a", "B", "c.d", "2", "x -> y"]]
        header, parsed = parse_table(format_table(rows).splitlines())
        self.assertEqual(header, COLUMNS)
        self.assertEqual(parsed, rows)


if __name__ == "__main__":
    if "--write" in sys.argv:
        write()
        print(f"wrote {FEATURE.relative_to(ROOT)}")
    else:
        unittest.main()
