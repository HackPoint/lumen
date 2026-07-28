/// Bring a database up to date: create anything missing, then migrate.
///
/// **Every sqlx consumer must call this instead of executing [`DDL`] alone.** DDL
/// is `CREATE TABLE IF NOT EXISTS`, so on a database that already has the tables
/// it does nothing at all — which meant a column added by [`MIGRATIONS`] never
/// reached any existing install. `turns.is_subagent` landed in 1.1.0 that way, and
/// because the daemon binds that column on every insert, ingest failed on every
/// row and the gauge simply froze. Only the rusqlite path in `meter.rs` had ever
/// applied the migrations.
///
/// Migration errors are swallowed, as they are on the rusqlite side: each
/// statement is additive and idempotent, so "duplicate column" is the expected
/// outcome on an already-current database.
pub async fn init_schema(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(DDL).execute(pool).await?;
    for migration in MIGRATIONS {
        // AssertSqlSafe is sound here: every statement is a literal in
        // MIGRATIONS above, so no runtime value can reach the SQL.
        let _ = sqlx::raw_sql(sqlx::AssertSqlSafe(*migration))
            .execute(pool)
            .await;
    }
    Ok(())
}

/// Additive migrations run after DDL to upgrade existing DBs.
/// Each statement is idempotent (errors for "duplicate column" are swallowed by the caller).
pub const MIGRATIONS: &[&str] = &[
    "ALTER TABLE read_events ADD COLUMN channel TEXT NOT NULL DEFAULT 'unknown'",
    // Subagent transcripts reuse the parent's sessionId, so their turns land in
    // the parent session. They are real spend (kept in cost) but carry their own
    // fresh context, so they must NOT drive the context-fill gauge.
    "ALTER TABLE turns ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0",
    // Backfill existing rows from the path they were ingested from. Idempotent,
    // so it is safe to re-run on every open.
    "UPDATE turns SET is_subagent = 1 \
     WHERE is_subagent = 0 AND source_file LIKE '%/subagents/%'",
];

pub const DDL: &str = r#"
PRAGMA journal_mode=WAL;

CREATE TABLE IF NOT EXISTS turns (
    message_id                  TEXT PRIMARY KEY,
    session_id                  TEXT NOT NULL,
    ts                          TEXT NOT NULL,
    model                       TEXT,
    input_tokens                INTEGER NOT NULL,
    output_tokens               INTEGER NOT NULL,
    cache_read_input_tokens     INTEGER NOT NULL,
    cache_creation_input_tokens INTEGER NOT NULL,
    cache_1h                    INTEGER,
    cache_5m                    INTEGER,
    web_search_requests         INTEGER,
    web_fetch_requests          INTEGER,
    service_tier                TEXT,
    source_file                 TEXT,
    -- 1 when the turn came from a subagent transcript. Counted in cost, excluded
    -- from the context-fill gauge (a subagent's context is not the session's).
    is_subagent                 INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS sessions (
    session_id  TEXT PRIMARY KEY,
    cwd         TEXT,
    model       TEXT,
    started_at  TEXT,
    source_file TEXT
);

CREATE TABLE IF NOT EXISTS calibration (
    message_id      TEXT PRIMARY KEY,
    real_output     INTEGER NOT NULL,
    est_output      INTEGER NOT NULL
);

CREATE VIEW IF NOT EXISTS correction_factor AS
SELECT CAST(SUM(real_output) AS FLOAT) / NULLIF(SUM(est_output), 0) AS factor
FROM calibration;

CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
CREATE INDEX IF NOT EXISTS idx_turns_ts      ON turns(ts);

-- Optimizer metering: one row per Read / lumen-tool call.
-- Additive migration: IF NOT EXISTS keeps it idempotent alongside existing tables.
CREATE TABLE IF NOT EXISTS read_events (
    ts              TEXT NOT NULL,
    tool            TEXT NOT NULL,
    path            TEXT NOT NULL,
    lines           INTEGER,
    tokens_returned INTEGER NOT NULL,
    full_tokens     INTEGER NOT NULL,
    saved_tokens    INTEGER NOT NULL,
    routed_via      TEXT NOT NULL,  -- builtin_read | smart_read | recall_file | compress_logs
    channel         TEXT NOT NULL DEFAULT 'unknown'  -- cli | vscode | unknown
);
CREATE INDEX IF NOT EXISTS idx_read_events_ts ON read_events(ts);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::TempDir;

    /// The `turns` table exactly as it existed before 1.1.0 — no `is_subagent`.
    ///
    /// Hand-written on purpose: using the current DDL would already include the
    /// column and the test could not fail.
    const PRE_1_1_0_TURNS: &str = "\
        CREATE TABLE turns ( \
            message_id TEXT PRIMARY KEY, \
            session_id TEXT NOT NULL, \
            ts TEXT NOT NULL, \
            model TEXT, \
            input_tokens INTEGER NOT NULL, \
            output_tokens INTEGER NOT NULL, \
            cache_read_input_tokens INTEGER NOT NULL, \
            cache_creation_input_tokens INTEGER NOT NULL, \
            source_file TEXT \
        )";

    async fn pool_in(dir: &TempDir) -> sqlx::SqlitePool {
        let db = dir.path().join("legacy.db");
        SqlitePoolOptions::new()
            .connect(&format!("sqlite:{}?mode=rwc", db.display()))
            .await
            .unwrap()
    }

    async fn columns(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
        let rows: Vec<(i64, String)> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
                .fetch_all(pool)
                .await
                .map(|rs: Vec<(i64, String, String, i64, Option<String>, i64)>| {
                    rs.into_iter().map(|r| (r.0, r.1)).collect()
                })
                .unwrap();
        rows.into_iter().map(|(_, name)| name).collect()
    }

    #[tokio::test]
    async fn ddl_alone_does_not_upgrade_an_existing_table() {
        // The defect itself: CREATE TABLE IF NOT EXISTS is a no-op once the table
        // exists, so a column added later never arrives. This is what shipped.
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_1_1_0_TURNS).execute(&pool).await.unwrap();

        sqlx::raw_sql(DDL).execute(&pool).await.unwrap();

        assert!(
            !columns(&pool, "turns")
                .await
                .contains(&"is_subagent".into()),
            "DDL on its own must not be relied on to migrate — if this starts \
             passing, the no-op assumption changed and init_schema needs review"
        );
    }

    #[tokio::test]
    async fn init_schema_adds_the_missing_column_to_a_legacy_database() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_1_1_0_TURNS).execute(&pool).await.unwrap();

        init_schema(&pool).await.unwrap();

        assert!(
            columns(&pool, "turns")
                .await
                .contains(&"is_subagent".into()),
            "a pre-1.1.0 database must gain is_subagent, or every daemon insert \
             fails on an unknown column and ingest silently stops"
        );
    }

    #[tokio::test]
    async fn a_legacy_database_can_be_written_after_init_schema() {
        // The user-visible symptom was ingest failing, so assert the insert the
        // daemon actually performs, not just the column's presence.
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_1_1_0_TURNS).execute(&pool).await.unwrap();
        init_schema(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO turns (message_id, session_id, ts, model, input_tokens, \
             output_tokens, cache_read_input_tokens, cache_creation_input_tokens, \
             source_file, is_subagent) VALUES (?,?,?,?,?,?,?,?,?,?)",
        )
        .bind("m1")
        .bind("s1")
        .bind("2026-01-01T00:00:00Z")
        .bind("claude-opus-5")
        .bind(1_i64)
        .bind(2_i64)
        .bind(3_i64)
        .bind(4_i64)
        .bind("/h/.claude/projects/-p/s1.jsonl")
        .bind(0_i64)
        .execute(&pool)
        .await
        .expect("insert must succeed once the migration has run");

        let n: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM turns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n.0, 1);
    }

    #[tokio::test]
    async fn the_backfill_marks_existing_subagent_rows() {
        // Rows already in a legacy database must be classified, not just future
        // ones — otherwise the gauge stays wrong for the whole existing history.
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_1_1_0_TURNS).execute(&pool).await.unwrap();
        for (id, file) in [
            ("main", "/h/.claude/projects/-p/abc.jsonl"),
            ("sub", "/h/.claude/projects/-p/abc/subagents/agent-1.jsonl"),
        ] {
            sqlx::query(
                "INSERT INTO turns (message_id, session_id, ts, input_tokens, \
                 output_tokens, cache_read_input_tokens, \
                 cache_creation_input_tokens, source_file) \
                 VALUES (?,'s1','2026-01-01T00:00:00Z',0,0,0,0,?)",
            )
            .bind(id)
            .bind(file)
            .execute(&pool)
            .await
            .unwrap();
        }

        init_schema(&pool).await.unwrap();

        let flagged: Vec<(String, i64)> =
            sqlx::query_as("SELECT message_id, is_subagent FROM turns ORDER BY message_id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            flagged,
            vec![("main".to_string(), 0), ("sub".to_string(), 1)],
            "the subagent row must be backfilled from its source_file"
        );
    }

    #[tokio::test]
    async fn init_schema_is_idempotent() {
        // It runs on every daemon start and every GUI connect, so re-running must
        // be harmless and must not duplicate or reset anything.
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        for _ in 0..3 {
            init_schema(&pool).await.expect("re-running must succeed");
        }
        assert!(
            columns(&pool, "turns")
                .await
                .contains(&"is_subagent".into())
        );
    }

    #[tokio::test]
    async fn init_schema_creates_everything_from_nothing() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        init_schema(&pool).await.unwrap();
        for table in ["turns", "read_events"] {
            assert!(
                !columns(&pool, table).await.is_empty(),
                "{table} should exist on a fresh database"
            );
        }
        assert!(
            columns(&pool, "read_events")
                .await
                .contains(&"channel".into())
        );
    }
}
