#!/usr/bin/env python3
"""Render the Homebrew formula (deliverable 32, FR-DS-1).

    release/render_formula.py --dist DIR --version 0.1.0 \
        --base-url https://github.com/ORG/stems/releases/download/v0.1.0 \
        [--homepage URL] [--out Formula/stems.rb]

DIR holds cargo-dist's tarballs `stems-cli-<target>.tar.xz` (and/or their
`.sha256` files). Each target's sha256 is computed from the tarball when it is
present, else read from `<tarball>.sha256`. Every target in TARGETS must be
found. `--base-url file://…` renders a formula that installs from local files
(the release smoke test does that before anything is published).
"""

from __future__ import annotations

import argparse
import hashlib
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
TEMPLATE = HERE / "formula" / "stems.rb.tmpl"
PACKAGE = "stems-cli"
PLACEHOLDER = re.compile(r"@(?:VERSION|HOMEPAGE|URL_[\w-]+|SHA256_[\w-]+)@")
TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
)


def tarball_name(target: str) -> str:
    return f"{PACKAGE}-{target}.tar.xz"


def sha256_of(dist: pathlib.Path, target: str) -> str:
    tarball = dist / tarball_name(target)
    if tarball.is_file():
        return hashlib.sha256(tarball.read_bytes()).hexdigest()
    sidecar = dist / (tarball_name(target) + ".sha256")
    if sidecar.is_file():
        digest = sidecar.read_text().split()[0].strip().lower()
        if len(digest) == 64 and all(c in "0123456789abcdef" for c in digest):
            return digest
        raise SystemExit(f"render_formula: {sidecar} does not hold a sha256")
    raise SystemExit(f"render_formula: neither {tarball} nor {sidecar} exists")


def render(version: str, base_url: str, homepage: str, shas: dict[str, str]) -> str:
    text = TEMPLATE.read_text()
    base = base_url.rstrip("/")
    values = {"@VERSION@": version, "@HOMEPAGE@": homepage}
    for target in TARGETS:
        values[f"@URL_{target}@"] = f"{base}/{tarball_name(target)}"
        values[f"@SHA256_{target}@"] = shas[target]
    for key, value in values.items():
        text = text.replace(key, value)
    leftover = PLACEHOLDER.findall(text)
    if leftover:
        raise SystemExit(f"render_formula: unreplaced placeholders: {leftover}")
    return text


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--dist", required=True, type=pathlib.Path)
    p.add_argument("--version", required=True, help="e.g. 0.1.0 or 0.1.0-rc.1 (no leading v)")
    p.add_argument("--base-url", required=True)
    p.add_argument("--homepage", default="https://github.com/humbertopoliti/stems")
    p.add_argument("--out", type=pathlib.Path)
    a = p.parse_args(argv)
    version = a.version[1:] if a.version.startswith("v") else a.version
    shas = {t: sha256_of(a.dist, t) for t in TARGETS}
    text = render(version, a.base_url, a.homepage, shas)
    if a.out:
        a.out.parent.mkdir(parents=True, exist_ok=True)
        a.out.write_text(text)
        print(f"render_formula: wrote {a.out}", file=sys.stderr)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
