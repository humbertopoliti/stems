#!/usr/bin/env bash
# scripts/nuke.sh — workspace-level "nuke-databases" script (FR-SC-6).
# `requires: [postgres]` in stems.yaml ensures postgres is healthy before
# this runs. Drops and recreates the shop database.
set -euo pipefail

POSTGRES_USER="${var_pg_user:-shop}"
CONTAINER="hello-shop-postgres"

if command -v docker >/dev/null 2>&1; then
  docker exec -i "$CONTAINER" psql -U "$POSTGRES_USER" -d postgres \
    -c "DROP DATABASE IF EXISTS shop;" \
    -c "CREATE DATABASE shop;"
else
  echo "docker not available; skipping actual drop/create (dry run)" >&2
fi

echo "nuke-databases: ok"
