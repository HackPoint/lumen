use lumen_core::meter::connect_db;
use lumen_core::record::Record;
use rusqlite::params;
use tempfile::TempDir;

fn tmp_db() -> (TempDir, std::path::PathBuf, rusqlite::Connection) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let conn = connect_db(&path).expect("connect_db");
    (dir, path, conn)
}

// ── Schema ────────────────────────────────────────────────────────────────────

#[test]
fn connect_db_creates_all_tables() {
    let (_dir, _path, conn) = tmp_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type='table' AND name IN ('turns','sessions','calibration','read_events')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4, "all four tables must exist after connect_db");
}

#[test]
fn connect_db_is_idempotent() {
    let (_dir, path, conn) = tmp_db();
    drop(conn);
    // Second open must not fail (DDL uses IF NOT EXISTS everywhere)
    let conn2 = connect_db(&path).expect("second connect_db");
    let _: i64 = conn2
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .unwrap();
}

// ── Dedup (INSERT OR IGNORE) ───────────────────────────────────────────────────

const INSERT_TURN: &str = "INSERT OR IGNORE INTO turns \
     (message_id, session_id, ts, model, \
      input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, \
      source_file) \
     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)";

#[test]
fn turns_dedup_by_message_id() {
    let (_dir, _path, conn) = tmp_db();

    conn.execute(
        INSERT_TURN,
        params![
            "msg-001",
            "sess-1",
            "2025-01-01T00:00:00Z",
            "claude-sonnet-4-6",
            100i64,
            20i64,
            0i64,
            0i64,
            "a.jsonl"
        ],
    )
    .unwrap();

    // Same message_id again — must be silently ignored
    conn.execute(
        INSERT_TURN,
        params![
            "msg-001",
            "sess-1",
            "2025-01-01T00:00:01Z",
            "claude-sonnet-4-6",
            100i64,
            20i64,
            0i64,
            0i64,
            "a.jsonl"
        ],
    )
    .unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM turns WHERE message_id='msg-001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "duplicate message_id must produce exactly one row"
    );
}

#[test]
fn turns_two_distinct_ids_both_inserted() {
    let (_dir, _path, conn) = tmp_db();

    for (id, ts) in [
        ("msg-A", "2025-01-01T00:00:00Z"),
        ("msg-B", "2025-01-01T00:00:01Z"),
    ] {
        conn.execute(
            INSERT_TURN,
            params![
                id,
                "sess-1",
                ts,
                "claude-sonnet-4-6",
                10i64,
                5i64,
                0i64,
                0i64,
                "a.jsonl"
            ],
        )
        .unwrap();
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "two distinct message_ids must produce two rows");
}

// ── record::Record parsing ─────────────────────────────────────────────────────

#[test]
fn fixture_file_parses_and_counts_billable() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonl");
    let content = std::fs::read_to_string(&fixture).expect("read fixture");

    let mut billable = 0usize;
    let mut total = 0usize;
    let mut skipped_bad = 0usize;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        match serde_json::from_str::<Record>(line) {
            Ok(r) if r.is_billable() => billable += 1,
            Ok(_) => {}
            Err(_) => skipped_bad += 1,
        }
    }

    // fixture has 3 billable lines (msg-001 appears twice + msg-003),
    // 1 user line, and 1 bad JSON line
    assert_eq!(skipped_bad, 1, "exactly one bad JSON line in fixture");
    assert_eq!(
        billable, 3,
        "three billable lines in fixture (one duplicate)"
    );
    assert_eq!(total, 5);
}

#[test]
fn fixture_dedup_yields_unique_ids() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonl");
    let content = std::fs::read_to_string(&fixture).expect("read fixture");

    let (_dir, _path, conn) = tmp_db();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        if !rec.is_billable() {
            continue;
        }
        let u = rec.message.usage.as_ref().unwrap();
        conn.execute(
            INSERT_TURN,
            params![
                rec.message.id,
                rec.session_id,
                rec.timestamp,
                rec.message.model,
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens,
                "fixture"
            ],
        )
        .unwrap();
    }

    // 3 billable inserts but msg-fixture-001 appears twice → 2 unique rows
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 2,
        "dedup should leave exactly 2 unique message_ids from fixture"
    );
}

// ── read_events ───────────────────────────────────────────────────────────────

#[test]
fn read_events_round_trip() {
    let (_dir, _path, conn) = tmp_db();

    conn.execute(
        "INSERT INTO read_events \
         (ts, tool, path, lines, tokens_returned, full_tokens, saved_tokens, routed_via, channel) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            "2025-01-01T00:00:00Z",
            "smart_read",
            "/tmp/foo.rs",
            100i64,
            200i64,
            2000i64,
            1800i64,
            "smart_read",
            "cli"
        ],
    )
    .unwrap();

    let (tool, saved): (String, i64) = conn
        .query_row(
            "SELECT tool, saved_tokens FROM read_events WHERE path='/tmp/foo.rs'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(tool, "smart_read");
    assert_eq!(saved, 1800);
}
