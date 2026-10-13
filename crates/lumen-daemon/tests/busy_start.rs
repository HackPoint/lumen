//! A daemon started while another connection holds the ledger waits for it instead of exiting.
//!
//! `main` opens the ledger before it serves anything, so a "database is locked" there ended the
//! process, and the live stream the app shows with it. SQLite refuses at once, whatever the busy
//! timeout says, when the connection that switches a rollback-journal ledger to WAL already holds
//! a read lock (see `lumen_core::busy`); only the real binary shows that the open `main` runs is
//! the one that waits.

use std::fs::File;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lumen_core::schema;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection, SqliteJournalMode};

/// The daemon prints this once, with the ledger it is about to open.
const MARKER: &str = "lumen-daemon using db: ";

const STALE_TURN: &str = "INSERT INTO turns (message_id, session_id, ts, input_tokens, \
     output_tokens, cache_read_input_tokens, cache_creation_input_tokens, source_file, \
     is_subagent) VALUES ('m1', 's1', '2026-01-01T00:00:00Z', 1, 1, 0, 0, \
     '/h/.claude/projects/-p/abc/subagents/agent-1.jsonl', 0)";

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reserve an unused port by binding and releasing it; the daemon has to bind it itself.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
    listener.local_addr().expect("local addr").port()
}

fn log_says(log: &Path, marker: &str) -> bool {
    std::fs::read_to_string(log).is_ok_and(|text| text.contains(marker))
}

#[tokio::test]
async fn a_daemon_started_while_a_writer_holds_the_ledger_serves_once_it_lets_go() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ledger.db");
    // A forgotten LUMEN_DB resolves to the developer's own ledger, so this is asserted.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the ledger must live in a temp dir: {db:?}"
    );
    let url = format!("sqlite:{}?mode=rwc", db.display());

    // A ledger with a turn the backfill has not marked yet, back in the rollback-journal mode
    // every ledger starts in, with a writer inside it before the daemon starts.
    let pool = schema::open_pool(&url).await.unwrap();
    sqlx::query(STALE_TURN).execute(&pool).await.unwrap();
    pool.close().await;
    let mut writer = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&db)
            .journal_mode(SqliteJournalMode::Delete),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut writer)
        .await
        .unwrap();

    let projects = dir.path().join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let log = dir.path().join("daemon.log");
    let addr = format!("127.0.0.1:{}", free_port());
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_lumen-daemon"))
            .env("HOME", dir.path())
            .env("LUMEN_PROJECTS_DIR", &projects)
            .env("LUMEN_DB", &db)
            .env("LUMEN_FAULT_SPOOL", dir.path().join("faults.jsonl"))
            .env("LUMEN_WS_ADDR", &addr)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log).unwrap())
            .spawn()
            .expect("spawn lumen-daemon"),
    );

    // It names the ledger just before it opens it; give it time to be refused, and to try again.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log_says(&log, MARKER) {
        assert!(
            Instant::now() < deadline,
            "the daemon never named its ledger:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    sqlx::query("COMMIT").execute(&mut writer).await.unwrap();

    // It binds only after the ledger is open and current, so an accepting port means it is.
    let deadline = Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        if let Some(status) = daemon.0.try_wait().expect("poll the daemon") {
            panic!(
                "the daemon exited with {status} instead of waiting for the writer:\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never opened ws://{addr}:\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(daemon);

    let mut check = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&db))
        .await
        .unwrap();
    let marked: i64 = sqlx::query_scalar("SELECT is_subagent FROM turns WHERE message_id = 'm1'")
        .fetch_one(&mut check)
        .await
        .unwrap();
    assert_eq!(marked, 1, "the daemon ran the migrations before it served");
}
