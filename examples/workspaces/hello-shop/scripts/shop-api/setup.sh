#!/usr/bin/env bash
# scripts/shop-api/setup.sh — shop-api "setup" script.
# Creates a Python venv under $STEMS_STATE_DIR (stems-managed), NEVER
# inside the shop-api codebase itself (FR-WS-2: code repos are never
# modified by stems).
set -euo pipefail

: "${STEMS_STATE_DIR:=/tmp/stems-state/shop-api}"
VENV_DIR="$STEMS_STATE_DIR/venv"

if ! command -v python3 >/dev/null 2>&1; then
  echo "ERROR: python3 is required but was not found on PATH" >&2
  exit 1
fi

mkdir -p "$STEMS_STATE_DIR"
python3 -m venv "$VENV_DIR"

# shop-api is Python-stdlib-only (see plan/02-example-services.md), so
# there are no dependencies to install; the venv's presence is what the
# setup stamp (FR-SC-5) tracks.

echo "shop-api setup: ok (venv at $VENV_DIR)"
