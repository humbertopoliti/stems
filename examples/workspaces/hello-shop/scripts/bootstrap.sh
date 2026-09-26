#!/usr/bin/env bash
# scripts/bootstrap.sh — workspace-level "bootstrap" script (FR-SC-6).
# Runs before any stem starts. Checks required tooling; warns (does not
# fail) if docker is missing, since this workspace can otherwise be
# validated on CI machines without Docker (see plan/DECISIONS.md).
set -euo pipefail

if ! command -v python3 >/dev/null 2>&1; then
  echo "ERROR: python3 is required but was not found on PATH" >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "WARNING: docker not found; postgres/redis stems will not be usable here" >&2
fi

echo "bootstrap: ok"
