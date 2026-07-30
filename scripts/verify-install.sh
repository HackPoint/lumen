#!/usr/bin/env bash
# verify-install.sh — check that an installed Lumen actually works.
#
# Not a file-existence check. The MCP server is driven over stdio with a real JSON-RPC
# handshake and asked for its tool list, because "the binary is present" and "the binary
# answers" are different claims, and only the second one matters to a user. Every install
# bug found so far was of the second kind: a bundled MCP server two days out of date, a
# hook script from June with no fail-open guards, a cask pointing at the wrong DMG.
#
# Usage:
#   ./scripts/verify-install.sh                 # discover an install
#   ./scripts/verify-install.sh --cli-only      # skip GUI/app checks
#   ./scripts/verify-install.sh --bin-dir DIR   # check binaries in DIR
#   ./scripts/verify-install.sh --expect 1.5.0  # require this version
#
# Exit codes: 0 all checks passed, 1 one or more failed.

set -uo pipefail

CLI_ONLY=0
BIN_DIR=""
EXPECT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --cli-only) CLI_ONLY=1; shift ;;
        --bin-dir)  BIN_DIR="${2:-}"; shift 2 ;;
        --expect)   EXPECT="${2:-}"; shift 2 ;;
        -h|--help)  sed -n '2,18p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

PASS=0; FAIL=0; SKIP=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
skip() { printf '  \033[33mSKIP\033[0m  %s\n' "$1"; SKIP=$((SKIP+1)); }
# Deliberately not named `head`: a function by that name shadows the coreutil, and
# `head -1` inside a command substitution then returned this banner instead of the
# first line — which made a passing check report a failure.
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }

# Wait briefly for a process to appear.
#
# The tray and the daemon are spawned asynchronously after launch, so checking immediately
# after `open -a` reported the daemon missing on one run and present on the next. A check
# whose answer depends on how fast the machine is is not a check.
wait_for_process() {
    local pattern="$1" tries=20
    while (( tries-- )); do
        pgrep -f "$pattern" >/dev/null && return 0
        sleep 0.25
    done
    return 1
}

case "$(uname -s)" in
    Darwin) PLATFORM=macos ;;
    Linux)  PLATFORM=linux ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM=windows ;;
    *)      PLATFORM=unknown ;;
esac
section "Lumen install verification — ${PLATFORM}, $(uname -m)"

# ── Locate the binaries ───────────────────────────────────────────────────────
#
# Search order mirrors how each platform actually installs: an explicit --bin-dir, then
# the macOS app bundle, then PATH.
find_bin() {
    local name="$1"
    if [[ -n "$BIN_DIR" && -x "$BIN_DIR/$name" ]]; then
        printf '%s\n' "$BIN_DIR/$name"; return 0
    fi
    if [[ -x "/Applications/Lumen.app/Contents/MacOS/$name" ]]; then
        printf '%s\n' "/Applications/Lumen.app/Contents/MacOS/$name"; return 0
    fi
    command -v "$name" 2>/dev/null && return 0
    return 1
}

# The CLI ships as `lumen` on PATH but is staged as `lumen-cli` inside the bundle, because
# a case-insensitive filesystem would otherwise collide it with the GUI's `Lumen`.
CLI="$(find_bin lumen-cli || find_bin lumen || true)"
MCP="$(find_bin lumen-mcp || true)"
TOK="$(find_bin lumen-tok || true)"

section "Binaries"
# The CLI distribution (Homebrew formula, and the release tarball) ships only `lumen`.
# The GUI packages ship all five. So a missing MCP server is a fact about which artifact
# was installed, not a broken install — but it does mean no optimizer tools, which is
# worth saying out loud either way.
if [[ -n "$CLI" && -x "$CLI" ]]; then ok "CLI found: $CLI"; else bad "CLI not found"; fi
for pair in "MCP:$MCP" "TOK:$TOK"; do
    label="${pair%%:*}"; path="${pair#*:}"
    if [[ -n "$path" && -x "$path" ]]; then
        ok "$label found: $path"
    elif [[ "$CLI_ONLY" == "1" ]]; then
        skip "$label absent — expected for a CLI-only install"
    else
        bad "$label not found"
    fi
done
if [[ "$CLI_ONLY" == "1" && -z "$MCP" ]]; then
    skip "no MCP server in this install: the optimizer tools are unavailable, only the dashboard"
fi

# ── Versions agree ────────────────────────────────────────────────────────────
section "Version"
if [[ -n "$CLI" ]]; then
    RAW="$("$CLI" --version 2>/dev/null | tr -d '\r')"
    VER="${RAW##* }"
    if [[ -n "$VER" ]]; then
        ok "CLI reports $VER"
        if [[ -n "$EXPECT" ]]; then
            if [[ "$VER" == "$EXPECT" ]]; then
                ok "matches expected $EXPECT"
            else
                # This is the check that would have caught shipping a bundle whose
                # sidecars were two days older than its UI.
                bad "expected $EXPECT, installed $VER"
            fi
        fi
    else
        bad "CLI produced no version"
    fi
else
    bad "no CLI to version-check"
fi

# ── The MCP server answers a real handshake ───────────────────────────────────
section "MCP server (JSON-RPC over stdio)"
if [[ -z "$MCP" && "$CLI_ONLY" == "1" ]]; then
    skip "no MCP binary in a CLI-only install"
elif [[ -z "$MCP" ]]; then
    bad "no MCP binary to drive"
else
    OUT="$(printf '%s\n%s\n' \
        '{"jsonrpc":"2.0","id":1,"method":"initialize"}' \
        '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
        | "$MCP" 2>/dev/null)"

    if grep -q '"protocolVersion"' <<<"$OUT"; then
        ok "initialize returned a protocolVersion"
    else
        bad "initialize did not return a protocolVersion"
    fi

    # The four tools are the product. A server that starts but lists nothing is a
    # working process and a broken install.
    for tool in smart_read recall_file compress_logs lumen_ping; do
        if grep -q "\"$tool\"" <<<"$OUT"; then ok "tool advertised: $tool"; else bad "tool missing: $tool"; fi
    done

    # The threshold the description advertises must match the hook's. These disagreed in
    # 1.4.0 — the model read one number and met another.
    if grep -q 'files ≥300 lines' <<<"$OUT"; then
        ok "smart_read advertises the ≥300-line threshold"
    elif grep -q 'files ≥100 lines' <<<"$OUT"; then
        bad "smart_read still advertises ≥100 lines (stale binary)"
    else
        skip "threshold text not found in the tool description"
    fi

    if grep -q '"jsonrpc"' <<<"$OUT" && ! grep -qE '^[^{]' <<<"$(head -1 <<<"$OUT")"; then
        ok "stdout is pure JSON-RPC (diagnostics went to stderr)"
    else
        bad "stdout was polluted with non-protocol output"
    fi
fi

# ── The tokenizer sidecar ─────────────────────────────────────────────────────
section "Tokenizer"
if [[ -n "$TOK" ]]; then
    N="$(printf 'fn main() {}\n' | "$TOK" 2>/dev/null | tr -dc '0-9')"
    if [[ -n "$N" && "$N" -gt 0 ]]; then
        ok "lumen-tok counted $N tokens"
    else
        bad "lumen-tok produced no count — metering would fall back to bytes/4"
    fi
elif [[ "$CLI_ONLY" == "1" ]]; then
    skip "no lumen-tok in a CLI-only install — hook metering would fall back to bytes/4"
else
    bad "no lumen-tok"
fi

# ── The CLI's own report path ─────────────────────────────────────────────────
section "CLI report path"
if [[ -n "$CLI" ]]; then
    if "$CLI" report --help >/dev/null 2>&1; then
        ok "report subcommand present"
    else
        bad "report subcommand missing"
    fi
    # Must refuse to file without --yes. A regression here publishes without consent.
    "$CLI" report --faults /nonexistent-fixture.json >/dev/null 2>&1
    rc=$?
    if [[ $rc -ne 0 ]]; then
        ok "report exits non-zero without --dry-run/--yes (rc=$rc)"
    else
        bad "report exited 0 without being asked to file or dry-run"
    fi
fi

# ── Hooks, if Setup has run ───────────────────────────────────────────────────
section "Hook scripts"
HOOK_DIR="${HOME}/.claude/lumen"
if [[ ! -d "$HOOK_DIR" ]]; then
    skip "no ~/.claude/lumen — Setup has not run on this machine"
else
    for f in lumen_read_intercept.sh lumen_meter.sh; do
        if [[ -x "$HOOK_DIR/$f" ]]; then ok "$f present and executable"; else bad "$f missing or not executable"; fi
    done
    # The fail-open guards. Their absence is what deadlocked a session, and the fix
    # reached the developer copy for a full release before it reached this one.
    if grep -q 'lumen_mcp_missing' "$HOOK_DIR/lumen_read_intercept.sh" 2>/dev/null \
       && grep -q 'retry_escape_valve' "$HOOK_DIR/lumen_read_intercept.sh" 2>/dev/null; then
        ok "intercept has both fail-open guards"
    else
        bad "intercept is missing a fail-open guard — a session can deadlock"
    fi
    if grep -q 'will be allowed through' "$HOOK_DIR/lumen_read_intercept.sh" 2>/dev/null; then
        ok "block message tells the model an escape exists"
    else
        bad "block message does not mention the retry escape"
    fi
fi

# ── GUI / widget ──────────────────────────────────────────────────────────────
section "Menu-bar widget"
if [[ "$CLI_ONLY" == "1" ]]; then
    skip "--cli-only: GUI checks not applicable"
elif [[ -n "$BIN_DIR" ]]; then
    # --bin-dir points at build output, not an installed package. Failing on a missing
    # app bundle there is a statement about the checkout, not about the install — which is
    # how this reported a failure in CI while passing on a machine that had Lumen
    # installed. Run without --bin-dir to check a real install.
    skip "--bin-dir given: verifying built binaries, not an installed package"
elif [[ "$PLATFORM" == "macos" ]]; then
    if [[ -d /Applications/Lumen.app ]]; then
        ok "app bundle installed"
        BV="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
              /Applications/Lumen.app/Contents/Info.plist 2>/dev/null)"
        [[ -n "$BV" ]] && ok "bundle version $BV" || bad "bundle has no version"
        if wait_for_process 'Lumen.app/Contents/MacOS/Lumen$'; then
            ok "tray process is running"
        else
            bad "tray process is not running — the widget would not be in the menu bar"
        fi
        if wait_for_process 'lumen-daemon'; then
            ok "daemon is running"
        else
            bad "daemon is not running — the gauge would not update"
        fi
        # Autostart is a LaunchAgent, so "runs automatically after setup" is checkable.
        # Matched case-insensitively: the file is Lumen.plist, and a `*lumen*` glob is
        # case-sensitive in bash — which reported autostart as off when it was on.
        AGENT="$(find "${HOME}/Library/LaunchAgents" -maxdepth 1 -iname '*lumen*' 2>/dev/null | head -1)"
        if [[ -n "$AGENT" ]]; then
            ok "LaunchAgent registered: $(basename "$AGENT")"
            # A login item aimed at a path that no longer exists fails silently at the one
            # moment it matters, so check the target rather than just the file.
            TARGET="$(plutil -p "$AGENT" 2>/dev/null | grep -oE '/[^"]*Lumen\.app[^"]*' | head -1)"
            if [[ -n "$TARGET" && -e "$TARGET" ]]; then
                ok "login item points at something that exists"
            elif [[ -n "$TARGET" ]]; then
                bad "login item points at a missing path: $TARGET"
            else
                skip "could not read the login item's target"
            fi
        else
            skip "no LaunchAgent — autostart is off, which is a user setting"
        fi
    else
        bad "no /Applications/Lumen.app"
    fi
elif [[ "$PLATFORM" == "linux" ]]; then
    if command -v lumen >/dev/null 2>&1 || [[ -x /usr/bin/lumen ]]; then
        ok "package payload on PATH"
    fi
    if [[ -f /usr/share/applications/Lumen.desktop || -f /usr/share/applications/lumen.desktop ]]; then
        ok "desktop entry installed"
    else
        bad "no .desktop entry — nothing would appear in a launcher or tray"
    fi
    # A tray icon needs a session bus and a status-notifier host. Neither exists in a
    # container, and asserting otherwise would be theatre.
    if [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
        skip "display present but tray placement still needs a human to confirm"
    else
        skip "headless: a tray widget cannot be verified without a desktop session"
    fi
else
    skip "widget checks not implemented for $PLATFORM here (see verify-install.ps1)"
fi

section "Result"
printf '  %d passed, %d failed, %d skipped\n\n' "$PASS" "$FAIL" "$SKIP"
[[ "$FAIL" -eq 0 ]]
