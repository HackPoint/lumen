#!/usr/bin/env bash
# lumen-generator: 1.9.0
# lumen_meter.sh — installed by Lumen Setup.
# Setup regenerates it when it drifts from the running build; do not hand-edit.
#
# PostToolUse hook for Read and Bash. `lumen-mcp hook meter` does the metering;
# this finds that binary, and when it cannot, leaves a fault and a line on stderr
# instead of losing the event in silence. It needs bash, cat and date.
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
[ -n "$bin" ] && exec "$bin" hook meter --writer lumen_meter.sh

# Lumen was moved or removed. Exit 0 all the same: a meter must never fail the
# tool call it observes.
cat >/dev/null
kind=meter_write_failed
consequence="this event was not metered"
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
