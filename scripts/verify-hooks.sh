#!/usr/bin/env bash
# verify-hooks.sh — run the hooks Lumen Setup installed and check that they work.
#
# Not a grep of the scripts: the hooks are run as Claude Code runs them, a payload on
# stdin, and judged by what they do. They run against a ledger made for this run:
# LUMEN_DB, LUMEN_FAULT_SPOOL, HOME and the temp dir holding the intercept's markers
# all point into a scratch directory, removed afterwards. Setup's hooks take each of
# those from the environment before what Setup baked in. Hooks from before
# `lumen-mcp hook` (1.5.1 and earlier) promise no such thing, so they are reported
# and not run.
#
# Usage:
#   ./scripts/verify-hooks.sh                  # the hooks in ~/.claude/lumen
#   ./scripts/verify-hooks.sh DIR              # the hooks in DIR
#   ./scripts/verify-hooks.sh --expect 1.6.0   # and require the build that wrote them
#
# Exit codes: 0 all checks passed, 1 one or more failed, 2 bad usage.

set -uo pipefail

HOOK_DIR=""
EXPECT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --expect)
            [[ $# -ge 2 ]] || { echo "--expect needs a version" >&2; exit 2; }
            EXPECT="$2"; shift 2 ;;
        -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
        -*) echo "unknown argument: $1" >&2; exit 2 ;;
        *) HOOK_DIR="$1"; shift ;;
    esac
done
if [[ -z "$HOOK_DIR" ]]; then
    [[ -n "${HOME:-}" ]] || { echo "no HOME, so no ~/.claude/lumen; name the directory" >&2; exit 2; }
    HOOK_DIR="$HOME/.claude/lumen"
fi

PASS=0; FAIL=0; SKIP=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
skip() { printf '  \033[33mSKIP\033[0m  %s\n' "$1"; SKIP=$((SKIP+1)); }
section() { printf '\n\033[1m%s\033[0m\n' "$1"; }
result() {
    printf '  hooks: %d passed, %d failed, %d skipped\n' "$PASS" "$FAIL" "$SKIP"
    [[ "$FAIL" -eq 0 ]]
}

# On Windows the hooks run in Git Bash, as Claude Code runs them, and lumen-mcp is a
# Windows program: a path it is handed must be one Windows can read.
case "${OSTYPE:-}" in
    msys* | cygwin*) WINDOWS=1 ;;
    *) WINDOWS=0 ;;
esac
native() { if [[ $WINDOWS == 1 ]]; then cygpath -m "$1"; else printf '%s\n' "$1"; fi; }

# The generator stamp Setup writes under the shebang: the version that wrote the script.
stamp_of() {
    local line
    while IFS= read -r line; do
        case "$line" in "# lumen-generator: "*) printf '%s\n' "${line#"# lumen-generator: "}"; return ;; esac
    done <"$1"
}

section "Installed hooks — $HOOK_DIR"
if [[ ! -d "$HOOK_DIR" ]]; then
    skip "no such directory — Setup has not run here"
    result; exit
fi
# Absolute, because each hook runs from a scratch project directory.
HOOK_DIR="$(cd "$HOOK_DIR" && pwd)"
METER="$HOOK_DIR/lumen_meter.sh"
INTERCEPT="$HOOK_DIR/lumen_read_intercept.sh"

runnable=1
for pair in "meter:$METER" "intercept:$INTERCEPT"; do
    sub="${pair%%:*}"; script="${pair#*:}"; name="${script##*/}"
    if [[ ! -e "$script" ]]; then
        bad "$name missing — run Setup again"; runnable=0; continue
    fi
    if [[ -x "$script" ]]; then
        ok "$name present and executable"
    else
        bad "$name is not executable — Claude Code runs it by path"; runnable=0
    fi
    stamp="$(stamp_of "$script")"
    by="written by Lumen ${stamp:-of unknown version (no stamp)}"
    if ! grep -qF "exec \"\$bin\" hook $sub" "$script"; then
        bad "$name, $by, predates \`lumen-mcp hook\` — launching Lumen 1.6.0 or later regenerates it, \
as does running Setup again. Not run: nothing in it promises to write a scratch ledger instead of yours"
        runnable=0
    elif [[ -n "$EXPECT" && "$stamp" != "$EXPECT" ]]; then
        bad "$name, $by, where $EXPECT was expected — launching Lumen regenerates it"
    else
        ok "$name hands each event to lumen-mcp ($by)"
    fi
done

section "What the hooks do (scratch ledger)"
if [[ $runnable == 0 ]]; then
    skip "not run — fix the failures above first"
    result; exit
fi

tmp="${TMPDIR:-/tmp}"
T="$(mktemp -d "${tmp%/}/lumen-verify-hooks.XXXXXX")" || { bad "cannot make a scratch directory under $tmp"; result; exit; }
trap 'rm -rf "$T"' EXIT
# TMPDIR may have come in as a Windows path.
[[ $WINDOWS == 1 ]] && T="$(cygpath -u "$T")"
mkdir -p "$T/home" "$T/tmp" "$T/ledger" "$T/proj"
NT="$(native "$T")"
# The payloads below are JSON put together with printf.
case "$NT" in *[\"\\]*) bad "the scratch path $NT has a quote or a backslash in it, which this script does not escape"; result; exit ;; esac
LEDGER="$NT/ledger/lumen.db"
SPOOL="$T/ledger/faults.jsonl"
NSPOOL="$(native "$SPOOL")"
SID="verify-hooks-$$-$RANDOM"

printf 'fn main() {}\n' >"$T/proj/small.rs"
i=0; while [[ $i -lt 400 ]]; do printf 'fn f%d() {}\n' "$i"; i=$((i + 1)); done >"$T/proj/big.rs"

# The payloads Claude Code sends, cut to the fields the hooks read.
pre_read() { # FILE SESSION
    printf '{"session_id":"%s","cwd":"%s","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"%s"},"tool_use_id":"toolu_verify_hooks"}' \
        "$2" "$NT/proj" "$NT/proj/$1"
}
post_read() { # SESSION
    printf '{"session_id":"%s","cwd":"%s","hook_event_name":"PostToolUse","tool_name":"Read","tool_input":{"file_path":"%s"},"tool_response":{"type":"text","file":{"filePath":"%s","content":"fn main() {}\\n","numLines":1,"startLine":1,"totalLines":1}},"tool_use_id":"toolu_verify_hooks"}' \
        "$1" "$NT/proj" "$NT/proj/small.rs" "$NT/proj/small.rs"
}
post_bash() { # SESSION
    printf '{"session_id":"%s","cwd":"%s","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls","description":"List files"},"tool_response":{"stdout":"big.rs\\nsmall.rs","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false},"tool_use_id":"toolu_verify_hooks"}' \
        "$1" "$NT/proj"
}

# One hook run as Claude Code makes it: the payload on stdin, the project as the
# working directory, and on Windows through `bash`. Every path a hook may write is
# under $T, and none of this shell's Lumen settings reach it. Sets CODE, OUT, ERR.
run() { # SCRIPT PAYLOAD [VAR=VALUE...]
    local script="$1"; printf '%s' "$2" >"$T/payload.json"; shift 2
    (
        cd "$T/proj" || exit 97
        unset LUMEN_HOOK_ENABLED LUMEN_LINE_THRESHOLD LUMEN_CAPTURE LUMEN_DEBUG \
            LUMEN_METER_BASH LUMEN_SESSION_ID LUMEN_CHANNEL LUMEN_MCP_BIN CLAUDE_CODE_SESSION_ID
        export HOME="$T/home" USERPROFILE="$NT/home" TMPDIR="$NT/tmp" TMP="$NT/tmp" \
            TEMP="$NT/tmp" LUMEN_DB="$LEDGER" LUMEN_FAULT_SPOOL="$NSPOOL"
        # Each word is VAR=VALUE.
        # shellcheck disable=SC2163
        [[ $# -gt 0 ]] && export "$@"
        if [[ $WINDOWS == 1 ]]; then exec bash "$script"; else exec "$script"; fi
    ) <"$T/payload.json" >"$T/out" 2>"$T/err"
    CODE=$?
    OUT="$(cat "$T/out")"
    ERR="$(cat "$T/err")"
}
# One line, for a PASS/FAIL line.
nl=$'\n'
said() { local e="${ERR//$nl/ | }"; printf 'exit %s, stdout [%s], stderr [%s]' "$CODE" "$OUT" "$e"; }
spooled() { grep -qF "$1" "$SPOOL" 2>/dev/null; }

run "$METER" "$(post_read "$SID")"
if [[ $CODE == 0 && -z "$OUT" && -z "$ERR" ]]; then
    ok "meter: a Read is metered in silence (exit 0, nothing on stdout or stderr)"
else
    bad "meter: a Read gave $(said)"
fi
run "$METER" "$(post_bash "$SID")"
if [[ $CODE == 0 && -z "$OUT" && -z "$ERR" ]]; then
    ok "meter: a Bash command is metered in silence"
else
    bad "meter: a Bash command gave $(said)"
fi
if [[ ! -s "$T/ledger/lumen.db" ]]; then
    bad "meter: nothing at the scratch ledger $LEDGER — the rows went somewhere else, or nowhere"
elif ! command -v sqlite3 >/dev/null 2>&1; then
    skip "meter: the scratch ledger was written, but there is no sqlite3 here to read its rows"
else
    rows="$(sqlite3 "$LEDGER" "SELECT tool, routed_via, token_source FROM read_events
        WHERE session_id = '$SID' ORDER BY rowid" 2>&1 | tr -d '\r')"
    want=$'Read|builtin_read|measured\nBash|bash_output|measured'
    if [[ "$rows" == "$want" ]]; then
        ok "meter: the scratch ledger holds both rows, token counts measured"
    else
        bad "meter: the scratch ledger holds [${rows//$nl/ | }] for this session, not [${want//$nl/ | }]"
    fi
fi
if [[ -s "$SPOOL" ]]; then
    bad "meter: faults were recorded: $(tr '\n' ' ' <"$SPOOL")"
else
    ok "meter: no fault recorded"
fi

run "$INTERCEPT" "$(pre_read small.rs "$SID")"
if [[ $CODE == 0 && -z "$OUT" && -z "$ERR" ]]; then
    ok "intercept: a 1-line file is read as asked"
else
    bad "intercept: a 1-line file gave $(said)"
fi
run "$INTERCEPT" "$(pre_read big.rs "$SID")"
if [[ $CODE == 2 && -z "$OUT" && "$ERR" == "Lumen intercept: "*" is 400 lines."* \
      && "$ERR" == *"lumen:smart_read("* && "$ERR" == *"it will be allowed through"* ]]; then
    ok "intercept: a 400-line file is sent to lumen:smart_read (exit 2), and the model is told a retry gets through"
else
    bad "intercept: a 400-line file gave $(said)"
fi
run "$INTERCEPT" "$(pre_read big.rs "$SID")"
if [[ $CODE == 0 && -z "$OUT" ]] && spooled '"variant":"retry_escape_valve"'; then
    ok "intercept: the retry is let through and recorded as retry_escape_valve"
else
    bad "intercept: the retry gave $(said); spool: $(tr '\n' ' ' <"$SPOOL" 2>/dev/null)"
fi

# Lumen moved or removed: no lumen-mcp where Setup put it, none on PATH. A PATH with
# none on it but bash, cat and date still there is, on Windows, PATH without the
# directories holding one. Elsewhere it is a directory of links, since a lumen-mcp
# installed into /usr/bin sits beside cat and date.
if [[ $WINDOWS == 1 ]]; then
    rest="$PATH:"; NO_MCP=""
    while [[ -n "$rest" ]]; do
        d="${rest%%:*}"; rest="${rest#*:}"
        [[ -z "$d" || -e "$d/lumen-mcp" || -e "$d/lumen-mcp.exe" ]] && continue
        NO_MCP="${NO_MCP:+$NO_MCP:}$d"
    done
else
    mkdir -p "$T/bin"
    for tool in bash cat date; do ln -s "$(command -v "$tool")" "$T/bin/$tool"; done
    NO_MCP="$T/bin"
fi
GONE=(LUMEN_MCP_BIN="$NT/gone/lumen-mcp" PATH="$NO_MCP")
run "$INTERCEPT" "$(pre_read big.rs "$SID-gone")" "${GONE[@]}"
if [[ $CODE == 0 && -z "$OUT" && "$ERR" == *"cannot run lumen-mcp"* ]] \
      && spooled '"kind":"hook_fail_open","variant":"lumen_mcp_missing"'; then
    ok "intercept without lumen-mcp: the read is let through, with a line on stderr and a fault"
else
    bad "intercept without lumen-mcp gave $(said)"
fi
run "$METER" "$(post_read "$SID-gone")" "${GONE[@]}"
if [[ $CODE == 0 && -z "$OUT" && "$ERR" == *"cannot run lumen-mcp"* ]] \
      && spooled '"kind":"meter_write_failed","variant":"lumen_mcp_missing"'; then
    ok "meter without lumen-mcp: exit 0, with a line on stderr and a fault"
else
    bad "meter without lumen-mcp gave $(said)"
fi

result
