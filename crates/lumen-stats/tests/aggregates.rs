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
async fn optimizer_missed_reads_are_cli_only() {
    let (_d, pool) = fixture().await;
    // The PostToolUse hook only fires in the CLI, so vscode builtin_read rows
    // are not counted as "missed" — documented behaviour, pinned here.
    event(
        &pool,
        "datetime('now')",
        "builtin_read",
        "vscode",
        (900, 900, 0),
    )
    .await;
    let o = get_optimizer_stats(&pool).await.unwrap();
    assert_eq!(o.missed_calls, 0, "only channel='cli' counts as missed");
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
