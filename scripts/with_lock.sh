#!/bin/sh
# Serialise e2e runs on one machine: two harness processes collide on port
# blocks and the orphan scan. Usage: scripts/with_lock.sh <command...>
set -eu
LOCK="${STEMS_E2E_LOCK:-/tmp/stems-e2e.lock}"
waited=0
while ! mkdir "$LOCK" 2>/dev/null; do
  # Reclaim a lock whose owner is gone.
  if [ -f "$LOCK/pid" ] && ! kill -0 "$(cat "$LOCK/pid")" 2>/dev/null; then
    rm -rf "$LOCK"; continue
  fi
  [ "$waited" -eq 0 ] && echo "with_lock: another e2e run holds $LOCK, waiting..." >&2
  waited=$((waited + 1)); sleep 2
done
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT INT TERM HUP
"$@"
