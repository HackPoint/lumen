#!/usr/bin/env bash
# release.sh — bump version across all files, generate changelog, commit + tag.
# The git push is intentionally manual; pushing the tag is what triggers CI.
#
# Usage: ./scripts/release.sh <patch|minor|major|X.Y.Z>

set -euo pipefail
cd "$(dirname "$0")/.."

# ── helpers ──────────────────────────────────────────────────────────────────

die() { echo "ERROR: $*" >&2; exit 1; }

require_cmd() { command -v "$1" &>/dev/null || die "'$1' not found — install it first"; }

semver_bump() {
    local current="$1" part="$2"
    local major minor patch
    IFS='.' read -r major minor patch <<< "${current%-*}"  # strip any prerelease suffix
    case "$part" in
        major) echo "$((major+1)).0.0" ;;
        minor) echo "${major}.$((minor+1)).0" ;;
        patch) echo "${major}.${minor}.$((patch+1))" ;;
        # explicit X.Y.Z
        [0-9]*) [[ "$part" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]] || \
                    die "invalid version: $part"; echo "$part" ;;
        *)      die "usage: $0 <patch|minor|major|X.Y.Z>" ;;
    esac
}

update_toml_version() {
    # Replaces only the first occurrence of 'version = "..."' (the [package] version)
    local file="$1" new="$2"
    python3 - "$file" "$new" << 'PY'
import sys, re
path, new = sys.argv[1], sys.argv[2]
txt = open(path).read()
# Replace only the first ^version = "..." line
patched = re.sub(r'^(version\s*=\s*)"[^"]+"', f'\\g<1>"{new}"', txt, count=1, flags=re.M)
open(path, 'w').write(patched)
PY
}

update_json_version() {
    python3 - "$1" "$2" << 'PY'
import sys, json
path, new = sys.argv[1], sys.argv[2]
with open(path) as f: d = json.load(f)
d['version'] = new
with open(path, 'w') as f: json.dump(d, f, indent=2, ensure_ascii=False); f.write('\n')
PY
}

update_ruby_version() {
    local file="$1" new="$2"
    sed -i '' "s/^  version \"[^\"]*\"/  version \"${new}\"/" "$file"
}

# ── pre-flight ────────────────────────────────────────────────────────────────

require_cmd git
require_cmd cargo
require_cmd python3

[[ $# -eq 1 ]] || die "usage: $0 <patch|minor|major|X.Y.Z>"

# Must be on main with a clean tree
current_branch=$(git rev-parse --abbrev-ref HEAD)
[[ "$current_branch" == "main" ]] || die "not on main (current: $current_branch)"

git diff --quiet && git diff --cached --quiet || die "working tree has uncommitted changes"

# Detect current version from tauri.conf.json (single source of truth for display)
CURRENT=$(python3 -c "import json; print(json.load(open('lumenator/src-tauri/tauri.conf.json'))['version'])")
NEW=$(semver_bump "$CURRENT" "$1")
TAG="v${NEW}"

echo "Bumping $CURRENT → $NEW (tag $TAG)"

# Tag must not already exist
git tag | grep -qx "$TAG" && die "tag $TAG already exists"

# ── update all version files ─────────────────────────────────────────────────

echo ""
echo "Updating version files…"

CARGO_TOML_FILES=(
    crates/lumen-core/Cargo.toml
    crates/lumen-cli/Cargo.toml
    crates/lumen-daemon/Cargo.toml
    crates/lumen-mcp/Cargo.toml
    lumenator/src-tauri/Cargo.toml
)

for f in "${CARGO_TOML_FILES[@]}"; do
    update_toml_version "$f" "$NEW"
    echo "  ✓ $f"
done

update_json_version lumenator/package.json "$NEW"
echo "  ✓ lumenator/package.json"

update_json_version lumenator/src-tauri/tauri.conf.json "$NEW"
echo "  ✓ lumenator/src-tauri/tauri.conf.json"

# Brew files: bump version only; CI will update sha256 after building artifacts.
# Leaving sha256 stale is intentional — it's overwritten by the CI tap-update job
# before anyone runs 'brew install'.
update_ruby_version Formula/lumen-cli.rb "$NEW"
echo "  ✓ Formula/lumen-cli.rb  (sha256 updated by CI after build)"

update_ruby_version Casks/lumen.rb "$NEW"
echo "  ✓ Casks/lumen.rb  (sha256 updated by CI after build)"

# Regenerate Cargo.lock so the workspace stays consistent
echo ""
echo "Regenerating Cargo.lock…"
cargo generate-lockfile --quiet
echo "  ✓ Cargo.lock"

# ── changelog ─────────────────────────────────────────────────────────────────

echo ""
echo "Generating CHANGELOG entry for $TAG…"

LAST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
if [[ -n "$LAST_TAG" ]]; then
    LOG_RANGE="${LAST_TAG}..HEAD"
    SINCE_MSG="since $LAST_TAG"
else
    LOG_RANGE="HEAD"
    SINCE_MSG="(all commits — no prior tag)"
fi

FEATS=$(git log --format='%s' "$LOG_RANGE" 2>/dev/null | grep -E '^feat(\(|!|:)' || true)
FIXES=$(git log --format='%s' "$LOG_RANGE" 2>/dev/null | grep -E '^fix(\(|!|:)'  || true)
CHORES=$(git log --format='%s' "$LOG_RANGE" 2>/dev/null | grep -E '^chore(\(|!|:)' | grep -v 'release' || true)
OTHERS=$(git log --format='%s' "$LOG_RANGE" 2>/dev/null | grep -Ev '^(feat|fix|chore|docs|style|refactor|test|ci|build)(\(|!|:)' || true)

ENTRY="## [$NEW] — $(date +%Y-%m-%d)\n"
[[ -n "$FEATS"  ]] && ENTRY+="\n### Features\n$(echo "$FEATS"  | sed 's/^/- /')\n"
[[ -n "$FIXES"  ]] && ENTRY+="\n### Fixes\n$(echo "$FIXES"    | sed 's/^/- /')\n"
[[ -n "$CHORES" ]] && ENTRY+="\n### Maintenance\n$(echo "$CHORES" | sed 's/^/- /')\n"
[[ -n "$OTHERS" ]] && ENTRY+="\n### Other\n$(echo "$OTHERS"   | sed 's/^/- /')\n"

echo ""
echo "─────────────────────────── CHANGELOG PREVIEW ───────────────────────────"
printf "$ENTRY"
echo "──────────────────────────────────────────────────────────────────────────"

if [[ -f CHANGELOG.md ]]; then
    # Prepend new entry after the first line (the # Changelog header)
    TMP=$(mktemp)
    head -1 CHANGELOG.md > "$TMP"
    printf "\n$ENTRY\n" >> "$TMP"
    tail -n +2 CHANGELOG.md >> "$TMP"
    mv "$TMP" CHANGELOG.md
else
    printf "# Changelog\n\n$ENTRY\n" > CHANGELOG.md
fi
echo "  ✓ CHANGELOG.md"

# ── confirm ───────────────────────────────────────────────────────────────────

echo ""
printf "Proceed? Commit chore(release): %s and tag %s. [y/N] " "$TAG" "$TAG"
read -r answer
[[ "${answer,,}" == "y" ]] || { echo "Aborted — no commit made."; exit 0; }

# ── commit + tag ──────────────────────────────────────────────────────────────

git add \
    crates/lumen-core/Cargo.toml \
    crates/lumen-cli/Cargo.toml \
    crates/lumen-daemon/Cargo.toml \
    crates/lumen-mcp/Cargo.toml \
    lumenator/src-tauri/Cargo.toml \
    lumenator/package.json \
    lumenator/src-tauri/tauri.conf.json \
    Formula/lumen-cli.rb \
    Casks/lumen.rb \
    Cargo.lock \
    CHANGELOG.md

git commit -m "chore(release): ${TAG}"
git tag -a "$TAG" -m "Release ${TAG}"

echo ""
echo "Done. To trigger the CI release pipeline, run:"
echo ""
echo "  git push origin main --tags"
echo ""
echo "That push of tag $TAG triggers .github/workflows/release.yml."
echo "CI will build artifacts, create the GitHub Release, and update the Homebrew tap."
echo ""
echo "Test tip: use a prerelease tag first (e.g. v${NEW}-rc.1) to smoke-test CI"
echo "before tagging a real release."
