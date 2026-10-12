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
    // (is_subagent was added here from 1.1.0 to 1.5.x. It is dropped at the end of the
    // read_events block — never re-add it above the DROP, or every open re-creates it.)
    //
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
    // ── 1.6.0: read_events.is_subagent is gone ───────────────────────────────
    // Every writer inserted a literal 0 from the day it was added, so every row said
    // "main agent" whether it was or not, and a GROUP BY on it looked exactly like a
    // measurement. It cannot be populated honestly for every writer: Claude Code hands
    // hooks an `agent_id` only inside a subagent, but an MCP tool call carries no agent
    // identity at all. A column that is true for one writer and a constant for another
    // is the same lie with better cover, and nothing ever read it. Dropping it loses no
    // information — every stored value was the literal.
    //
    // DROP COLUMN needs SQLite 3.35; both drivers bundle a newer one. On a database that
    // never had the column (fresh, or pre-E7) it fails with "no such column" and is
    // swallowed like every other already-applied migration.
    "ALTER TABLE read_events DROP COLUMN is_subagent",
    "CREATE INDEX IF NOT EXISTS idx_read_events_dedup \
     ON read_events(session_id, path, file_mtime)",
    // `faults` is also in DDL, which is enough for a fresh database. It is repeated
    // here because DDL is one batch that aborts on the first failing statement: on a
    // database old enough for an earlier statement to fail, a table declared in DDL
    // never gets created. MIGRATIONS statements are swallowed individually, so this
    // is the arm that cannot be skipped by someone else's error.
    "CREATE TABLE IF NOT EXISTS faults (\
        ts TEXT NOT NULL, kind TEXT NOT NULL, variant TEXT NOT NULL DEFAULT '-', \
        path TEXT, lines INTEGER, detail TEXT, session_id TEXT, \
        version TEXT, channel TEXT NOT NULL DEFAULT 'unknown')",
    "CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant)",
];

/// Column count of `read_events` on a current database — DDL plus every ALTER in
/// [`MIGRATIONS`]. `lumen report` compares the live table against this to detect a
/// database that missed a migration, so adding an ALTER means bumping this number;
/// `read_events_column_count_matches_the_constant` fails if you forget.
pub const READ_EVENTS_COLUMNS: usize = 24;

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

-- One row per fault occurrence; `lumen report` aggregates count/first/last at read
-- time, the way it already does for read_events. Writers never come here directly:
-- everything appends to the JSONL spool (see lumen_core::faults) and `lumen report`
-- drains it, so no hook on a blocking path ever waits on this lock.
CREATE TABLE IF NOT EXISTS faults (
    ts         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    -- Sub-kind: a decline route, or which fail-open guard fired. '-' when the kind
    -- has none, matching the sentinel the renderer fingerprints on.
    variant    TEXT NOT NULL DEFAULT '-',
    path       TEXT,
    lines      INTEGER,
    detail     TEXT,
    session_id TEXT,
    -- Version that produced the fault, so a fixed regression stops matching.
    version    TEXT,
    channel    TEXT NOT NULL DEFAULT 'unknown'
);
CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant);
-- idx_read_events_dedup is created in MIGRATIONS, not here. DDL runs as one batch
-- against a database that may predate E7, where session_id does not yet exist —
-- the index would fail, and because init_schema propagates that with `?`, the
-- whole migration would abort before a single ALTER ran. That is the 1.1.3 defect
-- with a new trigger, and it would have taken the daemon down with it.
"#;
