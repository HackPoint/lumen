#!/usr/bin/env bash
set -e
TRIPLE="aarch64-apple-darwin"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cargo build -p lumen-daemon -p lumen-mcp -p lumen-cli --release \
      --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$ROOT/lumenator/src-tauri/binaries"

for bin in lumen-daemon lumen-mcp lumen-tok; do
    cp "$ROOT/target/release/$bin" \
       "$ROOT/lumenator/src-tauri/binaries/${bin}-${TRIPLE}"
    echo "sidecar ready: binaries/${bin}-${TRIPLE}"
done

# lumen CLI binary (produced by lumen-cli crate)
cp "$ROOT/target/release/lumen" \
   "$ROOT/lumenator/src-tauri/binaries/lumen-${TRIPLE}"
echo "sidecar ready: binaries/lumen-${TRIPLE}"