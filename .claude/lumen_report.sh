#!/usr/bin/env bash
# lumen_report.sh — query read_events and print adoption + savings summary.
#
# Usage:
#   ./.claude/lumen_report.sh                    # summary since all time
#   ./.claude/lumen_report.sh --since 1h         # last hour
#   ./.claude/lumen_report.sh --since 30m        # last 30 minutes
#   LUMEN_DB=/path/to/lumen.db ./.claude/lumen_report.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(dirname "$SCRIPT_DIR")"
LUMEN_DB="${LUMEN_DB:-${WORKSPACE_ROOT}/lumen.db}"

# Optional time filter
SINCE_CLAUSE=""
if [ "${1:-}" = "--since" ] && [ -n "${2:-}" ]; then
    WINDOW="${2}"
    # Parse: 1h → 3600s, 30m → 1800s, 90s → 90s
    if [[ "$WINDOW" =~ ^([0-9]+)h$ ]]; then
        SECS=$(( ${BASH_REMATCH[1]} * 3600 ))
    elif [[ "$WINDOW" =~ ^([0-9]+)m$ ]]; then
        SECS=$(( ${BASH_REMATCH[1]} * 60 ))
    elif [[ "$WINDOW" =~ ^([0-9]+)s$ ]]; then
        SECS=${BASH_REMATCH[1]}
    else
        echo "Unknown time format: $WINDOW (use 1h, 30m, 90s)" >&2
        exit 1
    fi
    SINCE_CLAUSE="WHERE ts >= datetime('now', '-${SECS} seconds')"
    echo "=== Lumen read_events — last ${WINDOW} ==="
else
    echo "=== Lumen read_events — all time ==="
fi

echo ""

sqlite3 -column -header "$LUMEN_DB" <<SQL
-- ── Per-route breakdown ──────────────────────────────────────────────────────
SELECT
    routed_via                                               AS route,
    COUNT(*)                                                 AS calls,
    SUM(tokens_returned)                                     AS tok_returned,
    SUM(full_tokens)                                         AS tok_full,
    SUM(saved_tokens)                                        AS tok_saved,
    ROUND(100.0 * SUM(saved_tokens)
          / NULLIF(SUM(full_tokens), 0), 1) || '%'          AS pct_saved
FROM  read_events
${SINCE_CLAUSE}
GROUP BY routed_via
ORDER BY SUM(full_tokens) DESC;

SELECT '' AS '';

-- ── Totals row ───────────────────────────────────────────────────────────────
SELECT
    'TOTAL'                                                  AS route,
    COUNT(*)                                                 AS calls,
    SUM(tokens_returned)                                     AS tok_returned,
    SUM(full_tokens)                                         AS tok_full,
    SUM(saved_tokens)                                        AS tok_saved,
    ROUND(100.0 * SUM(saved_tokens)
          / NULLIF(SUM(full_tokens), 0), 1) || '%'          AS pct_saved
FROM  read_events
${SINCE_CLAUSE};

SELECT '' AS '';

-- ── Adoption rate ────────────────────────────────────────────────────────────
SELECT
    ROUND(100.0 * SUM(CASE WHEN routed_via != 'builtin_read' THEN 1 ELSE 0 END)
          / NULLIF(COUNT(*), 0), 1) || '%'                   AS lumen_adoption_pct,
    COUNT(*)                                                  AS total_file_reads,
    SUM(CASE WHEN routed_via != 'builtin_read' THEN 1 ELSE 0 END)
                                                             AS via_lumen,
    SUM(CASE WHEN routed_via  = 'builtin_read' THEN 1 ELSE 0 END)
                                                             AS via_builtin
FROM  read_events
${SINCE_CLAUSE};

SELECT '' AS '';

-- ── Top 5 files by full_tokens (missed vs captured) ─────────────────────────
SELECT
    path,
    routed_via,
    full_tokens,
    saved_tokens
FROM  read_events
${SINCE_CLAUSE}
ORDER BY full_tokens DESC
LIMIT 5;
SQL

echo ""
echo "DB: $LUMEN_DB"
echo ""
echo "Toggle hard routing:  LUMEN_HOOK_ENABLED=0 (off) / 1 (on, default)"
echo "Toggle threshold:     LUMEN_LINE_THRESHOLD=300 (default)"
echo "Clear measurements:   sqlite3 \$LUMEN_DB 'DELETE FROM read_events;'"
