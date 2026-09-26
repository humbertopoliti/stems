#!/usr/bin/env python3
"""Requirement traceability (REQUIREMENTS.md section 7.4, deliverable 04).

Maps every requirement id in REQUIREMENTS.md (``FR-*`` with P0/P1/P2, and
``NFR-*`` which count as P0) to the Gherkin scenarios tagged with it under
``tests/features/**/*.feature``, prints the matrix, and exits 1 when:

* a **P0** id has no tagged scenario **and** a deliverable that claims it
  (the ``**Requirements**`` line of ``plan/NN-*.md``) is marked ``done`` in
  the status table of ``plan/README.md`` (so phase 0 is not blocked by
  phase 3 gaps).

Ids tagged in features that do not exist in REQUIREMENTS.md are printed as
UNKNOWN (and fail the run only with ``--strict``). Scenarios listed in
``tests/features/PENDING.txt`` are marked ``pending`` in the matrix; they
still count as "a scenario exists" for the gate, because they are run and
expected to pass once their deliverable lands.

Usage: scripts/trace.py [--root DIR] [--strict] [--quiet]
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

FR_RE = re.compile(r"\*\*(FR-[A-Z]+-\d+)\s*\((P[012])\)\*\*")
NFR_RE = re.compile(r"\*\*(NFR-\d+)\b")
ID_RE = re.compile(r"\b(NFR-\d+|FR-[A-Z]+-\d+)\b")
TAG_ID_RE = re.compile(r"^@(NFR-\d+|FR-[A-Z]+-\d+)$")
# The status cell may carry a note: `done (pipeline unexercised: no remote)`.
STATUS_ROW_RE = re.compile(r"^\|\s*(\d{2})\s*\|.*\|\s*([A-Za-z-]+)(?:\s*\([^)|]*\))?\s*\|\s*$")


@dataclass
class Scenario:
    path: str
    line: int
    name: str
    tags: set[str] = field(default_factory=set)
    pending: bool = False

    def label(self) -> str:
        suffix = " (pending)" if self.pending else ""
        return f"{self.path}:{self.line} {self.name}{suffix}"


def parse_requirements(text: str) -> dict[str, str]:
    """Requirement id -> priority (NFRs are P0)."""
    reqs: dict[str, str] = {}
    for m in FR_RE.finditer(text):
        reqs[m.group(1)] = m.group(2)
    for m in NFR_RE.finditer(text):
        reqs.setdefault(m.group(1), "P0")
    return reqs


def parse_status_table(text: str) -> dict[str, str]:
    """Deliverable number ("04") -> status ("done", "todo", ...)."""
    out: dict[str, str] = {}
    for line in text.splitlines():
        m = STATUS_ROW_RE.match(line.strip())
        if m:
            out[m.group(1)] = m.group(2).lower()
    return out


def parse_plan_requirements(plan_dir: Path) -> dict[str, set[str]]:
    """Requirement id -> deliverable numbers claiming it."""
    out: dict[str, set[str]] = {}
    for f in sorted(plan_dir.glob("[0-9][0-9]-*.md")):
        num = f.name[:2]
        for line in f.read_text(encoding="utf-8").splitlines():
            if "**Requirements**" not in line:
                continue
            claimed = line.split("**Requirements**", 1)[1]
            for rid in ID_RE.findall(claimed):
                out.setdefault(rid, set()).add(num)
    return out


def parse_pending(text: str) -> list[tuple[str, int | None]]:
    entries = []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        path, _, num = line.rpartition(":")
        if path and num.isdigit():
            entries.append((path.removeprefix("./"), int(num)))
        else:
            entries.append((line.removeprefix("./"), None))
    return entries


def is_pending(entries, path: str, line: int) -> bool:
    for p, n in entries:
        ok = path.startswith(p) if p.endswith("/") else path == p
        if ok and (n is None or n == line):
            return True
    return False


def parse_feature(path: str, text: str) -> list[Scenario]:
    """Scenarios with inherited Feature/Rule tags (Examples tags included)."""
    scenarios: list[Scenario] = []
    feature_tags: set[str] = set()
    rule_tags: set[str] = set()
    pending_tags: set[str] = set()
    current: Scenario | None = None
    for i, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("@"):
            pending_tags |= {t for t in line.split() if t.startswith("@")}
            continue
        keyword = line.split(":", 1)[0].strip() if ":" in line else ""
        if keyword == "Feature":
            feature_tags, pending_tags = pending_tags, set()
        elif keyword == "Rule":
            rule_tags, pending_tags = pending_tags, set()
        elif keyword in ("Scenario", "Example", "Scenario Outline", "Scenario Template"):
            name = line.split(":", 1)[1].strip()
            current = Scenario(path, i, name, feature_tags | rule_tags | pending_tags)
            scenarios.append(current)
            pending_tags = set()
        elif keyword in ("Examples", "Scenarios"):
            if current is not None:
                current.tags |= pending_tags
            pending_tags = set()
        else:
            pending_tags = set()
    return scenarios


def collect_scenarios(root: Path) -> list[Scenario]:
    features = root / "tests" / "features"
    pending_file = features / "PENDING.txt"
    pending = parse_pending(pending_file.read_text(encoding="utf-8")) if pending_file.exists() else []
    out: list[Scenario] = []
    for f in sorted(features.rglob("*.feature")):
        rel = f.relative_to(root).as_posix()
        for sc in parse_feature(rel, f.read_text(encoding="utf-8")):
            sc.pending = is_pending(pending, sc.path, sc.line)
            out.append(sc)
    return out


def tagged_ids(sc: Scenario) -> set[str]:
    ids = set()
    for t in sc.tags:
        m = TAG_ID_RE.match(t)
        if m:
            ids.add(m.group(1))
    return ids


def id_sort_key(rid: str):
    parts = rid.split("-")
    return (parts[0], parts[1] if len(parts) > 2 else "", int(parts[-1]))


def trace(root: Path, strict: bool = False, quiet: bool = False, out=sys.stdout) -> int:
    reqs = parse_requirements((root / "REQUIREMENTS.md").read_text(encoding="utf-8"))
    status = parse_status_table((root / "plan" / "README.md").read_text(encoding="utf-8"))
    claims = parse_plan_requirements(root / "plan")
    scenarios = collect_scenarios(root)

    by_id: dict[str, list[Scenario]] = {}
    unknown: dict[str, list[Scenario]] = {}
    for sc in scenarios:
        for rid in tagged_ids(sc):
            (by_id if rid in reqs else unknown).setdefault(rid, []).append(sc)

    gaps: list[str] = []
    print(f"Requirement traceability: {len(reqs)} ids, {len(scenarios)} scenarios", file=out)
    print(f"{'ID':<11} {'PRI':<4} {'DELIVERABLES':<24} {'SCEN':>4}  STATUS", file=out)
    for rid in sorted(reqs, key=id_sort_key):
        pri = reqs[rid]
        dels = sorted(claims.get(rid, set()))
        del_s = ",".join(f"{d}:{status.get(d, '?')}" for d in dels) or "-"
        scs = by_id.get(rid, [])
        done = any(status.get(d) == "done" for d in dels)
        if scs:
            passing = [s for s in scs if not s.pending]
            state = "traced" if passing else "traced (pending only)"
        elif pri == "P0" and done:
            state = "MISSING (P0, deliverable done)"
            gaps.append(rid)
        elif pri == "P0":
            state = "untraced (deliverable not done)"
        else:
            state = "untraced"
        if quiet and not scs and rid not in gaps:
            continue
        print(f"{rid:<11} {pri:<4} {del_s:<24} {len(scs):>4}  {state}", file=out)
        if not quiet:
            for s in scs:
                print(f"{'':<11}   - {s.label()}", file=out)

    if unknown:
        print("\nUNKNOWN ids tagged in features (not in REQUIREMENTS.md):", file=out)
        for rid in sorted(unknown):
            for s in unknown[rid]:
                print(f"  {rid}: {s.label()}", file=out)

    if gaps:
        print(f"\nFAIL: {len(gaps)} P0 requirement(s) of done deliverables have no scenario: "
              + ", ".join(gaps), file=out)
        return 1
    if unknown and strict:
        print("\nFAIL: unknown requirement ids tagged (--strict)", file=out)
        return 1
    print("\nOK: every P0 requirement of a done deliverable is tagged by a scenario", file=out)
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    ap.add_argument("--strict", action="store_true", help="fail on unknown tagged ids")
    ap.add_argument("--quiet", action="store_true", help="omit untraced non-gating rows and scenario lists")
    args = ap.parse_args(argv)
    return trace(args.root, strict=args.strict, quiet=args.quiet)


if __name__ == "__main__":
    sys.exit(main())
