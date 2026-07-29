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
    // ── E7 instrumentation ───────────────────────────────────────────────────
    // Existing rows stay NULL. There is no honest way to backfill provenance for
    // events already recorded, and inventing it would be worse than admitting the
    // gap: a NULL here means "unknown", which is the truth.
    //
    // session_id  — from CLAUDE_CODE_SESSION_ID, which Claude Code exports to both
    //               hooks and the MCP server. Lets a read be tied to the turn that
    //               caused it, which bare second-precision timestamps cannot do
    //               when several sessions run at once.
    "ALTER TABLE read_events ADD COLUMN session_id TEXT",
    // file_mtime  — file modification time at read time, so a re-read of an
    //               unchanged file is distinguishable from a re-read after an edit.
    "ALTER TABLE read_events ADD COLUMN file_mtime INTEGER",
    // req_key     — identity of the REQUEST, not the file. Two recall_file calls on
    //               one file asking for different items are different requests, so
    //               keying dedup on path alone overstates the opportunity.
    "ALTER TABLE read_events ADD COLUMN req_key TEXT",
    // is_subagent — mirrors the turns classification.
    "ALTER TABLE read_events ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0",
    // writer_hook — which hook or binary wrote the row. Two hook installs were
    //               live simultaneously and their rows were indistinguishable.
    "ALTER TABLE read_events ADD COLUMN writer_hook TEXT",
    // token_source— 'measured' | 'estimated'. A dead LUMEN_TOK fell back to
    //               bytes/4 silently, so figures the README called "measured to
    //               the token" were estimates and nothing recorded which.
    "ALTER TABLE read_events ADD COLUMN token_source TEXT",
    // ── 1.3.0 ranked-outline decision provenance ─────────────────────────────
    // Why every input and not just the answer: S_min is derived from measured means
    // that will be re-derived once per-call (R, round cost) pairs exist, and a row
    // recording only its budget could not be compared against one scored under
    // different coefficients. Existing rows stay NULL, which is the truth — they
    // predate the decision entirely.
    //
    // budget      — full_tokens − S_min. Negative means no outline could pay.
    "ALTER TABLE read_events ADD COLUMN budget INTEGER",
    // s_min       — minimum saving that repays the one extra round interception forces.
    "ALTER TABLE read_events ADD COLUMN s_min INTEGER",
    // econ_*      — the C, R and O that produced S_min, so the arithmetic is
    //               reproducible from the row alone rather than from a constant that
    //               may since have changed.
    "ALTER TABLE read_events ADD COLUMN econ_context REAL",
    "ALTER TABLE read_events ADD COLUMN econ_rounds REAL",
    "ALTER TABLE read_events ADD COLUMN econ_output REAL",
    // econ_source — 'observed' when the local ledger supplied C and O, else
    //               'measured_defaults'. Without this an installation-specific mean
    //               and a shipped constant are indistinguishable in the data.
    "ALTER TABLE read_events ADD COLUMN econ_source TEXT",
    // k_selected / n_total — definitions included of definitions found. When these are
    //               equal the budget did not bind and the ranking had no effect, which
    //               is the difference between a gate and a trimmer.
    "ALTER TABLE read_events ADD COLUMN k_selected INTEGER",
    "ALTER TABLE read_events ADD COLUMN n_total INTEGER",
    // coeff_version — bumped on any change to weights, queries or ranking. Pooling rows
    //               across versions would make the A/B compare two things at once.
    "ALTER TABLE read_events ADD COLUMN coeff_version INTEGER",
    // target_outline — what the outline was aiming to cost. A tuning parameter swept
    //               downward during the A/B, so without it rows from different sweep
    //               values would be pooled and the follow-up-rate curve would be the
    //               average of several different experiments.
    "ALTER TABLE read_events ADD COLUMN target_outline INTEGER",
    "CREATE INDEX IF NOT EXISTS idx_read_events_dedup \
     ON read_events(session_id, path, file_mtime)",
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
    channel         TEXT NOT NULL DEFAULT 'unknown',  -- cli | vscode | unknown
    -- Declared in the order the ALTERs in MIGRATIONS add them, so a fresh database
    -- and a migrated one agree on column order as well as on the column set.
    session_id      TEXT,
    file_mtime      INTEGER,
    req_key         TEXT,
    is_subagent     INTEGER NOT NULL DEFAULT 0,
    writer_hook     TEXT,
    token_source    TEXT,             -- measured | estimated | unsupported | NULL
    -- 1.3.0 ranked-outline decision. NULL on every row not produced by that path,
    -- including all hook-written rows.
    budget          INTEGER,
    s_min           INTEGER,
    econ_context    REAL,
    econ_rounds     REAL,
    econ_output     REAL,
    econ_source     TEXT,             -- observed | measured_defaults
    k_selected      INTEGER,
    n_total         INTEGER,
    coeff_version   INTEGER,
    target_outline  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_read_events_ts ON read_events(ts);
-- idx_read_events_dedup is created in MIGRATIONS, not here. DDL runs as one batch
-- against a database that may predate E7, where session_id does not yet exist —
-- the index would fail, and because init_schema propagates that with `?`, the
-- whole migration would abort before a single ALTER ran. That is the 1.1.3 defect
-- with a new trigger, and it would have taken the daemon down with it.
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

    /// `read_events` exactly as it existed before E7 — the 0.1.0-era shape plus
    /// the `channel` column that 1.0.x added.
    ///
    /// Hand-written for the same reason as PRE_1_1_0_TURNS: generating it from the
    /// current DDL would already contain the new columns and the test could never
    /// fail.
    const PRE_E7_READ_EVENTS: &str = "\
        CREATE TABLE read_events ( \
            ts TEXT NOT NULL, \
            tool TEXT NOT NULL, \
            path TEXT NOT NULL, \
            lines INTEGER, \
            tokens_returned INTEGER NOT NULL, \
            full_tokens INTEGER NOT NULL, \
            saved_tokens INTEGER NOT NULL, \
            routed_via TEXT NOT NULL, \
            channel TEXT NOT NULL DEFAULT 'unknown' \
        )";

    /// The six columns E7 adds, and the index over them.
    const E7_COLUMNS: [&str; 6] = [
        "session_id",
        "file_mtime",
        "req_key",
        "is_subagent",
        "writer_hook",
        "token_source",
    ];

    async fn index_names(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
        let rows: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='{table}'"
        )))
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter().map(|(n,)| n).collect()
    }

    #[tokio::test]
    async fn a_pre_e7_database_gains_every_new_column_and_the_index() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_E7_READ_EVENTS)
            .execute(&pool)
            .await
            .unwrap();

        init_schema(&pool).await.unwrap();

        let cols = columns(&pool, "read_events").await;
        for c in E7_COLUMNS {
            assert!(
                cols.contains(&c.to_string()),
                "missing column {c}: {cols:?}"
            );
        }
        assert!(
            index_names(&pool, "read_events")
                .await
                .contains(&"idx_read_events_dedup".to_string()),
            "the dedup index must exist"
        );
    }

    #[tokio::test]
    async fn a_pre_e7_database_accepts_the_insert_the_meter_performs() {
        // Column presence is not enough — 1.1.3 was an insert failing on an
        // unknown column while every report looked fine.
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_E7_READ_EVENTS)
            .execute(&pool)
            .await
            .unwrap();
        init_schema(&pool).await.unwrap();

        sqlx::query(
            "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,\
             saved_tokens,routed_via,channel,session_id,file_mtime,req_key,\
             is_subagent,writer_hook,token_source) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("Read")
        .bind("/x.rs")
        .bind(10_i64)
        .bind(5_i64)
        .bind(9_i64)
        .bind(4_i64)
        .bind("builtin_read")
        .bind("cli")
        .bind("sess-1")
        .bind(1_700_000_000_i64)
        .bind("/x.rs")
        .bind(0_i64)
        .bind("lumen_meter.sh")
        .bind("measured")
        .execute(&pool)
        .await
        .expect("the meter's insert must succeed after migration");

        let got: (String, i64, String, String) =
            sqlx::query_as("SELECT session_id, file_mtime, req_key, token_source FROM read_events")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            got,
            (
                "sess-1".into(),
                1_700_000_000,
                "/x.rs".into(),
                "measured".into()
            )
        );
    }

    #[tokio::test]
    async fn a_fresh_database_and_a_migrated_one_are_structurally_identical() {
        // Compared by PRAGMA, not by .schema text. ALTER TABLE appends columns with
        // SQLite's `, col TYPE)` style while DDL declares them inline, so the raw
        // text necessarily differs even when the tables are equivalent.
        let fresh_dir = TempDir::new().unwrap();
        let fresh = pool_in(&fresh_dir).await;
        init_schema(&fresh).await.unwrap();

        let mig_dir = TempDir::new().unwrap();
        let migrated = pool_in(&mig_dir).await;
        sqlx::raw_sql(PRE_E7_READ_EVENTS)
            .execute(&migrated)
            .await
            .unwrap();
        init_schema(&migrated).await.unwrap();

        assert_eq!(
            columns(&fresh, "read_events").await,
            columns(&migrated, "read_events").await,
            "column sets and order must agree"
        );
        let mut a = index_names(&fresh, "read_events").await;
        let mut b = index_names(&migrated, "read_events").await;
        a.sort();
        b.sort();
        assert_eq!(a, b, "index sets must agree");
    }

    #[tokio::test]
    async fn migrating_twice_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(PRE_E7_READ_EVENTS)
            .execute(&pool)
            .await
            .unwrap();

        init_schema(&pool).await.unwrap();
        let once = columns(&pool, "read_events").await;
        init_schema(&pool).await.expect("second run must not error");
        let twice = columns(&pool, "read_events").await;

        assert_eq!(once, twice, "a second migration must be a no-op");
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
