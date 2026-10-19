#!/usr/bin/env bash
# lumen-generator: 1.9.0
# lumen_read_intercept.sh — installed by Lumen Setup.
# Setup regenerates it when it drifts from the running build; do not hand-edit.
#
# PreToolUse hook for Read and for Lumen's own tools. `lumen-mcp hook intercept`
# decides whether a large source or log file goes to the lumen tools (exit 2) or
# is read as asked (exit 0), and records which conversation each smart_read,
# recall_file or compress_logs call belongs to, so the server can tell /clear from
# a repeat. Without that binary the MCP server cannot be running either, so the
# call is let through, with a fault and a line on stderr. LUMEN_HOOK_ENABLED=0
# switches the Read intercept off; it does not switch the recording off.
#
# Each baked value is a default, not a constant, so a test can point the hook at a
# scratch ledger and a stub binary without editing it.
default_mcp='/H/.lumen-bench/bin/lumen-mcp'
default_db='/H/.lumen-bench/ledger.db'
default_spool='/H/.lumen-bench/faults.jsonl'
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-$default_mcp}"
export LUMEN_DB="${LUMEN_DB:-$default_db}"
export LUMEN_FAULT_SPOOL="${LUMEN_FAULT_SPOOL:-$default_spool}"

# A failed exec falls through to the report below instead of ending the script.
shopt -s execfail
bin="$LUMEN_MCP_BIN"
[ -x "$bin" ] || bin="$(command -v lumen-mcp 2>/dev/null)"
[ -n "$bin" ] && exec "$bin" hook intercept

# Lumen was moved or removed. Exit 0: the tools a block would send the model to
# are served by the binary that is missing.
cat >/dev/null
[ "${LUMEN_HOOK_ENABLED:-1}" = "0" ] && exit 0
kind=hook_fail_open
consequence="this read was let through"
if [ -n "$bin" ]; then variant=lumen_mcp_unrunnable; else variant=lumen_mcp_missing; fi
echo "lumen: cannot run lumen-mcp (looked for '$LUMEN_MCP_BIN', then on PATH); $consequence" >&2
[ "${LUMEN_CAPTURE:-1}" = "0" ] && exit 0

# A value enters the JSON line only if it cannot break it.
json() { case "$1" in "" | *[!A-Za-z0-9._-]*) printf 'null' ;; *) printf '"%s"' "$1" ;; esac; }
version=""
while IFS= read -r line; do
    case "$line" in "# lumen-generator: "*) version="${line#"# lumen-generator: "}"; break ;; esac
done 2>/dev/null <"$0"
# lumen_core::meter::channel_from, in shell.
case "${CLAUDE_CODE_ENTRYPOINT:-}" in
    *vscode*) channel=vscode ;;
    ?*) channel=cli ;;
    *) if [ -n "${VSCODE_PID+x}${VSCODE_CWD+x}" ]; then channel=vscode; else channel=unknown; fi ;;
esac
case "${LUMEN_CHANNEL:-}" in "" | *[!A-Za-z0-9._-]*) ;; *) channel="$LUMEN_CHANNEL" ;; esac
printf '{"ts":"%s","kind":"%s","variant":"%s","path":null,"lines":null,"detail":null,"session_id":%s,"version":%s,"channel":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$kind" "$variant" \
    "$(json "${LUMEN_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}")" "$(json "$version")" "$channel" \
    2>/dev/null >>"$LUMEN_FAULT_SPOOL" \
    || echo "lumen: nor could the fault be written to '$LUMEN_FAULT_SPOOL'" >&2
exit 0
