#!/usr/bin/env bash
# lumen-generator: 1.5.1
# lumen_read_intercept.sh — the Claude Code plugin's copy of the hook
# Lumen Setup installs, generated from the same template in setup.rs. Do not
# hand-edit; regenerate with `LUMEN_BLESS_HOOKS=1 cargo test -p Lumen`.
#
# PreToolUse hook for Read. `lumen-mcp hook intercept` decides whether a large
# source or log file goes to the lumen tools (exit 2) or is read as asked (exit 0).
# Without that binary the MCP server cannot be running either, so the read is let
# through, with a fault and a line on stderr. LUMEN_HOOK_ENABLED=0 switches the
# intercept off.
#
# Nothing is baked in: a plugin cannot know where Lumen is installed. The binary is
# the one built in this checkout, else lumen-mcp on PATH, and lumen-mcp finds the
# ledger itself. LUMEN_MCP_BIN overrides the first.
here="${BASH_SOURCE[0]%[/\\]*}"
[ "$here" = "${BASH_SOURCE[0]}" ] && here=.
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-$here/../../target/release/lumen-mcp}"

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
# Where lumen-mcp would have put the fault: LUMEN_FAULT_SPOOL, else faults.jsonl
# beside the ledger, which is LUMEN_DB, else ~/.lumen_db_path, else the per-OS one.
if [ -z "${LUMEN_FAULT_SPOOL:-}" ]; then
    db="${LUMEN_DB:-}"
    home="${HOME:-${USERPROFILE:-}}"
    if [ -z "$db" ] && [ -n "$home" ]; then
        db="$(cat "$home/.lumen_db_path" 2>/dev/null)"
        db="${db#"${db%%[![:space:]]*}"}"
        db="${db%"${db##*[![:space:]]}"}"
    fi
    if [ -z "$db" ] && [ -n "$home" ]; then
        case "${OSTYPE:-}" in
            darwin*) db="$home/Library/Application Support/io.speedata.lumen/lumen.db" ;;
            msys* | cygwin*) db="$home/AppData/Roaming/io.speedata.lumen/lumen.db" ;;
            *) db="$home/.local/share/io.speedata.lumen/lumen.db" ;;
        esac
    fi
    case "$db" in
        */* | *\\*) LUMEN_FAULT_SPOOL="${db%[/\\]*}/faults.jsonl" ;;
        ?*) LUMEN_FAULT_SPOOL=faults.jsonl ;;
    esac
fi
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
