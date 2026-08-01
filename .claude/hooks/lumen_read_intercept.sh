#!/usr/bin/env bash
# lumen_read_intercept.sh — PreToolUse hook for the Read tool.
#
# Blocks large source/log files and redirects the model to lumen optimizer tools.
# Uses exit 2 + stderr message: Claude Code shows the stderr text to the model.
#
# This hook blocks the *only other* way to read the file, so it must never block
# when the tools it redirects to cannot run. Two fail-open guards enforce that:
#   1. lumen-mcp binary missing        → never block (server cannot be serving)
#   2. same file intercepted twice     → yield (the lumen route did not work)
#
# A fired guard is a fault: routing degraded in the field. Each one appends a line
# to the JSONL fault spool, which `lumen report` drains. Nothing here touches
# SQLite — this hook decides whether the model may read a file at all, so it must
# never wait on a database lock. The extra work is on the fail-open path only; the
# ordinary blocking path is unchanged.
#
# Controls (env vars, set before launching Claude Code or in shell profile):
#   LUMEN_HOOK_ENABLED=0       — disable hard routing (soft layer still active)
#   LUMEN_LINE_THRESHOLD=300   — min lines before intercepting (default: 300)
#   LUMEN_MCP_BIN=<path>       — lumen-mcp binary used as the liveness probe
#                                (default: <workspace>/target/release/lumen-mcp)
#   LUMEN_FAULT_SPOOL=<path>   — fault spool (default: faults.jsonl beside the DB)
#   LUMEN_CAPTURE=0            — record nothing (routing behaviour unchanged)

set -euo pipefail

INPUT=$(cat)   # full hook JSON from stdin

HOOK_ENABLED="${LUMEN_HOOK_ENABLED:-1}"
THRESHOLD="${LUMEN_LINE_THRESHOLD:-300}"

# Fast-exit when hook is disabled (soft-only measurement mode)
if [ "$HOOK_ENABLED" = "0" ]; then
    exit 0
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"

# Resolve the spool the same way lumen_core::faults does, so `lumen report` looks
# where this wrote: explicit override, else beside whichever database is in play.
fault_spool_path() {
    if [ -n "${LUMEN_FAULT_SPOOL:-}" ]; then
        printf '%s\n' "$LUMEN_FAULT_SPOOL"
        return 0
    fi
    local db=""
    if [ -n "${LUMEN_DB:-}" ]; then
        db="$LUMEN_DB"
    elif [ -f "${HOME}/.lumen_db_path" ]; then
        db="$(cat "${HOME}/.lumen_db_path" 2>/dev/null || true)"
    fi
    if [ -n "$db" ]; then
        printf '%s\n' "$(dirname "$db")/faults.jsonl"
    else
        printf '%s\n' "${WORKSPACE_ROOT}/faults.jsonl"
    fi
}

# Append one hook_fail_open record. Best-effort: a hook that cannot record a fault
# must not turn that into a second fault, so every failure here is swallowed.
record_fault() {
    [ "${LUMEN_CAPTURE:-1}" != "0" ] || return 0
    local guard="$1" fpath="$2" flines="$3" fsession="$4" spool
    spool="$(fault_spool_path)"
    [ -n "$spool" ] || return 0

    python3 - "$spool" "$guard" "$fpath" "$flines" "$fsession" <<'PY' 2>/dev/null || true
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
    # Left null on purpose: the hook is a shell script and has no build version to
    # claim. `lumen report` stamps the reporting build's version instead.
    "version": None,
    "channel": "cli",
}
with open(spool, "a") as fh:
    fh.write(json.dumps(record) + "\n")
PY
}

# Extract tool_name, file_path and session_id using python3 (stdlib json,
# always available). Tab-separated so one process covers all three fields.
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

# Must be a real, readable file
if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

# Check extension — only intercept languages lumen handles + log files
EXT=$(echo "${FILE_PATH##*.}" | tr '[:upper:]' '[:lower:]')
case "$EXT" in
    rs|py|pyi|ts|tsx)
        FILE_TYPE="source"
        ;;
    log|out|output|txt)
        FILE_TYPE="log"
        ;;
    *)
        exit 0
        ;;
esac

# Count lines (fast, no token loading)
LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null | tr -d '[:space:]' || echo 0)
LINE_COUNT="${LINE_COUNT:-0}"

if [ "$LINE_COUNT" -lt "$THRESHOLD" ]; then
    exit 0
fi

# Both guards sit below the extension and threshold checks, not above them: a fired
# guard means "this read would have been redirected and could not be", so a file we
# were never going to intercept must not be recorded as a routing failure.

# Fail-open guard 1: if lumen-mcp is not on this machine, the MCP server cannot
# be serving smart_read/recall_file/compress_logs — redirecting there would
# strand the model with no way to read the file at all.
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-${WORKSPACE_ROOT}/target/release/lumen-mcp}"

if [ ! -x "$LUMEN_MCP_BIN" ] && ! command -v lumen-mcp >/dev/null 2>&1; then
    record_fault "lumen_mcp_missing" "$FILE_PATH" "$LINE_COUNT" "$SESSION_ID"
    exit 0
fi

# Fail-open guard 2: one redirect per file per session. A model coming back to
# built-in Read for the same file has already been told to use lumen; if it is
# asking again the lumen route failed for it (server down, permission denied,
# syntax it cannot outline). Yield instead of deadlocking the session.
SESSION_KEY="${SESSION_ID//[^A-Za-z0-9_-]/}"
STATE_FILE="${TMPDIR:-/tmp}"
STATE_FILE="${STATE_FILE%/}/lumen_intercept_${SESSION_KEY:-nosession}"

if [ -f "$STATE_FILE" ] && grep -Fxq -- "$FILE_PATH" "$STATE_FILE" 2>/dev/null; then
    record_fault "retry_escape_valve" "$FILE_PATH" "$LINE_COUNT" "$SESSION_ID"
    exit 0
fi
printf '%s\n' "$FILE_PATH" >> "$STATE_FILE" 2>/dev/null || true

# Block + redirect with a message the model can act on.
# Phrasing is directive, not conversational — the model should retry immediately.
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
