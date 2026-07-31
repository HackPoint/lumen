#!/usr/bin/env bash
# lumen_meter.sh — PostToolUse hook: records ONLY built-in Read events.
#
# mcp__lumen__* tools self-meter directly (works in both CLI and VS Code).
# This hook handles only the built-in Read tool, which fires in CLI only,
# providing the "missed optimization" baseline: reads that bypassed lumen.
#
# Writes to read_events with routed_via=builtin_read, saved_tokens=0.
#
# This is the developer copy. Setup installs its own from a template in setup.rs, and the
# two drifted badly: this one wrote nine columns where the installed one writes fifteen,
# and it resolved the database as <workspace>/lumen.db — a path with no schema, so every
# INSERT failed with "no such table: read_events" and `|| true` threw the error away. It
# recorded nothing at all, for weeks, while looking like it worked. The column set is now
# the same (asserted by lumen-core's meter_hooks_agree test) and a failed write leaves a
# line in lumen_hook_errors.log beside the database instead of vanishing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"

# Resolve the database the way lumen_core::meter does, rather than assuming the workspace.
# Assuming it is what made this hook write to a file that never had a schema.
resolve_db() {
    if [ -n "${LUMEN_DB:-}" ]; then printf '%s\n' "$LUMEN_DB"; return; fi
    if [ -f "${HOME}/.lumen_db_path" ]; then
        local p; p="$(cat "${HOME}/.lumen_db_path" 2>/dev/null || true)"
        if [ -n "$p" ]; then printf '%s\n' "$p"; return; fi
    fi
    case "$(uname -s)" in
        Darwin) printf '%s\n' "${HOME}/Library/Application Support/io.speedata.lumen/lumen.db" ;;
        *)      printf '%s\n' "${HOME}/.local/share/io.speedata.lumen/lumen.db" ;;
    esac
}

LUMEN_DB="$(resolve_db)"
LUMEN_TOK="${LUMEN_TOK:-${WORKSPACE_ROOT}/target/release/lumen-tok}"

INPUT=$(cat)

if [ "${LUMEN_DEBUG:-}" = "1" ]; then
    echo "$INPUT" > /tmp/lumen_hook_dump.json
fi

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

# Only handle built-in Read; all mcp__lumen__* tools self-meter.
if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null | tr -d '[:space:]' || echo 0)
LINE_COUNT="${LINE_COUNT:-0}"

# token_source records WHICH of these produced the count. Without it a bytes/4 estimate is
# indistinguishable from a real measurement, which is the whole point of the column.
if [ -x "$LUMEN_TOK" ]; then
    FULL_TOKENS=$("$LUMEN_TOK" < "$FILE_PATH" 2>/dev/null || echo 0)
    TOKEN_SOURCE="measured"
else
    FULL_TOKENS=$(( $(wc -c < "$FILE_PATH") / 4 ))
    TOKEN_SOURCE="estimated"
fi

# Parameterised, not interpolated: the previous version spliced the path into SQL and
# hand-escaped quotes, which is one apostrophe away from a broken statement.
python3 - "$LUMEN_DB" "$FILE_PATH" "$LINE_COUNT" "$FULL_TOKENS" "$SESSION_ID" "$TOKEN_SOURCE" <<'PY' 2>/dev/null || true
import os, sqlite3, sys, time

db, path, lines, full, sid, tsrc = sys.argv[1:7]
ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
try:
    mtime = int(os.path.getmtime(path))
except OSError:
    mtime = None
# Same dedup key the installed hook uses, so a repeated read of an unchanged file can be
# recognised rather than counted twice.
req = f"{sid}:{path}:{mtime}" if sid else None

try:
    con = sqlite3.connect(db)
    con.execute(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,"
        "saved_tokens,routed_via,channel,session_id,file_mtime,req_key,is_subagent,"
        "writer_hook,token_source) VALUES(?,?,?,?,?,?,0,?,?,?,?,?,0,?,?)",
        (ts, "Read", path, int(lines), int(full), int(full), "builtin_read", "cli",
         sid or None, mtime, req, "repo:.claude/hooks/lumen_meter.sh", tsrc),
    )
    con.commit()
    con.close()
except Exception as e:
    # Never fail the hook — a metering miss must not break the session. But do not vanish
    # either: swallowing this silently is exactly why nothing was recorded for weeks.
    try:
        log = os.path.join(os.path.dirname(db) or ".", "lumen_hook_errors.log")
        with open(log, "a") as fh:
            fh.write(f"{ts} lumen_meter.sh: {type(e).__name__}: {e}\n")
    except Exception:
        pass
PY

exit 0
