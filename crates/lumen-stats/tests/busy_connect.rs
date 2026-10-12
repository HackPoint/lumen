//! The GUI opens the ledger through `connect`, and a writer that holds it for a moment must not
//! fail that open with "database is locked": SQLite refuses at once, whatever the busy timeout
//! says, when the connection that switches a rollback-journal ledger to WAL already holds a
//! read lock (see `lumen_core::busy`), so `connect` has to wait for the writer itself.

use std::time::Duration;

use lumen_stats::connect;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};

#[tokio::test]
async fn connect_waits_for_a_writer_instead_of_failing_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lumen.db");
    let url = format!("sqlite:{}?mode=rwc", path.display());
    connect(&url).await.unwrap().close().await;

    // The ledger as it is before anything has switched it to WAL, with a writer inside it.
    let mut writer = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&path)
            .journal_mode(SqliteJournalMode::Delete),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();

    let let_go = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        sqlx::query("COMMIT").execute(&mut writer).await.unwrap();
    };
    let (opened, ()) = tokio::join!(connect(&url), let_go);

    let pool = opened.expect("a writer that lets go within a moment must not fail the open");
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
}
