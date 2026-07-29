// Integration tests for the aggregate queries.
//
// Every test builds a real SQLite file with the PRODUCTION DDL (via
// lumen_core::meter::connect_db) and then queries it through sqlx exactly as the
// GUI does. A drift between schema.rs and these queries therefore fails here
// rather than at runtime in the app.
//
// Timestamps are written as raw SQL relative to SQLite's own `now`, so the
// local-time calendar rollups can be tested without freezing the clock.

use lumen_core::meter::connect_db;
use lumen_stats::*;
use sqlx::SqlitePool;
use tempfile::TempDir;

/// Token counts for a turn: (input, output, cache_read, cache_write).
type Tokens = (i64, i64, i64, i64);

/// Metering counts for a read event: (tokens_returned, full_tokens, saved_tokens).
type Counts = (i64, i64, i64);

/// A temp DB with the real schema applied, plus a live sqlx pool over it.
async fn fixture() -> (TempDir, SqlitePool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    // Production DDL, then drop the rusqlite handle before sqlx opens it.
    drop(connect_db(&path).expect("connect_db"));
    let pool = connect(&format!("sqlite:{}?mode=rwc", path.display()))
        .await
        .expect("sqlx pool");
    (dir, pool)
}

/// Insert a turn. `ts` is raw SQL so tests can say `datetime('now','-2 hours')`.
async fn turn(pool: &SqlitePool, id: &str, session: &str, ts: &str, model: &str, t: Tokens) {
    let sql = format!(
        "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                           cache_read_input_tokens,cache_creation_input_tokens)
         VALUES(?1,?2,{ts},?3,?4,?5,?6,?7)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(session)
        .bind(model)
        .bind(t.0)
        .bind(t.1)
        .bind(t.2)
        .bind(t.3)
        .execute(pool)
        .await
        .expect("insert turn");
}

/// Insert a SUBAGENT turn — same session_id as the parent, as Claude Code writes
/// them, but flagged so it stays out of the context-fill gauge.
async fn subagent_turn(pool: &SqlitePool, id: &str, session: &str, ts: &str, t: Tokens) {
    let sql = format!(
        "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                           cache_read_input_tokens,cache_creation_input_tokens,
                           source_file,is_subagent)
         VALUES(?1,?2,{ts},'claude-sonnet-4-6',?3,?4,?5,?6,
                '/h/.claude/projects/-p/parent/subagents/agent-1.jsonl',1)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(session)
        .bind(t.0)
        .bind(t.1)
        .bind(t.2)
        .bind(t.3)
        .execute(pool)
        .await
        .expect("insert subagent turn");
}

/// Insert a turn with a NULL model.
async fn turn_no_model(pool: &SqlitePool, id: &str, session: &str, ts: &str) {
    let sql = format!(
        "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                           cache_read_input_tokens,cache_creation_input_tokens)
         VALUES(?1,?2,{ts},NULL,1,0,0,0)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(id)
        .bind(session)
        .execute(pool)
        .await
        .expect("insert turn");
}

/// Insert a read_events row. `ts` is raw SQL, as above.
async fn event(pool: &SqlitePool, ts: &str, routed_via: &str, channel: &str, c: Counts) {
    // `tool` mirrors what the real writers record: the MCP tool name for lumen
    // routes, "Read" for the built-in.
    let tool = if routed_via == "builtin_read" {
        "Read".to_string()
    } else {
        format!("mcp__lumen__{routed_via}")
    };
    let sql = format!(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel)
         VALUES({ts},?1,'/some/path.rs',100,?2,?3,?4,?5,?6)"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(tool)
        .bind(c.0)
        .bind(c.1)
        .bind(c.2)
        .bind(routed_via)
        .bind(channel)
        .execute(pool)
        .await
        .expect("insert read_event");
}

/// Local-time noon on the Monday that starts the current week, per the same
/// expression the production query uses.
const MONDAY_NOON: &str = "datetime(date('now','localtime','-'||((strftime('%w','now','localtime')+6)%7)||' days') || ' 12:00:00')";
/// Local-time noon on the Sunday immediately before that Monday.
const SUNDAY_BEFORE_NOON: &str = "datetime(date('now','localtime','-'||((strftime('%w','now','localtime')+6)%7)||' days','-1 days') || ' 12:00:00')";
const TODAY_NOON: &str = "datetime(date('now','localtime') || ' 12:00:00')";
const YESTERDAY_NOON: &str = "datetime(date('now','localtime','-1 days') || ' 12:00:00')";

// ── get_stats ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn stats_on_empty_db_are_zero_with_identity_factor() {
    let (_d, pool) = fixture().await;
    let s = get_stats(&pool).await.unwrap();
    assert_eq!(s.turns, 0);
    assert_eq!(s.output_total, 0);
    assert_eq!(
        s.factor, 1.0,
        "an uncalibrated DB must report the identity factor, not 0"
    );
}

#[tokio::test]
async fn stats_sum_turns_and_output() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "datetime('now')",
        "sonnet",
        (10, 100, 0, 0),
    )
    .await;
    turn(
        &pool,
        "m2",
        "s1",
        "datetime('now')",
        "sonnet",
        (20, 250, 0, 0),
    )
    .await;
    let s = get_stats(&pool).await.unwrap();
    assert_eq!(s.turns, 2);
    assert_eq!(s.output_total, 350);
}

#[tokio::test]
async fn stats_factor_comes_from_the_calibration_view() {
    let (_d, pool) = fixture().await;
    // correction_factor is a VIEW over calibration: SUM(real)/SUM(est).
    sqlx::query("INSERT INTO calibration(message_id,real_output,est_output) VALUES('m1',150,100)")
        .execute(&pool)
        .await
        .unwrap();
    let s = get_stats(&pool).await.unwrap();
    assert!(
        (s.factor - 1.5).abs() < f64::EPSILON,
        "expected 1.5, got {}",
        s.factor
    );
}

// ── get_usage: rolling windows ───────────────────────────────────────────────

#[tokio::test]
async fn usage_on_empty_db_is_all_zeros_and_no_window() {
    let (_d, pool) = fixture().await;
    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.rolling_5h, TokenAgg::default());
    assert_eq!(u.all_time, TokenAgg::default());
    assert_eq!(u.window_start, None, "no turns means no window start");
    assert_eq!(u.reset_approx, None, "and therefore no approximate reset");
}

#[tokio::test]
async fn usage_rolling_5h_includes_recent_and_excludes_older_turns() {
    let (_d, pool) = fixture().await;
    let recent = "datetime('now','-1 hours')";
    let stale = "datetime('now','-9 hours')";
    turn(&pool, "in", "s1", recent, "sonnet", (1, 2, 3, 4)).await;
    turn(&pool, "out", "s1", stale, "sonnet", (100, 200, 300, 400)).await;

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.rolling_5h.turns, 1, "only the 1h-old turn is inside 5h");
    assert_eq!(u.rolling_5h.input, 1);
    assert_eq!(u.rolling_5h.output, 2);
    assert_eq!(u.rolling_5h.cache_read, 3);
    assert_eq!(u.rolling_5h.cache_write, 4);
    assert_eq!(u.rolling_5h.total_tokens, 1 + 2 + 3 + 4);
    assert_eq!(u.all_time.turns, 2, "all-time still sees both");
}

#[tokio::test]
async fn usage_boundary_turn_just_inside_5h_is_counted() {
    let (_d, pool) = fixture().await;
    // 4h59m ago — inside the window. Guards an off-by-one in the comparison.
    let ts = "datetime('now','-4 hours','-59 minutes')";
    turn(&pool, "edge", "s1", ts, "sonnet", (5, 0, 0, 0)).await;
    assert_eq!(get_usage(&pool).await.unwrap().rolling_5h.turns, 1);
}

#[tokio::test]
async fn usage_boundary_turn_just_outside_5h_is_excluded() {
    let (_d, pool) = fixture().await;
    let ts = "datetime('now','-5 hours','-1 minutes')";
    turn(&pool, "edge", "s1", ts, "sonnet", (5, 0, 0, 0)).await;
    assert_eq!(get_usage(&pool).await.unwrap().rolling_5h.turns, 0);
}

#[tokio::test]
async fn usage_reset_approx_is_window_start_plus_five_hours() {
    let (_d, pool) = fixture().await;
    let ts = "datetime('now','-2 hours')";
    turn(&pool, "m1", "s1", ts, "sonnet", (1, 1, 1, 1)).await;
    let u = get_usage(&pool).await.unwrap();
    let start = u.window_start.expect("window start");
    let reset = u.reset_approx.expect("reset approx");

    // Assert the 5h delta via SQLite itself rather than reimplementing date
    // arithmetic in the test.
    let (delta_hours,): (f64,) = sqlx::query_as("SELECT (julianday(?1) - julianday(?2)) * 24.0")
        .bind(&reset)
        .bind(&start)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        (delta_hours - 5.0).abs() < 0.001,
        "reset_approx must be exactly 5h after window_start, got {delta_hours}h"
    );
}

#[tokio::test]
async fn usage_splits_opus_from_other_models_over_7d() {
    let (_d, pool) = fixture().await;
    let d1 = "datetime('now','-1 days')";
    let d2 = "datetime('now','-2 days')";
    turn(&pool, "o1", "s1", d1, "claude-opus-4", (10, 0, 0, 0)).await;
    turn(&pool, "o2", "s1", d2, "claude-3-opus", (5, 0, 0, 0)).await;
    turn(&pool, "s2", "s1", d1, "claude-sonnet-4", (70, 0, 0, 0)).await;

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(
        u.rolling_7d_opus.turns, 2,
        "both opus variants match '%opus%'"
    );
    assert_eq!(u.rolling_7d_opus.input, 15);
    assert_eq!(u.rolling_7d_other.turns, 1);
    assert_eq!(u.rolling_7d_other.input, 70);
}

#[tokio::test]
async fn usage_7d_split_excludes_turns_older_than_a_week() {
    let (_d, pool) = fixture().await;
    let ts = "datetime('now','-8 days')";
    turn(&pool, "old", "s1", ts, "claude-opus-4", (999, 0, 0, 0)).await;
    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.rolling_7d_opus, TokenAgg::default());
    assert_eq!(u.rolling_7d_other, TokenAgg::default());
    assert_eq!(u.all_time.input, 999, "but all-time still counts it");
}

#[tokio::test]
async fn usage_opus_matching_is_ascii_case_insensitive() {
    let (_d, pool) = fixture().await;
    // SQLite's LIKE is case-insensitive for ASCII, so 'OPUS' matches '%opus%'.
    // Pinning the real behaviour rather than assuming it.
    turn(
        &pool,
        "m1",
        "s1",
        "datetime('now')",
        "Claude-OPUS-4",
        (1, 0, 0, 0),
    )
    .await;
    let u = get_usage(&pool).await.unwrap();
    assert_eq!(
        u.rolling_7d_opus.turns, 1,
        "LIKE '%opus%' is ASCII-case-insensitive in SQLite"
    );
}

// ── get_usage: local-time calendar rollups ───────────────────────────────────
//
// The highest-risk arithmetic in the app: "today" and "this week" are computed
// in LOCAL time with a Monday week start.

#[tokio::test]
async fn usage_today_counts_only_todays_local_turns() {
    let (_d, pool) = fixture().await;
    turn(&pool, "today", "s1", TODAY_NOON, "sonnet", (10, 0, 0, 0)).await;
    turn(&pool, "yday", "s1", YESTERDAY_NOON, "sonnet", (50, 0, 0, 0)).await;

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.today.turns, 1, "yesterday must not leak into today");
    assert_eq!(u.today.input, 10);
}

#[tokio::test]
async fn usage_this_week_starts_on_monday_not_sunday() {
    let (_d, pool) = fixture().await;
    turn(&pool, "mon", "s1", MONDAY_NOON, "sonnet", (7, 0, 0, 0)).await;
    turn(
        &pool,
        "sun",
        "s1",
        SUNDAY_BEFORE_NOON,
        "sonnet",
        (99, 0, 0, 0),
    )
    .await;

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(
        u.this_week.turns, 1,
        "the Sunday before Monday belongs to LAST week (ISO-8601)"
    );
    assert_eq!(u.this_week.input, 7);
}

#[tokio::test]
async fn usage_this_week_includes_every_day_from_monday_to_today() {
    let (_d, pool) = fixture().await;
    // Days since Monday, per the same expression the query uses.
    let (days_since_monday,): (i64,) =
        sqlx::query_as("SELECT (strftime('%w','now','localtime')+6)%7")
            .fetch_one(&pool)
            .await
            .unwrap();

    for d in 0..=days_since_monday {
        let back = days_since_monday - d;
        let ts = format!("datetime(date('now','localtime','-{back} days') || ' 12:00:00')");
        turn(&pool, &format!("m{d}"), "s1", &ts, "sonnet", (1, 0, 0, 0)).await;
    }

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(
        u.this_week.turns,
        days_since_monday + 1,
        "every day Monday..today must be inside this_week"
    );
}

#[tokio::test]
async fn usage_all_time_has_no_where_clause_and_sums_everything() {
    let (_d, pool) = fixture().await;
    let ancient = "'2020-01-01T00:00:00Z'";
    turn(&pool, "old", "s1", ancient, "sonnet", (1, 2, 3, 4)).await;
    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.all_time.turns, 1);
    assert_eq!(u.all_time.total_tokens, 10);
    assert_eq!(u.today, TokenAgg::default(), "2020 is not today");
}

#[tokio::test]
async fn usage_total_tokens_is_the_sum_of_all_four_components() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "datetime('now')",
        "sonnet",
        (11, 22, 33, 44),
    )
    .await;
    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.all_time.total_tokens, 11 + 22 + 33 + 44);
}

// ── get_sessions ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn sessions_on_empty_db_is_an_empty_list() {
    let (_d, pool) = fixture().await;
    assert!(get_sessions(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn sessions_groups_turns_and_reports_peak_cache_read() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "sonnet",
        (10, 1, 500, 5),
    )
    .await;
    turn(
        &pool,
        "m2",
        "s1",
        "'2026-01-01T11:00:00Z'",
        "sonnet",
        (20, 2, 9000, 6),
    )
    .await;
    turn(
        &pool,
        "m3",
        "s1",
        "'2026-01-01T12:00:00Z'",
        "sonnet",
        (30, 3, 1200, 7),
    )
    .await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(rows.len(), 1, "one row per session_id");
    let s = &rows[0];
    assert_eq!(s.session_id, "s1");
    assert_eq!(s.turn_count, 3);
    assert_eq!(s.input, 60);
    assert_eq!(s.output, 6);
    assert_eq!(s.first_ts, "2026-01-01T10:00:00Z");
    assert_eq!(s.last_ts, "2026-01-01T12:00:00Z");
    assert_eq!(
        s.peak_cache_read, 9000,
        "peak is the MAX, not the last or the sum"
    );
}

#[tokio::test]
async fn sessions_are_ordered_by_most_recent_activity() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "a",
        "old-session",
        "'2026-01-01T10:00:00Z'",
        "s",
        (1, 0, 0, 0),
    )
    .await;
    turn(
        &pool,
        "b",
        "new-session",
        "'2026-06-01T10:00:00Z'",
        "s",
        (1, 0, 0, 0),
    )
    .await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(rows[0].session_id, "new-session", "newest activity first");
    assert_eq!(rows[1].session_id, "old-session");
}

#[tokio::test]
async fn sessions_model_is_the_most_recent_non_null_value() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "claude-sonnet-4",
        (1, 0, 0, 0),
    )
    .await;
    turn(
        &pool,
        "m2",
        "s1",
        "'2026-01-01T11:00:00Z'",
        "claude-opus-4",
        (1, 0, 0, 0),
    )
    .await;
    // A later turn with a NULL model must not blank out the reported model.
    turn_no_model(&pool, "m3", "s1", "'2026-01-01T12:00:00Z'").await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(
        rows[0].model.as_deref(),
        Some("claude-opus-4"),
        "a trailing NULL model must not erase the known model"
    );
}

#[tokio::test]
async fn sessions_is_capped_at_the_documented_limit() {
    let (_d, pool) = fixture().await;
    for i in 0..(SESSION_LIMIT + 10) {
        let id = format!("m{i}");
        let session = format!("s{i}");
        turn(&pool, &id, &session, "datetime('now')", "s", (1, 0, 0, 0)).await;
    }
    assert_eq!(
        get_sessions(&pool).await.unwrap().len() as i64,
        SESSION_LIMIT,
        "the query LIMIT and the documented SESSION_LIMIT must agree"
    );
}

// ── subagent turns: real spend, separate context ─────────────────────────────

#[tokio::test]
async fn a_subagents_tokens_count_toward_cost() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "sonnet",
        (10, 20, 300_000, 5),
    )
    .await;
    subagent_turn(
        &pool,
        "sub1",
        "s1",
        "'2026-01-01T10:01:00Z'",
        (7, 9, 4_000, 1),
    )
    .await;

    let u = get_usage(&pool).await.unwrap();
    assert_eq!(u.all_time.turns, 2, "a subagent turn is still a turn");
    assert_eq!(u.all_time.input, 17, "and its tokens are real spend");
    assert_eq!(u.all_time.output, 29);
    assert_eq!(u.all_time.cache_read, 304_000);
}

#[tokio::test]
async fn a_subagents_context_does_not_become_the_sessions_peak_fill() {
    // The bug: subagent transcripts reuse the parent's sessionId, so a subagent's
    // small fresh context was treated as the parent session's context fill.
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "sonnet",
        (0, 0, 300_000, 0),
    )
    .await;
    // Newest turn, tiny context — this is what used to hijack the gauge.
    subagent_turn(
        &pool,
        "sub1",
        "s1",
        "'2026-01-01T10:05:00Z'",
        (0, 0, 4_000, 0),
    )
    .await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(rows.len(), 1, "a subagent does not create a second session");
    assert_eq!(
        rows[0].peak_cache_read, 300_000,
        "the peak must come from the main agent, not the subagent"
    );
    assert_eq!(rows[0].turn_count, 2, "but both turns are counted");
}

#[tokio::test]
async fn a_subagent_with_a_larger_context_still_does_not_set_the_peak() {
    // Even if a subagent somehow reads more context than the parent, it is not
    // the parent's context — the exclusion is by kind, not by magnitude.
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "sonnet",
        (0, 0, 50_000, 0),
    )
    .await;
    subagent_turn(
        &pool,
        "sub1",
        "s1",
        "'2026-01-01T10:01:00Z'",
        (0, 0, 900_000, 0),
    )
    .await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(rows[0].peak_cache_read, 50_000);
}

#[tokio::test]
async fn a_session_of_only_subagent_turns_reports_no_fill() {
    // Honest answer: there is no main-agent context to report.
    let (_d, pool) = fixture().await;
    subagent_turn(
        &pool,
        "sub1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        (1, 1, 7_000, 1),
    )
    .await;

    let rows = get_sessions(&pool).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].peak_cache_read, 0,
        "no main-agent turn means no fill"
    );
    assert_eq!(
        rows[0].turn_count, 1,
        "the turn still exists and still cost money"
    );
}

// ── get_optimizer_stats ──────────────────────────────────────────────────────

#[tokio::test]
async fn optimizer_on_empty_db_reports_zeros_and_unknown_channel() {
    let (_d, pool) = fixture().await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.lifetime_optimized_tokens, 0);
    assert_eq!(o.lifetime_full_tokens, 0);
    assert_eq!(o.today_saved_tokens, 0);
    assert_eq!(o.this_week_saved_tokens, 0);
    assert!(o.by_channel.is_empty());
    assert!(o.by_tool.is_empty());
    assert_eq!(
        o.current_channel, "unknown",
        "no events means the channel is unknown, not empty string"
    );
    assert_eq!(o.missed_calls, 0);
}

#[tokio::test]
async fn optimizer_sums_only_lumen_routes() {
    let (_d, pool) = fixture().await;
    let now = "datetime('now')";
    for route in LUMEN_ROUTES {
        event(&pool, now, route, "cli", (100, 1000, 900)).await;
    }
    // builtin_read must be excluded from savings entirely.
    event(&pool, now, "builtin_read", "cli", (5000, 5000, 0)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.lifetime_optimized_tokens, 2700,
        "3 lumen calls × 900 saved; builtin_read contributes nothing"
    );
    assert_eq!(
        o.lifetime_full_tokens, 3000,
        "builtin_read's 5000 full tokens must NOT inflate the denominator"
    );
}

#[tokio::test]
async fn optimizer_counts_builtin_reads_as_missed_not_saved() {
    let (_d, pool) = fixture().await;
    let now = "datetime('now')";
    event(&pool, now, "builtin_read", "cli", (800, 800, 0)).await;
    event(&pool, now, "builtin_read", "cli", (200, 200, 0)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.missed_calls, 2);
    assert_eq!(o.missed_full_tokens, 1000);
    assert_eq!(
        o.lifetime_optimized_tokens, 0,
        "a missed read is never a saving"
    );
}

#[tokio::test]
async fn optimizer_counts_missed_reads_in_every_channel() {
    let (_d, pool) = fixture().await;
    // Inverted. This test used to assert that a vscode builtin_read was NOT
    // counted, on the documented premise that hooks fire only in the CLI. That
    // premise is false: hooks demonstrably fire in the VS Code extension, and the
    // meter hardcoded channel='cli' on every row anyway, so the filter it pinned
    // matched everything and the "CLI-only" label described nothing real.
    event(
        &pool,
        "datetime('now')",
        "builtin_read",
        "vscode",
        (900, 900, 0),
    )
    .await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.missed_calls, 1,
        "a bypassed read is a bypassed read regardless of channel"
    );
}

/// Insert a builtin_read whose tokens could not be measured, as the meter records
/// an image or other binary from 1.2.1 on.
async fn unmeasurable_event(pool: &SqlitePool, path: &str) {
    sqlx::query(sqlx::AssertSqlSafe(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel,token_source)
         VALUES(datetime('now'),'Read',?1,0,0,0,0,'builtin_read','vscode','unsupported')"
            .to_string(),
    ))
    .bind(path)
    .execute(pool)
    .await
    .expect("insert unmeasurable read_event");
}

/// A built-in Read of a PNG is not a missed optimization: there is no outline to
/// return, so there was no saving available to miss. Counting it inflates the very
/// number that is supposed to motivate switching tools.
#[tokio::test]
async fn optimizer_excludes_unmeasurable_reads_from_the_missed_metric() {
    let (_d, pool) = fixture().await;
    event(
        &pool,
        "datetime('now')",
        "builtin_read",
        "vscode",
        (4000, 4000, 0),
    )
    .await;
    unmeasurable_event(&pool, "/tmp/shot.png").await;
    unmeasurable_event(&pool, "/tmp/other.png").await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.missed_calls, 1,
        "only the real text read is a missed optimization"
    );
    assert_eq!(
        o.missed_full_tokens, 4000,
        "and only its tokens count toward the opportunity"
    );
    assert_eq!(
        o.unmeasurable_calls, 2,
        "the excluded rows are reported, not silently dropped — a metric that \
         discards part of its population reads as full coverage otherwise"
    );
}

/// Rows written before 1.2.1 carry a bytes/4 estimate labelled `estimated`, so they
/// cannot be recognised by provenance — only by extension. Half the historical missed
/// total came from these, and every one of the six largest rows was a screenshot.
#[tokio::test]
async fn optimizer_excludes_pre_labelling_binary_reads_by_extension() {
    let (_d, pool) = fixture().await;
    // Exactly the shape of a real pre-1.2.1 row: a PNG with a bytes/4 estimate.
    for (path, tokens) in [("/p/shot.png", 87_529), ("/p/logo.WEBP", 40_000)] {
        sqlx::query(sqlx::AssertSqlSafe(
            "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                     saved_tokens,routed_via,channel,token_source)
             VALUES(datetime('now'),'Read',?1,0,?2,?2,0,'builtin_read','cli','estimated')"
                .to_string(),
        ))
        .bind(path)
        .bind(tokens)
        .execute(&pool)
        .await
        .unwrap();
    }
    // A real source file, also 'estimated' — a broken tokenizer, not binary input.
    sqlx::query(sqlx::AssertSqlSafe(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel,token_source)
         VALUES(datetime('now'),'Read','/p/main.rs',400,3000,3000,0,'builtin_read','cli','estimated')"
            .to_string(),
    ))
    .execute(&pool)
    .await
    .unwrap();

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.missed_calls, 1,
        "only main.rs is a missed optimization; the PNG and the WEBP are not"
    );
    assert_eq!(o.missed_full_tokens, 3000, "and only its tokens");
    assert_eq!(
        o.unmeasurable_calls, 2,
        "both binaries are reported as excluded, regardless of case in the extension"
    );
}

/// Negative control for the filter above. Rows the meter *did* measure must still
/// count; a filter keyed on the wrong value would zero the metric entirely.
#[tokio::test]
async fn optimizer_still_counts_measured_and_unlabelled_missed_reads() {
    let (_d, pool) = fixture().await;
    // token_source NULL: every row written before 1.1.5 looks like this.
    event(
        &pool,
        "datetime('now')",
        "builtin_read",
        "cli",
        (100, 100, 0),
    )
    .await;
    // token_source 'measured'.
    sqlx::query(sqlx::AssertSqlSafe(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel,token_source)
         VALUES(datetime('now'),'Read','/a.rs',10,200,200,0,'builtin_read','cli','measured')"
            .to_string(),
    ))
    .execute(&pool)
    .await
    .unwrap();
    // token_source 'estimated': a broken tokenizer, but a real text file.
    sqlx::query(sqlx::AssertSqlSafe(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel,token_source)
         VALUES(datetime('now'),'Read','/b.rs',10,300,300,0,'builtin_read','cli','estimated')"
            .to_string(),
    ))
    .execute(&pool)
    .await
    .unwrap();

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.missed_calls, 3,
        "NULL, measured and estimated are all real reads of real text"
    );
    assert_eq!(o.missed_full_tokens, 600);
    assert_eq!(o.unmeasurable_calls, 0);
}

#[tokio::test]
async fn optimizer_breaks_down_by_channel_descending_by_saving() {
    let (_d, pool) = fixture().await;
    let now = "datetime('now')";
    event(&pool, now, "smart_read", "cli", (10, 100, 90)).await;
    event(&pool, now, "smart_read", "vscode", (10, 900, 890)).await;
    event(&pool, now, "smart_read", "vscode", (10, 110, 100)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.by_channel.len(), 2);
    assert_eq!(o.by_channel[0].channel, "vscode", "largest saving first");
    assert_eq!(o.by_channel[0].calls, 2);
    assert_eq!(o.by_channel[0].saved_tokens, 990);
    assert_eq!(o.by_channel[1].channel, "cli");
    assert_eq!(o.by_channel[1].saved_tokens, 90);
}

#[tokio::test]
async fn optimizer_breaks_down_by_tool_descending_by_saving() {
    let (_d, pool) = fixture().await;
    let now = "datetime('now')";
    event(&pool, now, "smart_read", "cli", (1, 100, 50)).await;
    event(&pool, now, "compress_logs", "cli", (1, 900, 800)).await;
    event(&pool, now, "recall_file", "cli", (1, 300, 200)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    let tools: Vec<&str> = o.by_tool.iter().map(|t| t.tool.as_str()).collect();
    assert_eq!(
        tools,
        vec!["compress_logs", "recall_file", "smart_read"],
        "ordered by saving, descending"
    );
}

#[tokio::test]
async fn optimizer_current_channel_is_the_latest_event() {
    let (_d, pool) = fixture().await;
    event(
        &pool,
        "'2026-01-01T10:00:00Z'",
        "smart_read",
        "cli",
        (1, 10, 9),
    )
    .await;
    event(
        &pool,
        "'2026-01-01T11:00:00Z'",
        "smart_read",
        "vscode",
        (1, 10, 9),
    )
    .await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.current_channel, "vscode", "most recent row wins");
}

#[tokio::test]
async fn optimizer_current_channel_considers_builtin_reads_too() {
    let (_d, pool) = fixture().await;
    // The "current channel" query has no routed_via filter, so a builtin_read is
    // a legitimate signal of where the user currently is.
    event(
        &pool,
        "'2026-01-01T10:00:00Z'",
        "smart_read",
        "cli",
        (1, 10, 9),
    )
    .await;
    event(
        &pool,
        "'2026-01-01T11:00:00Z'",
        "builtin_read",
        "vscode",
        (1, 10, 0),
    )
    .await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.current_channel, "vscode");
}

#[tokio::test]
async fn optimizer_today_excludes_yesterdays_savings() {
    let (_d, pool) = fixture().await;
    event(&pool, TODAY_NOON, "smart_read", "cli", (1, 100, 75)).await;
    event(&pool, YESTERDAY_NOON, "smart_read", "cli", (1, 100, 99)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.today_saved_tokens, 75);
    assert_eq!(
        o.lifetime_optimized_tokens, 174,
        "lifetime still counts both days"
    );
}

#[tokio::test]
async fn optimizer_this_week_starts_on_monday() {
    let (_d, pool) = fixture().await;
    event(&pool, MONDAY_NOON, "smart_read", "cli", (1, 100, 42)).await;
    event(&pool, SUNDAY_BEFORE_NOON, "smart_read", "cli", (1, 100, 99)).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(
        o.this_week_saved_tokens, 42,
        "the Sunday before Monday is LAST week"
    );
}

// ── Wire format ──────────────────────────────────────────────────────────────
//
// The Angular frontend reads these exact keys. Renaming a Rust field without the
// serde attribute would silently blank out a page rather than fail loudly.

#[tokio::test]
async fn optimizer_json_uses_camel_case_for_the_frontend() {
    let (_d, pool) = fixture().await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    let v = serde_json::to_value(&o).unwrap();
    for key in [
        "lifetimeOptimizedTokens",
        "lifetimeFullTokens",
        "todaySavedTokens",
        "thisWeekSavedTokens",
        "byChannel",
        "byTool",
        "currentChannel",
        "missedCalls",
        "missedFullTokens",
    ] {
        assert!(v.get(key).is_some(), "missing camelCase key: {key}");
    }
}

#[tokio::test]
async fn usage_json_uses_camel_case_for_the_frontend() {
    let (_d, pool) = fixture().await;
    let v = serde_json::to_value(get_usage(&pool).await.unwrap()).unwrap();
    for key in [
        "rolling5h",
        "windowStart",
        "resetApprox",
        "rolling7dOpus",
        "rolling7dOther",
        "today",
        "thisWeek",
        "allTime",
    ] {
        assert!(v.get(key).is_some(), "missing camelCase key: {key}");
    }
    assert!(
        v["allTime"].get("cacheRead").is_some(),
        "TokenAgg must also be camelCase"
    );
}

#[tokio::test]
async fn session_json_uses_camel_case_for_the_frontend() {
    let (_d, pool) = fixture().await;
    turn(
        &pool,
        "m1",
        "s1",
        "'2026-01-01T10:00:00Z'",
        "sonnet",
        (1, 2, 3, 4),
    )
    .await;
    let rows = get_sessions(&pool).await.unwrap();
    let v = serde_json::to_value(&rows[0]).unwrap();
    for key in [
        "sessionId",
        "model",
        "firstTs",
        "lastTs",
        "turnCount",
        "cacheRead",
        "cacheWrite",
        "totalTokens",
        "peakCacheRead",
    ] {
        assert!(v.get(key).is_some(), "missing camelCase key: {key}");
    }
}

// ── Context diagnostics ──────────────────────────────────────────────────────

/// Insert a read of `path` at `mtime` with `tokens`, `lines` long.
async fn read_of(pool: &SqlitePool, path: &str, tokens: i64, lines: i64, mtime: i64) {
    sqlx::query(sqlx::AssertSqlSafe(
        "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                 saved_tokens,routed_via,channel,file_mtime)
         VALUES(datetime('now'),'Read',?1,?2,?3,?3,0,'builtin_read','cli',?4)"
            .to_string(),
    ))
    .bind(path)
    .bind(lines)
    .bind(tokens)
    .bind(mtime)
    .execute(pool)
    .await
    .expect("insert read");
}

#[tokio::test]
async fn context_report_on_an_empty_db_is_all_zeros() {
    let (_d, pool) = fixture().await;
    let r = get_context_report(&pool).await.unwrap();
    assert_eq!(r.total_tokens_read, 0);
    assert_eq!(r.distinct_files, 0);
    assert!(r.top_files.is_empty());
    assert_eq!(r.top10_share_pct, 0.0);
}

#[tokio::test]
async fn context_report_ranks_files_by_cumulative_tokens_and_computes_share() {
    let (_d, pool) = fixture().await;
    for _ in 0..3 {
        read_of(&pool, "/p/hot.tsx", 1_000, 3_833, 1).await;
    }
    read_of(&pool, "/p/cool.rs", 500, 100, 1).await;

    let r = get_context_report(&pool).await.unwrap();
    assert_eq!(r.total_tokens_read, 3_500);
    assert_eq!(r.distinct_files, 2);
    assert_eq!(
        r.top_files[0].name, "hot.tsx",
        "ordered by cumulative tokens"
    );
    assert_eq!(r.top_files[0].reads, 3);
    assert_eq!(r.top_files[0].total_tokens, 3_000);
    assert!((r.top_files[0].share_pct - 85.714).abs() < 0.01);
    assert_eq!(r.top_files[0].lines, Some(3_833));
}

/// The re-read signal must distinguish "the file changed" from "we lost it".
#[tokio::test]
async fn unchanged_rereads_count_only_reads_that_learned_nothing_new() {
    let (_d, pool) = fixture().await;
    // Four reads of one version, then two of a second version.
    for _ in 0..4 {
        read_of(&pool, "/p/a.rs", 100, 50, 1_000).await;
    }
    for _ in 0..2 {
        read_of(&pool, "/p/a.rs", 100, 50, 2_000).await;
    }
    let r = get_context_report(&pool).await.unwrap();
    let f = &r.top_files[0];
    assert_eq!(f.reads, 6);
    assert_eq!(
        f.unchanged_rereads, 4,
        "6 reads across 2 versions leaves 4 that found the file unchanged"
    );
}

/// A file edited between every read learned something new each time, so nothing is a
/// wasted re-read. This is the negative control for the count above.
#[tokio::test]
async fn a_file_changed_between_every_read_has_no_unchanged_rereads() {
    let (_d, pool) = fixture().await;
    for i in 0..5 {
        read_of(&pool, "/p/b.rs", 100, 50, 1_000 + i).await;
    }
    let r = get_context_report(&pool).await.unwrap();
    assert_eq!(r.top_files[0].unchanged_rereads, 0);
    assert_eq!(r.total_unchanged_rereads, 0);
}

/// Rows predating `file_mtime` must not be assumed unchanged.
#[tokio::test]
async fn rows_without_an_mtime_are_excluded_rather_than_assumed_unchanged() {
    let (_d, pool) = fixture().await;
    for _ in 0..5 {
        event(
            &pool,
            "datetime('now')",
            "builtin_read",
            "cli",
            (100, 100, 0),
        )
        .await;
    }
    let r = get_context_report(&pool).await.unwrap();
    assert_eq!(
        r.total_unchanged_rereads, 0,
        "a NULL mtime means unknown, not unchanged"
    );
}

/// The worked example: a large file read many times gets the split recommendation,
/// because no read optimisation can beat not reading it.
#[tokio::test]
async fn a_large_repeatedly_read_file_is_recommended_for_splitting() {
    let (_d, pool) = fixture().await;
    for i in 0..25 {
        read_of(&pool, "/p/Run.tsx", 27_000, 3_833, 1_000 + i).await;
    }
    let r = get_context_report(&pool).await.unwrap();
    let rec = r.top_files[0].recommendation.as_deref().unwrap_or("");
    assert!(
        rec.contains("3833 lines") && rec.contains("splitting"),
        "expected a split recommendation, got {rec:?}"
    );
}

/// And a small file read a few times gets none — a suggestion on every row is noise.
#[tokio::test]
async fn an_ordinary_file_gets_no_recommendation() {
    let (_d, pool) = fixture().await;
    read_of(&pool, "/p/small.rs", 100, 40, 1).await;
    read_of(&pool, "/p/other.rs", 100, 40, 1).await;
    let r = get_context_report(&pool).await.unwrap();
    // Neither is large, neither is mostly re-reads; both are 50% share, so only the
    // dominance rule could fire — assert it is the dominance text, not the others.
    for f in &r.top_files {
        if let Some(rec) = &f.recommendation {
            assert!(
                rec.contains("% of everything"),
                "an ordinary small file must not get a split or re-read suggestion: {rec}"
            );
        }
    }
}

#[tokio::test]
async fn top10_share_is_computed_over_all_files_not_just_the_reported_ones() {
    let (_d, pool) = fixture().await;
    // 12 files: the top 10 should be 10/12 of the tokens.
    for i in 0..12 {
        read_of(&pool, &format!("/p/f{i}.rs"), 100, 10, 1).await;
    }
    let r = get_context_report(&pool).await.unwrap();
    assert!(
        (r.top10_share_pct - 100.0 * 10.0 / 12.0).abs() < 0.01,
        "expected {:.2}%, got {:.2}%",
        100.0 * 10.0 / 12.0,
        r.top10_share_pct
    );
}

#[tokio::test]
async fn context_report_json_uses_camel_case_for_the_frontend() {
    let (_d, pool) = fixture().await;
    read_of(&pool, "/p/a.rs", 100, 10, 1).await;
    let r = get_context_report(&pool).await.unwrap();
    let j = serde_json::to_value(&r).unwrap();
    for k in [
        "totalTokensRead",
        "distinctFiles",
        "topFiles",
        "top10SharePct",
        "totalUnchangedRereads",
    ] {
        assert!(j.get(k).is_some(), "missing camelCase key {k}");
    }
    let f = &j["topFiles"][0];
    for k in ["totalTokens", "sharePct", "unchangedRereads"] {
        assert!(f.get(k).is_some(), "missing camelCase key topFiles[].{k}");
    }
}

// ── Net dollar value ─────────────────────────────────────────────────────────

/// Populate enough turns for the cost side to be computable.
async fn turns_for_cost(pool: &SqlitePool, n: i64, ctx: i64, out: i64) {
    for i in 0..n {
        turn(
            pool,
            &format!("nv{i}"),
            "s",
            "datetime('now')",
            "claude-opus-5",
            (0, out, ctx, 0),
        )
        .await;
    }
}

/// With too few turns to average, the cost side is unknown — and a gross figure with no
/// cost against it is exactly the overstatement the dollar headline replaces.
#[tokio::test]
async fn net_value_is_zero_when_there_are_too_few_turns_to_price_a_round() {
    let (_d, pool) = fixture().await;
    event(
        &pool,
        "datetime('now')",
        "smart_read",
        "cli",
        (100, 5_000, 4_900),
    )
    .await;
    turns_for_cost(&pool, 10, 300_000, 1_000).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.round_cost_usd, 0.0);
    assert_eq!(
        o.gross_value_usd, 0.0,
        "a gross figure with no cost beside it is the overstatement being removed"
    );
    assert_eq!(o.net_value_usd, 0.0);
}

#[tokio::test]
async fn net_value_prices_saved_tokens_against_the_rounds_they_cost() {
    let (_d, pool) = fixture().await;
    // One call saving 100,000 tokens.
    event(
        &pool,
        "datetime('now')",
        "smart_read",
        "cli",
        (1_000, 101_000, 100_000),
    )
    .await;
    turns_for_cost(&pool, 200, 100_000, 1_000).await;

    let o = get_optimizer_stats(&pool).await.unwrap();

    // gross = 100_000 x (6.25 + 0.5 x 194) / 1e6
    let want_gross = 100_000.0 * (6.25 + 0.5 * 194.0) / 1e6;
    assert!(
        (o.gross_value_usd - want_gross).abs() < 1e-9,
        "gross {} vs {want_gross}",
        o.gross_value_usd
    );

    // cost = 1 call x ((100_000 x 0.5 + 1_000 x 25) / 1e6) x 1.604
    let want_cost = ((100_000.0 * 0.5 + 1_000.0 * 25.0) / 1e6) * 1.604;
    assert!(
        (o.round_cost_usd - want_cost).abs() < 1e-9,
        "cost {} vs {want_cost}",
        o.round_cost_usd
    );
    assert!((o.net_value_usd - (want_gross - want_cost)).abs() < 1e-9);
    assert_eq!(o.value_rounds, 194.0, "R must be surfaced, not hidden");
    assert_eq!(o.pair_multiplier, 1.604);
}

/// A call that saves almost nothing must come out negative. This is the whole reason the
/// headline changed: the token ratio would still have looked like a win.
#[tokio::test]
async fn a_call_that_saves_little_comes_out_negative() {
    let (_d, pool) = fixture().await;
    // 300 tokens saved out of 400 — an effectiveness ratio of 75%, and a loss.
    event(
        &pool,
        "datetime('now')",
        "smart_read",
        "cli",
        (100, 400, 300),
    )
    .await;
    turns_for_cost(&pool, 200, 400_000, 1_200).await;

    let o = get_optimizer_stats(&pool).await.unwrap();
    let ratio = 1.0
        - (o.lifetime_full_tokens - o.lifetime_optimized_tokens) as f64
            / o.lifetime_full_tokens as f64;
    assert!(
        ratio > 0.7,
        "premise: the token ratio looks good ({ratio:.2})"
    );
    assert!(
        o.net_value_usd < 0.0,
        "and yet the call is a loss: net {}",
        o.net_value_usd
    );
}

/// Subagent turns carry their own context and must not price the main agent's rounds.
#[tokio::test]
async fn subagent_turns_do_not_price_the_round() {
    let (_d, pool) = fixture().await;
    event(
        &pool,
        "datetime('now')",
        "smart_read",
        "cli",
        (100, 5_000, 4_900),
    )
    .await;
    turns_for_cost(&pool, 200, 100_000, 1_000).await;
    for i in 0..200 {
        subagent_turn(
            &pool,
            &format!("sa{i}"),
            "s",
            "datetime('now')",
            (0, 1_000, 1, 0),
        )
        .await;
    }
    let o = get_optimizer_stats(&pool).await.unwrap();
    let want = ((100_000.0 * 0.5 + 1_000.0 * 25.0) / 1e6) * 1.604;
    assert!(
        (o.round_cost_usd - want).abs() < 1e-9,
        "subagent rows with a 1-token context dragged the round cost to {}",
        o.round_cost_usd
    );
}

#[tokio::test]
async fn net_value_fields_are_camel_case_for_the_frontend() {
    let (_d, pool) = fixture().await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    let j = serde_json::to_value(&o).unwrap();
    for k in [
        "netValueUsd",
        "grossValueUsd",
        "roundCostUsd",
        "valueRounds",
        "pairMultiplier",
    ] {
        assert!(j.get(k).is_some(), "missing camelCase key {k}");
    }
}
