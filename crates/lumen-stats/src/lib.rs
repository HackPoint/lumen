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

// Both moved to `lumen_core::coverage`, which is the one place that answers "would Lumen have
// handled this file?". Five places used to decide it independently and disagreed, which is how
// the efficiency report came to publish a 64.9% "bypass" rate whose real value on the honest
// denominator is 0.0%. Re-exported so every existing caller keeps compiling.
pub use lumen_core::coverage::{
    UNMEASURABLE_EXTS, not_unmeasurable_sql as not_unmeasurable_clause,
};

/// Resolve the metering DB into a sqlx connection URL.
pub fn db_url() -> String {
    let p = std::env::var("LUMEN_DB").unwrap_or_else(|_| "../../lumen.db".to_string());
    format!("sqlite:{p}?mode=rwc")
}

/// Open a pool against `url`.
pub async fn connect(url: &str) -> Result<SqlitePool, String> {
    let pool = SqlitePoolOptions::new()
        .connect(url)
        .await
        .map_err(|e| e.to_string())?;
    // Every query below references turns.is_subagent, which only exists after the
    // migrations run. Opening the GUI before the daemon has started would
    // otherwise hit "no such column" on a database created before 1.1.0.
    lumen_core::schema::init_schema(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(pool)
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

#[derive(Serialize, Debug, PartialEq)]
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
    /// Reads that bypassed Lumen (builtin_read, any channel).
    /// Label as "not optimized (read in full)". Never count as savings.
    pub missed_calls: i64,
    pub missed_full_tokens: i64,
    /// Built-in Reads of files with no token count — images, binaries. Excluded
    /// from `missed_calls` because no optimization was available to miss, and
    /// reported separately so the exclusion is visible rather than implied.
    pub unmeasurable_calls: i64,
    /// How many rows in the lifetime window have no recorded token provenance.
    ///
    /// Rows written before 1.1.5 carry no `token_source`, and on installs whose
    /// baked tokenizer path was dead the hook silently substituted `bytes / 4`.
    /// The UI must not present those as exact while any remain, so it reports the
    /// count instead of the claim. Deliberately "unverified", not "estimated":
    /// asserting they are all estimates would be its own unmeasured claim.
    pub unverified_provenance_rows: i64,
    /// Total rows considered, so the frontend can render "N of M".
    pub provenance_total_rows: i64,

    /// Net dollar value of interception: what the avoided tokens are worth, less what the
    /// extra rounds cost.
    ///
    /// The headline from 1.4.0 on. The token ratio it replaces flattered the product — a
    /// smaller reply that forces another round is a loss however good the ratio looks.
    pub net_value_usd: f64,
    pub gross_value_usd: f64,
    pub round_cost_usd: f64,
    /// Rounds a saving is assumed to keep paying for.
    ///
    /// Surfaced because the result is more sensitive to it than to anything else: the sign
    /// holds across its plausible range but the magnitude moves by an order of magnitude,
    /// so a UI that hid it would overstate its own precision. A per-call value needs the
    /// transcript replay in `scripts/lumen_percall.py`; this is a measured constant.
    pub value_rounds: f64,
    /// Rounds each intercept actually costs. Measured at 1.604 — 60.4% of `smart_read`
    /// calls are followed by a `recall_file` on the same file.
    pub pair_multiplier: f64,
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

    // ── Token provenance ─────────────────────────────────────────────────────
    // Counted over every metered row, not just lumen routes: a user's confidence in
    // the effectiveness figure depends on the whole ledger being measured.
    let (unverified_provenance_rows, provenance_total_rows): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(CASE WHEN token_source IS NULL OR token_source <> 'measured'
                                  THEN 1 ELSE 0 END),0),
                COUNT(*)
         FROM read_events",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Current channel (most recent event) ──────────────────────────────────
    let current_channel: (Option<String>,) =
        sqlx::query_as("SELECT channel FROM read_events ORDER BY ts DESC LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap_or((None,));

    // ── Missed reads ─────────────────────────────────────────────────────────
    // builtin_read rows: the model used the built-in Read instead of a lumen tool.
    // saved_tokens = 0 always.
    //
    // The `AND channel = 'cli'` that used to be here was a no-op dressed as a
    // filter. The meter hardcoded channel='cli' on every row it wrote, so the
    // clause matched 2,694 of 2,694 rows and the metric was mislabelled as
    // CLI-only. Hooks fire in the VS Code extension too — verified by 108 rows
    // written during a session whose entrypoint was claude-vscode — so there was
    // never a CLI-only population to filter for. The meter now records the real
    // channel from CLAUDE_CODE_ENTRYPOINT; until enough rows carry it, splitting
    // by channel would describe the hardcoded past, not the present.
    //
    // Rows whose token count could not be measured are excluded. A built-in Read of
    // a PNG is not a missed optimization: there is no outline to return and no
    // saving available, so counting it inflates the very number it is supposed to
    // motivate. Worse, until 1.2.1 those rows carried a bytes/4 estimate — three
    // screenshots were recorded as 119,921 tokens against roughly 2,750 actual, a
    // ~44x overstatement, which is how a fabricated multi-million-token
    // "opportunity" came to be presented as a reason to build a feature.
    //
    // The excluded rows are counted rather than dropped: a metric that quietly
    // discards part of its population reads as complete coverage when it is not.
    let exclude = not_unmeasurable_clause();
    let (missed_calls, missed_full_tokens): (i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*), COALESCE(SUM(full_tokens),0)
             FROM read_events
             WHERE routed_via = 'builtin_read'
               AND COALESCE(token_source, '') <> 'unsupported'{exclude}"
        )))
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;

    let (unmeasurable_calls,): (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM read_events
         WHERE routed_via = 'builtin_read'
           AND (COALESCE(token_source, '') = 'unsupported'
                OR NOT (1=1{exclude}))"
    )))
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // ── Net dollar value ─────────────────────────────────────────────────────
    //
    // Value of the tokens Lumen avoided, less the cost of the extra rounds interception
    // forced. Computed here rather than in the frontend because it needs the context and
    // output means from `turns`, and because one place computing it means one place to
    // correct when per-call R lands.
    //
    // The context mean comes from this installation's own turns, excluding subagents:
    // their context is not the context an interception adds to.
    let (ctx_mean, out_mean, turn_count): (Option<f64>, Option<f64>, i64) = sqlx::query_as(
        "SELECT AVG(cache_read_input_tokens), AVG(output_tokens), COUNT(*)
         FROM turns WHERE COALESCE(is_subagent,0) = 0",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // Lumen-route calls and the tokens they avoided.
    let (lumen_calls, lumen_saved): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(saved_tokens),0) FROM read_events
         WHERE routed_via IN ('smart_read','recall_file','compress_logs','ranked_outline')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // A saved token avoids one cache write and a cache read on each remaining round.
    let value_per_token = (6.25 + 0.5 * VALUE_ROUNDS) / 1e6;
    let gross = lumen_saved as f64 * value_per_token;

    // With too few turns to average, the cost side is unknown — and reporting a gross
    // figure with no cost against it is exactly the overstatement this replaces. Zero
    // both, so the UI shows nothing rather than something flattering.
    let round_cost = match (ctx_mean, out_mean) {
        (Some(c), Some(o)) if turn_count >= 200 => {
            let one_round = (c * 0.5 + o * 25.0) / 1e6;
            lumen_calls as f64 * one_round * PAIR_MULTIPLIER
        }
        _ => 0.0,
    };
    let gross = if round_cost == 0.0 { 0.0 } else { gross };

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
        unmeasurable_calls,
        unverified_provenance_rows,
        provenance_total_rows,
        net_value_usd: gross - round_cost,
        gross_value_usd: gross,
        round_cost_usd: round_cost,
        value_rounds: VALUE_ROUNDS,
        pair_multiplier: PAIR_MULTIPLIER,
    })
}

/// Rounds over which a saved token keeps paying, bounded by the next compaction.
///
/// Measured at a per-call median of 194–249 by replaying transcripts; the lower end is
/// used so the published figure is the conservative one. Not the 65 assumed before 1.3.1,
/// which was a session-length median rather than call-weighted — calls concentrate in long
/// sessions.
const VALUE_ROUNDS: f64 = 194.0;

/// Rounds one intercept costs. 60.4% of `smart_read` calls are followed by a
/// `recall_file` on the same file, so an intercept averages more than a single round.
const PAIR_MULTIPLIER: f64 = 1.604;

// ── Context diagnostics: where is this project's context actually going? ──────
//
// Diagnosis, not savings. This answers "where does your context go" and makes no claim
// to have saved anything — it costs zero tokens, intercepts nothing and forces no rounds,
// so unlike every other figure in the product it cannot be net-negative. Given that the
// savings claim is the number that has been unstable, the diagnostic framing is the one
// that survives contact with the data.

/// How many files the report names. Enough to act on, short enough to read.
const HOTSPOT_LIMIT: i64 = 15;

/// A file that a meaningful share of the project's context has gone into.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileHotspot {
    pub path: String,
    /// Basename, so the UI need not split paths.
    pub name: String,
    pub reads: i64,
    pub total_tokens: i64,
    /// Share of every token this project has read.
    pub share_pct: f64,
    /// Line count at the most recent read, when it was recorded.
    pub lines: Option<i64>,
    /// Re-reads where the file had not changed since the previous read of it.
    ///
    /// A proxy for context loss rather than new information: the bytes were identical, so
    /// nothing was learned that a retained context would not already have held. The
    /// direct signal would be "re-read after a compaction", but compaction is recorded in
    /// the transcript and not in this database, and `file_mtime` equality answers the same
    /// question from data that is here.
    pub unchanged_rereads: i64,
    /// Present only where the data warrants one.
    pub recommendation: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContextReport {
    pub total_tokens_read: i64,
    pub distinct_files: i64,
    pub top_files: Vec<FileHotspot>,
    /// Share of all tokens read that sits in the ten largest files.
    pub top10_share_pct: f64,
    /// Re-reads of unchanged files, across every file.
    pub total_unchanged_rereads: i64,
}

/// A recommendation, only where the numbers justify one.
///
/// Deliberately narrow. A suggestion attached to every row is noise, and the point of a
/// diagnostic is that the reader trusts it — so this stays silent unless the case is
/// clear, and it says what to do rather than restating the measurement.
fn recommend(lines: Option<i64>, reads: i64, unchanged: i64, share: f64) -> Option<String> {
    // A large file read many times: no read optimisation beats splitting it, because
    // every read pays for the whole file however it is summarised.
    if let Some(l) = lines
        && l >= 1_000
        && reads >= 20
    {
        return Some(format!(
            "{l} lines read {reads} times — splitting it saves more than any read \
             optimisation can, because every read pays for the whole file"
        ));
    }
    // Mostly re-reads of an unchanged file: the content keeps being re-acquired.
    if reads >= 10 && unchanged * 2 >= reads {
        return Some(format!(
            "{unchanged} of {reads} reads found the file unchanged — that context was \
             re-acquired rather than retained"
        ));
    }
    // A single file dominating the project's context.
    if share >= 10.0 {
        return Some(format!(
            "{share:.0}% of everything this project has read is this one file"
        ));
    }
    None
}

/// Where the project's context has gone.
pub async fn get_context_report(pool: &SqlitePool) -> Result<ContextReport, String> {
    let (total, distinct): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(full_tokens),0), COUNT(DISTINCT path) FROM read_events",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    if total == 0 {
        return Ok(ContextReport {
            total_tokens_read: 0,
            distinct_files: 0,
            top_files: Vec::new(),
            top10_share_pct: 0.0,
            total_unchanged_rereads: 0,
        });
    }

    // Re-reads that found the file unchanged, per path.
    //
    // Counted as "rows sharing a (path, file_mtime) beyond the first", which is why the
    // subtraction is COUNT(*) - COUNT(DISTINCT file_mtime): a file read five times across
    // two versions contributes three. Rows predating `file_mtime` are excluded rather
    // than assumed unchanged.
    let unchanged: Vec<(String, i64)> = sqlx::query_as(
        "SELECT path, COUNT(*) - COUNT(DISTINCT file_mtime)
         FROM read_events
         WHERE file_mtime IS NOT NULL
         GROUP BY path",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let unchanged: std::collections::HashMap<String, i64> = unchanged.into_iter().collect();

    let rows: Vec<(String, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT path, COUNT(*), SUM(full_tokens), MAX(lines)
         FROM read_events
         GROUP BY path
         ORDER BY SUM(full_tokens) DESC
         LIMIT ?1",
    )
    .bind(HOTSPOT_LIMIT)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let top_files: Vec<FileHotspot> = rows
        .into_iter()
        .map(|(path, reads, tokens, lines)| {
            let share = 100.0 * tokens as f64 / total as f64;
            let u = unchanged.get(&path).copied().unwrap_or(0);
            FileHotspot {
                name: path.rsplit(['/', '\\']).next().unwrap_or(&path).to_string(),
                recommendation: recommend(lines, reads, u, share),
                path,
                reads,
                total_tokens: tokens,
                share_pct: share,
                lines,
                unchanged_rereads: u,
            }
        })
        .collect();

    let (top10,): (i64,) = sqlx::query_as(
        "SELECT COALESCE(SUM(t),0) FROM (
             SELECT SUM(full_tokens) AS t FROM read_events GROUP BY path
             ORDER BY t DESC LIMIT 10)",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(ContextReport {
        total_tokens_read: total,
        distinct_files: distinct,
        top_files,
        top10_share_pct: 100.0 * top10 as f64 / total as f64,
        total_unchanged_rereads: unchanged.values().sum(),
    })
}
