#!/usr/bin/env bash
# lumen_read_intercept.sh — PreToolUse hook for the Read tool.
#
# Blocks large source/log files and redirects the model to lumen optimizer tools.
# Uses exit 2 + stderr message: Claude Code shows the stderr text to the model.
#
# Controls (env vars, set before launching Claude Code or in shell profile):
#   LUMEN_HOOK_ENABLED=0       — disable hard routing (soft layer still active)
#   LUMEN_LINE_THRESHOLD=300   — min lines before intercepting (default: 300)

set -euo pipefail

INPUT=$(cat)   # full hook JSON from stdin

HOOK_ENABLED="${LUMEN_HOOK_ENABLED:-1}"
THRESHOLD="${LUMEN_LINE_THRESHOLD:-300}"

# Fast-exit when hook is disabled (soft-only measurement mode)
if [ "$HOOK_ENABLED" = "0" ]; then
    exit 0
fi

# Extract tool_name and file_path using python3 (stdlib json, always available)
TOOL_NAME=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_name', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

FILE_PATH=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_input', {}).get('file_path', ''))
" "$INPUT" 2>/dev/null || echo "")

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
    log|out|txt)
        FILE_TYPE="log"
        ;;
    *)
        exit 0
        ;;
esac

# Count lines (fast, no token loading)
LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null || echo 0)

if [ "$LINE_COUNT" -lt "$THRESHOLD" ]; then
    exit 0
fi

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
MSG
else
    cat >&2 <<MSG
Lumen intercept: ${FILE_PATH} is ${LINE_COUNT} lines.
Instead of reading the full file, call:
  1. lumen:smart_read(path="${FILE_PATH}")       → structural outline, ~5-10% token cost
  2. lumen:recall_file(path="${FILE_PATH}", names=["<item>"]) → fetch only what you need
This typically saves 80-93% of context vs. reading the whole file.
Use smart_read(mode="full") only if you truly need every line.
MSG
fi

exit 2
