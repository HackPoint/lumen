#!/usr/bin/env bash
# lumen_report.sh — query read_events and print adoption + savings summary.
#
# Usage:
#   ./.claude/lumen_report.sh                    # summary since all time
#   ./.claude/lumen_report.sh --since 1h         # last hour
#   ./.claude/lumen_report.sh --since 30m        # last 30 minutes
#   LUMEN_DB=/path/to/lumen.db ./.claude/lumen_report.sh

set -euo pipefail

# Resolve the DB the same way every writer does: LUMEN_DB, then the pointer file the
# GUI writes, then the canonical per-OS location. It used to default to
# <workspace>/lumen.db, which is the shadow ledger the metering writers created when
# LUMEN_DB was unset — so this script reported on 195 events while the real ledger
# held 4,140. Never guess a path from where the script happens to live.
resolve_db() {
    if [ -n "${LUMEN_DB:-}" ]; then printf '%s' "$LUMEN_DB"; return; fi
    if [ -r "$HOME/.lumen_db_path" ]; then
        local p; p=$(tr -d '\n' < "$HOME/.lumen_db_path")
        if [ -n "$p" ]; then printf '%s' "$p"; return; fi
    fi
    case "$(uname -s)" in
        Darwin) printf '%s' "$HOME/Library/Application Support/io.speedata.lumen/lumen.db" ;;
        *)      printf '%s' "${XDG_DATA_HOME:-$HOME/.local/share}/io.speedata.lumen/lumen.db" ;;
    esac
}
LUMEN_DB="$(resolve_db)"

if [ ! -r "$LUMEN_DB" ]; then
    echo "No metering database at: $LUMEN_DB" >&2
    echo "Set LUMEN_DB explicitly, or launch Lumen once so it writes the pointer file." >&2
    exit 1
fi

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

-- ── Unmeasurable reads ───────────────────────────────────────────────────────
-- Built-in Reads of files with no token count: images, binaries. These are
-- excluded from the missed-optimization metric because no optimization was
-- available to miss, and printed here so the exclusion is auditable rather than
-- implied. Rows written before 1.2.1 are NOT in this count: back then a failing
-- tokenizer produced a bytes/4 estimate labelled 'estimated', indistinguishable
-- from a genuinely broken tokenizer on a text file. Compare 'estimated' below
-- against the extension breakdown before trusting any historical total.
SELECT
    COALESCE(token_source, '(unlabelled)')                    AS token_source,
    COUNT(*)                                                  AS events,
    SUM(full_tokens)                                          AS full_tokens
FROM  read_events
WHERE routed_via = 'builtin_read'
GROUP BY 1
ORDER BY events DESC;

SELECT '' AS '';

-- Extension breakdown of everything still labelled 'estimated', which is where a
-- pre-1.2.1 binary read hides.
SELECT
    CASE WHEN path LIKE '%.%'
         THEN lower(replace(path, rtrim(path, replace(path, '.', '')), ''))
         ELSE '(none)' END                                    AS ext,
    COUNT(*)                                                  AS events,
    SUM(full_tokens)                                          AS full_tokens_recorded
FROM  read_events
WHERE routed_via = 'builtin_read' AND token_source = 'estimated'
GROUP BY 1
ORDER BY full_tokens_recorded DESC
LIMIT 12;

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
echo "── Token provenance ─────────────────────────────────────────────────────────"
sqlite3 -column -header "$LUMEN_DB" <<SQL
SELECT COALESCE(token_source,'unknown')  AS provenance,
       COUNT(*)                          AS events,
       SUM(full_tokens)                  AS tokens
FROM read_events
${SINCE_CLAUSE}
GROUP BY provenance ORDER BY events DESC;
SQL

echo ""
echo "── Negative savings (calls that cost more than they saved) ──────────────────"
echo "Reported separately, never netted into the totals above: a loss is a"
echo "measurement, and hiding it in an average is how the clamp went unnoticed."
sqlite3 -column -header "$LUMEN_DB" <<SQL
SELECT routed_via,
       COUNT(*)            AS losses,
       SUM(saved_tokens)   AS net_tokens,
       MIN(saved_tokens)   AS worst,
       CAST(AVG(full_tokens) AS INT) AS avg_file_tokens
FROM read_events
WHERE saved_tokens < 0 AND routed_via <> 'builtin_read'
${SINCE_CLAUSE:+AND ${SINCE_CLAUSE#WHERE }}
GROUP BY routed_via ORDER BY losses DESC;
SQL

echo ""
echo "DB: $LUMEN_DB"
echo ""
echo "Toggle hard routing:  LUMEN_HOOK_ENABLED=0 (off) / 1 (on, default)"
echo "Toggle threshold:     LUMEN_LINE_THRESHOLD=300 (default)"
echo "Clear measurements:   sqlite3 \$LUMEN_DB 'DELETE FROM read_events;'"
