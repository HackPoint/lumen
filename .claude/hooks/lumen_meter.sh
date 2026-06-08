#!/usr/bin/env bash
# lumen_meter.sh — PostToolUse hook: records ONLY built-in Read events.
#
# mcp__lumen__* tools self-meter directly (works in both CLI and VS Code).
# This hook handles only the built-in Read tool, which fires in CLI only,
# providing the "missed optimization" baseline: reads that bypassed lumen.
#
# Writes to read_events with routed_via=builtin_read, channel=cli, saved_tokens=0.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"
LUMEN_DB="${LUMEN_DB:-${WORKSPACE_ROOT}/lumen.db}"
LUMEN_TOK="${LUMEN_TOK:-${WORKSPACE_ROOT}/target/release/lumen-tok}"

INPUT=$(cat)

if [ "${LUMEN_DEBUG:-}" = "1" ]; then
    echo "$INPUT" > /tmp/lumen_hook_dump.json
fi

TOOL_NAME=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_name', ''))
" "$INPUT" 2>/dev/null || echo "")

# Only handle built-in Read; all mcp__lumen__* tools self-meter.
if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

FILE_PATH=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_input', {}).get('file_path', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null || echo 0)

if [ -x "$LUMEN_TOK" ]; then
    FULL_TOKENS=$("$LUMEN_TOK" < "$FILE_PATH" 2>/dev/null || echo 0)
else
    FULL_TOKENS=$(( $(wc -c < "$FILE_PATH") / 4 ))
fi

sqlite3 "$LUMEN_DB" \
    "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,routed_via,channel)
     VALUES('${TS}','Read','${FILE_PATH//\'/\'\'}',${LINE_COUNT},${FULL_TOKENS},${FULL_TOKENS},0,'builtin_read','cli');" \
    2>/dev/null || true

exit 0
