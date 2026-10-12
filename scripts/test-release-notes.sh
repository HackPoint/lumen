#!/usr/bin/env bash
# Tests for scripts/release-notes.sh, and for the places that must not bring back release
# notes from anywhere but CHANGELOG.md.  Run: bash scripts/test-release-notes.sh
set -u

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
SCRIPT="$ROOT/scripts/release-notes.sh"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

failed=0
pass() { printf 'ok   - %s\n' "$1"; }
fail() {
  printf 'FAIL - %s\n' "$1"
  [ -n "${2:-}" ] && printf '         %s\n' "$2"
  failed=$((failed + 1))
}

run() {
  bash "$SCRIPT" "$@" >"$WORK/out" 2>"$WORK/err"
  status=$?
}

printed() {
  if [ "$status" -eq 0 ] && cmp -s "$WORK/out" "$WORK/want"; then
    pass "$1"
  else
    fail "$1" "exit $status; stderr: $(head -c 300 "$WORK/err")"
  fi
}

refused() {
  if [ "$status" -eq "$2" ] && [ ! -s "$WORK/out" ] && grep -qF -- "$3" "$WORK/err"; then
    pass "$1"
  else
    fail "$1" "exit $status (want $2), $(wc -c <"$WORK/out" | tr -d ' ') bytes on stdout; stderr: $(head -c 300 "$WORK/err")"
  fi
}

cat >"$WORK/changelog.md" <<'EOF'
# Changelog

## [Unreleased]

### Not shipped yet

Pending text.

## [1.8.0] — 2026-10-12

### Repeat reads are on by default

Text of 1.8.0, first paragraph.

- a bullet
- another bullet

## [1.8.0-rc.1] — 2026-10-10

Text of the candidate.

## [1.7.0] — 2026-10-11

Text of 1.7.0.
EOF

# ── extraction ────────────────────────────────────────────────────────────────

cat >"$WORK/want" <<'EOF'
### Repeat reads are on by default

Text of 1.8.0, first paragraph.

- a bullet
- another bullet
EOF
run 1.8.0 "$WORK/changelog.md"
printed "a block is printed without its header, the next block or the blank lines around it"

printf 'Text of the candidate.\n' >"$WORK/want"
run 1.8.0-rc.1 "$WORK/changelog.md"
printed "a prerelease prints its own block, not the release's"

printf 'Text of 1.7.0.\n' >"$WORK/want"
run 1.7.0 "$WORK/changelog.md"
printed "the last block runs to the end of the file"

run 1.9.0 "$WORK/changelog.md"
refused "a version with no block fails, prints nothing and names the version" 1 'no "## [1.9.0]" block'

cat >"$WORK/only-release.md" <<'EOF'
# Changelog

## [1.8.0] — 2026-10-12

Text of 1.8.0.
EOF
run 1.8.0-rc.1 "$WORK/only-release.md"
refused "a prerelease does not fall back to the release's block" 1 'no "## [1.8.0-rc.1]" block'

cat >"$WORK/lookalikes.md" <<'EOF'
# Changelog

## [1x8y0] — 2026-10-12

Matches 1.8.0 if the version is read as a pattern.

## [1.8.01] — 2026-10-12

Starts with 1.8.0.

## [11.8.0] — 2026-10-12

Ends with 1.8.0.
EOF
run 1.8.0 "$WORK/lookalikes.md"
refused "the version is matched literally, not as a pattern or a prefix" 1 'no "## [1.8.0]" block'

run Unreleased "$WORK/changelog.md"
refused "the Unreleased block is never a release's notes" 2 'not a version'

run "" "$WORK/changelog.md"
refused "an empty version is refused" 2 'not a version'

run
refused "no arguments is a usage error" 2 'usage:'

printf '# Changelog\n\n## [1.8.0] — 2026-10-12\n\n## [1.7.0] — 2026-10-11\n\nText.\n' >"$WORK/empty.md"
run 1.8.0 "$WORK/empty.md"
refused "an empty block fails" 1 'is empty'

printf '# Changelog\n\n## [1.8.0] — 2026-10-12\n\n  \n\t\n\r\n## [1.7.0] — 2026-10-11\n\nText.\n' >"$WORK/blank.md"
run 1.8.0 "$WORK/blank.md"
refused "a block of only blank lines fails" 1 'is empty'

printf '# Changelog\n\n## [1.8.0] — 2026-10-12\n\n' >"$WORK/header-only.md"
run 1.8.0 "$WORK/header-only.md"
refused "a header at the very end of the file fails" 1 'is empty'

printf '# Changelog\n\n## [1.8.0] — 2026-10-12\n\nNo newline at the end' >"$WORK/no-eol.md"
printf 'No newline at the end\n' >"$WORK/want"
run 1.8.0 "$WORK/no-eol.md"
printed "a last line without a newline is kept"

awk '{ printf "%s\r\n", $0 }' "$WORK/changelog.md" >"$WORK/crlf.md"
run 1.8.0 "$WORK/crlf.md"
tr -d '\r' <"$WORK/out" >"$WORK/out-lf"
cat >"$WORK/want" <<'EOF'
### Repeat reads are on by default

Text of 1.8.0, first paragraph.

- a bullet
- another bullet
EOF
if [ "$status" -eq 0 ] && cmp -s "$WORK/out-lf" "$WORK/want"; then
  pass "a CRLF changelog is read the same way"
else
  fail "a CRLF changelog is read the same way" "exit $status; stderr: $(head -c 300 "$WORK/err")"
fi

run 1.8.0 "$WORK/not-there.md"
refused "a changelog that is not there fails" 1 'cannot read'

newest=$(grep -m1 -E '^## \[[0-9]+\.[0-9]+\.[0-9]+' "$ROOT/CHANGELOG.md" | sed -E 's/^## \[([^]]+)\].*/\1/')
run "$newest" "$ROOT/CHANGELOG.md"
if [ "$status" -eq 0 ] && [ -s "$WORK/out" ] && ! grep -q '^## \[' "$WORK/out" &&
  grep -q '[^[:space:]]' "$WORK/out"; then
  pass "the repository's newest release block ($newest) parses"
else
  fail "the repository's newest release block ($newest) parses" "exit $status; stderr: $(head -c 300 "$WORK/err")"
fi

# ── nothing else may supply a release's notes ─────────────────────────────────

if [ ! -e "$ROOT/RELEASE_NOTES.md" ]; then
  pass "RELEASE_NOTES.md does not exist"
else
  fail "RELEASE_NOTES.md does not exist" "it holds the text of an old release; delete it"
fi

workflow="$ROOT/.github/workflows/release.yml"
if ! grep -nE 'RELEASE_NOTES|generate-notes' "$workflow" >"$WORK/hits"; then
  pass "the release workflow names no other source of notes"
else
  fail "the release workflow names no other source of notes" "$(head -c 300 "$WORK/hits")"
fi

job_block() {
  awk -v j="$1" '$0 == "  " j ":" { on = 1; next } on && /^  [a-z0-9-]+:$/ { exit } on' "$workflow"
}

gate='bash scripts/release-notes.sh "$VERSION" CHANGELOG.md > /dev/null'
if job_block resolve | grep -qF -- "$gate"; then
  pass "the resolve job refuses a tag with no CHANGELOG block before anything is built"
else
  fail "the resolve job refuses a tag with no CHANGELOG block before anything is built" \
    "the resolve job must run: $gate"
fi

publish='bash scripts/release-notes.sh "$VERSION" CHANGELOG.md > /tmp/release_notes.md'
if job_block build | grep -qF -- "$publish" &&
  job_block build | grep -qF -- '--notes-file /tmp/release_notes.md'; then
  pass "the release is published with the CHANGELOG block as its notes"
else
  fail "the release is published with the CHANGELOG block as its notes" \
    "the build job must run: $publish, and pass --notes-file /tmp/release_notes.md to gh"
fi

if [ "$failed" -ne 0 ]; then
  printf '\n%s check(s) failed\n' "$failed"
  exit 1
fi
printf '\nall checks passed\n'
