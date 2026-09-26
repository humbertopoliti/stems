#!/bin/sh
# Generate shell completions and man pages for the release tarballs
# (deliverable 32, FR-CL-2, FR-DS-1).
#
#   release/build-extras.sh [OUTDIR] [TARGET]
#
# OUTDIR   default target/dist-extras; gets completions/{stems.bash,_stems,
#          stems.fish} and man/stems.1, man/stems-up.1, … (one per command).
# TARGET   a rust target triple the runner can execute (release.yml passes the
#          matrix target: every release target builds on a native runner), so
#          `dist build` afterwards reuses this build. Empty: the host.
# STEMS_BIN=<path> skips the build and uses that binary.
#
# cargo-dist packs OUTDIR's two folders into every tarball
# (dist-workspace.toml `include`); the Homebrew formula installs them.
set -eu

OUT=${1:-target/dist-extras}
TARGET=${2:-}
BIN=${STEMS_BIN:-}

if [ -z "$BIN" ]; then
    # Same profile as `dist build` ([profile.dist] inherits release), so dist
    # reuses this build instead of compiling again.
    if [ -n "$TARGET" ]; then
        cargo build --profile dist --locked -p stems-cli --target "$TARGET"
        BIN=target/$TARGET/dist/stems
    else
        cargo build --profile dist --locked -p stems-cli
        BIN=target/dist/stems
    fi
fi

rm -rf "$OUT/completions" "$OUT/man"
mkdir -p "$OUT/completions" "$OUT/man"
"$BIN" completions bash > "$OUT/completions/stems.bash"
"$BIN" completions zsh > "$OUT/completions/_stems"
"$BIN" completions fish > "$OUT/completions/stems.fish"
"$BIN" __man "$OUT/man" --json > /dev/null

for f in completions/stems.bash completions/_stems completions/stems.fish man/stems.1 man/stems-up.1; do
    if [ ! -s "$OUT/$f" ]; then
        echo "build-extras: $OUT/$f is missing or empty" >&2
        exit 1
    fi
done
echo "build-extras: $(ls "$OUT/man" | wc -l | tr -d ' ') man pages and 3 completion scripts in $OUT"
