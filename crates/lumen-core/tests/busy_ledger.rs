//! A ledger that another connection holds for a moment is waited for, not failed on.
//!
//! The case SQLite will not wait for by itself (see `lumen_core::busy`): the ledger is still in
//! the rollback-journal mode, a writer holds the write lock, and the opener, which already holds
//! a read lock, asks for the write lock to switch to WAL. It is refused at once, whatever the
//! busy timeout says.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lumen_core::{busy, meter, schema};
use rusqlite::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

const STALE_TURN: &str = "INSERT INTO turns (message_id, session_id, ts, input_tokens, \
     output_tokens, cache_read_input_tokens, cache_creation_input_tokens, source_file, \
     is_subagent) VALUES ('m1', 's1', '2026-01-01T00:00:00Z', 1, 1, 0, 0, \
     '/h/.claude/projects/-p/abc/subagents/agent-1.jsonl', 0)";

/// Another connection holding a ledger's write lock, taken before [`Writer::hold`] returns.
struct Writer {
    release: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

impl Writer {
    /// Holds the lock until [`Writer::release`] or `at_most`, whichever comes first.
    fn hold(path: &Path, at_most: Duration) -> Writer {
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let path = path.to_owned();
        let thread = thread::spawn(move || {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(at_most);
            conn.execute_batch("COMMIT").unwrap();
        });
        locked_rx.recv().unwrap();
        Writer { release, thread }
    }

    fn release(self) {
        let _ = self.release.send(());
        self.thread.join().unwrap();
    }
}

fn journal_mode(conn: &Connection) -> String {
    conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap()
}

fn url(path: &Path) -> String {
    format!("sqlite:{}?mode=rwc", path.display())
}

/// A ledger with the current schema, left in the rollback-journal mode every SQLite file starts
/// in, holding one turn from a subagent that the backfill migration has not marked yet.
fn old_ledger(dir: &Path) -> PathBuf {
    let path = dir.join("lumen.db");
    drop(meter::connect_db(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let _: String = conn
        .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
        .unwrap();
    conn.execute_batch(STALE_TURN).unwrap();
    assert_eq!(journal_mode(&conn), "delete");
    path
}

/// A ledger already in WAL (an install that has run before), with the same stale turn.
fn wal_ledger(dir: &Path) -> PathBuf {
    let path = dir.join("lumen.db");
    let conn = meter::connect_db(&path).unwrap();
    conn.execute_batch(STALE_TURN).unwrap();
    assert_eq!(journal_mode(&conn), "wal");
    path
}

fn backfilled(conn: &Connection) -> bool {
    let marked: i64 = conn
        .query_row(
            "SELECT is_subagent FROM turns WHERE message_id = 'm1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    marked == 1
}

async fn backfilled_in(pool: &sqlx::SqlitePool) -> bool {
    let marked: i64 = sqlx::query_scalar("SELECT is_subagent FROM turns WHERE message_id = 'm1'")
        .fetch_one(pool)
        .await
        .unwrap();
    marked == 1
}

const AGENT_ID: &str =
    "SELECT COUNT(*) FROM pragma_table_info('read_events') WHERE name = 'agent_id'";

/// A ledger in WAL one column short, so the migration that adds the column has to run.
fn ledger_missing_a_column(dir: &Path) -> PathBuf {
    let path = wal_ledger(dir);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE read_events DROP COLUMN agent_id")
        .unwrap();
    assert!(!has_agent_id(&conn));
    path
}

fn has_agent_id(conn: &Connection) -> bool {
    conn.query_row(AGENT_ID, [], |r| r.get::<_, i64>(0)).unwrap() == 1
}

async fn has_agent_id_in(pool: &sqlx::SqlitePool) -> bool {
    sqlx::query_scalar::<_, i64>(AGENT_ID)
        .fetch_one(pool)
        .await
        .unwrap()
        == 1
}

#[test]
fn the_rusqlite_open_waits_for_a_writer_instead_of_failing_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = old_ledger(dir.path());
    let writer = Writer::hold(&path, Duration::from_millis(300));

    let conn = meter::connect_db(&path)
        .expect("a writer that lets go within a moment must not fail the open");
    writer.release();

    assert_eq!(journal_mode(&conn), "wal");
    assert!(backfilled(&conn), "the open ran the migrations");
}

#[test]
fn the_rusqlite_open_gives_up_busy_once_the_budget_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let path = old_ledger(dir.path());
    let writer = Writer::hold(&path, Duration::from_secs(60));

    let started = Instant::now();
    let err = meter::connect_db(&path).expect_err("a writer that never lets go fails the open");
    let took = started.elapsed();
    writer.release();

    assert!(busy::is_busy_rusqlite(&err), "{err}");
    assert!(
        took >= busy::BUSY_BUDGET,
        "gave up after {took:?}, which is early"
    );
    assert!(
        took < busy::BUSY_BUDGET + Duration::from_secs(3),
        "kept trying for {took:?}"
    );

    let conn = meter::connect_db(&path).expect("nothing is left half-done");
    assert_eq!(journal_mode(&conn), "wal");
    assert!(backfilled(&conn));
}

#[test]
fn the_rusqlite_open_does_not_count_a_migration_the_ledger_was_busy_for_as_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = ledger_missing_a_column(dir.path());
    let writer = Writer::hold(&path, Duration::from_secs(60));

    let err = meter::connect_db(&path)
        .expect_err("a migration that could not run is not one that already had");
    writer.release();

    assert!(busy::is_busy_rusqlite(&err), "{err}");

    let conn = meter::connect_db(&path).expect("the next open finishes the job");
    assert!(has_agent_id(&conn));
}

#[test]
fn the_rusqlite_open_goes_on_without_a_backfill_the_ledger_stays_busy_for() {
    let dir = tempfile::tempdir().unwrap();
    let path = wal_ledger(dir.path());
    let writer = Writer::hold(&path, Duration::from_secs(60));

    let conn = meter::connect_db(&path)
        .expect("the schema is whole, and the next open does the backfill");
    assert!(
        !backfilled(&conn),
        "the writer held the ledger throughout, so the backfill cannot have run"
    );
    writer.release();
    drop(conn);

    let conn = meter::connect_db(&path).expect("the next open");
    assert!(backfilled(&conn));
}

#[tokio::test]
async fn the_pool_open_waits_for_a_writer_instead_of_failing_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = old_ledger(dir.path());
    let writer = Writer::hold(&path, Duration::from_millis(300));

    let pool = schema::open_pool(&url(&path))
        .await
        .expect("a writer that lets go within a moment must not fail the open");
    writer.release();

    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert!(backfilled_in(&pool).await, "the open ran the migrations");
}

#[tokio::test]
async fn the_pool_open_gives_up_busy_once_the_budget_is_spent() {
    let dir = tempfile::tempdir().unwrap();
    let path = old_ledger(dir.path());
    let writer = Writer::hold(&path, Duration::from_secs(60));

    let started = Instant::now();
    let err = schema::open_pool(&url(&path))
        .await
        .expect_err("a writer that never lets go fails the open");
    let took = started.elapsed();
    writer.release();

    assert!(busy::is_busy(&err), "{err}");
    assert!(
        took >= busy::BUSY_BUDGET,
        "gave up after {took:?}, which is early"
    );
    assert!(
        took < busy::BUSY_BUDGET + Duration::from_secs(3),
        "kept trying for {took:?}"
    );

    let pool = schema::open_pool(&url(&path))
        .await
        .expect("nothing is left half-done");
    assert!(backfilled_in(&pool).await);
}

/// A pool that stops waiting for the write lock after 100 ms, so that a migration which needs
/// the lock is refused while a writer holds it, as one would be after the default five seconds.
async fn impatient_pool(path: &Path) -> sqlx::SqlitePool {
    SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .busy_timeout(Duration::from_millis(100)),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_column_the_ledger_was_busy_to_add_is_added_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = ledger_missing_a_column(dir.path());
    let pool = impatient_pool(&path).await;
    let writer = Writer::hold(&path, Duration::from_millis(400));

    schema::init_schema(&pool).await.expect("init_schema");
    writer.release();

    assert!(
        has_agent_id_in(&pool).await,
        "the migration was refused while the writer held the ledger and never ran again"
    );
}

#[tokio::test]
async fn a_ledger_that_stays_busy_is_an_error_not_an_already_applied_migration() {
    let dir = tempfile::tempdir().unwrap();
    let path = ledger_missing_a_column(dir.path());
    let pool = impatient_pool(&path).await;
    let writer = Writer::hold(&path, Duration::from_secs(60));

    let started = Instant::now();
    let err = schema::init_schema(&pool)
        .await
        .expect_err("a migration that could not run is not one that already had");
    let took = started.elapsed();
    writer.release();

    assert!(busy::is_busy(&err), "{err}");
    assert!(
        took >= busy::BUSY_BUDGET,
        "gave up after {took:?}, which is early"
    );
    assert!(
        took < busy::BUSY_BUDGET + Duration::from_secs(3),
        "kept trying for {took:?}"
    );

    schema::init_schema(&pool)
        .await
        .expect("the next open finishes the job");
    assert!(has_agent_id_in(&pool).await);
}

#[tokio::test]
async fn a_backfill_the_ledger_was_busy_for_is_run_again_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = wal_ledger(dir.path());
    let pool = impatient_pool(&path).await;
    let writer = Writer::hold(&path, Duration::from_millis(400));

    schema::init_schema(&pool).await.expect("init_schema");
    writer.release();

    assert!(
        backfilled_in(&pool).await,
        "the backfill was refused while the writer held the ledger and never ran again"
    );
}

#[tokio::test]
async fn a_backfill_the_ledger_stays_busy_for_does_not_fail_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = wal_ledger(dir.path());
    let pool = impatient_pool(&path).await;
    let writer = Writer::hold(&path, Duration::from_secs(60));

    schema::init_schema(&pool)
        .await
        .expect("the schema is whole, and the next open does the backfill");
    assert!(
        !backfilled_in(&pool).await,
        "the writer held the ledger throughout, so the backfill cannot have run"
    );
    writer.release();

    schema::init_schema(&pool).await.expect("the next open");
    assert!(backfilled_in(&pool).await);
}
