#!/bin/bash
# issue5-launch-check.sh — issue #5, created on purpose, and the built app launched into it.
#
# macOS remembers a status item ⌘-dragged out of the menu bar as "NSStatusItem Visible
# <autosave>" = false in the app's own preferences (and, on recent releases, as "NSStatusItem
# VisibleCC <autosave>"), and from then on creates that item hidden. The tray builds, AppKit
# reports success, and nothing is on screen. It is per-user state, which is why the bug
# reproduced on nobody's machine but the reporter's; this script makes that state, then
# launches the app and checks what 1.6.0 promises:
#
#   - both preferences are cleared before the tray is built, and do not read false afterwards
#   - the icon is on screen, and the main window opens
#   - the window says why: the icon had been hidden by macOS, and Quit Lumen is how to stop it
#
# Then the control: a launch with nothing planted must do none of that, and keep its window
# closed.
#
# It writes the app's preferences and launches it as the logged-in user, which on a real
# machine registers a login item and rewrites Lumen's hooks. So it runs on CI runners only:
#
#   bash scripts/issue5-launch-check.sh target/debug/bundle/macos/Lumen.app "$RUNNER_TEMP/issue5"
#
# Everything it observed is written to the second argument, for the job to keep.

set -uo pipefail

APP=${1:?usage: issue5-launch-check.sh <Lumen.app> <output dir>}
OUT=${2:?usage: issue5-launch-check.sh <Lumen.app> <output dir>}

if [ -z "${CI:-}" ]; then
  echo "refusing to run: this writes io.speedata.lumen's preferences and launches Lumen as you." >&2
  echo "It is for CI runners only; see the header." >&2
  exit 2
fi
if [ "$(uname -s)" != Darwin ]; then
  echo "macOS only: the hidden-icon preference is AppKit's" >&2
  exit 2
fi

DOMAIN=io.speedata.lumen
KEYS=("NSStatusItem Visible Item-0" "NSStatusItem VisibleCC Item-0")
EXE="$APP/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")"
mkdir -p "$OUT"
FAILED=0
LIMITED=0

ok() { printf 'ok:         %s\n' "$*"; }
fail() { printf 'FAIL:       %s\n' "$*"; FAILED=1; }
limited() { printf 'LIMITATION: %s\n' "$*"; LIMITED=1; }

# Windows as the window server lists them, one per line:
#   layer=<n> onscreen=<0|1> bounds=<x>,<y>,<w>x<h> id=<window number> owner=<name>
# for one PID, or with "statusbar" every window in the status bar's layer, whoever owns it.
# Layer 0 is an ordinary window; 25 is the status bar, where a menu-bar icon lives. Neither
# layer nor bounds needs the Screen Recording permission; window titles would.
WINDOWS_JS="$OUT/windows.js"
cat >"$WINDOWS_JS" <<'JXA'
ObjC.import('CoreGraphics');
function run(argv) {
  // kCGWindowListOptionAll, kCGNullWindowID: off-screen windows too, so a hidden one is seen.
  const all = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo(0, 0)));
  const wanted = argv[0] === 'statusbar'
    ? w => w.kCGWindowLayer === 25
    : w => w.kCGWindowOwnerPID === Number(argv[0]);
  return all
    .filter(wanted)
    .map(w => {
      const b = w.kCGWindowBounds;
      return `layer=${w.kCGWindowLayer} onscreen=${w.kCGWindowIsOnscreen ? 1 : 0} ` +
        `bounds=${b.X},${b.Y},${b.Width}x${b.Height} id=${w.kCGWindowNumber} owner=${w.kCGWindowOwnerName}`;
    })
    .join('\n');
}
JXA

# Every static text in the PID's windows, through the accessibility API. A web view's text is
# exposed there like any other. Needs the Accessibility permission for whatever runs osascript.
TEXTS_JS="$OUT/texts.js"
cat >"$TEXTS_JS" <<'JXA'
function run(argv) {
  const pid = Number(argv[0]);
  const se = Application('System Events');
  const procs = se.processes.whose({ unixId: pid });
  if (procs.length === 0) throw new Error(`no process ${pid}`);
  const out = [];
  for (const w of procs[0].windows()) {
    for (const el of w.entireContents()) {
      try {
        if (el.role() === 'AXStaticText') {
          const v = el.value();
          if (v) out.push(v);
        }
      } catch (e) {
        // An element that went away while being read.
      }
    }
  }
  return out.join('\n');
}
JXA

# The text in a picture, by Vision: the fallback when the accessibility API is closed.
OCR_SWIFT="$OUT/ocr.swift"
cat >"$OCR_SWIFT" <<'SWIFT'
import AppKit
import Vision

let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let image = NSImage(contentsOf: url),
      let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    FileHandle.standardError.write("cannot read \(url.path)\n".data(using: .utf8)!)
    exit(2)
}
let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
try VNImageRequestHandler(cgImage: cg, options: [:]).perform([request])
for observation in request.results ?? [] {
    if let best = observation.topCandidates(1).first { print(best.string) }
}
SWIFT

windows_of() { osascript -l JavaScript "$WINDOWS_JS" "$1"; }

# The icon is a status-bar window of the app's own on macOS 14. From macOS 26 Control Center
# draws every app's icon, and nothing in the window list says which is whose, so what the
# status bar holds is kept for whoever reads a failure.
icon_on_screen() { # pid, name for the record
  if grep -q '^layer=25 onscreen=1 ' "$OUT/windows-$2.txt"; then
    return 0
  fi
  windows_of statusbar >"$OUT/statusbar-$2.txt"
  echo "the status bar's windows, any owner ($(sw_vers -productVersion)):"
  cat "$OUT/statusbar-$2.txt"
  return 1
}

# wait_for <file> <fixed string> <seconds>
wait_for() {
  local i
  for ((i = 0; i < $3 * 4; i++)); do
    grep -qF -- "$2" "$1" && return 0
    sleep 0.25
  done
  return 1
}

show_prefs() {
  local k
  for k in "${KEYS[@]}"; do
    printf '$ defaults read %s "%s"\n' "$DOMAIN" "$k"
    defaults read "$DOMAIN" "$k" 2>&1
    printf '[exit %s]\n' "$?"
  done
}

stop_app() {
  kill "$1" 2>/dev/null
  for _ in $(seq 1 40); do kill -0 "$1" 2>/dev/null || break; sleep 0.25; done
  kill -9 "$1" 2>/dev/null
  # The daemon is the app's sidecar and holds 127.0.0.1:9999; the next launch needs it free.
  pkill -x lumen-daemon 2>/dev/null
  sleep 1
}

echo "== the app: $EXE"
[ -x "$EXE" ] || { echo "no executable at $EXE" >&2; exit 2; }

# Setup has run: without the marker the first launch opens the window for Setup, which is not
# what is under test.
mkdir -p "$HOME/.claude/lumen"
: >"$HOME/.claude/lumen/.setup_done"

echo
echo "== 1. the state issue #5 left behind: both forms of the hidden-icon preference, false"
for k in "${KEYS[@]}"; do
  printf '$ defaults write %s "%s" -bool false\n' "$DOMAIN" "$k"
  defaults write "$DOMAIN" "$k" -bool false
done
show_prefs | tee "$OUT/prefs-planted.txt"

echo
echo "== 2. launch"
LOG="$OUT/launch-stdout.log"
"$EXE" >"$LOG" 2>&1 &
PID=$!
echo "pid $PID"

for line in \
  "TRAY: clearing NSStatusItem Visible Item-0" \
  "TRAY: clearing NSStatusItem VisibleCC Item-0" \
  "TRAY: restored a hidden menu-bar icon (2 pref(s))" \
  "FALLBACK: revealed the main window (the menu-bar icon was restored)"; do
  if wait_for "$LOG" "$line" 30; then ok "logged: $line"; else fail "never logged: $line"; fi
done
# The checks after Ready: 500, 1,500 and 4,000 ms apart.
if wait_for "$LOG" "TRAY: presence check" 20 && sleep 7 && grep -q "TRAY: presence check . of 3 → Present" "$LOG"; then
  ok "the presence check found the icon: $(grep -m1 'TRAY: presence check . of 3 → Present' "$LOG" | sed 's/.*TRAY:/TRAY:/')"
else
  fail "the presence check never found the icon: $(grep 'TRAY: presence check\|TRAY: not visible' "$LOG" | sed 's/.*TRAY:/TRAY:/' | tr '\n' ';')"
fi

echo
echo "== 3. the preferences afterwards (AppKit may write Visible back as 1; it must not read 0)"
show_prefs | tee "$OUT/prefs-after.txt"
for k in "${KEYS[@]}"; do
  if [ "$(defaults read "$DOMAIN" "$k" 2>/dev/null)" = 0 ]; then
    fail "\"$k\" still reads 0"
  else
    ok "\"$k\" no longer reads 0"
  fi
done

echo
echo "== 4. the windows the app has on screen"
windows_of "$PID" | tee "$OUT/windows-launch.txt"
MAIN_ID=$(sed -n 's/^layer=0 onscreen=1 .* id=\([0-9]*\) owner=.*$/\1/p' "$OUT/windows-launch.txt" | head -n1)
if [ -n "$MAIN_ID" ]; then ok "a window is on screen (window $MAIN_ID, layer 0)"; else fail "no window on screen"; fi
if icon_on_screen "$PID" launch; then
  ok "the menu-bar icon is on screen (layer 25)"
else
  fail "the window server lists no menu-bar icon of the app's on screen"
fi

echo
echo "== 5. what the window says"
if [ -n "$MAIN_ID" ]; then
  screencapture -x -o -l "$MAIN_ID" "$OUT/main-window.png" 2>&1 && echo "screenshot: $OUT/main-window.png"
fi
TEXT=$(osascript -l JavaScript "$TEXTS_JS" "$PID" 2>"$OUT/texts-error.txt")
HOW="the accessibility API"
if [ -z "$TEXT" ]; then
  echo "the accessibility API gave nothing: $(cat "$OUT/texts-error.txt")"
  if [ -s "$OUT/main-window.png" ]; then
    TEXT=$(swift "$OCR_SWIFT" "$OUT/main-window.png" 2>"$OUT/ocr-error.txt")
    HOW="text recognition on the screenshot"
  fi
fi
printf '%s\n' "$TEXT" >"$OUT/window-text.txt"
printf -- '--- the window text, by %s ---\n%s\n---\n' "$HOW" "$TEXT"
if [ -z "$TEXT" ]; then
  limited "this runner let neither the accessibility API nor a screenshot read the window"
elif grep -q "hidden by macOS" <<<"$TEXT" && grep -q "Quit Lumen" <<<"$TEXT"; then
  ok "the window says the icon had been hidden by macOS, and names Quit Lumen"
elif [ "$HOW" != "the accessibility API" ] && ! grep -qi "lumen" <<<"$TEXT"; then
  # A screenshot without the Screen Recording permission holds the wallpaper, not the window.
  limited "the screenshot does not show the window's content (no Screen Recording permission)"
else
  fail "the window does not explain the restored icon"
fi

cp "$HOME/Library/Logs/$DOMAIN/Lumen.log" "$OUT/Lumen.log" 2>/dev/null
stop_app "$PID"

echo
echo "== 6. the control: nothing planted"
for k in "${KEYS[@]}"; do defaults delete "$DOMAIN" "$k" 2>/dev/null; done
show_prefs | tee "$OUT/prefs-control.txt"
CLOG="$OUT/control-stdout.log"
"$EXE" >"$CLOG" 2>&1 &
CPID=$!
echo "pid $CPID"
if wait_for "$CLOG" "TRAY: presence check" 30; then
  # Past the last check, and the reveal that would follow it.
  sleep 9
  ok "the presence checks ran: $(grep 'TRAY: presence check' "$CLOG" | sed 's/.*TRAY:/TRAY:/' | tr '\n' ';')"
else
  fail "the control launch never ran its presence checks"
fi
for line in "TRAY: clearing" "TRAY: restored" "FALLBACK: revealed"; do
  if grep -qF "$line" "$CLOG"; then
    fail "the control logged \"$line\": $(grep -F "$line" "$CLOG")"
  else
    ok "the control did not log \"$line\""
  fi
done
windows_of "$CPID" | tee "$OUT/windows-control.txt"
if grep -q '^layer=0 onscreen=1 ' "$OUT/windows-control.txt"; then
  fail "the control launch has a window on screen"
else
  ok "the control launch keeps its window closed"
fi
if icon_on_screen "$CPID" control; then
  ok "the control's menu-bar icon is on screen"
else
  fail "the window server lists no menu-bar icon of the control's on screen"
fi
stop_app "$CPID"

echo
if [ "$FAILED" != 0 ]; then
  echo "issue #5 launch check: FAILED"
  exit 1
fi
if [ "$LIMITED" != 0 ]; then
  echo "issue #5 launch check: passed, except what this runner would not let it see (LIMITATION above)"
else
  echo "issue #5 launch check: passed"
fi
