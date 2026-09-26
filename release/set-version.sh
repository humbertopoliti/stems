#!/bin/sh
# Set the workspace version (and the internal crates' version requirements)
# in the root Cargo.toml, then refresh Cargo.lock. cargo-dist only releases a
# tag whose version equals the package version, so `v0.1.0-rc.1` needs
# `release/set-version.sh 0.1.0-rc.1` first (deliverable 32, RELEASING.md).
set -eu
new=${1:?usage: release/set-version.sh X.Y.Z[-pre]}
cd "$(dirname "$0")/.."
old=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
[ -n "$old" ] || { echo "set-version: no [workspace.package] version found" >&2; exit 1; }
sed -i.bak \
    -e "s/^version = \"$old\"\$/version = \"$new\"/" \
    -e "s/\(path = \"crates\/[a-z-]*\", version = \)\"$old\"/\1\"$new\"/" \
    Cargo.toml
rm -f Cargo.toml.bak
cargo update --workspace --offline >/dev/null 2>&1 || cargo update --workspace >/dev/null
echo "set-version: $old -> $new"
