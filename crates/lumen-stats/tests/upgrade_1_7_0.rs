//! The GUI opens the ledger through `connect`, which is its own code path into the shared
//! `init_schema`: a 1.6.0 database must come out with 1.7.0's table, index and columns.

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
async fn the_gui_path_upgrades_a_1_6_0_database_and_keeps_its_rows() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("lumen.db").display());

    // Build a 1.6.0 database: today's schema minus what 1.7.0 added.
    let pool = connect(&url).await.unwrap();
    sqlx::query(
        "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, routed_via) \
         VALUES ('2026-09-01T10:00:00Z','Read','/a.rs',5100,5100,0,'builtin_read')",
    )
    .execute(&pool)
    .await
    .unwrap();
    for stmt in [
        "DROP TABLE session_events",
        "ALTER TABLE read_events DROP COLUMN repeat_arm",
        "ALTER TABLE read_events DROP COLUMN repeat_plan",
    ] {
        sqlx::query(stmt).execute(&pool).await.unwrap();
    }
    pool.close().await;

    let pool = connect(&url).await.unwrap();
    let cols = names(&pool, "SELECT name FROM pragma_table_info('read_events')").await;
    assert!(cols.contains(&"repeat_arm".to_string()) && cols.contains(&"repeat_plan".to_string()));
    let events = names(&pool, "SELECT name FROM pragma_table_info('session_events')").await;
    assert_eq!(events, ["session_id", "kind", "uuid", "ts_ms", "source_file"]);
    let idx = names(&pool, "SELECT name FROM pragma_index_list('session_events')").await;
    assert!(idx.contains(&"idx_session_events_session".to_string()));
    let n: i64 = sqlx::query("SELECT count(*) FROM read_events")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}
