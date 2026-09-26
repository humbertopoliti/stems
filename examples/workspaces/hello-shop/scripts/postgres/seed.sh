#!/usr/bin/env bash
# scripts/postgres/seed.sh — postgres "seed" script.
# Applies the shop-api migration and inserts a few sample rows, via
# `docker exec ... psql` against the running postgres container.
#
# Uses $STEMS_WORKSPACE (set by stems for every script run, FR-SC-3) to
# locate the migration file in the sibling shop-api example repo, so this
# script does not depend on $STEMS_CODEBASE (that would be postgres's own
# codebase, which postgres, a docker stem, does not have).
set -euo pipefail

POSTGRES_USER="${POSTGRES_USER:-shop}"
CONTAINER="hello-shop-postgres"
MIGRATION="${STEMS_WORKSPACE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}/../../repos/shop-api/migrations/001_init.sql"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not available; cannot seed postgres" >&2
  exit 1
fi

docker exec -i "$CONTAINER" psql -U "$POSTGRES_USER" -d shop < "$MIGRATION"

docker exec -i "$CONTAINER" psql -U "$POSTGRES_USER" -d shop <<'SQL'
INSERT INTO products (name, price_cents) VALUES
  ('Widget', 999),
  ('Gadget', 1999),
  ('Gizmo', 2999)
ON CONFLICT DO NOTHING;
SQL

echo "postgres seed: ok"
