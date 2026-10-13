//! A ledger from before 1.9.0 has a `faults` table without `agent_id`, which the savings summary
//! reads and the fault drain writes. The GUI's opener must add it, and the summary must then read
//! the ledger and its old rows.

use lumen_core::savings::{BillingView, Facts, Uncalibrated};
use lumen_stats::connect;
use sqlx::Row;

#[tokio::test]
async fn the_gui_path_gives_a_1_8_0_faults_table_its_agent_column() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("lumen.db").display());

    // A 1.8.0 ledger: today's schema with `faults` as 1.8.0 made it, and one escape-valve fault.
    let pool = connect(&url).await.unwrap();
    for stmt in [
        "DROP TABLE faults",
        "CREATE TABLE faults (ts TEXT NOT NULL, kind TEXT NOT NULL, \
         variant TEXT NOT NULL DEFAULT '-', path TEXT, lines INTEGER, detail TEXT, \
         session_id TEXT, version TEXT, channel TEXT NOT NULL DEFAULT 'unknown')",
        "INSERT INTO faults (ts, kind, variant, path, session_id, version) VALUES \
         ('2026-10-12T01:00:20Z', 'hook_fail_open', 'retry_escape_valve', '/r/b.rs', 's', '1.8.0')",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(stmt))
            .execute(&pool)
            .await
            .unwrap();
    }
    pool.close().await;

    let pool = connect(&url).await.unwrap();
    let columns: Vec<String> = sqlx::query("SELECT name FROM pragma_table_info('faults')")
        .fetch_all(&pool)
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect();
    assert!(columns.iter().any(|c| c == "agent_id"), "{columns:?}");
    let facts = Facts {
        billing_mode: BillingView::Api,
        identity_hook_registered: None,
    };
    let s = lumen_stats::savings_summary(&pool, 1_791_806_400_000, None, &facts, &Uncalibrated)
        .await
        .unwrap();
    assert_eq!(s.all_time.counts.blocked_reads, 1);
}
