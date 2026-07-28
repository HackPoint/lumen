// lumen-stats — read-only aggregate queries over the metering DB.
//
// HONESTY (carried over from the GUI, where this code used to live): these are
// CONSUMPTION figures. Plan quota size / remaining / the real reset are
// server-side and unknown locally, so nothing here is a "% of limit" or
// "remaining". Dollar costs are NOT computed here — the frontend applies the
// single RATE table to these token sums so there is one price source of truth.
//
// Every struct is the wire format the Angular frontend consumes. The
// `rename_all = "camelCase"` attributes are load-bearing: renaming a field here
// silently breaks the UI.

use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

/// The three `routed_via` values that represent a Lumen tool call which returned
/// fewer tokens than the full file. `builtin_read` is deliberately excluded — it
/// records a *missed* optimization and must never count as a saving.
pub const LUMEN_ROUTES: [&str; 3] = ["smart_read", "recall_file", "compress_logs"];

/// Resolve the metering DB into a sqlx connection URL.
pub fn db_url() -> String {
    let p = std::env::var("LUMEN_DB").unwrap_or_else(|_| "../../lumen.db".to_string());
    format!("sqlite:{p}?mode=rwc")
}

/// Open a pool against `url`.
pub async fn connect(url: &str) -> Result<SqlitePool, String> {
    SqlitePoolOptions::new()
        .connect(url)
        .await
        .map_err(|e| e.to_string())
}

/// Open a pool against the ambient `LUMEN_DB`.
pub async fn connect_default() -> Result<SqlitePool, String> {
    connect(&db_url()).await
}

// ── Basic stats ──────────────────────────────────────────────────────────────

#[derive(Serialize, Debug, PartialEq)]
pub struct Stats {
    pub turns: i64,
    pub output_total: i64,
    pub factor: f64,
}

pub async fn get_stats(pool: &SqlitePool) -> Result<Stats, String> {
    let (turns, output_total): (i64, i64) =
        sqlx::query_as("SELECT COUNT(*), COALESCE(SUM(output_tokens),0) FROM turns")
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;

    let factor: (Option<f64>,) = sqlx::query_as("SELECT factor FROM correction_factor")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(Stats {
        turns,
        output_total,
        // A NULL factor means "not yet calibrated" — 1.0 is the identity, never 0.
        factor: factor.0.unwrap_or(1.0),
    })
}

// ── Usage & cost aggregates ──────────────────────────────────────────────────

#[derive(Serialize, Default, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TokenAgg {
    pub turns: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub total_tokens: i64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub rolling_5h: TokenAgg,
    /// earliest turn inside the trailing 5h window (ISO-8601 UTC), or null
    pub window_start: Option<String>,
    /// PROXY reset = window_start + 5h. NOT the real server reset — "approx".
    pub reset_approx: Option<String>,
    pub rolling_7d_opus: TokenAgg,
    pub rolling_7d_other: TokenAgg,
    pub today: TokenAgg,
    pub this_week: TokenAgg,
    pub all_time: TokenAgg,
}

/// Run the standard token-aggregate SELECT with a caller-supplied WHERE clause
/// (pass "" for all-time). The `&'static str` bound is what makes the
/// `AssertSqlSafe` below sound: no runtime value can reach the clause, so no
/// user input is interpolated.
pub async fn fetch_agg(pool: &SqlitePool, where_clause: &'static str) -> Result<TokenAgg, String> {
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0)
         FROM turns {where_clause}"
    );
    let t: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(TokenAgg {
        turns: t.0,
        input: t.1,
        output: t.2,
        cache_read: t.3,
        cache_write: t.4,
        total_tokens: t.5,
    })
}

/// WHERE clause for "today" in the user's local timezone.
pub const WHERE_TODAY: &str = "WHERE date(ts,'localtime') = date('now','localtime')";

/// WHERE clause for "this week", week starting MONDAY (ISO-8601):
/// strftime('%w') is 0=Sun..6=Sat, so (%w + 6) % 7 = days since Monday.
pub const WHERE_THIS_WEEK: &str = "WHERE date(ts,'localtime') >= \
     date('now','localtime','-'||((strftime('%w','now','localtime')+6)%7)||' days')";

pub async fn get_usage(pool: &SqlitePool) -> Result<UsageReport, String> {
    // (a) Rolling 5h consumption. `ts` is ISO-8601 UTC ('…Z'); datetime()
    //     normalizes both sides to canonical UTC so the comparison is correct.
    //     reset_approx is a PROXY (window_start + 5h), not the server reset.
    let r5: (i64, i64, i64, i64, i64, i64, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0),
                MIN(ts),
                datetime(MIN(ts), '+5 hours')
         FROM turns
         WHERE datetime(ts) >= datetime('now','-5 hours')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // (b) Rolling 7d consumption, split Opus vs other.
    let rows: Vec<(String, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT CASE WHEN model LIKE '%opus%' THEN 'opus' ELSE 'other' END AS model_class,
                COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(input_tokens + output_tokens
                           + cache_read_input_tokens + cache_creation_input_tokens),0)
         FROM turns
         WHERE datetime(ts) >= datetime('now','-7 days')
         GROUP BY model_class",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let mut opus = TokenAgg::default();
    let mut other = TokenAgg::default();
    for (class, turns, input, output, cache_read, cache_write, total_tokens) in rows {
        let agg = TokenAgg {
            turns,
            input,
            output,
            cache_read,
            cache_write,
            total_tokens,
        };
        if class == "opus" {
            opus = agg;
        } else {
            other = agg;
        }
    }

    // (c) Calendar rollups in LOCAL time.
    let today = fetch_agg(pool, WHERE_TODAY).await?;
    let this_week = fetch_agg(pool, WHERE_THIS_WEEK).await?;
    let all_time = fetch_agg(pool, "").await?;

    Ok(UsageReport {
        rolling_5h: TokenAgg {
            turns: r5.0,
            input: r5.1,
            output: r5.2,
            cache_read: r5.3,
            cache_write: r5.4,
            total_tokens: r5.5,
        },
        window_start: r5.6,
        reset_approx: r5.7,
        rolling_7d_opus: opus,
        rolling_7d_other: other,
        today,
        this_week,
        all_time,
    })
}

// ── Session history ──────────────────────────────────────────────────────────
//
// One summary row per session_id, newest activity first. Reads straight from the
// DB, so it is independent of the frontend's in-memory live-session cap and can
// list far more sessions.
//
// `peak_cache_read` (MAX cache_read in the session) is the peak-context-fill
// proxy, mirroring the live gauge's `fill`.

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub model: Option<String>,
    pub first_ts: String,
    pub last_ts: String,
    pub turn_count: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub total_tokens: i64,
    pub peak_cache_read: i64,
}

/// Cap on rows returned, mirroring the SQL `LIMIT`.
pub const SESSION_LIMIT: i64 = 100;

pub async fn get_sessions(pool: &SqlitePool) -> Result<Vec<SessionSummary>, String> {
    // One row per session. `model` = the most recent non-null model seen in the
    // session (sessions are normally single-model). Newest activity first.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<String>,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(
        "SELECT
            session_id,
            (SELECT t2.model FROM turns t2
                WHERE t2.session_id = t.session_id AND t2.model IS NOT NULL
                ORDER BY t2.ts DESC LIMIT 1)                                       AS model,
            MIN(ts)                                                                AS first_ts,
            MAX(ts)                                                                AS last_ts,
            COUNT(*)                                                               AS turn_count,
            COALESCE(SUM(input_tokens),0),
            COALESCE(SUM(output_tokens),0),
            COALESCE(SUM(cache_read_input_tokens),0),
            COALESCE(SUM(cache_creation_input_tokens),0),
            COALESCE(SUM(input_tokens + output_tokens
                       + cache_read_input_tokens + cache_creation_input_tokens),0) AS total_tokens,
            -- Peak fill EXCLUDES subagent turns: they reuse the parent's
            -- sessionId but carry their own fresh context, so counting them
            -- would misreport how full this session's context actually got.
            -- Their tokens stay in the SUMs above, because they are real spend.
            (SELECT COALESCE(MAX(cache_read_input_tokens),0) FROM turns t4
               WHERE t4.session_id = t.session_id AND t4.is_subagent = 0)       AS peak_cache_read
         FROM turns t
         GROUP BY session_id
         ORDER BY MAX(ts) DESC
         LIMIT 100",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| SessionSummary {
            session_id: r.0,
            model: r.1,
            first_ts: r.2,
            last_ts: r.3,
            turn_count: r.4,
            input: r.5,
            output: r.6,
            cache_read: r.7,
            cache_write: r.8,
            total_tokens: r.9,
            peak_cache_read: r.10,
        })
        .collect())
}

// ── Optimizer savings ────────────────────────────────────────────────────────
//
// Savings CAUSED by Lumen. Distinct from "caching saved", which is REPORTED by
// Claude Code and stored in the turns table's cache_read column.
//
// saved_tokens is exact BPE measured per call. Dollar conversion uses RATE.input
// (these are input token reads); the frontend owns RATE and applies it so there
// is one price source of truth.
//
// builtin_read rows have saved_tokens=0 and represent CLI-only "missed
// optimizations" — surfaced honestly but NEVER counted as savings.

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBreakdown {
    pub channel: String,
    pub calls: i64,
    pub saved_tokens: i64,
    pub full_tokens: i64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolBreakdown {
    pub tool: String,
    pub calls: i64,
    pub saved_tokens: i64,
    pub full_tokens: i64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OptimizerReport {
    /// SUM(saved_tokens) over lumen routes — CAUSED by Lumen, not reported.
    /// Convert to USD in the frontend: lifetimeOptimizedTokens * RATE.input.
    pub lifetime_optimized_tokens: i64,
    /// SUM(full_tokens) over lumen routes — denominator for effectivenessPct.
    pub lifetime_full_tokens: i64,
    /// Calendar rollups (local time, same method as get_usage).
    pub today_saved_tokens: i64,
    pub this_week_saved_tokens: i64,
    /// Per-channel breakdown (cli | vscode | unknown).
    pub by_channel: Vec<ChannelBreakdown>,
    /// Per-tool breakdown (smart_read | recall_file | compress_logs).
    pub by_tool: Vec<ToolBreakdown>,
    /// Channel of the most recent read_events row — proxy for active context.
    pub current_channel: String,
    /// CLI-only: reads that bypassed Lumen (builtin_read, channel=cli).
    /// Label as "not optimized (read in full)". Never count as savings.
    pub missed_calls: i64,
    pub missed_full_tokens: i64,
}

pub async fn get_optimizer_stats(pool: &SqlitePool) -> Result<OptimizerReport, String> {
    // ── Lifetime totals ──────────────────────────────────────────────────────
    let (lifetime_optimized_tokens, lifetime_full_tokens): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Calendar rollups (local time — same method as get_usage) ─────────────
    let (today_saved_tokens,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
           AND date(ts,'localtime') = date('now','localtime')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // Week starts Monday (ISO-8601): (strftime('%w')+6)%7 = days since Monday.
    let (this_week_saved_tokens,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(saved_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
           AND date(ts,'localtime') >= date('now','localtime',
               '-'||((strftime('%w','now','localtime')+6)%7)||' days')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Per-channel breakdown ────────────────────────────────────────────────
    let channel_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT channel, COUNT(*),
                COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
         GROUP BY channel
         ORDER BY SUM(saved_tokens) DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let by_channel = channel_rows
        .into_iter()
        .map(
            |(channel, calls, saved_tokens, full_tokens)| ChannelBreakdown {
                channel,
                calls,
                saved_tokens,
                full_tokens,
            },
        )
        .collect();

    // ── Per-tool breakdown ───────────────────────────────────────────────────
    let tool_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT routed_via, COUNT(*),
                COALESCE(SUM(saved_tokens),0), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs')
         GROUP BY routed_via
         ORDER BY SUM(saved_tokens) DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let by_tool = tool_rows
        .into_iter()
        .map(|(tool, calls, saved_tokens, full_tokens)| ToolBreakdown {
            tool,
            calls,
            saved_tokens,
            full_tokens,
        })
        .collect();

    // ── Current channel (most recent event) ──────────────────────────────────
    let current_channel: (Option<String>,) =
        sqlx::query_as("SELECT channel FROM read_events ORDER BY ts DESC LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap_or((None,));

    // ── CLI missed reads ─────────────────────────────────────────────────────
    // builtin_read rows written by the CLI PostToolUse hook when the model used
    // the built-in Read instead of lumen tools. saved_tokens=0 always.
    let (missed_calls, missed_full_tokens): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(full_tokens),0)
         FROM read_events
         WHERE routed_via = 'builtin_read' AND channel = 'cli'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(OptimizerReport {
        lifetime_optimized_tokens,
        lifetime_full_tokens,
        today_saved_tokens,
        this_week_saved_tokens,
        by_channel,
        by_tool,
        current_channel: current_channel.0.unwrap_or_else(|| "unknown".to_string()),
        missed_calls,
        missed_full_tokens,
    })
}
