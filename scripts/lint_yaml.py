#!/usr/bin/env python3
"""scripts/lint_yaml.py

Dependency-free-by-default YAML lint for everything under examples/.

- If PyYAML is importable, `yaml.safe_load` every *.yml/*.yaml file under
  examples/ and fail (non-zero exit) on the first parse error.
- If PyYAML is NOT importable, print a message and exit 0 rather than
  failing `make check` on a machine without it (this repo does not want a
  hard PyYAML dependency just for CI lint).
- Independent of the above, always verify that every examples/workspaces/
  broken/* directory has an EXPECTED.json that parses as JSON and has the
  keys: code, path, exit.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
EXAMPLES_DIR = REPO_ROOT / "examples"
BROKEN_DIR = EXAMPLES_DIR / "workspaces" / "broken"

REQUIRED_EXPECTED_KEYS = ("code", "path", "exit")


def lint_yaml_files() -> int:
    try:
        import yaml  # type: ignore
    except ImportError:
        print("pyyaml not available, skipping yaml lint")
        return 0

    failures = 0
    yaml_files = sorted(
        list(EXAMPLES_DIR.rglob("*.yml")) + list(EXAMPLES_DIR.rglob("*.yaml"))
    )
    if not yaml_files:
        print("no yaml files found under examples/", file=sys.stderr)
        return 1

    for path in yaml_files:
        try:
            with path.open("r", encoding="utf-8") as f:
                yaml.safe_load(f)
        except Exception as exc:  # noqa: BLE001 - want to report any parse error
            print(f"FAIL: {path.relative_to(REPO_ROOT)}: {exc}", file=sys.stderr)
            failures += 1
        else:
            print(f"ok:   {path.relative_to(REPO_ROOT)}")

    if failures:
        print(f"{failures} yaml file(s) failed to parse", file=sys.stderr)
        return 1

    print(f"{len(yaml_files)} yaml file(s) parsed ok")
    return 0


def check_expected_json() -> int:
    if not BROKEN_DIR.is_dir():
        print(f"FAIL: {BROKEN_DIR} does not exist", file=sys.stderr)
        return 1

    failures = 0
    broken_dirs = sorted(p for p in BROKEN_DIR.iterdir() if p.is_dir())
    if not broken_dirs:
        print(f"FAIL: no broken/* workspace directories found", file=sys.stderr)
        return 1

    for workspace_dir in broken_dirs:
        expected_path = workspace_dir / "EXPECTED.json"
        rel = expected_path.relative_to(REPO_ROOT)
        if not expected_path.is_file():
            print(f"FAIL: {rel} missing", file=sys.stderr)
            failures += 1
            continue
        try:
            with expected_path.open("r", encoding="utf-8") as f:
                data = json.load(f)
        except Exception as exc:  # noqa: BLE001
            print(f"FAIL: {rel} does not parse as JSON: {exc}", file=sys.stderr)
            failures += 1
            continue

        missing = [k for k in REQUIRED_EXPECTED_KEYS if k not in data]
        if missing:
            print(f"FAIL: {rel} missing keys: {missing}", file=sys.stderr)
            failures += 1
            continue

        print(f"ok:   {rel}")

    if failures:
        print(f"{failures} EXPECTED.json file(s) invalid", file=sys.stderr)
        return 1

    print(f"{len(broken_dirs)} broken/* workspace(s) have a valid EXPECTED.json")
    return 0


def main() -> int:
    yaml_rc = lint_yaml_files()
    expected_rc = check_expected_json()
    return yaml_rc or expected_rc


if __name__ == "__main__":
    sys.exit(main())
