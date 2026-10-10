#!/usr/bin/env bash
# lumen-generator: 1.5.1
# lumen_meter.sh — installed by Lumen Setup. Regenerated automatically when it
# drifts from the running build; do not hand-edit.
#
# PostToolUse hook. Records built-in Read events (the "missed optimization"
# baseline) and Bash output volume. mcp__lumen__* tools self-meter in-process.
#
# Reads no file contents beyond tokenizing the file that was already read, writes
# only to the local SQLite DB, makes no network calls, and executes nothing from
# the payload.
# Both are overridable. The generated path is the default, not a constant: with it
# hardcoded there was no way to exercise this script without writing to the real
# ledger, so the installed hook — the one that actually runs — was the only part of
# the pipeline that could not be tested.
LUMEN_DB="${LUMEN_DB:-__HOME__/Library/Application Support/io.speedata.lumen/lumen.db}"
LUMEN_TOK="${LUMEN_TOK:-/Applications/Lumen.app/Contents/MacOS/lumen-tok}"

set -uo pipefail

INPUT=$(cat)

if [ "${LUMEN_DEBUG:-}" = "1" ]; then
    printf '%s' "$INPUT" > /tmp/lumen_hook_dump.json
fi

# Channel comes from the environment Claude Code exports, not a hardcoded string.
# It used to be the literal 'cli' on every row, which made the "By channel"
# breakdown a constant dressed up as a measurement.
case "${CLAUDE_CODE_ENTRYPOINT:-}" in
    *vscode*) CHANNEL="vscode" ;;
    "")       CHANNEL="unknown" ;;
    *)        CHANNEL="cli" ;;
esac
SESSION_ID="${CLAUDE_CODE_SESSION_ID:-}"

# An explicit template, not `mktemp -t lumen_bash_out`. BSD mktemp accepts a bare
# prefix and appends its own suffix; GNU mktemp requires at least three X's and
# fails outright on a template without them. So on Linux OUT_FILE came back empty,
# python3 raised FileNotFoundError opening "", the `|| exit 0` below swallowed it,
# and the hook recorded nothing at all — silently, on every read, since 1.1.5. The
# script-level tests that would have caught it did not exist until 1.2.1.
OUT_FILE="$(mktemp "${TMPDIR:-/tmp}/lumen_bash_out.XXXXXX" 2>/dev/null)"
if [ -z "${OUT_FILE:-}" ] || [ ! -f "$OUT_FILE" ]; then
    # Say so rather than disappearing. Still exit 0: a metering hook must never
    # fail the tool call it is observing.
    echo "lumen_meter: cannot create a temp file under ${TMPDIR:-/tmp}; skipping" >&2
    exit 0
fi
trap 'rm -f "$OUT_FILE"' EXIT

# One python call, not four: the old script spawned a fresh interpreter per field.
# Fields are TAB-separated so no value needs shell quoting. Any Bash output is
# written to OUT_FILE rather than passed through the shell.
FIELDS=$(printf '%s' "$INPUT" | LUMEN_OUT="$OUT_FILE" python3 -c '
import json, os, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(1)
tool = d.get("tool_name") or ""
ti = d.get("tool_input") or {}
tr = d.get("tool_response")
path = ti.get("file_path") or ""
cmd = ti.get("command") or ""
out = ""
if isinstance(tr, dict):
    out = (tr.get("stdout") or "") + (tr.get("stderr") or "")
elif isinstance(tr, str):
    out = tr
with open(os.environ["LUMEN_OUT"], "w") as f:
    f.write(out)
clean = lambda s: s.replace("\t", " ").replace("\n", " ")

def cmd_label(s):
    # Program and subcommand only — "cargo test", "git status", "npm run".
    #
    # Not the whole command line. A command line routinely carries credentials
    # (curl -H "Authorization: Bearer ...", psql "postgres://u:p@host") and this
    # value is stored in a database that gets backed up and shipped around. The
    # measurement is output volume by kind of command, which two tokens answer
    # fully. Leading VAR=value assignments are dropped first so that
    # `TOKEN=secret curl ...` records "curl" rather than the secret.
    toks = clean(s).split()
    while toks and "=" in toks[0] and not toks[0].startswith("-"):
        toks.pop(0)
    return " ".join(toks[:2])[:60]

sys.stdout.write("\t".join([tool, clean(path), cmd_label(cmd)]))
') || exit 0
[ -n "${FIELDS:-}" ] || exit 0

TOOL_NAME=$(printf '%s' "$FIELDS" | cut -f1)
FILE_PATH=$(printf '%s' "$FIELDS" | cut -f2)
COMMAND=$(printf '%s' "$FIELDS" | cut -f3)

# Count the tokens in the file named by $1. Emits "<count> <provenance>" on one
# line, where provenance is measured | unsupported | estimated.
#
# The provenance must travel with the value. An earlier version set a TOKEN_SOURCE
# variable inside the function, but every call site is a command substitution — a
# subshell — so the assignment was discarded and estimates were recorded as
# "measured". Laundering an estimate as a measurement is worse than no label.
#
# Takes a path rather than reading stdin. With `count_tokens < "$f"` the tokenizer
# and the fallback shared one file descriptor and therefore one offset, so whatever
# the tokenizer consumed before failing was missing from the fallback's count. Each
# redirect below opens the file independently.
count_tokens() {
    _f="$1"
    if [ -x "$LUMEN_TOK" ]; then
        _c=$("$LUMEN_TOK" < "$_f" 2>/dev/null)
        _rc=$?
        [ "$_rc" -eq 0 ] && { printf '%s measured\n' "$_c"; return 0; }
        # Exit 3 means the input is not text. A PNG has no token count, and
        # inventing one is not a lesser error than admitting it: bytes/4 overstates
        # a screenshot by ~40x, and that fabricated number is what put a 4.3M-token
        # "optimization opportunity" in front of a feature decision.
        [ "$_rc" -eq 3 ] && { printf '0 unsupported\n'; return 0; }
    fi
    # A genuinely broken tokenizer still gets a row — a row beats no row — but the
    # estimate is labelled and logged rather than passed off as a measurement.
    echo "lumen_meter: LUMEN_TOK unusable at $LUMEN_TOK — recording an estimate" >&2
    printf '%s estimated\n' "$(wc -c < "$_f" | awk '{print int($1/4)}')"
}

# Modification time as a Unix timestamp, on both stat dialects.
#
# Validating the output instead of trusting the exit code, because the obvious
# `stat -f %m "$f" || stat -c %Y "$f"` is wrong in a way that exit codes hide: on
# BSD `-f` is "format", but on GNU `-f` is "display filesystem status". So on Linux
# the first branch SUCCEEDED and printed six lines of filesystem information, the
# fallback never ran, and that multi-line value was spliced into a newline-delimited
# field list — shifting every field after it and making the insert throw. Requiring
# a pure integer catches that regardless of which dialect is present or what it
# returns.
file_mtime() {
    _m=$(stat -c %Y "$1" 2>/dev/null)
    case "${_m:-x}" in *[!0-9]*) _m=$(stat -f %m "$1" 2>/dev/null) ;; esac
    case "${_m:-x}" in *[!0-9]*) _m="" ;; esac
    printf '%s' "$_m"
}

TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

case "$TOOL_NAME" in
Read)
    [ -n "$FILE_PATH" ] && [ -f "$FILE_PATH" ] || exit 0
    LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null || echo 0)
    _r=$(count_tokens "$FILE_PATH")
    FULL_TOKENS=${_r%% *}; TOKEN_SOURCE=${_r##* }
    MTIME=$(file_mtime "$FILE_PATH")
    ROUTE="builtin_read"; REQ_KEY="$FILE_PATH"; RETURNED="$FULL_TOKENS"; TARGET="$FILE_PATH"
    ;;
Bash)
    # Observation only. No PreToolUse on Bash, no interception, no wrapper.
    _r=$(count_tokens "$OUT_FILE")
    FULL_TOKENS=${_r%% *}; TOKEN_SOURCE=${_r##* }
    LINE_COUNT=""; MTIME=""
    ROUTE="bash_output"; REQ_KEY=""; RETURNED=0; TARGET="$COMMAND"
    # Nothing measurable means nothing worth a row. Unlike a Read, where the event
    # itself is the datum, a Bash call with no output carries no information.
    [ "${FULL_TOKENS:-0}" -gt 0 ] || exit 0
    ;;
*)
    exit 0
    ;;
esac

# Bind every value as a parameter. tool_input.command is attacker-influenced text
# and must never be interpolated into SQL.
LUMEN_ARGS="$LUMEN_DB
$TS
$TOOL_NAME
$TARGET
$LINE_COUNT
$RETURNED
$FULL_TOKENS
$ROUTE
$CHANNEL
$SESSION_ID
$MTIME
$REQ_KEY
$TOKEN_SOURCE"
printf '%s' "$LUMEN_ARGS" | python3 -c '
import sqlite3, sys
f = sys.stdin.read().split("\n")
if len(f) < 13:
    sys.exit(0)
db, ts, tool, path, lines, ret, full, route, chan, sid, mtime, req, tsrc = f[:13]
n = lambda v: int(v) if v not in ("", None) else None
con = sqlite3.connect(db, timeout=5)
con.execute(
    "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,"
    "saved_tokens,routed_via,channel,session_id,file_mtime,req_key,is_subagent,"
    "writer_hook,token_source) VALUES(?,?,?,?,?,?,0,?,?,?,?,?,0,?,?)",
    (ts, tool, path, n(lines), n(ret), n(full), route, chan,
     sid or None, n(mtime), req or None, "lumen_meter.sh", tsrc),
)
con.commit()
con.close()
' 2>/dev/null || true

exit 0
