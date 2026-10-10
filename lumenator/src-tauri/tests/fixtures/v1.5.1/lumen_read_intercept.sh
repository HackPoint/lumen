#!/usr/bin/env bash
# lumen-generator: 1.5.1
# lumen_read_intercept.sh — installed by Lumen Setup.
#
# This hook blocks the only other way to read the file, so it must never block when
# the tools it redirects to cannot run. Two fail-open guards enforce that:
#   1. lumen-mcp missing        — the MCP server cannot be serving; do not block.
#   2. same file intercepted    — the model was already told to use Lumen, so if it
#      twice in one session       is back on the built-in Read that route failed.
#
# Both write: guard 2 needs one marker file per session under TMPDIR, and a fired
# guard appends one line to the fault spool. Never SQLite from here — this hook
# gates whether the model may read a file at all and must not wait on a lock.
# Set LUMEN_CAPTURE=0 to keep the guards but record nothing.
set -euo pipefail

INPUT=$(cat)
HOOK_ENABLED="${LUMEN_HOOK_ENABLED:-1}"
THRESHOLD="${LUMEN_LINE_THRESHOLD:-300}"
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-/Applications/Lumen.app/Contents/MacOS/lumen-mcp}"
SPOOL="${LUMEN_FAULT_SPOOL:-__HOME__/Library/Application Support/io.speedata.lumen/faults.jsonl}"

if [ "$HOOK_ENABLED" = "0" ]; then
    exit 0
fi

# Append one hook_fail_open record. Best-effort: a hook that cannot record a fault
# must not turn that into a second fault, so every failure here is swallowed.
record_fault() {
    [ "${LUMEN_CAPTURE:-1}" != "0" ] || return 0
    [ -n "$SPOOL" ] || return 0
    python3 - "$SPOOL" "$1" "$2" "$3" "$4" <<'PY' 2>/dev/null || true
import json, sys, time

spool, guard, path, lines, session = sys.argv[1:6]
record = {
    "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "kind": "hook_fail_open",
    "variant": guard,
    "path": path,
    "lines": int(lines) if lines.isdigit() else None,
    "detail": None,
    "session_id": session or None,
    "version": None,
    "channel": "cli",
}
with open(spool, "a") as fh:
    fh.write(json.dumps(record) + "\n")
PY
}

# Tab-separated so one python3 process covers all three fields.
PARSED=$(python3 -c '
import sys, json
d = json.loads(sys.argv[1])
print("\t".join([
    d.get("tool_name", ""),
    d.get("tool_input", {}).get("file_path", ""),
    str(d.get("session_id", "")),
]))
' "$INPUT" 2>/dev/null || echo "")

TOOL_NAME=""; FILE_PATH=""; SESSION_ID=""
IFS=$'\t' read -r TOOL_NAME FILE_PATH SESSION_ID <<<"$PARSED" || true

if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

EXT=$(echo "${FILE_PATH##*.}" | tr '[:upper:]' '[:lower:]')
case "$EXT" in
    rs|py|pyi|ts|tsx) FILE_TYPE="source" ;;
    log|out|txt)      FILE_TYPE="log"    ;;
    *)                exit 0             ;;
esac

LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null | tr -d '[:space:]' || echo 0)
LINE_COUNT="${LINE_COUNT:-0}"
if [ "$LINE_COUNT" -lt "$THRESHOLD" ]; then
    exit 0
fi

# Both guards sit below the extension and threshold checks, not above them: a fired
# guard means "this read would have been redirected and could not be", so a file we
# were never going to intercept must not be recorded as a routing failure.

# Guard 1: no lumen-mcp on this machine means the MCP server cannot be serving
# smart_read/recall_file/compress_logs, so redirecting there would strand the model.
if [ ! -x "$LUMEN_MCP_BIN" ] && ! command -v lumen-mcp >/dev/null 2>&1; then
    record_fault "lumen_mcp_missing" "$FILE_PATH" "$LINE_COUNT" "$SESSION_ID"
    exit 0
fi

# Guard 2: one redirect per file per session.
SESSION_KEY="${SESSION_ID//[^A-Za-z0-9_-]/}"
STATE_FILE="${TMPDIR:-/tmp}"
STATE_FILE="${STATE_FILE%/}/lumen_intercept_${SESSION_KEY:-nosession}"

if [ -f "$STATE_FILE" ] && grep -Fxq -- "$FILE_PATH" "$STATE_FILE" 2>/dev/null; then
    record_fault "retry_escape_valve" "$FILE_PATH" "$LINE_COUNT" "$SESSION_ID"
    exit 0
fi
printf '%s\n' "$FILE_PATH" >> "$STATE_FILE" 2>/dev/null || true

if [ "$FILE_TYPE" = "log" ]; then
    cat >&2 <<MSG
Lumen intercept: ${FILE_PATH} is ${LINE_COUNT} lines (log/output file).
Before reading the full file, call:
  lumen:compress_logs(path="${FILE_PATH}")
This collapses repeated lines and stack frames deterministically (typically 40-80%
token reduction). Analyze the compressed output; the full file is still readable
via smart_read(mode="full") if needed.

If the lumen tools are unavailable to you (server down, permission denied), retry
this exact Read — it will be allowed through. Do not abandon the task.
MSG
else
    cat >&2 <<MSG
Lumen intercept: ${FILE_PATH} is ${LINE_COUNT} lines.
Instead of reading the full file, call:
  1. lumen:smart_read(path="${FILE_PATH}")       → structural outline, ~5-10% token cost
  2. lumen:recall_file(path="${FILE_PATH}", names=["<item>"]) → fetch only what you need
This typically saves 80-93% of context vs. reading the whole file.
Use smart_read(mode="full") only if you truly need every line.

If the lumen tools are unavailable to you (server down, permission denied), retry
this exact Read — it will be allowed through. Do not abandon the task.
MSG
fi

exit 2
