#!/usr/bin/env bash
# Build the four sidecar binaries Tauri bundles into the app.
#
# Tauri resolves externalBin entries by appending the target triple, so each
# binary must land at binaries/<name>-<triple>. The triple is derived from the
# toolchain rather than hard-coded: the previous version pinned
# aarch64-apple-darwin, so on any other host it produced files Tauri could not
# find and the build failed on a missing sidecar.
#
# Usage:
#   ./build-sidecar.sh                                   # host triple
#   TRIPLE=x86_64-unknown-linux-gnu ./build-sidecar.sh   # explicit override
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# `rustc -vV` reports the host triple; honour an override for cross builds.
TRIPLE="${TRIPLE:-$(rustc -vV | sed -n 's/^host: //p')}"
if [[ -z "$TRIPLE" ]]; then
    echo "error: could not determine the target triple (is rustc on PATH?)" >&2
    exit 1
fi
echo "building sidecars for ${TRIPLE}"

cargo build -p lumen-daemon -p lumen-mcp -p lumen-cli --release \
      --manifest-path "$ROOT/Cargo.toml"

BIN_DIR="$ROOT/lumenator/src-tauri/binaries"
mkdir -p "$BIN_DIR"

# Windows binaries carry .exe on both sides of the copy.
EXE=""
case "$TRIPLE" in
    *windows*) EXE=".exe" ;;
esac

# lumen-tok is a second binary of the lumen-mcp crate; lumen comes from lumen-cli.
for bin in lumen-daemon lumen-mcp lumen-tok lumen; do
    src="$ROOT/target/release/${bin}${EXE}"
    if [[ ! -f "$src" ]]; then
        echo "error: expected ${src} after the build — did a crate fail to produce it?" >&2
        exit 1
    fi
    cp "$src" "$BIN_DIR/${bin}-${TRIPLE}${EXE}"
    echo "sidecar ready: binaries/${bin}-${TRIPLE}${EXE}"
done
