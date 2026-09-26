#!/usr/bin/env bash
# Release smoke test (deliverable 32, FR-DS-1/2/3): exercise an INSTALLED
# stems binary end to end, Docker-free. Exits non-zero on the first failure.
#
#   release/smoke.sh [BIN]            BIN default: `stems` on PATH
#   make smoke [BIN=target/release/stems]
#
# Steps: --version (text + JSON with install_method) · `init --from
# hello-shop` + validate in a temp dir · copy examples/ to a temp dir and
# `up --detach` the minimal workspace's process stem · `status --json` all
# healthy · `down --all` · leak check (every started pid dead, no
# stemsd.sock/stemsd.lock left under STEMS_HOME, strict mode: no
# app.py/serve.py/worker.py process at all) · completions and man page
# installed under the prefix (skipped with a message when BIN is not an
# installed binary).
#
# Environment:
#   SMOKE_EXAMPLES=<dir>   the repo's examples/ (default: ../examples next to
#                          this script, when present)
#   SMOKE_PREFIX=<dir>     install prefix to check for share/zsh/site-functions/
#                          _stems and share/man/man1/stems.1 (default: the
#                          parent of BIN's directory; with SMOKE_BREW=1,
#                          `brew --prefix`)
#   SMOKE_BREW=1           BIN was installed by brew: completions and man page
#                          are REQUIRED (not skipped)
#   SMOKE_STRICT_LEAKS=1   fail on ANY app.py/serve.py/worker.py process (CI:
#                          nothing else runs them); default checks our pids only
#   SMOKE_DOCKER=1         also bring the full hello-shop workspace up and down
#                          (needs Docker)
#   SMOKE_KEEP=1           keep the temp dir
set -euo pipefail

BIN=${1:-stems}
if [[ "$BIN" == */* ]]; then
    BIN=$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")
else
    BIN=$(command -v "$BIN") || { echo "smoke: $1 not found on PATH" >&2; exit 1; }
fi
HERE=$(cd "$(dirname "$0")" && pwd)
EXAMPLES=${SMOKE_EXAMPLES:-}
if [[ -z "$EXAMPLES" && -d "$HERE/../examples" ]]; then
    EXAMPLES=$(cd "$HERE/../examples" && pwd)
fi

# /tmp, not $TMPDIR: macOS's per-user TMPDIR makes the daemon socket path
# longer than a Unix socket allows (SUN_LEN, 104 bytes).
TMP=$(mktemp -d /tmp/stems-smoke.XXXXXX)
TMP=$(cd "$TMP" && pwd -P)
export STEMS_HOME="$TMP/home"
export STEMS_NO_COLOR=1
unset STEMS_WORKSPACE STEMS_FAKE_INSTALL_METHOD STEMS_FAKE_VERSION

step=0
say() { step=$((step + 1)); printf '\n[smoke %02d] %s\n' "$step" "$*"; }
ok() { printf '  ok: %s\n' "$*"; }
skip() { printf '  SKIP: %s\n' "$*"; }
fail() { printf '  FAIL: %s\n' "$*" >&2; exit 1; }
# json <file> <python expression over `d`>: print the value.
json() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$@"; }

cleanup() {
    local code=$?
    # Never leave a daemon behind, whatever failed.
    for ws in "$TMP/examples/workspaces/minimal" "$TMP/examples/workspaces/hello-shop"; do
        [[ -f "$ws/stems.yaml" ]] && (cd "$ws" && "$BIN" down --all --json >/dev/null 2>&1 || true)
    done
    if [[ $code -ne 0 ]]; then
        echo "smoke: FAILED (exit $code); artefacts in $TMP" >&2
    elif [[ -n "${SMOKE_KEEP:-}" ]]; then
        echo "smoke: kept $TMP"
    else
        rm -rf "$TMP"
    fi
}
trap cleanup EXIT

echo "smoke: binary $BIN"
echo "smoke: STEMS_HOME $STEMS_HOME"

say "stems --version"
out=$("$BIN" --version)
[[ "$out" =~ ^stems\ [0-9]+\.[0-9]+\.[0-9]+ ]] || fail "unexpected --version output: $out"
ok "$out"
"$BIN" --version --json > "$TMP/version.json"
[[ $(json "$TMP/version.json" 'd["ok"]') == True ]] || fail "--version --json not ok"
method=$(json "$TMP/version.json" 'd["data"]["install_method"]')
[[ "$method" =~ ^(brew|tarball|cargo)$ ]] || fail "install_method is '$method'"
ok "version $(json "$TMP/version.json" 'd["data"]["version"]'), commit $(json "$TMP/version.json" 'd["data"]["commit"]'), install_method $method"
if [[ -n "${SMOKE_BREW:-}" && "$method" != brew ]]; then
    fail "installed by brew but install_method is $method"
fi
"$BIN" upgrade --dry-run --json > "$TMP/upgrade.json"
ok "upgrade would run: $(json "$TMP/upgrade.json" 'd["data"]["command"]')"

say "stems init --from hello-shop + validate (temp dir)"
mkdir -p "$TMP/init"
(cd "$TMP/init" && "$BIN" init --from hello-shop --json > "$TMP/init.json") || fail "init failed: $(cat "$TMP/init.json")"
[[ -f "$TMP/init/stems.yaml" ]] || fail "init wrote no stems.yaml"
(cd "$TMP/init" && "$BIN" validate --skip-requires --json > "$TMP/validate.json") \
    || fail "validate of the init'd workspace failed: $(cat "$TMP/validate.json")"
ok "hello-shop scaffold validates"

say "stems up --detach (examples/workspaces/minimal, process stem, no Docker)"
if [[ -z "$EXAMPLES" || ! -d "$EXAMPLES/workspaces/minimal" ]]; then
    fail "examples/ not found: set SMOKE_EXAMPLES=<repo>/examples"
fi
cp -R "$EXAMPLES" "$TMP/examples"
WS="$TMP/examples/workspaces/minimal"
cd "$WS"
"$BIN" validate --skip-requires --json > "$TMP/validate-minimal.json" || fail "minimal does not validate"
# Explicit stem names (not --profile): the minimal workspace has no profiles.
if ! "$BIN" up --detach --json echo-svc > "$TMP/up.jsonl" 2> "$TMP/up.err"; then
    tail -n 5 "$TMP/up.jsonl" "$TMP/up.err" >&2 || true
    fail "stems up --detach failed"
fi
ok "up returned"

say "stems status --json: every stem healthy"
healthy=0
for _ in $(seq 1 50); do
    "$BIN" status --json > "$TMP/status.json" || true
    if python3 - "$TMP/status.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
stems = d["data"]["stems"]
sys.exit(0 if stems and all(s.get("state") == "healthy" for s in stems) else 1)
PY
    then healthy=1; break; fi
    sleep 0.2
done
[[ $healthy -eq 1 ]] || fail "not healthy within 10s: $(cat "$TMP/status.json")"
pids=$(json "$TMP/status.json" '" ".join(str(s["pid"]) for s in d["data"]["stems"] if s.get("pid"))')
ok "healthy: $(json "$TMP/status.json" '", ".join(s["name"] for s in d["data"]["stems"])') (pids $pids)"

say "stems down --all"
"$BIN" down --all --json > "$TMP/down.json" || fail "down --all failed: $(cat "$TMP/down.json")"
ok "down"

say "leak check"
for _ in $(seq 1 50); do
    alive=""
    for p in $pids; do kill -0 "$p" 2>/dev/null && alive="$alive $p"; done
    left=$(find "$STEMS_HOME" \( -name stemsd.sock -o -name stemsd.lock \) 2>/dev/null || true)
    [[ -z "$alive" && -z "$left" ]] && break
    sleep 0.1
done
[[ -z "$alive" ]] || fail "stem processes still alive:$alive"
[[ -z "$left" ]] || fail "daemon socket/lock left behind: $left"
if [[ -n "${SMOKE_STRICT_LEAKS:-}" ]]; then
    strays=$(ps -eo pid=,command= | grep -E 'python3? (app|serve|worker)\.py' | grep -v grep || true)
    [[ -z "$strays" ]] || fail "example processes still running: $strays"
    ok "no app.py/serve.py/worker.py process (strict)"
fi
ok "no stem process, socket or lock left"

if [[ -n "${SMOKE_DOCKER:-}" ]]; then
    say "full hello-shop up/down (SMOKE_DOCKER=1)"
    cd "$TMP/examples/workspaces/hello-shop"
    "$BIN" up --detach --json > "$TMP/up-full.jsonl" || fail "full up failed: $(tail -n 3 "$TMP/up-full.jsonl")"
    "$BIN" status --json > "$TMP/status-full.json" || true
    "$BIN" down --all --json > "$TMP/down-full.json" || fail "full down failed"
    ok "hello-shop up and down"
fi

say "completions and man page installed"
if [[ -n "${SMOKE_PREFIX:-}" ]]; then
    PREFIX=$SMOKE_PREFIX
elif [[ -n "${SMOKE_BREW:-}" ]]; then
    PREFIX=$(brew --prefix)
else
    PREFIX=$(cd "$(dirname "$BIN")/.." && pwd)
fi
zsh_comp="$PREFIX/share/zsh/site-functions/_stems"
man_page="$PREFIX/share/man/man1/stems.1"
if [[ -f "$zsh_comp" ]]; then
    grep -q '#compdef stems' "$zsh_comp" || fail "$zsh_comp is not the stems completion"
    ok "$zsh_comp"
elif [[ -n "${SMOKE_BREW:-}" ]]; then
    fail "$zsh_comp missing (the formula should install it)"
else
    skip "$zsh_comp not present (not an installed prefix); checking the generator instead"
    "$BIN" completions zsh | grep -q '#compdef stems' || fail "stems completions zsh"
    ok "stems completions zsh generates the script"
fi
if [[ -f "$man_page" ]]; then
    MANPATH="$PREFIX/share/man" man -P cat stems 2>/dev/null | grep -qi 'stems' || fail "man stems did not render"
    MANPATH="$PREFIX/share/man" man -P cat stems-up 2>/dev/null | grep -qi 'stems' || fail "man stems-up did not render"
    ok "man stems, man stems-up"
elif [[ -n "${SMOKE_BREW:-}" ]]; then
    fail "$man_page missing (the formula should install it)"
else
    skip "$man_page not present (not an installed prefix); checking the generator instead"
    "$BIN" __man "$TMP/man" --json > /dev/null
    grep -q '^\.TH stems 1' "$TMP/man/stems.1" || fail "__man wrote no stems.1"
    if command -v man >/dev/null 2>&1; then
        man -P cat "$TMP/man/stems.1" 2>/dev/null | grep -qi stems || fail "stems.1 does not render"
    fi
    ok "stems __man generates renderable pages"
fi

printf '\nsmoke: PASS (%d steps)\n' "$step"
