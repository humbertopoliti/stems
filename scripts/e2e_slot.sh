#!/bin/sh
# Run an e2e command in one of N machine-wide slots so several harness runs
# can share a machine without sharing ports or temp dirs.
#
# Usage: scripts/e2e_slot.sh [--exclusive] <command...>
#
# Claims the lowest free slot i in 0..N-1 (N = STEMS_E2E_SLOTS, default 4,
# max 10) as the directory $STEMS_E2E_SLOTS_DIR/<i> (default
# /tmp/stems-e2e-slots; mkdir is atomic), waiting only while every slot is
# busy. A slot whose recorded owner pid is dead is reclaimed. The command
# gets STEMS_E2E_SLOT=<i> and STEMS_E2E_PORT_START=20000+i*4000; the harness
# allocates its 20-port scenario blocks inside [start, start+4000) and names
# its temp dirs /tmp/stems-e2e-s<i>-XXXXXX, so runs in different slots never
# share a declared port (the daemon's orphan scan only looks at declared
# ports) or a scenario dir (the leak hook matches by scenario dir).
#
# --exclusive claims every slot (in order) and runs as slot 0: used by the
# Docker tier, whose compose projects and containers are machine-global.
set -eu
DIR="${STEMS_E2E_SLOTS_DIR:-/tmp/stems-e2e-slots}"
N="${STEMS_E2E_SLOTS:-4}"
LEGACY_LOCK=/tmp/stems-e2e.lock
exclusive=0
if [ "${1:-}" = "--exclusive" ]; then exclusive=1; shift; fi
case "$N" in ''|*[!0-9]*) echo "e2e_slot: STEMS_E2E_SLOTS must be 1..10" >&2; exit 2 ;; esac
if [ "$N" -lt 1 ] || [ "$N" -gt 10 ]; then
  echo "e2e_slot: STEMS_E2E_SLOTS must be 1..10 (slot 9 ends at port 60000)" >&2; exit 2
fi
mkdir -p "$DIR"

held=""
release() { for s in $held; do rm -rf "$DIR/$s"; done; held=""; }
trap release EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# claim <i>: 0 if slot i is now ours, 1 if it is held by a live run.
claim() {
  while :; do
    if mkdir "$DIR/$1" 2>/dev/null; then
      echo $$ > "$DIR/$1/pid"
      held="$held $1"
      return 0
    fi
    # Reclaim a slot whose owner is gone (a slot without a pid file is being
    # claimed right now: leave it).
    owner=$(cat "$DIR/$1/pid" 2>/dev/null || true)
    if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
      rm -rf "$DIR/$1"; continue
    fi
    return 1
  done
}

# Transitional: a run still holding the old exclusive lock uses ports from
# 20000 and does not know about slots; wait for it.
legacy_busy() {
  [ -d "$LEGACY_LOCK" ] || return 1
  owner=$(cat "$LEGACY_LOCK/pid" 2>/dev/null || true)
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    rm -rf "$LEGACY_LOCK"; return 1
  fi
  return 0
}

said=0
wait_note() {
  [ "$said" -eq 0 ] && echo "e2e_slot: $1, waiting..." >&2
  said=1
}

while legacy_busy; do wait_note "a legacy run holds $LEGACY_LOCK"; sleep 2; done

if [ "$exclusive" -eq 1 ]; then
  i=0
  while [ "$i" -lt "$N" ]; do
    until claim "$i"; do wait_note "slot $i is busy (exclusive run needs all $N)"; sleep 2; done
    i=$((i + 1))
  done
  slot=0
else
  slot=""
  while [ -z "$slot" ]; do
    i=0
    while [ "$i" -lt "$N" ]; do
      if claim "$i"; then slot=$i; break; fi
      i=$((i + 1))
    done
    if [ -z "$slot" ]; then wait_note "all $N e2e slots in $DIR are busy"; sleep 2; fi
  done
fi

STEMS_E2E_SLOT=$slot
STEMS_E2E_PORT_START=$((20000 + slot * 4000))
export STEMS_E2E_SLOT STEMS_E2E_PORT_START
[ "$exclusive" -eq 1 ] && mode=" (exclusive)" || mode=""
echo "e2e_slot: slot $slot$mode, ports $STEMS_E2E_PORT_START-$((STEMS_E2E_PORT_START + 3999))" >&2
# Not exec: the trap must release the slot when the command ends.
"$@"
