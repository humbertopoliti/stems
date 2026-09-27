#!/bin/sh
# Compatibility name for scripts/e2e_slot.sh (e2e runs used to be serialised
# by an exclusive lock; they now get one of STEMS_E2E_SLOTS slots, each with
# its own port range and temp-dir prefix). Usage: scripts/with_lock.sh <command...>
exec "$(dirname "$0")/e2e_slot.sh" "$@"
