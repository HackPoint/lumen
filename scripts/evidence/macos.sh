#!/usr/bin/env bash
# A published release's evidence on macOS, installed the way users install it: the lumen-cli
# formula and the lumen-app cask from the Homebrew tap. Keeps the app's tabs, the menu-bar menu,
# the CLI in Terminal (PNG and raw text), Gatekeeper's verdict, and for an upgrade the all-time
# savings before and after, which must match.
#
# Environment: VERSION (e.g. 1.10.0), SCENARIO (fresh | from-1.9.0 | from-1.8.0), OUT (a folder),
# HELPERS (the app crate's test binary, for the simulated previous install and the wait).
# It installs into /Applications and the real profile: GitHub-hosted runners only.
set -uo pipefail
: "${VERSION:?}" "${SCENARIO:?}" "${OUT:?}"
if [ "${GITHUB_ACTIONS:-}" != true ] || [ "${RUNNER_ENVIRONMENT:-}" != github-hosted ]; then
  echo "refusing: this installs into /Applications and the real profile; GitHub-hosted runners only" >&2
  exit 2
fi
mkdir -p "$OUT"
LABEL="macos-$SCENARIO"
NOTES="$OUT/notes-$LABEL.txt"
FAILED=0
note() { printf '%s\n' "$*" | tee -a "$NOTES"; }
cannot() { note "CANNOT SHOW: $*"; }
fail() { note "FAIL: $*"; FAILED=1; }
WORK=$(mktemp -d)
helper() {
  "${HELPERS:?}" --ignored --exact "setup::hook_e2e::upgrade_ci::$1" --nocapture >>"$OUT/helpers-$LABEL.txt" 2>&1
}

# ── Accessibility and window helpers (as in scripts/issue5-launch-check.sh) ─────────────────
cat >"$WORK/windows.js" <<'JXA'
ObjC.import('CoreGraphics');
function run(argv) {
  const all = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo(0, 0)));
  return all
    .filter(w => w.kCGWindowOwnerPID === Number(argv[0]) && w.kCGWindowLayer === 0 && w.kCGWindowIsOnscreen)
    .map(w => `${w.kCGWindowNumber} ${w.kCGWindowBounds.Width * w.kCGWindowBounds.Height}`)
    .join('\n');
}
JXA
cat >"$WORK/press.js" <<'JXA'
function run(argv) {
  const se = Application('System Events');
  const procs = se.processes.whose({ unixId: Number(argv[0]) });
  if (procs.length === 0) throw new Error(`no process ${argv[0]}`);
  const attr = (el, name) => { try { return String(el[name]() || ''); } catch (e) { return ''; } };
  const seen = [];
  for (const w of procs[0].windows()) {
    for (const el of w.entireContents()) {
      const role = attr(el, 'role');
      if (!['AXRadioButton', 'AXTab', 'AXLink', 'AXButton'].includes(role)) continue;
      const label = ['title', 'description', 'name', 'value'].map(a => attr(el, a)).find(Boolean) || '';
      seen.push(`${role} "${label}"`);
      if (label.trim().startsWith(argv[1])) { el.actions.byName('AXPress').perform(); return `pressed ${label}`; }
    }
  }
  throw new Error(`no control named ${argv[1]} among: ${seen.join(', ')}`);
}
JXA
cat >"$WORK/texts.js" <<'JXA'
function run(argv) {
  const se = Application('System Events');
  const procs = se.processes.whose({ unixId: Number(argv[0]) });
  if (procs.length === 0) throw new Error(`no process ${argv[0]}`);
  const out = [];
  for (const w of procs[0].windows()) {
    for (const el of w.entireContents()) {
      try { if (el.role() === 'AXStaticText' && el.value()) out.push(el.value()); } catch (e) { /* gone */ }
    }
  }
  return out.join('\n');
}
JXA
cat >"$WORK/menu.js" <<'JXA'
function run(argv) {
  const se = Application('System Events');
  const p = se.processes.whose({ unixId: Number(argv[0]) })[0];
  const item = p.menuBars[1].menuBarItems[0];
  item.click();
  delay(1.5);
  return item.menus[0].menuItems().map(m => { try { return m.name() || '———'; } catch (e) { return '?'; } }).join('\n');
}
JXA
largest_window() { osascript -l JavaScript "$WORK/windows.js" "$1" | sort -k2 -n | tail -n1 | cut -d' ' -f1; }

# ── 1. The previous install, for an upgrade, with history in it ─────────────────────────────
PREV_DB="$HOME/Library/Application Support/example.previous.app/lumen.db"
if [ "$SCENARIO" != fresh ]; then
  export LUMEN_UPGRADE_FROM="${SCENARIO#from-}" LUMEN_UPGRADE_LOGIN=off
  if helper seed; then note "seeded a previous install from $LUMEN_UPGRADE_FROM (simulated: its binaries are no longer published)"; else fail "seeding the previous install"; fi
fi

# ── 2. The CLI, as users install it ─────────────────────────────────────────────────────────
brew tap hackpoint/tap >>"$OUT/brew-$LABEL.txt" 2>&1
if ! brew install hackpoint/tap/lumen-cli >>"$OUT/brew-$LABEL.txt" 2>&1; then fail "brew install lumen-cli"; fi
GOT=$(lumen --version 2>&1)
note "lumen --version: $GOT"
case "$GOT" in *"$VERSION"*) ;; *) fail "the formula installed '$GOT', not $VERSION" ;; esac
if [ "$SCENARIO" != fresh ]; then
  LUMEN_DB="$PREV_DB" lumen report --json >"$OUT/before-report-$LABEL.json" 2>>"$NOTES"
  LUMEN_DB="$PREV_DB" lumen report >"$OUT/before-report-$LABEL.txt" 2>&1
fi

# ── 3. The app, as users install it: the cask, whose postflight launches it ────────────────
# Over a previous app, as `brew upgrade` replaces it: the stand-in was not installed by Homebrew,
# so --force is what lets the cask take its place.
FORCE=""
[ "$SCENARIO" != fresh ] && FORCE="--force"
# shellcheck disable=SC2086
if ! brew install --cask $FORCE hackpoint/tap/lumen-app >>"$OUT/brew-$LABEL.txt" 2>&1; then fail "brew install --cask lumen-app"; fi
spctl --assess --type execute -vv /Applications/Lumen.app >"$OUT/gatekeeper-cask-$LABEL.txt" 2>&1
note "Gatekeeper, cask install (the cask clears the quarantine): $(head -n1 "$OUT/gatekeeper-cask-$LABEL.txt")"
# The DMG as a browser downloads it: quarantined, which is what Gatekeeper judges on first launch.
if gh release download "v$VERSION" -R HackPoint/lumen -p "Lumen_${VERSION}_aarch64.dmg" -D "$WORK" --clobber >>"$NOTES" 2>&1 \
   && hdiutil attach -nobrowse -readonly -mountpoint "$WORK/mnt" "$WORK/Lumen_${VERSION}_aarch64.dmg" >/dev/null; then
  cp -R "$WORK/mnt/Lumen.app" "$WORK/Lumen-from-dmg.app"
  hdiutil detach "$WORK/mnt" >/dev/null
  xattr -w com.apple.quarantine "0081;$(printf %x "$(date +%s)");Safari;" "$WORK/Lumen-from-dmg.app"
  spctl --assess --type execute -vv "$WORK/Lumen-from-dmg.app" >"$OUT/gatekeeper-dmg-$LABEL.txt" 2>&1
  note "Gatekeeper, DMG downloaded by a browser: $(head -n1 "$OUT/gatekeeper-dmg-$LABEL.txt")"
else
  fail "the DMG of v$VERSION could not be downloaded or mounted"
fi
cannot "Gatekeeper's first-launch dialog itself: a runner has no one to click it; its verdict is in gatekeeper-*.txt"

# ── 4. The start: an upgrade moves the previous install first ───────────────────────────────
for _ in $(seq 1 30); do pgrep -f '/Applications/Lumen.app/Contents/MacOS/Lumen$' >/dev/null && break; sleep 1; done
PID=$(pgrep -f '/Applications/Lumen.app/Contents/MacOS/Lumen$' | head -n1)
[ -n "$PID" ] || fail "the app did not start after the cask install"
if [ "$SCENARIO" != fresh ]; then
  if helper wait_for_the_move; then note "the start moved the previous install"; else fail "the start did not move the previous install"; fi
fi
sleep 10

# ── 5. All-time savings after, from lumen report ────────────────────────────────────────────
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

# ── 6. The app's tabs ───────────────────────────────────────────────────────────────────────
lumen show >>"$NOTES" 2>&1
sleep 3
WID=""
for _ in $(seq 1 20); do WID=$(largest_window "$PID"); [ -n "$WID" ] && break; sleep 0.5; done
if [ -z "$WID" ]; then
  fail "lumen show did not bring the main window on screen"
else
  sleep 3
  screencapture -x -o -l "$WID" "$OUT/home-$LABEL.png"
  osascript -l JavaScript "$WORK/texts.js" "$PID" >"$OUT/home-text-$LABEL.txt" 2>&1
  for TAB in Optimizer Details Settings; do
    if osascript -l JavaScript "$WORK/press.js" "$PID" "$TAB" >>"$NOTES" 2>&1; then
      sleep 2
      LOWER=$(printf '%s' "$TAB" | tr '[:upper:]' '[:lower:]')
      screencapture -x -o -l "$WID" "$OUT/$LOWER-$LABEL.png"
      osascript -l JavaScript "$WORK/texts.js" "$PID" >"$OUT/$LOWER-text-$LABEL.txt" 2>&1
    else
      fail "could not open the $TAB tab"
    fi
  done
  if grep -q "$VERSION" "$OUT/settings-text-$LABEL.txt"; then note "Settings shows $VERSION"; else fail "Settings does not show $VERSION"; fi
  if grep -qi 'Repeat reads' "$OUT/settings-text-$LABEL.txt"; then note "Settings shows the Repeat reads switch"; else fail "Settings does not show the Repeat reads switch"; fi
fi

# ── 7. The menu-bar menu, with the version ──────────────────────────────────────────────────
if osascript -l JavaScript "$WORK/menu.js" "$PID" >"$OUT/tray-menu-text-$LABEL.txt" 2>&1; then
  screencapture -x "$OUT/tray-menu-$LABEL.png"
  osascript -e 'tell application "System Events" to key code 53' >/dev/null 2>&1
  if grep -q "$VERSION" "$OUT/tray-menu-text-$LABEL.txt"; then note "the menu-bar menu shows $VERSION"; else fail "the menu-bar menu does not show $VERSION"; fi
else
  cannot "the menu-bar menu: the status item could not be opened through the accessibility API ($(tail -n1 "$OUT/tray-menu-text-$LABEL.txt"))"
fi

# ── 8. The CLI, in Terminal and as raw text ─────────────────────────────────────────────────
lumen --version >"$OUT/cli-version-$LABEL.txt" 2>&1
lumen >"$OUT/cli-line-$LABEL.txt" 2>&1
lumen report >"$OUT/cli-report-$LABEL.txt" 2>&1
lumen doctor >"$OUT/cli-doctor-$LABEL.txt" 2>&1
CMDF="$WORK/lumen-cli.command"
cat >"$CMDF" <<'SH'
#!/bin/bash
printf '\e[8;70;150t'
clear
for c in "lumen --version" "lumen" "lumen report" "lumen doctor"; do printf '$ %s\n' "$c"; $c; echo; done
SH
chmod +x "$CMDF"
open -a Terminal "$CMDF"
sleep 10
TPID=$(pgrep -x Terminal | head -n1)
TWID=""
[ -n "$TPID" ] && TWID=$(largest_window "$TPID")
if [ -n "$TWID" ]; then
  screencapture -x -o -l "$TWID" "$OUT/cli-terminal-$LABEL.png"
  note "the CLI in Terminal: cli-terminal-$LABEL.png (a long report scrolls; every command's full output is in cli-*.txt)"
else
  cannot "the CLI in Terminal: Terminal's window was not found; the raw text is in cli-*.txt"
fi
pkill -x Terminal >/dev/null 2>&1

note "done: $LABEL, failures: $FAILED"
exit "$FAILED"
