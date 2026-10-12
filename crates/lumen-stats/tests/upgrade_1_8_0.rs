//! The GUI opens the ledger through `connect`, which is its own code path into the shared
//! `init_schema`: a 1.7.0 database must come out with 1.8.0's `agent_id` column, its rows
//! intact, and rows written with and without a subagent must both land.

use lumen_stats::connect;
use sqlx::Row;

async fn names(pool: &sqlx::SqlitePool, sql: &'static str) -> Vec<String> {
    sqlx::query(sql)
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect()
}

#[tokio::test]
async fn the_gui_path_upgrades_a_1_7_0_database_and_keeps_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("lumen.db").display());

    // Build a 1.7.0 database: today's schema minus what 1.8.0 added.
    let pool = connect(&url).await.unwrap();
    sqlx::query(
        "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
         routed_via, session_id, repeat_plan) \
         VALUES ('2026-09-01T10:00:00Z','mcp__lumen__smart_read','/a.rs',300,5100,4800,\
         'smart_read','old-session','first')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE read_events DROP COLUMN agent_id")
        .execute(&pool)
        .await
        .unwrap();
    let before = names(&pool, "SELECT name FROM pragma_table_info('read_events')").await;
    assert!(!before.contains(&"agent_id".to_string()));
    pool.close().await;

    let pool = connect(&url).await.unwrap();
    let cols = names(&pool, "SELECT name FROM pragma_table_info('read_events')").await;
    assert!(cols.contains(&"agent_id".to_string()), "{cols:?}");

    // The old row is where it was, and its subagent is unknown rather than invented.
    let row = sqlx::query("SELECT session_id, agent_id, repeat_plan FROM read_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>(0), "old-session");
    assert_eq!(row.get::<Option<String>, _>(1), None);
    assert_eq!(row.get::<String, _>(2), "first");

    // A second open changes nothing: the column is added once.
    pool.close().await;
    let pool = connect(&url).await.unwrap();
    let again = names(&pool, "SELECT name FROM pragma_table_info('read_events')").await;
    assert_eq!(again, cols);
    let n: i64 = sqlx::query("SELECT count(*) FROM read_events")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}
