#!/usr/bin/env bash
# scripts/postgres/seed-large.sh — postgres "seed-large" script.
# Loads a synthetic perf dataset. Accepts --rows N (default 1000000, per
# the `args` schema declared on this script in stems.yaml).
set -euo pipefail

ROWS=1000000

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rows)
      ROWS="$2"
      shift 2
      ;;
    --rows=*)
      ROWS="${1#*=}"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

POSTGRES_USER="${POSTGRES_USER:-shop}"
CONTAINER="hello-shop-postgres"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not available; cannot seed postgres" >&2
  exit 1
fi

docker exec -i "$CONTAINER" psql -U "$POSTGRES_USER" -d shop <<SQL
INSERT INTO products (name, price_cents)
SELECT 'Bulk item ' || g, (g % 10000)
FROM generate_series(1, ${ROWS}) AS g
ON CONFLICT DO NOTHING;
SQL

echo "postgres seed-large: ok (rows=${ROWS})"
