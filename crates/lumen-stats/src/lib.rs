// lumen-stats — read-only aggregate queries over the metering DB.
//
// HONESTY (carried over from the GUI, where this code used to live): these are
// CONSUMPTION figures. Plan quota size / remaining / the real reset are
// server-side and unknown locally, so nothing here is a "% of limit" or
// "remaining". Usage carries no dollars: every token sum comes split by model, and the
// frontend prices each model's share at that model's rates, from the table it gets
// from `lumen_prices` (lumen_core::rates::PRICES). What Lumen saved, in tokens and dollars, is
// not computed here: `savings_summary` loads the rows and `lumen_core::savings` does the rest,
// for the app exactly as for the CLI.
//
// Every struct is the wire format the Angular frontend consumes. The
// `rename_all = "camelCase"` attributes are load-bearing: renaming a field here
// silently breaks the UI.

use lumen_core::repeat::{
    ARM_TREATMENT, PLAN_FRESH, PLAN_UNIDENTIFIED, ROUTE_DELTA, ROUTE_RECALL_DELTA,
    ROUTE_RECALL_UNCHANGED, ROUTE_UNCHANGED,
};
use lumen_core::savings::{
    self, BlockedReadRow, BoundaryRow, Clock, Ledger, SavingsSummary, load as sql,
};
use serde::Serialize;
use sqlx::SqlitePool;

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
///
/// Every query below references turns.is_subagent, which only exists after the migrations run.
/// Opening the GUI before the daemon has started would otherwise hit "no such column" on a
/// database created before 1.1.0, so the pool comes back migrated.
pub async fn connect(url: &str) -> Result<SqlitePool, String> {
    lumen_core::schema::open_pool(url)
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
    /// The same sums, per model. The sums above cannot be priced: a token costs five times
    /// as much on Opus as on Haiku, so the frontend prices these instead.
    pub by_model: Vec<ModelAgg>,
}

impl TokenAgg {
    /// The totals of `by_model`, carrying it.
    fn from_models(by_model: Vec<ModelAgg>) -> TokenAgg {
        let mut agg = TokenAgg::default();
        for m in &by_model {
            agg.turns += m.turns;
            agg.input += m.input;
            agg.output += m.output;
            agg.cache_read += m.cache_read;
            agg.cache_write += m.cache_write;
        }
        agg.total_tokens = agg.input + agg.output + agg.cache_read + agg.cache_write;
        agg.by_model = by_model;
        agg
    }
}

/// One model's share of a [`TokenAgg`], at one speed in one region.
#[derive(Serialize, Default, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelAgg {
    /// As the transcript recorded it, dated ids included. Null for turns that recorded none,
    /// which have no price.
    pub model: Option<String>,
    /// `usage.speed`, "standard" or "fast". Null for turns that did not record it, every one
    /// ingested before 1.9.0: on a model with fast mode, those are priced at the standard rate
    /// as a floor.
    pub speed: Option<String>,
    /// `usage.inference_geo`, "us", "global" or "not_available". US-only inference costs more
    /// on the 4.6 and later models; null for turns that did not record it, every one ingested
    /// before 1.9.0, and on those models those and "not_available" ones are priced at the
    /// global rate as a floor.
    pub inference_geo: Option<String>,
    pub turns: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// The part of `cache_write` the transcript recorded as 1-hour writes.
    pub cache_write_1h: i64,
    /// The part of `cache_write` whose duration was not recorded: every turn ingested before
    /// 1.9.0. Priced at the 5-minute rate, so a figure holding it is a floor.
    pub cache_write_unknown: i64,
}

/// Per-model, per-speed, per-region sums over the turns `where_clause` selects (pass "" for all
/// of them).
/// The `&'static str` bound is what makes the `AssertSqlSafe` below sound: no runtime value
/// can reach the clause, so no user input is interpolated.
async fn model_aggs(
    pool: &SqlitePool,
    where_clause: &'static str,
) -> Result<Vec<ModelAgg>, String> {
    let sql = format!(
        "SELECT model, speed, inference_geo,
                COUNT(*),
                COALESCE(SUM(input_tokens),0),
                COALESCE(SUM(output_tokens),0),
                COALESCE(SUM(cache_read_input_tokens),0),
                COALESCE(SUM(cache_creation_input_tokens),0),
                COALESCE(SUM(cache_1h),0),
                -- A turn that recorded no split has a write of unknown duration.
                COALESCE(SUM(CASE WHEN cache_1h IS NULL
                                  THEN cache_creation_input_tokens ELSE 0 END),0)
         FROM turns {where_clause}
         GROUP BY model, speed, inference_geo
         ORDER BY model, speed, inference_geo"
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(
            |(
                model,
                speed,
                inference_geo,
                turns,
                input,
                output,
                cache_read,
                cache_write,
                w_1h,
                w_unknown,
            )| {
                ModelAgg {
                    model,
                    speed,
                    inference_geo,
                    turns,
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cache_write_1h: w_1h,
                    cache_write_unknown: w_unknown,
                }
            },
        )
        .collect())
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

/// Token sums over the turns `where_clause` selects (pass "" for all-time), split by model.
pub async fn fetch_agg(pool: &SqlitePool, where_clause: &'static str) -> Result<TokenAgg, String> {
    Ok(TokenAgg::from_models(model_aggs(pool, where_clause).await?))
}

/// WHERE clause for "today" in the user's local timezone.
pub const WHERE_TODAY: &str = "WHERE date(ts,'localtime') = date('now','localtime')";

/// WHERE clause for "this week", week starting MONDAY (ISO-8601):
/// strftime('%w') is 0=Sun..6=Sat, so (%w + 6) % 7 = days since Monday.
pub const WHERE_THIS_WEEK: &str = "WHERE date(ts,'localtime') >= \
     date('now','localtime','-'||((strftime('%w','now','localtime')+6)%7)||' days')";

/// WHERE clause for the trailing 5 hours. `ts` is ISO-8601 UTC ('…Z'); datetime()
/// normalizes both sides to canonical UTC so the comparison is correct.
const WHERE_LAST_5H: &str = "WHERE datetime(ts) >= datetime('now','-5 hours')";

/// WHERE clause for the trailing 7 days.
const WHERE_LAST_7D: &str = "WHERE datetime(ts) >= datetime('now','-7 days')";

/// The 7-day split's test, as SQL's `model LIKE '%opus%'` reads it: ASCII case folded, and
/// a turn with no model is "other".
fn is_opus(m: &ModelAgg) -> bool {
    m.model
        .as_deref()
        .is_some_and(|id| id.to_ascii_lowercase().contains("opus"))
}

pub async fn get_usage(pool: &SqlitePool) -> Result<UsageReport, String> {
    // (a) Rolling 5h consumption. reset_approx is a PROXY (window_start + 5h), not the
    //     server reset.
    let rolling_5h = fetch_agg(pool, WHERE_LAST_5H).await?;
    let (window_start, reset_approx): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT MIN(ts), datetime(MIN(ts), '+5 hours')
         FROM turns
         WHERE datetime(ts) >= datetime('now','-5 hours')",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    // (b) Rolling 7d consumption, split Opus vs other.
    let (opus, other): (Vec<ModelAgg>, Vec<ModelAgg>) = model_aggs(pool, WHERE_LAST_7D)
        .await?
        .into_iter()
        .partition(is_opus);

    // (c) Calendar rollups in LOCAL time.
    let today = fetch_agg(pool, WHERE_TODAY).await?;
    let this_week = fetch_agg(pool, WHERE_THIS_WEEK).await?;
    let all_time = fetch_agg(pool, "").await?;

    Ok(UsageReport {
        rolling_5h,
        window_start,
        reset_approx,
        rolling_7d_opus: TokenAgg::from_models(opus),
        rolling_7d_other: TokenAgg::from_models(other),
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
// saved_tokens is a cl100k_base count per call. A read event records no model, so these
// tokens carry no dollar figure here; `savings_summary` prices each call at the model of the
// turn that read it.
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

/// What repeat reads did, counted apart from everything else in [`OptimizerReport`].
///
/// A served answer is a row whose route is one of the four repeat routes. None of them is in
/// [`LUMEN_ROUTES`], so the token totals and the breakdowns above do not include
/// these rows; the provenance counts, which take every row that carries a count, do.
///
/// Served rows are found by route and by nothing else. In the A/B experiment a control row
/// records the plan the server WOULD have run (`unchanged`, `delta`) under an ordinary route:
/// nothing was served, so counting by plan would count answers nobody was given.
#[derive(Serialize, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepeatReads {
    /// Reads answered "unchanged since an earlier read" instead of with the result again.
    pub unchanged_served: i64,
    /// Reads of an edited file answered with only what changed.
    pub deltas_served: i64,
    /// `saved_tokens` of the served answers whose count was measured: what each did not send,
    /// against reading the whole file again. Not against the outline the same call would have
    /// returned with the feature off, which is a smaller baseline.
    pub tokens_not_resent: i64,
    /// The served answers behind `tokens_not_resent`. Fewer than `unchanged_served +
    /// deltas_served` means the others carry no measured count and add nothing to it.
    pub measured_answers: i64,
    /// Calls that passed `fresh=true` while repeat reads were answering (the treatment arm): the
    /// whole result asked for again. A control call, or one made while the feature was off, was
    /// not a request to bypass an answer.
    pub fresh_requests: i64,
    /// Calls answered in full because Lumen could not tell which conversation they came from,
    /// in either arm: it says something about the install, not about the feature.
    pub unidentified_reads: i64,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OptimizerReport {
    /// SUM(saved_tokens) over lumen routes — CAUSED by Lumen, not reported. No dollar
    /// figure: a read event records no model to price it by.
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
    /// How many rows in the lifetime window carry a token count not known to be measured.
    ///
    /// Rows written before 1.1.5 carry no `token_source`, and on installs whose
    /// baked tokenizer path was dead the hook silently substituted `bytes / 4`.
    /// The UI must not present those as exact while any remain, so it reports the
    /// count instead of the claim. Deliberately "unverified", not "estimated":
    /// asserting they are all estimates would be its own unmeasured claim.
    pub unverified_provenance_rows: i64,
    /// Rows that carry a count, so the frontend can render "N of M". Reads marked
    /// `unsupported` have none and are in neither figure.
    pub provenance_total_rows: i64,

    /// Repeat reads, lifetime. Counted apart from every figure above.
    pub repeat_reads: RepeatReads,
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
    //
    // A row marked 'unsupported' — an image, a binary — carries no count, and says so,
    // so it is on neither side. Counted as unverified, every screenshot read made the
    // Optimizer say the ledger was partly unverified, on an install that has never
    // estimated a count.
    let (unverified_provenance_rows, provenance_total_rows): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(CASE WHEN token_source IS NULL OR token_source <> 'measured'
                                  THEN 1 ELSE 0 END),0),
                COUNT(*)
         FROM read_events
         WHERE COALESCE(token_source, '') <> 'unsupported'",
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

    let repeat_reads = repeat_reads(pool).await?;

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
        repeat_reads,
    })
}

async fn repeat_reads(pool: &SqlitePool) -> Result<RepeatReads, String> {
    // The four routes are found by name and the plan is never consulted for them: a control-arm
    // row has the same plan and an ordinary route, and nothing was served.
    let (
        unchanged_served,
        deltas_served,
        tokens_not_resent,
        measured_answers,
        fresh_requests,
        unidentified_reads,
    ): (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT
            COALESCE(SUM(CASE WHEN routed_via IN (?1, ?2) THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN routed_via IN (?3, ?4) THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN routed_via IN (?1, ?2, ?3, ?4) AND token_source = 'measured'
                              THEN saved_tokens ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN routed_via IN (?1, ?2, ?3, ?4) AND token_source = 'measured'
                              THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN repeat_plan = ?5 AND repeat_arm = ?6 THEN 1 ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN repeat_plan = ?7 THEN 1 ELSE 0 END), 0)
         FROM read_events",
    )
    .bind(ROUTE_UNCHANGED)
    .bind(ROUTE_RECALL_UNCHANGED)
    .bind(ROUTE_DELTA)
    .bind(ROUTE_RECALL_DELTA)
    .bind(PLAN_FRESH)
    .bind(ARM_TREATMENT)
    .bind(PLAN_UNIDENTIFIED)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(RepeatReads {
        unchanged_served,
        deltas_served,
        tokens_not_resent,
        measured_answers,
        fresh_requests,
        unidentified_reads,
    })
}

// ── Savings ──────────────────────────────────────────────────────────────────
//
// The app's loader for `lumen_core::savings`: the engine's own SQL (`savings::load`), run through
// sqlx, into the engine's own rows. Nothing is computed here. The CLI runs the same statements
// through rusqlite, and `crates/lumen-cli/tests/savings_parity.rs` holds the two summaries to the
// same bytes.

fn db_err(e: sqlx::Error) -> String {
    e.to_string()
}

/// The engine's rows and clock at `now_ms`, read through `pool`. `spool` is the fault spool whose
/// pending blocked Reads count too (`None` reads only the table).
pub async fn load_savings_ledger(
    pool: &SqlitePool,
    now_ms: i64,
    spool: Option<&std::path::Path>,
) -> Result<(Ledger, Clock), String> {
    let now_s = now_ms.div_euclid(1000);
    let (week_start_s, week_start): (i64, String) = sqlx::query_as(sql::WEEK_SQL)
        .bind(now_s)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;

    let (packed,): (Option<String>,) = sqlx::query_as(sql::CALLS_SQL)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    let calls = sql::parse_calls(packed.as_deref())?;

    let mut blocked_reads: Vec<BlockedReadRow> = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(sql::BLOCKED_SQL)
    .fetch_all(pool)
    .await
    .map_err(db_err)?
    .into_iter()
    .map(
        |(ts, session_id, path, detail, agent_id, version)| BlockedReadRow {
            ts,
            session_id,
            path,
            detail,
            agent_id,
            version,
        },
    )
    .collect();
    let (spooled, spool_gap) = sql::spooled_blocked_reads(spool);
    blocked_reads.extend(spooled);

    let sessions = sql::sessions_json(&calls, &blocked_reads);
    // Each chunk of sessions on a connection of its own, side by side, unpacked in chunk order.
    let chunks = sql::session_chunks(&calls, &blocked_reads);
    let packed: Vec<Vec<(String, String)>> =
        futures_util::future::try_join_all(chunks.iter().map(|chunk| {
            sqlx::query_as::<_, (String, String)>(sql::TURNS_SQL)
                .bind(chunk)
                .fetch_all(pool)
        }))
        .await
        .map_err(db_err)?;
    let (sessions_turns, classes) = sql::parse_chunks(packed)?;

    let boundaries: Vec<BoundaryRow> =
        sqlx::query_as::<_, (String, String, i64, Option<String>)>(sql::BOUNDARIES_SQL)
            .bind(&sessions)
            .fetch_all(pool)
            .await
            .map_err(db_err)?
            .into_iter()
            .map(|(session_id, kind, ts_ms, source_file)| BoundaryRow {
                session_id,
                kind,
                ts_ms,
                source_file,
            })
            .collect();

    let (recent_turns,): (i64,) = sqlx::query_as(sql::RECENT_TURNS_SQL)
        .bind(sql::recent_since(now_ms))
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    let (events_from, subagent_events_from, meter_agents_from): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = sqlx::query_as(sql::COVERAGE_SQL)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    let (valve_sessions, valve_paths) = sql::valve_keys_json(&blocked_reads);
    let (packed,): (Option<String>,) = sqlx::query_as(sql::VALVE_READS_SQL)
        .bind(valve_sessions)
        .bind(valve_paths)
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    let valve_reads = sql::parse_valve_reads(packed.as_deref())?;

    Ok((
        Ledger {
            calls,
            sessions: sessions_turns,
            classes,
            turns: Vec::new(),
            boundaries,
            blocked_reads,
            recent_turns,
            events_from,
            subagent_events_from,
            valve_reads,
            meter_agents_from,
            gaps: spool_gap.into_iter().collect(),
        },
        Clock {
            now_ms,
            week_start_ms: week_start_s * 1000,
            week_start,
        },
    ))
}

/// The newest row of each table the summary reads (`savings::load::FINGERPRINT_SQL`): equal
/// twice running, the ledger has not changed in between.
pub async fn savings_fingerprint(pool: &SqlitePool) -> Result<[Option<i64>; 4], String> {
    let (a, b, c, d): (Option<i64>, Option<i64>, Option<i64>, Option<i64>) =
        sqlx::query_as(sql::FINGERPRINT_SQL)
            .fetch_one(pool)
            .await
            .map_err(db_err)?;
    Ok([a, b, c, d])
}

/// What Lumen saved, as the app shows it: the engine's summary of the ledger `pool` holds, at
/// `now_ms`.
pub async fn savings_summary(
    pool: &SqlitePool,
    now_ms: i64,
    spool: Option<&std::path::Path>,
    facts: &savings::Facts,
    cal: &dyn savings::Calibrator,
) -> Result<SavingsSummary, String> {
    let (ledger, clock) = load_savings_ledger(pool, now_ms, spool).await?;
    Ok(savings::summarize(&ledger, &clock, facts, cal))
}

// ── Context diagnostics: where is the context actually going? ────────────────
//
// Diagnosis, not savings. This answers "where does your context go" and makes no claim
// to have saved anything — it costs zero tokens, intercepts nothing and forces no rounds,
// so unlike every other figure in the product it cannot be net-negative. Given that the
// savings claim is the number that has been unstable, the diagnostic framing is the one
// that survives contact with the data.

/// How many files the report names. Enough to act on, short enough to read.
const HOTSPOT_LIMIT: usize = 15;

/// A file that a meaningful share of the project's context has gone into.
#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileHotspot {
    pub path: String,
    /// Basename, so the UI need not split paths.
    pub name: String,
    pub reads: i64,
    pub total_tokens: i64,
    /// Share of every token read, in every project the ledger holds.
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
            "{share:.0}% of everything read, across every project, is this one file"
        ));
    }
    None
}

/// Where the project's context has gone.
pub async fn get_context_report(pool: &SqlitePool) -> Result<ContextReport, String> {
    // Every file's reads and tokens, heaviest first. The total, the file count, the ten
    // largest files' share and the files reported all come from this one pass.
    let mut files: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT path, COUNT(*), SUM(full_tokens) FROM read_events
         GROUP BY path
         ORDER BY SUM(full_tokens) DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let total: i64 = files.iter().map(|f| f.2).sum();

    if total == 0 {
        return Ok(ContextReport {
            total_tokens_read: 0,
            distinct_files: 0,
            top_files: Vec::new(),
            top10_share_pct: 0.0,
            total_unchanged_rereads: 0,
        });
    }
    let distinct = files.len() as i64;
    let top10: i64 = files.iter().take(10).map(|f| f.2).sum();

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

    files.truncate(HOTSPOT_LIMIT);

    // The line count is the latest read's, not the largest: through 1.5.1 it was MAX(lines),
    // so a file that had shrunk, split on this report's own advice say, kept its old size here
    // and the advice with it. Looked up for the reported files only, once they are chosen.
    let paths: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT path, lines FROM read_events
         WHERE lines IS NOT NULL AND path IN (SELECT value FROM json_each(?1))
         ORDER BY ts, rowid",
    )
    .bind(serde_json::to_string(&paths).map_err(|e| e.to_string())?)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let mut latest = std::collections::HashMap::new();
    for (path, lines) in counts {
        // In the order they were read, so each file is left with its latest read's count.
        latest.insert(path, lines);
    }

    let top_files: Vec<FileHotspot> = files
        .into_iter()
        .map(|(path, reads, tokens)| {
            let share = 100.0 * tokens as f64 / total as f64;
            let u = unchanged.get(&path).copied().unwrap_or(0);
            let lines = latest.get(&path).copied();
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

    Ok(ContextReport {
        total_tokens_read: total,
        distinct_files: distinct,
        top_files,
        top10_share_pct: 100.0 * top10 as f64 / total as f64,
        total_unchanged_rereads: unchanged.values().sum(),
    })
}
