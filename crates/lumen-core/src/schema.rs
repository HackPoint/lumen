use crate::busy;

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
/// The ledger is read first, and a current one is left alone: no write, no write lock, so an
/// open of it never queues behind a writer. What a current ledger holds is what a fresh one
/// holds ([`Shape`]), and [`MIGRATIONS`] only ever add to it or drop the few things they name.
/// When something is missing the upgrade is one transaction that asks for the write lock at its
/// start (`BEGIN IMMEDIATE`, which waits for it) and reads again inside, because another opener
/// may have done the work in the meantime. A deferred transaction that reads first and writes
/// later fails at once, whatever the busy timeout, when another connection committed after its
/// read: SQLITE_BUSY_SNAPSHOT, which with many openers at one instant could refuse the same one
/// every time.
///
/// Migration errors are swallowed, as they are on the rusqlite side: each
/// statement is additive and idempotent, so "duplicate column" is the expected
/// outcome on an already-current database. Any other failure leaves a column
/// missing, and the insert that binds it fails and is filed (`ingest_failed`,
/// `meter_write_failed`). One error is not read that way: a ledger that stayed
/// busy (see [`crate::busy`]) means the statement did not run, and calling that
/// "already applied" would leave a column or an index missing for good. It is
/// waited for, and returned if the wait runs out; the next open runs the
/// migration again. [`BACKFILLS`] run in that transaction when it happens, and only when a row
/// needs it; on a ledger whose schema is whole they are a transaction of their own, which a
/// ledger that stays busy for does not fail the open.
pub async fn init_schema(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    busy::retry_busy(|| upgrade(pool)).await?;
    busy::retry_busy(|| backfill(pool)).await.ok();
    Ok(())
}

async fn is_wal(db: &mut sqlx::SqliteConnection) -> Result<bool, sqlx::Error> {
    let mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
        .fetch_one(db)
        .await?;
    Ok(mode.eq_ignore_ascii_case("wal"))
}

async fn live_shape(db: &mut sqlx::SqliteConnection) -> Result<Shape, sqlx::Error> {
    let objects = sqlx::query_as::<_, (String, String)>(Shape::OBJECTS_SQL)
        .fetch_all(&mut *db)
        .await?;
    let columns = sqlx::query_as::<_, (String, String)>(Shape::COLUMNS_SQL)
        .fetch_all(&mut *db)
        .await?;
    Ok(Shape::from_rows(objects, columns))
}

async fn backfill_pending(
    db: &mut sqlx::SqliteConnection,
    backfill: &Backfill,
) -> Result<i64, sqlx::Error> {
    // Literals in BACKFILLS, as below.
    sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(backfill.pending))
        .fetch_one(db)
        .await
}

/// One attempt at the schema upgrade; [`busy::retry_busy`] runs it again when the ledger was
/// busy. A current ledger is only read.
async fn upgrade(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    {
        let mut conn = pool.acquire().await?;
        if is_wal(&mut conn).await? && !live_shape(&mut conn).await?.lacks_something() {
            return Ok(());
        }
        // The switch to WAL cannot happen inside a transaction. It is a no-op once done.
        if !is_wal(&mut conn).await? {
            sqlx::raw_sql("PRAGMA journal_mode=WAL")
                .execute(&mut *conn)
                .await?;
        }
    }
    // An error leaves the transaction to roll back as it drops.
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if live_shape(&mut tx).await?.lacks_something() {
        sqlx::raw_sql(sqlx::AssertSqlSafe(ddl_in_transaction()))
            .execute(&mut *tx)
            .await?;
        for migration in MIGRATIONS {
            // AssertSqlSafe is sound here: every statement is a literal in
            // MIGRATIONS above, so no runtime value can reach the SQL.
            if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(*migration))
                .execute(&mut *tx)
                .await
                && busy::is_busy(&e)
            {
                return Err(e);
            }
            probe::after_migration();
        }
    }
    rewrite_rows(&mut tx).await?;
    tx.commit().await
}

/// [`BACKFILLS`] that have a row to rewrite, inside the caller's write transaction.
async fn rewrite_rows(db: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    for backfill in BACKFILLS {
        let pending = backfill_pending(db, backfill).await.unwrap_or(0);
        if pending != 0 {
            probe::before_rewrite();
            if let Err(e) = sqlx::raw_sql(sqlx::AssertSqlSafe(backfill.rewrite))
                .execute(&mut *db)
                .await
                && busy::is_busy(&e)
            {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// The row rewrites on a ledger whose schema is whole: read first, so a ledger with no row to
/// rewrite is not asked for the write lock, then one transaction that takes it first.
async fn backfill(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    let mut pending = false;
    {
        let mut conn = pool.acquire().await?;
        for backfill in BACKFILLS {
            pending |= backfill_pending(&mut conn, backfill).await? != 0;
        }
    }
    if !pending {
        return Ok(());
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    rewrite_rows(&mut tx).await?;
    tx.commit().await
}

/// What the ledger holds: its tables, views and indexes by name, and the columns of each table.
///
/// A fresh ledger's shape ([`Shape::current`]) is what a current ledger has at least, and
/// whatever [`MIGRATIONS`] drop is what it has none of. That is the whole question of whether an
/// open has anything to write, answered by reading `sqlite_master`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shape {
    objects: std::collections::BTreeSet<(String, String)>,
    columns: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

impl Shape {
    /// Type and name of every table, view and index SQLite did not make itself.
    pub const OBJECTS_SQL: &'static str = "SELECT type, name FROM sqlite_master \
        WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'";
    /// Table and column name of every column of every table.
    pub const COLUMNS_SQL: &'static str = "SELECT m.name, p.name FROM sqlite_master m \
        JOIN pragma_table_info(m.name) p WHERE m.type = 'table' \
        AND m.name NOT LIKE 'sqlite\\_%' ESCAPE '\\'";

    pub fn from_rows(
        objects: impl IntoIterator<Item = (String, String)>,
        columns: impl IntoIterator<Item = (String, String)>,
    ) -> Shape {
        let mut shape = Shape {
            objects: objects.into_iter().collect(),
            ..Shape::default()
        };
        for (table, column) in columns {
            shape.columns.entry(table).or_default().insert(column);
        }
        shape
    }

    /// The shape of a ledger made from nothing by [`DDL`] and [`MIGRATIONS`], built once in memory.
    fn current() -> &'static Shape {
        static CURRENT: std::sync::OnceLock<Shape> = std::sync::OnceLock::new();
        CURRENT.get_or_init(|| {
            let read = |conn: &rusqlite::Connection, sql: &str| -> Vec<(String, String)> {
                let mut stmt = conn.prepare(sql).expect("read sqlite_master");
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .expect("read sqlite_master")
                    .map(|row| row.expect("a row of sqlite_master"))
                    .collect()
            };
            let conn = rusqlite::Connection::open_in_memory().expect("an in-memory database");
            conn.execute_batch(ddl_in_transaction())
                .expect("DDL on an empty database");
            for migration in MIGRATIONS {
                // Statements that find their work done fail, as on every ledger.
                if conn.execute_batch(migration).is_err() {
                    continue;
                }
            }
            Shape::from_rows(
                read(&conn, Shape::OBJECTS_SQL),
                read(&conn, Shape::COLUMNS_SQL),
            )
        })
    }

    /// What [`MIGRATIONS`] remove: `DROP TABLE IF EXISTS t` and `ALTER TABLE t DROP COLUMN c`.
    fn dropped() -> (Vec<&'static str>, Vec<(&'static str, &'static str)>) {
        let (mut tables, mut columns) = (Vec::new(), Vec::new());
        for migration in MIGRATIONS {
            let words: Vec<&str> = migration.split_whitespace().collect();
            match words.as_slice() {
                ["DROP", "TABLE", "IF", "EXISTS", table] => tables.push(*table),
                ["ALTER", "TABLE", table, "DROP", "COLUMN", column] => {
                    columns.push((*table, *column));
                }
                _ => {}
            }
        }
        (tables, columns)
    }

    /// Whether this ledger is missing something a fresh one has, or still has something the
    /// migrations drop. Anything else it holds, a newer build's table or column, is left alone.
    pub fn lacks_something(&self) -> bool {
        let current = Shape::current();
        if !current.objects.is_subset(&self.objects) {
            return true;
        }
        for (table, wanted) in &current.columns {
            if self
                .columns
                .get(table)
                .is_none_or(|have| !wanted.is_subset(have))
            {
                return true;
            }
        }
        let (tables, columns) = Shape::dropped();
        tables
            .iter()
            .any(|t| self.objects.iter().any(|(_, name)| name == t))
            || columns
                .iter()
                .any(|(t, c)| self.columns.get(*t).is_some_and(|have| have.contains(*c)))
    }
}

/// [`DDL`] without its `PRAGMA journal_mode=WAL`, which cannot run inside a transaction.
pub fn ddl_in_transaction() -> &'static str {
    static TABLES: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| DDL.replacen("PRAGMA journal_mode=WAL;", "", 1))
}

/// Outside tests, the seams do nothing.
#[cfg(not(test))]
pub(crate) mod probe {
    #[inline(always)]
    pub fn after_migration() {}

    #[inline(always)]
    pub fn before_rewrite() {}
}

/// Test seams, on the thread that runs the upgrade: one runs after each migration statement, the
/// other after a backfill has found a row to rewrite and before it rewrites it.
#[cfg(test)]
pub(crate) mod probe {
    use std::cell::RefCell;

    type Hook = Box<dyn FnMut()>;

    thread_local! {
        static AFTER_MIGRATION: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static BEFORE_REWRITE: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub fn set(hook: impl FnMut() + 'static) {
        AFTER_MIGRATION.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn set_before_rewrite(hook: impl FnMut() + 'static) {
        BEFORE_REWRITE.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    }

    pub fn clear() {
        AFTER_MIGRATION.with(|h| *h.borrow_mut() = None);
        BEFORE_REWRITE.with(|h| *h.borrow_mut() = None);
    }

    pub fn after_migration() {
        AFTER_MIGRATION.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }

    pub fn before_rewrite() {
        BEFORE_REWRITE.with(|h| {
            if let Some(hook) = h.borrow_mut().as_mut() {
                hook();
            }
        });
    }
}

/// Open the ledger at `url` as a pool and bring it up to date: what the daemon runs before it
/// serves anything and what the app runs before it reads.
///
/// A ledger another program holds at that moment is waited for, not failed on, by [`init_schema`].
/// The connection takes no lock a writer could refuse: sqlx leaves the journal mode alone unless
/// asked, and the switch to WAL is the DDL's.
pub async fn open_pool(url: &str) -> Result<sqlx::SqlitePool, sqlx::Error> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new().connect(url).await?;
    if let Err(e) = init_schema(&pool).await {
        pool.close().await;
        return Err(e);
    }
    Ok(pool)
}

/// Additive migrations run after DDL to upgrade existing DBs.
/// Each statement is idempotent: the openers read any error but a busy ledger as "already applied".
pub const MIGRATIONS: &[&str] = &[
    "ALTER TABLE read_events ADD COLUMN channel TEXT NOT NULL DEFAULT 'unknown'",
    // Subagent transcripts reuse the parent's sessionId, so their turns land in
    // the parent session. They are real spend (kept in cost) but carry their own
    // fresh context, so they must NOT drive the context-fill gauge.
    "ALTER TABLE turns ADD COLUMN is_subagent INTEGER NOT NULL DEFAULT 0",
    // ── E7 instrumentation ───────────────────────────────────────────────────
    // Existing rows stay NULL. There is no honest way to backfill provenance for
    // events already recorded, and inventing it would be worse than admitting the
    // gap: a NULL here means "unknown", which is the truth.
    //
    // session_id  — from CLAUDE_CODE_SESSION_ID, which Claude Code exports to both
    //               hooks and the MCP server. Lets a read be tied to the turn that
    //               caused it, which bare second-precision timestamps cannot do
    //               when several sessions run at once. From 1.8.0 the MCP tools' rows
    //               take it from the PreToolUse hook's record of the call, not from
    //               the environment: that server outlives `/clear`, and its
    //               environment names the conversation it started in. NULL is the
    //               honest value for a call the hook did not announce.
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
    // ── 1.7.0: repeat reads ──────────────────────────────────────────────────
    // repeat_arm  — 'treatment' when the repeat-read path was live for this read,
    //               'control' when the path was withheld by the A/B split. NULL on every
    //               row written while repeat reads are off, and on every hook row.
    // repeat_plan — what the repeat decision was: first, fresh, unchanged, delta, or a full
    //               answer for a stated reason (compaction, rewind, unverifiable, no_base,
    //               outline, delta_too_big, unchanged_not_smaller), or `unidentified` when the
    //               call could not be tied to a conversation. A control
    //               row records what the treatment arm WOULD have done, which is what
    //               makes the two arms comparable on the same population of repeat reads.
    "ALTER TABLE read_events ADD COLUMN repeat_arm TEXT",
    "ALTER TABLE read_events ADD COLUMN repeat_plan TEXT",
    // ── 1.8.0: whose call it was ─────────────────────────────────────────────
    // agent_id — which subagent made the call, NULL for the main agent. Subagents share their
    //            parent's session_id, so the pair (session_id, agent_id) is the context
    //            that holds what a read returned. NULL also on every row whose session_id is
    //            NULL: nothing is claimed about a call that could not be placed.
    "ALTER TABLE read_events ADD COLUMN agent_id TEXT",
    // Compactions and rewinds the daemon saw in a session's transcript, one row per
    // event, so lumen-mcp can tell that what it delivered earlier may no longer be in the
    // model's context. Also in DDL; repeated here for the reason `faults` is.
    "CREATE TABLE IF NOT EXISTS session_events (\
        session_id TEXT NOT NULL, kind TEXT NOT NULL, uuid TEXT NOT NULL, \
        ts_ms INTEGER NOT NULL, source_file TEXT, PRIMARY KEY (session_id, kind, uuid))",
    // An index goes here, after the statement that creates its table, never in DDL.
    "CREATE INDEX IF NOT EXISTS idx_session_events_session ON session_events(session_id, ts_ms)",
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
        version TEXT, channel TEXT NOT NULL DEFAULT 'unknown', agent_id TEXT)",
    "CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant)",
    // ── 1.9.0: the sessions table is gone ────────────────────────────────────
    // Every DDL from the first release to 1.8.0 created `sessions`, and no release ever wrote a
    // row to it or read one: a session is the `session_id` its turns carry. An empty table
    // that looks like the place to find sessions misleads whoever reads the schema next.
    //
    // IF EXISTS makes this a no-op on a fresh database, and on one where it already ran.
    // An older build that opens the ledger afterwards creates the table again, empty, through
    // its own CREATE TABLE IF NOT EXISTS, and the next open by this one drops it again. Nothing
    // in either reads it, so the round trip changes no figure.
    "DROP TABLE IF EXISTS sessions",
    // ── 1.9.0: the speed a turn ran at ───────────────────────────────────────
    // usage.speed, "standard" or "fast". Fast mode costs a premium on the models that have
    // it, so a turn is priced at the rate of the speed it ran at. NULL on every turn ingested
    // before 1.9.0, and on a transcript that does not record it: unknown, which is the truth,
    // and a cost that holds such a turn on a model with fast mode is shown as a floor.
    "ALTER TABLE turns ADD COLUMN speed TEXT",
    // ── 1.9.0: where a turn's inference ran ──────────────────────────────────
    // usage.inference_geo as the transcript wrote it: "us", "global", or "not_available", which
    // Claude Code writes when it has no region to report. US-only inference costs 1.1 times on
    // the 4.6 and later models, so a turn is priced at the rate of the region it ran in. NULL on
    // every turn ingested before 1.9.0, and on a transcript that does not record it: unknown, and
    // a cost that holds such a turn, or a "not_available" one, on a model where US-only costs more
    // is shown as a floor. A 1.8.0 build that writes to the ledger afterwards names its columns,
    // so its turns get NULL here too.
    "ALTER TABLE turns ADD COLUMN inference_geo TEXT",
    // ── 1.9.0: whose Read a fault was ────────────────────────────────────────
    // agent_id — the subagent the hook event belonged to; NULL for the main agent, and on every
    //            fault recorded before 1.9.0. The intercept blocks a path once per context, a
    //            subagent's apart from its parent's, so the savings summary counts a block per
    //            session, agent and path; without this, a subagent's block and its parent's on
    //            the same file could not be told apart.
    "ALTER TABLE faults ADD COLUMN agent_id TEXT",
    // ── 1.9.0: the Reads the intercept blocked, from the transcripts ─────────
    // A block writes no row (the hook keeps only a marker file), and the escape valve's fault
    // records only a later Read of a blocked path. The transcript of the agent that tried the
    // Read records every block: the daemon reads them there, one row per blocked Read, keyed by
    // the Read's tool_use id, which survives the daemon re-reading every transcript on start.
    //
    // Both tables are also in DDL, which is enough for a fresh ledger, and are repeated here as
    // `session_events` (1.7.0) and `faults` are: DDL runs as one batch that stops at its first
    // failing statement, so on a ledger old enough for an earlier statement there to fail, a table
    // declared only in DDL is never created. Each statement here runs on its own. Their indexes
    // are here and not in DDL, for the reason given above `idx_session_events_session`.
    "CREATE TABLE IF NOT EXISTS intercept_blocks (\
        tool_use_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent_id TEXT, \
        path TEXT NOT NULL, lines INTEGER, kind TEXT NOT NULL, ts_ms INTEGER NOT NULL, \
        uuid TEXT NOT NULL, source_file TEXT)",
    "CREATE INDEX IF NOT EXISTS idx_intercept_blocks_session \
     ON intercept_blocks(session_id, ts_ms)",
    // Which transcripts the daemon has read for blocks from their first line, and through which
    // turn of each session in them: where one has been read, a block it does not hold did not
    // happen there, so the savings summary need not bound one from the faults.
    "CREATE TABLE IF NOT EXISTS transcript_scans (\
        source_file TEXT NOT NULL, session_id TEXT NOT NULL, agent_id TEXT, \
        matcher INTEGER NOT NULL, through_ms INTEGER NOT NULL, \
        PRIMARY KEY (source_file, session_id))",
    "CREATE INDEX IF NOT EXISTS idx_transcript_scans_session ON transcript_scans(session_id)",
    // 1.10.0: how far the late-row merge (legacy::merge) has carried each old ledger, per table, and
    // a spool copy in flight. Kept in this ledger so a batch and its progress commit together:
    // rows are merged by position (rowid past `hwm`), never by matching their content.
    "CREATE TABLE IF NOT EXISTS legacy_merge (\
        source TEXT NOT NULL, tbl TEXT NOT NULL, hwm INTEGER NOT NULL, sig TEXT, \
        PRIMARY KEY (source, tbl))",
    "CREATE TABLE IF NOT EXISTS legacy_spool_intent (\
        source TEXT PRIMARY KEY, src_from INTEGER NOT NULL, src_to INTEGER NOT NULL, \
        dst_before INTEGER NOT NULL)",
];

/// A row rewrite and the read that says whether it has anything to do.
pub struct Backfill {
    /// A query for one integer, non-zero when [`Self::rewrite`] would change a row. It only reads,
    /// so a ledger with nothing to rewrite is not asked for the write lock.
    pub pending: &'static str,
    pub rewrite: &'static str,
}

/// Row rewrites, run after [`MIGRATIONS`]: a ledger that stays busy for one is skipped, not an
/// error, and the next open runs it again.
pub const BACKFILLS: &[Backfill] = &[
    // Mark the turns ingested from a subagent path. Idempotent, so safe to re-run on every open.
    // Either separator: a ledger written on Windows holds backslashes, and the savings summary
    // reads a turn's subagent from this column alone (LIKE has no escape character here, so the
    // backslash is literal).
    Backfill {
        pending: "SELECT EXISTS (SELECT 1 FROM turns \
            WHERE is_subagent = 0 \
              AND (source_file LIKE '%/subagents/%' OR source_file LIKE '%\\subagents\\%'))",
        rewrite: "UPDATE turns SET is_subagent = 1 \
            WHERE is_subagent = 0 \
              AND (source_file LIKE '%/subagents/%' OR source_file LIKE '%\\subagents\\%')",
    },
];

/// Column count of `read_events` on a current database — DDL plus every ALTER in
/// [`MIGRATIONS`]. `lumen report` compares the live table against this to detect a
/// database that missed a migration, so adding an ALTER means bumping this number;
/// `read_events_column_count_matches_the_constant` fails if you forget.
pub const READ_EVENTS_COLUMNS: usize = 27;

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
    is_subagent                 INTEGER NOT NULL DEFAULT 0,
    -- 1.9.0: usage.speed, standard | fast | NULL when not recorded. Last, where the ALTER
    -- in MIGRATIONS puts it on an older ledger, so the two agree on column order.
    speed                       TEXT,
    -- 1.9.0: usage.inference_geo, us | global | not_available | NULL when not recorded.
    -- After speed, in the order the ALTERs in MIGRATIONS add them.
    inference_geo               TEXT
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
    target_outline  INTEGER,
    -- 1.7.0 repeat reads. NULL while repeat reads are off.
    repeat_arm      TEXT,             -- treatment | control
    repeat_plan     TEXT,
    -- 1.8.0 conversation identity. The subagent that made the call; NULL for the main agent.
    agent_id        TEXT
);
CREATE INDEX IF NOT EXISTS idx_read_events_ts ON read_events(ts);

-- Compactions and rewinds seen in a session's transcript (kind: compaction | rewind).
-- The daemon re-reads transcripts from the start after a restart, so a row is keyed by the
-- transcript record that caused it and written with INSERT OR IGNORE. Its index is in
-- MIGRATIONS, not here.
CREATE TABLE IF NOT EXISTS session_events (
    session_id  TEXT NOT NULL,
    kind        TEXT NOT NULL,
    uuid        TEXT NOT NULL,
    ts_ms       INTEGER NOT NULL,    -- the record's own timestamp, unix ms
    source_file TEXT,
    PRIMARY KEY (session_id, kind, uuid)
);

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
    channel    TEXT NOT NULL DEFAULT 'unknown',
    -- 1.9.0: the subagent the hook event belonged to; NULL for the main agent and before.
    -- Declared where the ALTER in MIGRATIONS adds it on an older database.
    agent_id   TEXT
);
CREATE INDEX IF NOT EXISTS idx_faults_kind ON faults(kind, variant);

-- 1.9.0: Reads the intercept blocked, as the transcript of the agent that tried each records it
-- (the hook writes no row for a block). Keyed by the Read's tool_use id and written with INSERT OR
-- IGNORE: the daemon re-reads every transcript from its first line on start. Its index is in
-- MIGRATIONS, not here.
CREATE TABLE IF NOT EXISTS intercept_blocks (
    tool_use_id TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    agent_id    TEXT,              -- the subagent whose transcript holds it; NULL for the main agent
    path        TEXT NOT NULL,     -- as the hook named it, which is what its faults name
    lines       INTEGER,
    kind        TEXT NOT NULL,     -- redirect | log | served (blocked again after a Lumen answer)
    ts_ms       INTEGER NOT NULL,  -- the tool_result record's own timestamp, unix ms
    uuid        TEXT NOT NULL,     -- the tool_result record's uuid
    source_file TEXT
);

-- 1.9.0: transcripts the daemon has read for blocks from their first line (by matcher, the
-- version of lumen_core::intercept that read them): one row per file and session in it, through
-- that session's latest turn read there.
CREATE TABLE IF NOT EXISTS transcript_scans (
    source_file TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    agent_id    TEXT,
    matcher     INTEGER NOT NULL,
    through_ms  INTEGER NOT NULL,
    PRIMARY KEY (source_file, session_id)
);

-- 1.10.0: progress of the late-row merge from an old ledger (see MIGRATIONS).
CREATE TABLE IF NOT EXISTS legacy_merge (
    source TEXT NOT NULL,
    tbl    TEXT NOT NULL,
    hwm    INTEGER NOT NULL,
    sig    TEXT,
    PRIMARY KEY (source, tbl)
);
CREATE TABLE IF NOT EXISTS legacy_spool_intent (
    source     TEXT PRIMARY KEY,
    src_from   INTEGER NOT NULL,
    src_to     INTEGER NOT NULL,
    dst_before INTEGER NOT NULL
);
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

    /// `lumen report` reports a database whose `read_events` column count differs from
    /// [`READ_EVENTS_COLUMNS`] as `schema_drift`. If an ALTER lands without bumping the
    /// constant, every install would report drift against a correct database — so the
    /// constant is pinned to what DDL plus MIGRATIONS actually produce.
    #[test]
    fn read_events_column_count_matches_the_constant() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(DDL).unwrap();
        for m in MIGRATIONS {
            let _ = conn.execute_batch(m);
        }

        let mut stmt = conn.prepare("PRAGMA table_info(read_events)").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(
            names.len(),
            READ_EVENTS_COLUMNS,
            "read_events has {} columns ({}), but READ_EVENTS_COLUMNS says {}",
            names.len(),
            names.join(", "),
            READ_EVENTS_COLUMNS,
        );
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
            (
                "win",
                r"C:\Users\me\.claude\projects\C--p\abc\subagents\agent-2.jsonl",
            ),
            ("winmain", r"C:\Users\me\.claude\projects\C--p\abc.jsonl"),
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
            vec![
                ("main".to_string(), 0),
                ("sub".to_string(), 1),
                ("win".to_string(), 1),
                ("winmain".to_string(), 0),
            ],
            "the subagent row must be backfilled from its source_file, under either separator"
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

    /// The columns E7 adds that are still current, and the index over them. E7 also
    /// added `is_subagent`, which 1.6.0 drops.
    const E7_COLUMNS: [&str; 5] = [
        "session_id",
        "file_mtime",
        "req_key",
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
            !cols.contains(&"is_subagent".to_string()),
            "a pre-E7 database must not gain the dropped column on its way up: {cols:?}"
        );
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
             writer_hook,token_source) \
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
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
        assert!(
            columns(&pool, "sessions").await.is_empty(),
            "a fresh database must not have the dropped sessions table"
        );
    }

    // ── 1.6.0: read_events.is_subagent dropped ───────────────────────────────

    /// `read_events` exactly as 1.5.1 shipped it — 25 columns with `is_subagent` among
    /// them, and both indexes. Hand-written for the same reason as the shapes above: the
    /// current DDL no longer has the column, so generating this from it could not fail.
    const READ_EVENTS_1_5_1: &str = "\
        CREATE TABLE read_events ( \
            ts TEXT NOT NULL, tool TEXT NOT NULL, path TEXT NOT NULL, lines INTEGER, \
            tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL, \
            saved_tokens INTEGER NOT NULL, routed_via TEXT NOT NULL, \
            channel TEXT NOT NULL DEFAULT 'unknown', session_id TEXT, file_mtime INTEGER, \
            req_key TEXT, is_subagent INTEGER NOT NULL DEFAULT 0, writer_hook TEXT, \
            token_source TEXT, budget INTEGER, s_min INTEGER, econ_context REAL, \
            econ_rounds REAL, econ_output REAL, econ_source TEXT, k_selected INTEGER, \
            n_total INTEGER, coeff_version INTEGER, target_outline INTEGER); \
        CREATE INDEX idx_read_events_ts ON read_events(ts); \
        CREATE INDEX idx_read_events_dedup ON read_events(session_id, path, file_mtime);";

    /// The kinds of row a 1.5.1 ledger holds: a hook Read, an MCP read carrying the full
    /// ranked provenance, an estimated Bash row. Values differ column to column, so a
    /// shifted or truncated column cannot compare equal by accident.
    const ROWS_1_5_1: &str = "\
        INSERT INTO read_events VALUES ('2026-09-01T10:00:00Z','Read','/a.rs',412,5100, \
            5100,0,'builtin_read','cli','s-1',1767000000,'/a.rs',0,'lumen_meter.sh', \
            'measured',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
        INSERT INTO read_events VALUES ('2026-09-01T10:00:05Z','mcp__lumen__smart_read', \
            '/b.rs',900,700,9000,8300,'smart_read','vscode','s-2',1767000100, \
            's-2:/b.rs:1767000100',0,'lumen-mcp','measured',4100,4900,61000.5,65.0, \
            1200.25,'observed',7,19,3,600); \
        INSERT INTO read_events VALUES ('2026-09-01T10:00:09Z','Bash','cargo test',NULL,0, \
            321,0,'bash_output','unknown',NULL,NULL,NULL,0,'lumen_meter.sh','estimated', \
            NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL);";

    /// Every column that survives the drop, in order.
    const SURVIVING: [&str; 24] = [
        "ts",
        "tool",
        "path",
        "lines",
        "tokens_returned",
        "full_tokens",
        "saved_tokens",
        "routed_via",
        "channel",
        "session_id",
        "file_mtime",
        "req_key",
        "writer_hook",
        "token_source",
        "budget",
        "s_min",
        "econ_context",
        "econ_rounds",
        "econ_output",
        "econ_source",
        "k_selected",
        "n_total",
        "coeff_version",
        "target_outline",
    ];

    /// What a migrated 1.5.1 database has: the surviving 1.5.1 columns, then 1.7.0's two and
    /// 1.8.0's one.
    fn migrated_columns() -> Vec<String> {
        let mut c = SURVIVING.map(String::from).to_vec();
        c.extend(["repeat_arm".into(), "repeat_plan".into(), "agent_id".into()]);
        c
    }

    /// Each row rendered by SQLite's own `quote()`, so both drivers compare the stored
    /// values byte for byte rather than through their type mappings.
    fn rendered_rows_sql() -> String {
        let cols: Vec<String> = SURVIVING.iter().map(|c| format!("quote({c})")).collect();
        format!(
            "SELECT {} FROM read_events ORDER BY ts",
            cols.join(" || '|' || ")
        )
    }

    fn rendered_rows(conn: &rusqlite::Connection) -> Vec<String> {
        let mut stmt = conn.prepare(&rendered_rows_sql()).unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn rusqlite_columns(conn: &rusqlite::Connection) -> Vec<String> {
        let mut stmt = conn.prepare("PRAGMA table_info(read_events)").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[tokio::test]
    async fn a_populated_1_5_1_database_loses_is_subagent_and_keeps_every_value() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(READ_EVENTS_1_5_1)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(ROWS_1_5_1).execute(&pool).await.unwrap();
        assert!(
            columns(&pool, "read_events")
                .await
                .contains(&"is_subagent".into()),
            "precondition: the 1.5.1 shape has the column"
        );
        let before: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(before.len(), 3);

        init_schema(&pool).await.unwrap();

        let cols = columns(&pool, "read_events").await;
        assert_eq!(cols, migrated_columns());
        assert_eq!(cols.len(), READ_EVENTS_COLUMNS);
        let after: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            after, before,
            "every row and every value must survive the drop"
        );

        let fresh_dir = TempDir::new().unwrap();
        let fresh = pool_in(&fresh_dir).await;
        init_schema(&fresh).await.unwrap();
        assert_eq!(
            columns(&fresh, "read_events").await,
            cols,
            "a migrated 1.5.1 database and a fresh one must agree on columns and order"
        );
        let mut a = index_names(&fresh, "read_events").await;
        let mut b = index_names(&pool, "read_events").await;
        a.sort();
        b.sort();
        assert_eq!(a, b, "index sets must agree");
    }

    /// The same upgrade through the other driver. lumen-mcp and the hook open the
    /// database with rusqlite, the daemon with sqlx, and whichever runs first after an
    /// upgrade is the one that migrates — so both arms have to do it.
    #[test]
    fn the_rusqlite_open_drops_it_too() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1_5_1.db");
        let before = {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(READ_EVENTS_1_5_1).unwrap();
            c.execute_batch(ROWS_1_5_1).unwrap();
            assert!(rusqlite_columns(&c).contains(&"is_subagent".to_string()));
            rendered_rows(&c)
        };
        assert_eq!(before.len(), 3);

        let conn = crate::meter::connect_db(&path).unwrap();

        assert_eq!(rusqlite_columns(&conn), migrated_columns());
        assert_eq!(rendered_rows(&conn), before);

        drop(conn);
        let again = crate::meter::connect_db(&path).expect("a second open must be a no-op");
        assert_eq!(rendered_rows(&again), before);
        assert_eq!(rusqlite_columns(&again).len(), READ_EVENTS_COLUMNS);
    }

    // ── 1.7.0: session_events, from a 1.6.0 database ─────────────────────────

    /// `read_events` exactly as 1.6.0 shipped it: the 24 surviving columns, both indexes,
    /// and none of 1.7.0's. Hand-written, like the shapes above, so it cannot drift with
    /// the current DDL.
    const READ_EVENTS_1_6_0: &str = "\
        CREATE TABLE read_events ( \
            ts TEXT NOT NULL, tool TEXT NOT NULL, path TEXT NOT NULL, lines INTEGER, \
            tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL, \
            saved_tokens INTEGER NOT NULL, routed_via TEXT NOT NULL, \
            channel TEXT NOT NULL DEFAULT 'unknown', session_id TEXT, file_mtime INTEGER, \
            req_key TEXT, writer_hook TEXT, token_source TEXT, budget INTEGER, \
            s_min INTEGER, econ_context REAL, econ_rounds REAL, econ_output REAL, \
            econ_source TEXT, k_selected INTEGER, n_total INTEGER, coeff_version INTEGER, \
            target_outline INTEGER); \
        CREATE INDEX idx_read_events_ts ON read_events(ts); \
        CREATE INDEX idx_read_events_dedup ON read_events(session_id, path, file_mtime);";

    const ROWS_1_6_0: &str = "\
        INSERT INTO read_events VALUES ('2026-09-01T10:00:00Z','Read','/a.rs',412,5100, \
            5100,0,'builtin_read','cli','s-1',1767000000,'/a.rs','lumen_meter.sh', \
            'measured',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL); \
        INSERT INTO read_events VALUES ('2026-09-01T10:00:05Z','mcp__lumen__smart_read', \
            '/b.rs',900,700,9000,8300,'smart_read','vscode','s-2',1767000100, \
            's-2:/b.rs:1767000100','lumen-mcp','measured',4100,4900,61000.5,65.0, \
            1200.25,'observed',7,19,3,600);";

    const SESSION_EVENT_COLUMNS: [&str; 5] = ["session_id", "kind", "uuid", "ts_ms", "source_file"];

    #[tokio::test]
    async fn a_1_6_0_database_gains_session_events_and_its_index_and_keeps_every_row() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(READ_EVENTS_1_6_0)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(ROWS_1_6_0).execute(&pool).await.unwrap();
        assert!(columns(&pool, "session_events").await.is_empty());
        let before: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(before.len(), 2);

        init_schema(&pool).await.unwrap();

        assert_eq!(columns(&pool, "read_events").await, migrated_columns());
        assert_eq!(
            columns(&pool, "session_events").await,
            SESSION_EVENT_COLUMNS
        );
        assert!(
            index_names(&pool, "session_events")
                .await
                .contains(&"idx_session_events_session".to_string())
        );
        let after: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(after, before);

        // The key makes a re-ingest harmless.
        for _ in 0..2 {
            sqlx::query(
                "INSERT OR IGNORE INTO session_events VALUES ('s-1','rewind','u-1',1000,'f.jsonl')",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let n: (i64,) = sqlx::query_as("SELECT count(*) FROM session_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n.0, 1);
        init_schema(&pool).await.expect("a second run is a no-op");
    }

    #[test]
    fn the_rusqlite_open_adds_session_events_to_a_1_6_0_database() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1_6_0.db");
        let before = {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(READ_EVENTS_1_6_0).unwrap();
            c.execute_batch(ROWS_1_6_0).unwrap();
            rendered_rows(&c)
        };
        assert_eq!(before.len(), 2);

        let conn = crate::meter::connect_db(&path).unwrap();

        assert_eq!(rusqlite_columns(&conn), migrated_columns());
        assert_eq!(rendered_rows(&conn), before);
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(session_events)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(cols, SESSION_EVENT_COLUMNS);
        let idx: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'idx_session_events_session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
        drop(conn);
        crate::meter::connect_db(&path).expect("a second open must be a no-op");
    }

    // ── 1.9.0: sessions dropped ──────────────────────────────────────────────

    /// `sessions` as every release through 1.8.0 created it, holding a row. Hand-written: the
    /// current DDL no longer has the table.
    const SESSIONS_THROUGH_1_8_0: &str = "\
        CREATE TABLE sessions ( \
            session_id TEXT PRIMARY KEY, cwd TEXT, model TEXT, started_at TEXT, \
            source_file TEXT); \
        INSERT INTO sessions VALUES ('s-1','/p','claude-opus-5','2026-09-01T10:00:00Z', \
            's-1.jsonl');";

    #[tokio::test]
    async fn a_database_with_sessions_loses_it_and_keeps_every_other_row() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        for sql in [READ_EVENTS_1_6_0, ROWS_1_6_0, SESSIONS_THROUGH_1_8_0] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        assert!(
            !columns(&pool, "sessions").await.is_empty(),
            "precondition: the table is there"
        );
        let before: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(before.len(), 2);

        init_schema(&pool).await.unwrap();

        assert!(
            columns(&pool, "sessions").await.is_empty(),
            "sessions must be dropped"
        );
        let after: Vec<(String,)> = sqlx::query_as(sqlx::AssertSqlSafe(rendered_rows_sql()))
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(after, before, "no other row may change");

        init_schema(&pool)
            .await
            .expect("a second run, with nothing left to drop, must not error");
        assert!(columns(&pool, "sessions").await.is_empty());
    }

    #[test]
    fn the_rusqlite_open_drops_sessions_too() {
        let has_sessions = |c: &rusqlite::Connection| -> bool {
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
                == 1
        };
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1_8_0.db");
        let before = {
            let c = rusqlite::Connection::open(&path).unwrap();
            for sql in [READ_EVENTS_1_6_0, ROWS_1_6_0, SESSIONS_THROUGH_1_8_0] {
                c.execute_batch(sql).unwrap();
            }
            assert!(has_sessions(&c), "precondition: the table is there");
            rendered_rows(&c)
        };
        assert_eq!(before.len(), 2);

        let conn = crate::meter::connect_db(&path).unwrap();

        assert!(!has_sessions(&conn), "sessions must be dropped");
        assert_eq!(rendered_rows(&conn), before);

        drop(conn);
        let again = crate::meter::connect_db(&path).expect("a second open must be a no-op");
        assert!(!has_sessions(&again));
        assert_eq!(rendered_rows(&again), before);
    }

    // ── 1.9.0: turns.speed and turns.inference_geo ───────────────────────────

    /// `turns` as every release through 1.8.0 created it, holding a turn. Hand-written: the
    /// current DDL has the columns.
    const TURNS_THROUGH_1_8_0: &str = "\
        CREATE TABLE turns ( \
            message_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, ts TEXT NOT NULL, \
            model TEXT, input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL, \
            cache_read_input_tokens INTEGER NOT NULL, \
            cache_creation_input_tokens INTEGER NOT NULL, cache_1h INTEGER, cache_5m INTEGER, \
            web_search_requests INTEGER, web_fetch_requests INTEGER, service_tier TEXT, \
            source_file TEXT, is_subagent INTEGER NOT NULL DEFAULT 0); \
        INSERT INTO turns (message_id, session_id, ts, model, input_tokens, output_tokens, \
            cache_read_input_tokens, cache_creation_input_tokens) \
            VALUES ('m-1','s-1','2026-09-01T10:00:00Z','claude-opus-4-8',1,2,3,4);";

    /// The turn 1.8.0's daemon writes, its INSERT verbatim: it names its columns, so a ledger
    /// this build has upgraded takes it, and the columns it does not know are NULL.
    const TURN_WRITTEN_BY_1_8_0: &str = "\
        INSERT OR IGNORE INTO turns
             (message_id, session_id, ts, model,
              input_tokens, output_tokens,
              cache_read_input_tokens, cache_creation_input_tokens, source_file,
              is_subagent)
             VALUES ('m-2','s-1','2026-09-01T11:00:00Z','claude-opus-4-8',5,6,7,8,'f.jsonl',0)";

    #[tokio::test]
    async fn a_ledger_from_before_1_9_0_gains_turns_speed_and_region_unknown_on_every_old_turn() {
        let dir = TempDir::new().unwrap();
        let pool = pool_in(&dir).await;
        sqlx::raw_sql(TURNS_THROUGH_1_8_0)
            .execute(&pool)
            .await
            .unwrap();
        init_schema(&pool).await.unwrap();

        let cols = columns(&pool, "turns").await;
        assert_eq!(
            cols[cols.len() - 2..],
            ["speed".to_string(), "inference_geo".to_string()]
        );
        sqlx::raw_sql(TURN_WRITTEN_BY_1_8_0)
            .execute(&pool)
            .await
            .unwrap();
        let rows: Vec<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT message_id, speed, inference_geo FROM turns ORDER BY rowid")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            [
                ("m-1".to_string(), None, None),
                ("m-2".to_string(), None, None)
            ],
            "an old turn's speed and region are unknown, and so are those of a turn 1.8.0 writes"
        );

        // A fresh ledger declares them in the same place.
        let fresh_dir = TempDir::new().unwrap();
        let fresh = pool_in(&fresh_dir).await;
        init_schema(&fresh).await.unwrap();
        assert_eq!(columns(&fresh, "turns").await, cols);
    }

    #[test]
    fn the_rusqlite_open_adds_turns_speed_and_region_too() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("1_8_0.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(TURNS_THROUGH_1_8_0)
            .unwrap();
        let conn = crate::meter::connect_db(&path).unwrap();
        conn.execute_batch(TURN_WRITTEN_BY_1_8_0).unwrap();
        let rows: Vec<(Option<String>, Option<String>)> = conn
            .prepare("SELECT speed, inference_geo FROM turns ORDER BY rowid")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows, [(None, None), (None, None)]);
    }

    // ── write-lock pressure ──────────────────────────────────────────────────

    /// A 1.5.1 ledger (its `read_events`, and `turns` as 1.8.0 left it) that has been through
    /// DDL once, so what a test measures after that is the migrations alone.
    fn old_ledger_after_ddl(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("old.db");
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(READ_EVENTS_1_5_1).unwrap();
        c.execute_batch(TURNS_THROUGH_1_8_0).unwrap();
        c.execute_batch(DDL).unwrap();
        path
    }

    /// A connection that has read the ledger, which is what keeps the log from being checkpointed
    /// and deleted when the connection under test closes.
    fn holding_the_log(db: &std::path::Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
        conn
    }

    /// How many transactions the write-ahead log holds, counted from its frames: a commit is the
    /// frame that records the size of the database after it. The caller keeps a connection open
    /// ([`holding_the_log`]).
    fn commits_in_wal(db: &std::path::Path) -> usize {
        let wal = std::fs::read(db.with_extension("db-wal")).unwrap_or_default();
        let Some(header) = wal.get(..32) else {
            return 0;
        };
        let page = u32::from_be_bytes(header[8..12].try_into().unwrap()) as usize;
        wal[32..]
            .chunks_exact(24 + page)
            .filter(|frame| frame[4..8] != [0; 4])
            .count()
    }

    /// Every ALTER was a write transaction of its own, each a lock to win against the other
    /// openers and a commit to sync: 12 for this ledger, 26 for a 1.1.4 one, 8 of them added by
    /// 1.9.0. The migrations are one transaction now.
    #[test]
    fn the_rusqlite_open_migrates_in_one_commit() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        let held_open = holding_the_log(&path);
        let before = commits_in_wal(&path);
        drop(crate::meter::connect_db(&path).unwrap());
        assert_eq!(commits_in_wal(&path) - before, 1);
        drop(held_open);
    }

    #[tokio::test]
    async fn the_sqlx_open_migrates_in_one_commit() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        let held_open = holding_the_log(&path);
        let before = commits_in_wal(&path);
        let pool = SqlitePoolOptions::new()
            .connect(&format!("sqlite:{}?mode=rwc", path.display()))
            .await
            .unwrap();
        init_schema(&pool).await.unwrap();
        assert_eq!(commits_in_wal(&path) - before, 1);
        drop(held_open);
    }

    /// Opening a current ledger writes nothing, so it must not queue behind a writer: the backfill
    /// was an UPDATE, which asks for the write lock whether or not a row matches. Run on a thread
    /// because, before the fix, the open sat in SQLite's busy wait until the writer let go.
    fn opens_without_waiting_for_a_writer(open: impl FnOnce(&std::path::Path) + Send + 'static) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("current.db");
        drop(crate::meter::connect_db(&path).unwrap());
        let writer = rusqlite::Connection::open(&path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let reader = std::thread::spawn({
            let path = path.clone();
            move || {
                open(&path);
                done.send(()).ok();
            }
        });
        let waited = finished
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_err();
        writer.execute_batch("ROLLBACK").unwrap();
        reader.join().unwrap();
        assert!(
            !waited,
            "the open waited for a writer it had nothing to write beside"
        );
    }

    #[test]
    fn the_rusqlite_open_of_a_current_ledger_does_not_wait_for_a_writer() {
        opens_without_waiting_for_a_writer(|path| {
            crate::meter::connect_db(path).unwrap();
        });
    }

    #[test]
    fn the_sqlx_open_of_a_current_ledger_does_not_wait_for_a_writer() {
        opens_without_waiting_for_a_writer(|path| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    open_pool(&format!("sqlite:{}?mode=rwc", path.display()))
                        .await
                        .unwrap();
                });
        });
    }

    /// The backfill still runs when a row needs it: the read that gates it must agree with it.
    #[test]
    fn the_backfill_still_marks_a_subagent_turn_and_only_that() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("l.db");
        drop(crate::meter::connect_db(&path).unwrap());
        let c = rusqlite::Connection::open(&path).unwrap();
        for (id, file) in [
            ("main", "/h/projects/p/abc.jsonl"),
            ("unix", "/h/projects/p/abc/subagents/agent-1.jsonl"),
            ("win", r"C:\h\projects\p\abc\subagents\agent-2.jsonl"),
        ] {
            c.execute(
                "INSERT INTO turns (message_id, session_id, ts, input_tokens, output_tokens, \
                 cache_read_input_tokens, cache_creation_input_tokens, source_file) \
                 VALUES (?1, 's', 't', 1, 1, 0, 0, ?2)",
                rusqlite::params![id, file],
            )
            .unwrap();
        }
        drop(crate::meter::connect_db(&path).unwrap());
        let marked: Vec<(String, i64)> = c
            .prepare("SELECT message_id, is_subagent FROM turns ORDER BY message_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            marked,
            [("main".into(), 0), ("unix".into(), 1), ("win".into(), 1)]
        );
    }

    // ── the upgrade takes the write lock first ───────────────────────────────

    /// The mechanism, with no Lumen code in it: a deferred transaction that has read, and whose
    /// snapshot another connection then outdates by committing, is refused its first write at
    /// once with SQLITE_BUSY_SNAPSHOT, not after the busy timeout. Waiting could not help it: the
    /// snapshot it holds is the one that is stale.
    #[test]
    fn a_deferred_transaction_that_read_first_is_refused_at_once_after_another_commit() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        let a = rusqlite::Connection::open(&path).unwrap();
        a.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        a.execute_batch("BEGIN").unwrap();
        a.query_row("SELECT count(*) FROM calibration", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
        let b = rusqlite::Connection::open(&path).unwrap();
        b.execute_batch("INSERT INTO calibration VALUES ('b', 1, 1)")
            .unwrap();
        let started = std::time::Instant::now();
        let refused = a
            .execute_batch("INSERT INTO calibration VALUES ('a', 1, 1)")
            .unwrap_err();
        let took = started.elapsed();
        let rusqlite::Error::SqliteFailure(failure, _) = &refused else {
            panic!("{refused:?}");
        };
        assert_eq!(
            failure.extended_code,
            rusqlite::ffi::SQLITE_BUSY_SNAPSHOT,
            "{refused}"
        );
        assert!(
            took < std::time::Duration::from_millis(500),
            "the 5 s busy timeout was consulted: {took:?}"
        );
    }

    /// Another connection that commits a row whenever the lock is free, and counts what it was
    /// refused: `busy_timeout` 0, so it never waits.
    struct Interferer {
        committed: std::rc::Rc<std::cell::Cell<u32>>,
        refused: std::rc::Rc<std::cell::Cell<u32>>,
    }

    /// Make `Interferer` act after every migration statement of the upgrade on this thread.
    fn interfere_after_every_migration(db: &std::path::Path) -> Interferer {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let committed = std::rc::Rc::new(std::cell::Cell::new(0));
        let refused = std::rc::Rc::new(std::cell::Cell::new(0));
        let (c, r) = (committed.clone(), refused.clone());
        super::probe::set(move || {
            let n = c.get() + r.get();
            match conn.execute(
                "INSERT INTO calibration VALUES (?1, 1, 1)",
                [format!("interference-{n}")],
            ) {
                Ok(_) => c.set(c.get() + 1),
                Err(_) => r.set(r.get() + 1),
            }
        });
        Interferer { committed, refused }
    }

    /// A 1.8.0 ledger: every migration up to `turns.speed` finds its work done. A `CREATE ... IF
    /// NOT EXISTS` that finds its table reads the ledger, so by the time `ADD COLUMN speed`
    /// writes, the transaction has a read snapshot that a neighbour can outdate.
    fn ledger_of_1_8_0(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("v1_8_0.db");
        drop(crate::meter::connect_db(&path).unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE turns").unwrap();
        conn.execute_batch(TURNS_THROUGH_1_8_0).unwrap();
        conn.execute_batch(
            "CREATE INDEX idx_turns_session ON turns(session_id); \
             CREATE INDEX idx_turns_ts ON turns(ts)",
        )
        .unwrap();
        path
    }

    fn assert_upgrade_is_not_outdated_by_a_neighbour(interferer: &Interferer, what: &str) {
        super::probe::clear();
        assert!(
            interferer.refused.get() > 0,
            "{what}: the neighbour never got a turn"
        );
        assert_eq!(
            interferer.committed.get(),
            0,
            "{what}: another connection committed in the middle of the upgrade"
        );
    }

    /// On a deferred transaction, every attempt reads before it writes, and the neighbour commits
    /// in between each time: the opener is refused SQLITE_BUSY_SNAPSHOT on every try until its
    /// budget runs out and gives the error back. Nothing is left to chance.
    #[test]
    fn the_rusqlite_open_is_not_outdated_by_a_neighbour_that_commits_between_statements() {
        let dir = TempDir::new().unwrap();
        let path = ledger_of_1_8_0(&dir);
        let neighbour = interfere_after_every_migration(&path);
        let opened = crate::meter::connect_db(&path);
        super::probe::clear();
        opened.expect("the upgrade went through");
        assert_upgrade_is_not_outdated_by_a_neighbour(&neighbour, "rusqlite");
        assert_eq!(columns_of(&path, "turns").last().unwrap(), "inference_geo");
    }

    #[tokio::test]
    async fn the_sqlx_open_is_not_outdated_by_a_neighbour_that_commits_between_statements() {
        let dir = TempDir::new().unwrap();
        let path = ledger_of_1_8_0(&dir);
        let neighbour = interfere_after_every_migration(&path);
        let opened = open_pool(&format!("sqlite:{}?mode=rwc", path.display())).await;
        super::probe::clear();
        opened.expect("the upgrade went through");
        assert_upgrade_is_not_outdated_by_a_neighbour(&neighbour, "sqlx");
        assert_eq!(columns_of(&path, "turns").last().unwrap(), "inference_geo");
    }

    fn columns_of(db: &std::path::Path, table: &str) -> Vec<String> {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn shape_of(db: &std::path::Path) -> Shape {
        let conn = rusqlite::Connection::open(db).unwrap();
        crate::meter::live_shape(&conn).unwrap()
    }

    /// What breaks a current ledger in each way an open has to notice, and what it must show after.
    const BREAKAGES: &[(&str, &str)] = &[
        ("an index gone", "DROP INDEX idx_turns_ts"),
        ("an index of a migration gone", "DROP INDEX idx_faults_kind"),
        ("a column gone", "ALTER TABLE faults DROP COLUMN agent_id"),
        ("a table gone", "DROP TABLE transcript_scans"),
        ("a view gone", "DROP VIEW correction_factor"),
        (
            "a dropped column put back",
            "ALTER TABLE read_events ADD COLUMN is_subagent INTEGER",
        ),
        (
            "a dropped table put back",
            "CREATE TABLE sessions (id TEXT)",
        ),
        (
            "a turn a backfill is for",
            "INSERT INTO turns (message_id, session_id, ts, input_tokens, output_tokens, \
             cache_read_input_tokens, cache_creation_input_tokens, source_file, is_subagent) \
             VALUES ('m', 's', 't', 1, 1, 1, 1, '/p/subagents/a.jsonl', 0)",
        ),
        ("the rollback journal", "PRAGMA journal_mode=DELETE"),
    ];

    /// The read that decides whether an open writes must see every one of these, or the ledger
    /// stays as it is for good.
    #[test]
    fn every_open_repairs_a_current_ledger_in_each_way_the_read_must_notice() {
        let dir = TempDir::new().unwrap();
        let fresh = dir.path().join("fresh.db");
        drop(crate::meter::connect_db(&fresh).unwrap());
        let fresh_shape = shape_of(&fresh);
        for (what, breakage) in BREAKAGES {
            for opener in ["rusqlite", "sqlx"] {
                let path = dir.path().join(format!("{opener}.db"));
                let _ = std::fs::remove_file(&path);
                drop(crate::meter::connect_db(&path).unwrap());
                let conn = rusqlite::Connection::open(&path).unwrap();
                conn.execute_batch(breakage).unwrap();
                drop(conn);
                if opener == "rusqlite" {
                    drop(crate::meter::connect_db(&path).unwrap());
                } else {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(open_pool(&format!("sqlite:{}?mode=rwc", path.display())))
                        .unwrap();
                }
                let at = format!("{opener}, {what}");
                assert_eq!(shape_of(&path), fresh_shape, "{at}");
                let conn = rusqlite::Connection::open(&path).unwrap();
                let mode: String = conn
                    .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(mode, "wal", "{at}");
                let left: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM turns WHERE is_subagent = 0 \
                         AND source_file LIKE '%/subagents/%'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(left, 0, "{at}");
            }
        }
    }

    /// Every statement of MIGRATIONS that removes something is one the read knows to look for.
    #[test]
    fn every_drop_in_the_migrations_is_one_the_read_looks_for() {
        let drops = MIGRATIONS
            .iter()
            .filter(|m| m.starts_with("DROP ") || m.contains(" DROP COLUMN "))
            .count();
        let (tables, columns) = Shape::dropped();
        assert_eq!(tables.len() + columns.len(), drops);
        assert_eq!(tables, ["sessions"]);
        assert_eq!(columns, [("read_events", "is_subagent")]);
    }

    /// A column or table a newer build added is not a ledger to rewrite on every open.
    #[test]
    fn a_ledger_with_more_than_a_fresh_one_is_current() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("newer.db");
        drop(crate::meter::connect_db(&path).unwrap());
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE turns ADD COLUMN from_the_future TEXT; CREATE TABLE future (a TEXT)",
        )
        .unwrap();
        assert!(!crate::meter::live_shape(&conn).unwrap().lacks_something());
    }

    // ── a neighbour that holds the write lock ────────────────────────────────

    /// The mechanism, with no Lumen code in it: a deferred transaction that has read, and then
    /// asks for the write lock while another connection holds it, is refused at once with plain
    /// SQLITE_BUSY (5), not after the busy timeout, and not after waiting for the holder: the
    /// snapshot it holds could not be written from once the holder commits, and waiting could
    /// deadlock the two. 517 is the same thing once the other has already committed.
    #[test]
    fn a_deferred_transaction_that_read_first_is_refused_at_once_while_another_holds_the_lock() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        let a = rusqlite::Connection::open(&path).unwrap();
        a.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        a.execute_batch("BEGIN").unwrap();
        a.query_row("SELECT count(*) FROM calibration", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
        let b = rusqlite::Connection::open(&path).unwrap();
        b.execute_batch("BEGIN IMMEDIATE").unwrap();
        let started = std::time::Instant::now();
        let refused = a
            .execute_batch("INSERT INTO calibration VALUES ('a', 1, 1)")
            .unwrap_err();
        let took = started.elapsed();
        let rusqlite::Error::SqliteFailure(failure, _) = &refused else {
            panic!("{refused:?}");
        };
        assert_eq!(
            failure.extended_code,
            rusqlite::ffi::SQLITE_BUSY,
            "{refused}"
        );
        assert!(
            took < std::time::Duration::from_millis(500),
            "the 5 s busy timeout was consulted: {took:?}"
        );
    }

    /// Another connection that asks for the write lock whenever the upgrade gives it a turn, and
    /// keeps it until its next turn if it is granted: `busy_timeout` 0, so it never waits.
    struct Holder {
        granted: std::rc::Rc<std::cell::Cell<u32>>,
        refused: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl Holder {
        fn hooked(db: &std::path::Path) -> (Holder, impl FnMut() + 'static) {
            let conn = rusqlite::Connection::open(db).unwrap();
            conn.busy_timeout(std::time::Duration::ZERO).unwrap();
            let granted = std::rc::Rc::new(std::cell::Cell::new(0));
            let refused = std::rc::Rc::new(std::cell::Cell::new(0));
            let (g, r) = (granted.clone(), refused.clone());
            let mut holding = false;
            let turn = move || {
                if holding {
                    conn.execute_batch("ROLLBACK").unwrap();
                    holding = false;
                } else if conn.execute_batch("BEGIN IMMEDIATE").is_ok() {
                    g.set(g.get() + 1);
                    holding = true;
                } else {
                    r.set(r.get() + 1);
                }
            };
            (Holder { granted, refused }, turn)
        }

        fn assert_never_granted(&self, what: &str) {
            super::probe::clear();
            assert!(
                self.refused.get() > 0,
                "{what}: the neighbour never got a turn"
            );
            assert_eq!(
                self.granted.get(),
                0,
                "{what}: another connection held the write lock in the middle of the upgrade"
            );
        }
    }

    /// On a deferred transaction the opener has read by the time the neighbour takes the write
    /// lock, and its next write is refused with SQLITE_BUSY (5), whatever the busy timeout: the
    /// error the Windows run reported when a neighbour was still inside its commit. With the lock
    /// taken first, the neighbour is the one refused.
    #[test]
    fn the_rusqlite_open_is_not_refused_by_a_neighbour_that_holds_the_write_lock() {
        let dir = TempDir::new().unwrap();
        let path = ledger_of_1_8_0(&dir);
        let (neighbour, turn) = Holder::hooked(&path);
        super::probe::set(turn);
        let opened = crate::meter::connect_db(&path);
        super::probe::clear();
        opened.expect("the upgrade went through");
        neighbour.assert_never_granted("rusqlite");
        assert_eq!(columns_of(&path, "turns").last().unwrap(), "inference_geo");
    }

    #[tokio::test]
    async fn the_sqlx_open_is_not_refused_by_a_neighbour_that_holds_the_write_lock() {
        let dir = TempDir::new().unwrap();
        let path = ledger_of_1_8_0(&dir);
        let (neighbour, turn) = Holder::hooked(&path);
        super::probe::set(turn);
        let opened = open_pool(&format!("sqlite:{}?mode=rwc", path.display())).await;
        super::probe::clear();
        opened.expect("the upgrade went through");
        neighbour.assert_never_granted("sqlx");
        assert_eq!(columns_of(&path, "turns").last().unwrap(), "inference_geo");
    }

    // ── the row rewrites ─────────────────────────────────────────────────────

    /// A turn the backfill is for, put in through plain SQL.
    const SUBAGENT_TURN: &str = "INSERT INTO turns (message_id, session_id, ts, input_tokens, \
        output_tokens, cache_read_input_tokens, cache_creation_input_tokens, source_file) \
        VALUES ('sub', 's', 't', 1, 1, 0, 0, '/h/projects/p/abc/subagents/agent-1.jsonl')";

    fn marked_as_subagent(db: &std::path::Path) -> i64 {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row(
                "SELECT is_subagent FROM turns WHERE message_id = 'sub'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// A current ledger with one turn to rewrite, and a neighbour that asks for the write lock
    /// the moment the backfill has found the row and before it has rewritten it. Taken first, the
    /// lock is the backfill's and the neighbour is refused. Taken at the rewrite (a deferred
    /// BEGIN), the backfill holds a snapshot the neighbour then outdates, is refused with
    /// SQLITE_BUSY_SNAPSHOT on every try, and the open swallows that: the row is left as it was.
    fn a_current_ledger_with_a_turn_to_rewrite(dir: &TempDir) -> std::path::PathBuf {
        let path = dir.path().join("current.db");
        drop(crate::meter::connect_db(&path).unwrap());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(SUBAGENT_TURN)
            .unwrap();
        path
    }

    fn commit_a_row_whenever_the_backfill_pauses(db: &std::path::Path) -> Interferer {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let committed = std::rc::Rc::new(std::cell::Cell::new(0));
        let refused = std::rc::Rc::new(std::cell::Cell::new(0));
        let (c, r) = (committed.clone(), refused.clone());
        super::probe::set_before_rewrite(move || {
            let n = c.get() + r.get();
            match conn.execute(
                "INSERT INTO calibration VALUES (?1, 1, 1)",
                [format!("interference-{n}")],
            ) {
                Ok(_) => c.set(c.get() + 1),
                Err(_) => r.set(r.get() + 1),
            }
        });
        Interferer { committed, refused }
    }

    #[test]
    fn the_rusqlite_backfill_alone_takes_the_write_lock_before_it_rewrites() {
        let dir = TempDir::new().unwrap();
        let path = a_current_ledger_with_a_turn_to_rewrite(&dir);
        let neighbour = commit_a_row_whenever_the_backfill_pauses(&path);
        let opened = crate::meter::connect_db(&path);
        super::probe::clear();
        drop(opened.unwrap());
        assert!(neighbour.refused.get() > 0, "the backfill never ran");
        assert_eq!(neighbour.committed.get(), 0, "a neighbour committed first");
        assert_eq!(marked_as_subagent(&path), 1);
    }

    #[tokio::test]
    async fn the_sqlx_backfill_alone_takes_the_write_lock_before_it_rewrites() {
        let dir = TempDir::new().unwrap();
        let path = a_current_ledger_with_a_turn_to_rewrite(&dir);
        let neighbour = commit_a_row_whenever_the_backfill_pauses(&path);
        let opened = open_pool(&format!("sqlite:{}?mode=rwc", path.display())).await;
        super::probe::clear();
        drop(opened.unwrap());
        assert!(neighbour.refused.get() > 0, "the backfill never ran");
        assert_eq!(neighbour.committed.get(), 0, "a neighbour committed first");
        assert_eq!(marked_as_subagent(&path), 1);
    }

    /// An upgrade with a row to rewrite is still one commit: the rewrite is inside the
    /// transaction that holds the lock, not a second one after it.
    #[test]
    fn the_rusqlite_upgrade_commits_its_backfill_with_its_migrations() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(SUBAGENT_TURN)
            .unwrap();
        let held_open = holding_the_log(&path);
        let before = commits_in_wal(&path);
        drop(crate::meter::connect_db(&path).unwrap());
        assert_eq!(commits_in_wal(&path) - before, 1);
        assert_eq!(marked_as_subagent(&path), 1);
        drop(held_open);
    }

    #[tokio::test]
    async fn the_sqlx_upgrade_commits_its_backfill_with_its_migrations() {
        let dir = TempDir::new().unwrap();
        let path = old_ledger_after_ddl(&dir);
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(SUBAGENT_TURN)
            .unwrap();
        let held_open = holding_the_log(&path);
        let before = commits_in_wal(&path);
        drop(
            open_pool(&format!("sqlite:{}?mode=rwc", path.display()))
                .await
                .unwrap(),
        );
        assert_eq!(commits_in_wal(&path) - before, 1);
        assert_eq!(marked_as_subagent(&path), 1);
        drop(held_open);
    }

    // ── what MIGRATIONS may hold ─────────────────────────────────────────────

    /// Why a statement may not be in [`MIGRATIONS`], or `None` when it may. A current ledger is
    /// recognised by reading its shape ([`Shape::lacks_something`]) and is not upgraded at all,
    /// so a migration whose effect that read cannot see would never run on it.
    fn what_is_wrong_with_a_migration(
        statement: &str,
        indexes: &mut std::collections::BTreeMap<String, String>,
    ) -> Option<String> {
        // Both openers run an entry as a batch, so a second statement after a `;` would run
        // unseen by the checks below: one statement per entry.
        if statement.contains(';') {
            return Some(format!(
                "{statement:?} holds more than one statement: both openers run an entry as a \
                 batch, so what follows the `;` is never checked here. Give each statement its \
                 own entry. A ledger that already has the shape of a fresh one is not upgraded, \
                 so a row rewrite after the `;` would be skipped on it forever."
            ));
        }
        let words: Vec<&str> = statement.split_whitespace().collect();
        let kind = match words.as_slice() {
            ["ALTER", "TABLE", _, "ADD", "COLUMN", ..] => return None,
            ["ALTER", "TABLE", _, "DROP", "COLUMN", _] => return None,
            ["CREATE", "TABLE", "IF", "NOT", "EXISTS", ..] => return None,
            ["DROP", "TABLE", "IF", "EXISTS", _] => return None,
            ["CREATE", "INDEX", "IF", "NOT", "EXISTS", name, "ON", ..]
            | [
                "CREATE",
                "UNIQUE",
                "INDEX",
                "IF",
                "NOT",
                "EXISTS",
                name,
                "ON",
                ..,
            ] => {
                let definition = words[words.iter().position(|w| w == name).unwrap()..].join(" ");
                // Repeating DDL's own statement, as `faults` is repeated, changes nothing.
                match indexes.insert((*name).to_string(), definition.clone()) {
                    Some(earlier) if earlier != definition => {}
                    _ => return None,
                }
                "an index redefined under a name that is already taken: the shape read sees \
                 the name and takes the index to be there, so the new definition is never applied"
            }
            [verb, ..] if ["UPDATE", "INSERT", "DELETE", "REPLACE"].contains(verb) => {
                "a row rewrite: the shape read cannot see a row, so it would never run on a \
                 ledger of the current shape. Row rewrites go in BACKFILLS, each with its own \
                 `pending` read"
            }
            _ => {
                "a statement of a kind the shape read cannot see. Only ADD COLUMN, DROP COLUMN \
                 (ALTER TABLE t DROP COLUMN c), CREATE TABLE IF NOT EXISTS, CREATE [UNIQUE] \
                 INDEX IF NOT EXISTS name ON ... (a new name) and DROP TABLE IF EXISTS t (the \
                 spellings `Shape::dropped` parses) show in the ledger's shape"
            }
        };
        Some(format!(
            "{statement:?} is {kind}. A ledger that already has the shape of a fresh one is not \
             upgraded, so this statement would be skipped on it forever."
        ))
    }

    /// Each index DDL creates, by name, with its definition from the name on.
    fn indexes_in_ddl() -> std::collections::BTreeMap<String, String> {
        DDL.split(';')
            .filter_map(|statement| {
                let words: Vec<&str> = statement.split_whitespace().collect();
                match words.as_slice() {
                    ["CREATE", "INDEX", "IF", "NOT", "EXISTS", name, rest @ ..]
                    | [
                        "CREATE",
                        "UNIQUE",
                        "INDEX",
                        "IF",
                        "NOT",
                        "EXISTS",
                        name,
                        rest @ ..,
                    ] => Some(((*name).to_string(), format!("{name} {}", rest.join(" ")))),
                    _ => None,
                }
            })
            .collect()
    }

    #[test]
    fn every_migration_is_a_statement_the_shape_read_can_see() {
        let mut indexes = indexes_in_ddl();
        let problems: Vec<String> = MIGRATIONS
            .iter()
            .filter_map(|m| what_is_wrong_with_a_migration(m, &mut indexes))
            .collect();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Row rewrites live in BACKFILLS, which read before they write: the one read of `pending`
    /// and the one rewrite, and nothing the shape read has to see.
    #[test]
    fn row_rewrites_live_only_in_the_backfills_and_are_gated_by_their_own_read() {
        assert!(!BACKFILLS.is_empty());
        for backfill in BACKFILLS {
            assert!(
                backfill.pending.trim_start().starts_with("SELECT "),
                "a backfill's pending is a read: {}",
                backfill.pending
            );
            assert!(
                backfill.rewrite.trim_start().starts_with("UPDATE "),
                "a backfill's rewrite is an UPDATE: {}",
                backfill.rewrite
            );
        }
    }

    #[test]
    fn the_migration_guard_refuses_what_the_shape_read_cannot_see() {
        let ok = [
            "ALTER TABLE t ADD COLUMN c TEXT",
            "ALTER TABLE t DROP COLUMN c",
            "CREATE TABLE IF NOT EXISTS t (a TEXT)",
            "CREATE INDEX IF NOT EXISTS idx_new ON t(a)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_new_unique ON t(a)",
            "DROP TABLE IF EXISTS t",
        ];
        let bad = [
            "UPDATE turns SET speed = 'standard' WHERE speed IS NULL",
            "INSERT INTO calibration VALUES ('a', 1, 1)",
            "DELETE FROM faults",
            "CREATE INDEX IF NOT EXISTS idx_turns_ts ON turns(ts, session_id)",
            "DROP INDEX IF EXISTS idx_turns_ts",
            "DROP TABLE t",
            "CREATE TABLE t (a TEXT)",
            "ALTER TABLE t RENAME COLUMN a TO b",
            "ALTER TABLE t DROP COLUMN c, DROP COLUMN d",
            // A row write hidden behind a `;` in an entry the classifier would otherwise pass.
            "ALTER TABLE turns ADD COLUMN foo TEXT; UPDATE turns SET foo = 'x'",
            "CREATE TABLE IF NOT EXISTS foo (a TEXT); INSERT INTO foo VALUES ('x')",
            "CREATE INDEX IF NOT EXISTS idx_new2 ON t(a); DELETE FROM faults",
        ];
        let mut indexes = indexes_in_ddl();
        for statement in ok {
            assert_eq!(
                what_is_wrong_with_a_migration(statement, &mut indexes),
                None
            );
        }
        for statement in bad {
            let why = what_is_wrong_with_a_migration(statement, &mut indexes)
                .unwrap_or_else(|| panic!("allowed: {statement}"));
            assert!(why.contains("skipped on it forever"), "{why}");
        }
    }
}
