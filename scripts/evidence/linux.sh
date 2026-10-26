#!/usr/bin/env bash
# A published release's evidence on Ubuntu, installed the way users install it: the .deb, the
# AppImage and the CLI tarball from the GitHub release. Keeps the app's tabs (through WebDriver,
# lumenator/e2e/evidence.mjs), the CLI in bash in an xterm (PNG and raw text), and for an upgrade
# the all-time savings before and after, which must match.
#
# Environment: VERSION, SCENARIO (fresh | from-1.9.0 | from-1.8.0), OUT, HELPERS (the app crate's
# test binary), DISPLAY (an X server, Xvfb on a runner). Installs packages and writes the real
# profile: GitHub-hosted runners only.
set -uo pipefail
: "${VERSION:?}" "${SCENARIO:?}" "${OUT:?}" "${DISPLAY:?}"
if [ "${GITHUB_ACTIONS:-}" != true ] || [ "${RUNNER_ENVIRONMENT:-}" != github-hosted ]; then
  echo "refusing: this installs packages and writes the real profile; GitHub-hosted runners only" >&2
  exit 2
fi
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT"
LABEL="linux-$SCENARIO"
NOTES="$OUT/notes-$LABEL.txt"
FAILED=0
note() { printf '%s\n' "$*" | tee -a "$NOTES"; }
cannot() { note "CANNOT SHOW: $*"; }
fail() { note "FAIL: $*"; FAILED=1; }
WORK=$(mktemp -d)
helper() {
  "${HELPERS:?}" --ignored --exact "setup::hook_e2e::upgrade_ci::$1" --nocapture >>"$OUT/helpers-$LABEL.txt" 2>&1
}

# ── What was published ─────────────────────────────────────────────────────────────────────
if ! gh release download "v$VERSION" -R HackPoint/lumen -D "$WORK" \
     -p "lumen_${VERSION}_amd64.deb" -p "Lumen-${VERSION}-x86_64.AppImage" \
     -p "lumen-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz" >>"$NOTES" 2>&1; then
  fail "the v$VERSION assets could not be downloaded"
  exit 1
fi
mkdir -p "$WORK/cli"
tar -xzf "$WORK/lumen-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz" -C "$WORK/cli"
LUMEN=$(find "$WORK/cli" -type f -name lumen | head -n1)
[ -x "$LUMEN" ] || fail "no lumen binary in the CLI tarball"
CLI_DIR=$(dirname "$LUMEN")
export PATH="$CLI_DIR:$PATH"

# ── 1. The previous install, for an upgrade, with history in it ────────────────────────────
PREV_DB="$HOME/.local/share/example.previous.app/lumen.db"
if [ "$SCENARIO" != fresh ]; then
  export LUMEN_UPGRADE_FROM="${SCENARIO#from-}" LUMEN_UPGRADE_LOGIN=off
  if helper seed; then note "seeded a previous install from $LUMEN_UPGRADE_FROM (simulated: its binaries are no longer published)"; else fail "seeding the previous install"; fi
  LUMEN_DB="$PREV_DB" lumen report --json >"$OUT/before-report-$LABEL.json" 2>>"$NOTES"
  LUMEN_DB="$PREV_DB" lumen report >"$OUT/before-report-$LABEL.txt" 2>&1
fi

# ── 2. The .deb, then its first start ──────────────────────────────────────────────────────
# The notes file is the runner user's: only dpkg runs as root, its output goes through tee.
if ! sudo dpkg -i "$WORK/lumen_${VERSION}_amd64.deb" 2>&1 | tee -a "$NOTES" >/dev/null; then fail "dpkg -i the .deb"; fi
APP=$(dpkg -L lumen | grep -E '^/usr/bin/' | grep -v -E 'lumen-(daemon|mcp|tok|cli)$' | head -n1)
note "installed: $APP ($(dpkg-query -W -f '${Package} ${Version}' lumen))"
GOT=$(lumen --version 2>&1)
note "lumen --version: $GOT"
case "$GOT" in *"$VERSION"*) ;; *) fail "the CLI tarball holds '$GOT', not $VERSION" ;; esac
"$APP" >"$OUT/app-first-start-$LABEL.log" 2>&1 &
APID=$!
if [ "$SCENARIO" != fresh ]; then
  if helper wait_for_the_move; then note "the start moved the previous install, and Setup ran"; else fail "the start did not move the previous install"; fi
else
  sleep 15
fi
kill "$APID" >/dev/null 2>&1
pkill -f lumen-daemon >/dev/null 2>&1
sleep 3

# ── 3. All-time savings after ───────────────────────────────────────────────────────────────
lumen report --json >"$OUT/after-report-$LABEL.json" 2>>"$NOTES"
lumen report >"$OUT/after-report-$LABEL.txt" 2>&1
if [ "$SCENARIO" != fresh ]; then
  if python3 - "$OUT/before-report-$LABEL.json" "$OUT/after-report-$LABEL.json" >>"$NOTES" <<'PY'
import json, sys
b, a = (json.load(open(p))["allTime"] for p in sys.argv[1:3])
keys = lambda x: {"tokensSaved": x["tokensSaved"], "tokensFull": x["tokensFull"], "sharePct": x["sharePct"], "net": x["measured"]["net"]}
print("all-time before:", json.dumps(keys(b)))
print("all-time after: ", json.dumps(keys(a)))
sys.exit(0 if keys(b) == keys(a) else 1)
PY
  then note "all-time savings match before and after the upgrade"; else fail "all-time savings differ before and after the upgrade"; fi
fi

# ── 4. The app's tabs, from the .deb, and the AppImage ─────────────────────────────────────
if ! node "$ROOT/lumenator/e2e/evidence.mjs" "$APP" "$OUT" "$LABEL" >>"$NOTES" 2>&1; then
  fail "the app's tabs could not be captured (the .deb)"
fi
if [ "$SCENARIO" = fresh ]; then
  chmod +x "$WORK/Lumen-${VERSION}-x86_64.AppImage"
  export APPIMAGE_EXTRACT_AND_RUN=1
  if node "$ROOT/lumenator/e2e/evidence.mjs" "$WORK/Lumen-${VERSION}-x86_64.AppImage" "$OUT" "linux-appimage" >>"$NOTES" 2>&1; then
    note "the AppImage starts and shows its tabs (*-linux-appimage.png)"
  else
    fail "the AppImage's tabs could not be captured"
  fi
fi
cannot "the tray menu: Xvfb has no system tray to hold Lumen's icon"

# ── 5. The CLI, in bash in an xterm, and as raw text ────────────────────────────────────────
lumen --version >"$OUT/cli-version-$LABEL.txt" 2>&1
lumen >"$OUT/cli-line-$LABEL.txt" 2>&1
lumen report >"$OUT/cli-report-$LABEL.txt" 2>&1
lumen doctor >"$OUT/cli-doctor-$LABEL.txt" 2>&1
cat >"$WORK/cli.sh" <<'SH'
#!/bin/bash
for c in "lumen --version" "lumen" "lumen report" "lumen doctor"; do printf '$ %s\n' "$c"; $c; echo; done
sleep 60
SH
chmod +x "$WORK/cli.sh"
xterm -geometry 160x75+0+0 -fa Monospace -fs 9 -e "$WORK/cli.sh" &
XPID=$!
sleep 8
if import -window root "$OUT/cli-bash-$LABEL.png" >>"$NOTES" 2>&1; then
  note "the CLI in bash: cli-bash-$LABEL.png (a long report scrolls; the full output is in cli-*.txt)"
else
  cannot "the CLI in bash as a picture (import failed); the raw text is in cli-*.txt"
fi
kill "$XPID" >/dev/null 2>&1

note "done: $LABEL, failures: $FAILED"
exit "$FAILED"
