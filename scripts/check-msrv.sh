#!/usr/bin/env bash
# The compiler is the declared rust-version, and every crate declares it.
#
# CI's msrv job installs the toolchain named in the workspace Cargo.toml and runs this
# before it builds, so a build on some other compiler cannot pass for an MSRV build, and a
# crate added without `rust-version.workspace = true` fails here by name.
#
# Usage:
#   RUSTUP_TOOLCHAIN=1.94.0 bash scripts/check-msrv.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

want=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml")
if [[ -z "$want" ]]; then
    echo "error: no rust-version in $ROOT/Cargo.toml" >&2
    exit 1
fi

have=$(rustc --version | awk '{print $2}')
if [[ "$have" != "$want" ]]; then
    echo "error: rustc is $have, the declared rust-version is $want" >&2
    exit 1
fi

off=$(cargo metadata --no-deps --format-version 1 --locked --manifest-path "$ROOT/Cargo.toml" |
    jq -r --arg want "$want" \
        '.packages[] | select(.rust_version != $want) | "  \(.name): \(.rust_version // "none")"')
if [[ -n "$off" ]]; then
    echo "error: crates not on rust-version $want:" >&2
    echo "$off" >&2
    exit 1
fi

echo "rustc $have; every crate declares rust-version $want"
