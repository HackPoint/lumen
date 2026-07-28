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
#   ./build-sidecar.sh                                 # host triple
#   TRIPLE=x86_64-unknown-linux-gnu ./build-sidecar.sh # must match the host
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

HOST="$(rustc -vV | sed -n 's/^host: //p')"
if [[ -z "$HOST" ]]; then
    echo "error: could not determine the host target triple (is rustc on PATH?)" >&2
    exit 1
fi

# TRIPLE only names the output files; `cargo build` below has no --target, so the
# binaries are always built for the host. A TRIPLE that disagrees with the host
# therefore labels host binaries as some other platform's, which is how the
# release pipeline came to put Linux binaries in binaries/*-aarch64-apple-darwin:
# the workflow sets a global TRIPLE for the macOS job, and the Linux job silently
# inherited it. Refuse the mismatch rather than producing mislabeled artifacts.
TRIPLE="${TRIPLE:-$HOST}"
if [[ "$TRIPLE" != "$HOST" ]]; then
    echo "error: TRIPLE=${TRIPLE} does not match the host (${HOST})." >&2
    echo "       This script builds host binaries and only names them after" >&2
    echo "       TRIPLE, so a mismatch would mislabel them. Either unset TRIPLE" >&2
    echo "       or run this on a ${TRIPLE} host." >&2
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

# Pairs of "built binary:sidecar name". They differ for the CLI: it is built as
# `lumen`, but the app's own executable is `Lumen`, and macOS filesystems are
# case-insensitive — so staging the CLI as `lumen` put both at the same path
# inside Contents/MacOS/ and the GUI silently overwrote the CLI. Staging it as
# `lumen-cli` keeps them distinct; the command users type is still `lumen`,
# because that name comes from the symlink Setup creates, not from the bundle.
#
# lumen-tok is a second binary of the lumen-mcp crate; lumen comes from lumen-cli.
for pair in lumen-daemon:lumen-daemon lumen-mcp:lumen-mcp lumen-tok:lumen-tok lumen:lumen-cli; do
    built="${pair%%:*}"
    staged="${pair##*:}"
    src="$ROOT/target/release/${built}${EXE}"
    if [[ ! -f "$src" ]]; then
        echo "error: expected ${src} after the build — did a crate fail to produce it?" >&2
        exit 1
    fi
    dst="$BIN_DIR/${staged}-${TRIPLE}${EXE}"
    cp "$src" "$dst"
    echo "sidecar ready: binaries/${staged}-${TRIPLE}${EXE}"
done
