#!/usr/bin/env bash
# Build the lumen CLI tarball and compute SHA256 for the Homebrew formula.
# Run from the workspace root after bumping the version in crates/lumen-cli/Cargo.toml.
#
# Usage:
#   ./scripts/build-cli-release.sh
#
# Output:
#   dist/lumen-v<version>-aarch64-apple-darwin.tar.gz
#   dist/lumen-v<version>-aarch64-apple-darwin.tar.gz.sha256
#   (prints the formula snippet to paste into Formula/lumen.rb)

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TRIPLE="aarch64-apple-darwin"

VERSION=$(grep '^version' "$ROOT/crates/lumen-cli/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')
TARBALL="lumen-v${VERSION}-${TRIPLE}.tar.gz"
DIST="$ROOT/dist"

echo "Building lumen v${VERSION} for ${TRIPLE}..."
cargo build -p lumen-cli --release --manifest-path "$ROOT/Cargo.toml"

mkdir -p "$DIST"

# Create tarball from the binary only
STAGING=$(mktemp -d)
cp "$ROOT/target/release/lumen" "$STAGING/lumen"
tar -czf "$DIST/$TARBALL" -C "$STAGING" lumen
rm -rf "$STAGING"

SHA=$(shasum -a 256 "$DIST/$TARBALL" | awk '{print $1}')
echo "$SHA  $TARBALL" > "$DIST/$TARBALL.sha256"

echo ""
echo "Tarball: dist/$TARBALL"
echo "SHA256:  $SHA"
echo ""
echo "Paste into Formula/lumen.rb:"
echo "  url \"https://github.com/HackPoint/lumen/releases/download/v${VERSION}/${TARBALL}\""
echo "  sha256 \"${SHA}\""
