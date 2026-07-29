// End-to-end test of the product's data path, with no GUI involved.
//
// Spawns the REAL lumen-daemon binary against a temp HOME and a temp DB, writes
// Claude Code transcript lines to a .jsonl file exactly as Claude Code does, then
// asserts that:
//   1. a WebSocket client receives the snapshot and the live turn frames,
//   2. the rows land durably in SQLite,
//   3. the lumen-stats aggregates — the numbers the GUI actually shows a user —
//      come out right.
//
// This is the chain a real session travels. Anything that breaks between the
// transcript on disk and the figure on screen fails here.

use futures_util::StreamExt;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;

/// A running daemon, killed on drop so a failing assertion cannot leak it.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One assistant transcript line, in the shape Claude Code writes.
fn transcript_line(id: &str, session: &str, ts: &str, tokens: (i64, i64, i64, i64)) -> String {
    let (input, output, cache_read, cache_write) = tokens;
    format!(
        r#"{{"sessionId":"{session}","timestamp":"{ts}","message":{{"id":"{id}","model":"claude-sonnet-4-6","role":"assistant","content":[{{"type":"text","text":"hello there"}}],"usage":{{"input_tokens":{input},"output_tokens":{output},"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":{cache_write}}}}}}}"#
    )
}

/// Poll `check` until it returns true or the deadline passes.
async fn wait_until<F>(what: &str, mut check: F)
where
    F: FnMut() -> bool,
{
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

struct Fixture {
    _home: TempDir,
    _dbdir: TempDir,
    db: std::path::PathBuf,
    projects: std::path::PathBuf,
    ws_url: String,
    _daemon: Daemon,
}

/// Reserve an unused port by binding and immediately releasing it.
///
/// The daemon must own the listener itself, so the test cannot simply hand it a
/// bound socket. This is a small race in theory; in practice the OS does not
/// hand out the same ephemeral port again within the microseconds involved.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
    l.local_addr().unwrap().port()
}

/// Start the real daemon with an isolated HOME and DB. The daemon watches
/// $HOME/.claude/projects, so pointing HOME at a tempdir keeps it away from the
/// developer's real transcripts.
async fn start_daemon() -> Fixture {
    let home = TempDir::new().expect("home tempdir");
    let dbdir = TempDir::new().expect("db tempdir");
    let db = dbdir.path().join("e2e.db");
    // See the note in the MCP harness: a forgotten LUMEN_DB now resolves to the
    // user's real metering database, so the redirection is asserted, not assumed.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the metering DB must live in a temp dir, got {db:?}"
    );
    let projects = home.path().join(".claude/projects/test-project");
    std::fs::create_dir_all(&projects).expect("create projects dir");

    // A private port. Using the default 9999 would silently connect to a daemon
    // the developer already has running — which is exactly what happened the
    // first time these tests were written, and the assertions then ran against
    // that daemon's real sessions.
    let addr = format!("127.0.0.1:{}", free_port());

    let child = Command::new(env!("CARGO_BIN_EXE_lumen-daemon"))
        .env("HOME", home.path())
        // HOME alone does not isolate the daemon on Windows: dirs::home_dir()
        // consults HOME only on Unix and resolves %USERPROFILE% on Windows, so
        // these tests watched the real user profile there and every one of them
        // timed out. The explicit override is platform-independent.
        .env("LUMEN_PROJECTS_DIR", home.path().join(".claude/projects"))
        .env("LUMEN_DB", &db)
        .env("LUMEN_WS_ADDR", &addr)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lumen-daemon");

    // The daemon binds during startup; wait for it to accept.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "daemon never opened ws://{addr}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Fixture {
        _home: home,
        _dbdir: dbdir,
        db,
        projects,
        ws_url: format!("ws://{addr}"),
        _daemon: Daemon(child),
    }
}

/// Append lines to a transcript file, flushing so the daemon can see them.
fn append(path: &std::path::Path, lines: &[String]) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open transcript");
    for line in lines {
        writeln!(f, "{line}").expect("write line");
    }
    f.flush().expect("flush");
}

fn turn_count(db: &std::path::Path) -> i64 {
    let conn = match lumen_core::meter::connect_db(db) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    conn.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))
        .unwrap_or(0)
}

// Each test gets its own HOME, its own DB and its own WS port, so they are safe
// to run in parallel and cannot see a daemon running outside the test suite.

#[tokio::test]
async fn a_transcript_line_becomes_a_durable_row_and_a_live_frame() {
    let fx = start_daemon().await;

    // Connect BEFORE writing, so the live broadcast is observable.
    let (mut ws, _) = tokio_tungstenite::connect_async(&fx.ws_url)
        .await
        .expect("connect");

    // First frame is always the snapshot — empty on a fresh DB.
    let first = ws.next().await.expect("frame").expect("ok");
    let snapshot: serde_json::Value =
        serde_json::from_str(&first.into_text().unwrap()).expect("json");
    assert_eq!(snapshot["type"], "snapshot");

    let path = fx.projects.join("session.jsonl");
    append(
        &path,
        &[transcript_line(
            "msg-1",
            "sess-a",
            "2026-01-01T10:00:00Z",
            (100, 200, 50_000, 300),
        )],
    );

    // The daemon's notify watcher should pick this up; the 3s poll is the
    // backstop, so allow generously.
    let event = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let msg = ws.next().await.expect("frame").expect("ok");
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "event" {
                    return v;
                }
            }
        }
    })
    .await
    .expect("no event frame arrived");

    assert_eq!(event["turn"]["message_id"], "msg-1");
    assert_eq!(event["turn"]["session_id"], "sess-a");
    assert_eq!(event["turn"]["input_tokens"], 100);
    assert_eq!(event["turn"]["output_tokens"], 200);
    assert_eq!(event["turn"]["cache_read_input_tokens"], 50_000);
    assert_eq!(event["turn"]["model"], "claude-sonnet-4-6");

    // And it is durable, not merely broadcast.
    let db = fx.db.clone();
    wait_until("the turn to be committed", || turn_count(&db) == 1).await;
}

#[tokio::test]
async fn the_aggregates_a_user_sees_match_what_was_ingested() {
    let fx = start_daemon().await;
    let path = fx.projects.join("session.jsonl");

    // Two turns in one session, timestamped now so the rolling windows include them.
    let now = "2026-01-01T10:00:00Z";
    append(
        &path,
        &[
            transcript_line("agg-1", "sess-x", now, (10, 100, 1_000, 5)),
            transcript_line("agg-2", "sess-x", now, (20, 200, 9_000, 7)),
        ],
    );

    let db = fx.db.clone();
    wait_until("both turns to be ingested", || turn_count(&db) == 2).await;

    // Now read the very functions the GUI calls.
    let pool = lumen_stats::connect(&format!("sqlite:{}?mode=rwc", fx.db.display()))
        .await
        .expect("pool");

    let usage = lumen_stats::get_usage(&pool).await.expect("usage");
    assert_eq!(usage.all_time.turns, 2);
    assert_eq!(usage.all_time.input, 30);
    assert_eq!(usage.all_time.output, 300);
    assert_eq!(usage.all_time.cache_read, 10_000);
    assert_eq!(usage.all_time.cache_write, 12);
    assert_eq!(usage.all_time.total_tokens, 30 + 300 + 10_000 + 12);

    let sessions = lumen_stats::get_sessions(&pool).await.expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "sess-x");
    assert_eq!(sessions[0].turn_count, 2);
    assert_eq!(
        sessions[0].peak_cache_read, 9_000,
        "the peak-fill proxy is the MAX cache_read, not the sum"
    );

    let stats = lumen_stats::get_stats(&pool).await.expect("stats");
    assert_eq!(stats.turns, 2);
    assert_eq!(stats.output_total, 300);
}

#[tokio::test]
async fn appending_to_a_live_transcript_only_ingests_the_new_lines() {
    // Offset tracking is what stops the daemon re-reading a growing transcript
    // from byte 0 on every poll. INSERT OR IGNORE would hide a regression here,
    // so assert on the row count after several appends.
    let fx = start_daemon().await;
    let path = fx.projects.join("session.jsonl");
    let db = fx.db.clone();

    for i in 0..3 {
        append(
            &path,
            &[transcript_line(
                &format!("inc-{i}"),
                "sess-inc",
                "2026-01-01T10:00:00Z",
                (1, 1, 1, 1),
            )],
        );
        let expected = i + 1;
        wait_until(&format!("{expected} turns"), || turn_count(&db) == expected).await;
    }

    assert_eq!(turn_count(&db), 3, "exactly one row per appended line");
}

#[tokio::test]
async fn a_partial_final_line_is_not_ingested_until_it_is_complete() {
    // Claude Code writes transcripts incrementally, so the daemon can observe a
    // half-written line. Parsing it would either fail or store a truncated turn.
    let fx = start_daemon().await;
    let path = fx.projects.join("session.jsonl");
    let db = fx.db.clone();

    let full = transcript_line("partial-1", "sess-p", "2026-01-01T10:00:00Z", (1, 2, 3, 4));
    let split = full.len() / 2;

    // First half, with NO trailing newline.
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&full.as_bytes()[..split]).unwrap();
        f.flush().unwrap();
    }
    // Give the daemon a poll cycle to see the incomplete line.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        turn_count(&db),
        0,
        "an incomplete line must not produce a row"
    );

    // Now complete it.
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", &full[split..]).unwrap();
        f.flush().unwrap();
    }
    wait_until("the completed line to be ingested", || turn_count(&db) == 1).await;
}

#[tokio::test]
async fn a_duplicate_transcript_line_does_not_double_count() {
    // Re-ingesting the same message_id must be a no-op, or a restart would
    // inflate every user's totals.
    let fx = start_daemon().await;
    let db = fx.db.clone();

    let line = transcript_line("dup-1", "sess-d", "2026-01-01T10:00:00Z", (5, 5, 5, 5));
    append(&fx.projects.join("a.jsonl"), std::slice::from_ref(&line));
    wait_until("the first copy", || turn_count(&db) == 1).await;

    // Same message id, different file — as happens when a transcript is copied.
    append(&fx.projects.join("b.jsonl"), &[line]);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(turn_count(&db), 1, "message_id is the primary key");
}

#[tokio::test]
async fn a_new_session_file_created_after_startup_is_discovered() {
    // The notify watcher may miss a create; the 3s poll is the correctness
    // backbone that must still find it.
    let fx = start_daemon().await;
    let db = fx.db.clone();

    let nested = fx.projects.join("another-project");
    std::fs::create_dir_all(&nested).unwrap();
    append(
        &nested.join("late.jsonl"),
        &[transcript_line(
            "late-1",
            "sess-late",
            "2026-01-01T10:00:00Z",
            (1, 1, 1, 1),
        )],
    );

    wait_until("a file created after startup", || turn_count(&db) == 1).await;
}

#[tokio::test]
async fn non_billable_and_malformed_lines_are_ignored() {
    let fx = start_daemon().await;
    let path = fx.projects.join("mixed.jsonl");
    let db = fx.db.clone();

    append(
        &path,
        &[
            "not json at all !@#".to_string(),
            // A user message carries no usage, so it is not a billable turn.
            r#"{"sessionId":"s","timestamp":"2026-01-01T10:00:00Z","message":{"id":"u1","role":"user","content":[]}}"#.to_string(),
            // An assistant message with no usage block either.
            r#"{"sessionId":"s","timestamp":"2026-01-01T10:00:00Z","message":{"id":"a1","role":"assistant","content":[]}}"#.to_string(),
            transcript_line("real-1", "sess-m", "2026-01-01T10:00:00Z", (1, 1, 1, 1)),
        ],
    );

    wait_until("the one billable turn", || turn_count(&db) == 1).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        turn_count(&db),
        1,
        "only the assistant turn with a usage block counts"
    );
}

#[tokio::test]
async fn a_late_joining_client_gets_the_history_in_its_snapshot() {
    // The GUI can start after the daemon has been running for hours. Its first
    // frame must carry the existing sessions, not an empty gauge.
    let fx = start_daemon().await;
    let db = fx.db.clone();

    append(
        &fx.projects.join("history.jsonl"),
        &[
            transcript_line("h1", "sess-h", "2026-01-01T10:00:00Z", (10, 20, 1_000, 1)),
            transcript_line("h2", "sess-h", "2026-01-01T11:00:00Z", (10, 20, 4_000, 1)),
        ],
    );
    wait_until("history to be ingested", || turn_count(&db) == 2).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(&fx.ws_url)
        .await
        .expect("connect");
    let first = ws.next().await.expect("frame").expect("ok");
    let snapshot: serde_json::Value =
        serde_json::from_str(&first.into_text().unwrap()).expect("json");

    assert_eq!(snapshot["type"], "snapshot");
    let sessions = snapshot["sessions"].as_array().expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], "sess-h");
    assert_eq!(sessions[0]["input"], 20, "summed across the session");
    assert_eq!(
        sessions[0]["fill"], 4_000,
        "fill comes from the newest turn so the gauge is current"
    );
}

// ── subagent handling, end to end ────────────────────────────────────────────

#[tokio::test]
async fn a_subagent_transcript_is_ingested_but_does_not_hijack_the_gauge() {
    // Claude Code writes subagent transcripts under
    //   <project>/<session-uuid>/subagents/agent-<id>.jsonl
    // and — critically — they carry the PARENT's sessionId. Before the fix, the
    // subagent's small fresh context was treated as the session's context fill,
    // so the gauge dipped whenever a subagent ran.
    let fx = start_daemon().await;
    let db = fx.db.clone();

    // Main agent, deep into a large context.
    append(
        &fx.projects.join("session.jsonl"),
        &[transcript_line(
            "main-1",
            "shared-session",
            "2026-01-01T10:00:00Z",
            (10, 20, 300_000, 5),
        )],
    );
    wait_until("the main turn", || turn_count(&db) == 1).await;

    // Subagent, same sessionId, tiny fresh context, and NEWER.
    let sub_dir = fx.projects.join("shared-session/subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    append(
        &sub_dir.join("agent-abc123.jsonl"),
        &[transcript_line(
            "sub-1",
            "shared-session",
            "2026-01-01T10:05:00Z",
            (7, 9, 4_000, 1),
        )],
    );
    wait_until("the subagent turn", || turn_count(&db) == 2).await;

    let conn = lumen_core::meter::connect_db(&db).unwrap();

    // Both rows land, and only the subagent one is flagged.
    let flagged: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM turns WHERE is_subagent = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1, "exactly the subagent turn is flagged");
    let which: String = conn
        .query_row(
            "SELECT message_id FROM turns WHERE is_subagent = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(which, "sub-1");

    // Cost keeps both; the gauge keeps only the main agent.
    let pool = lumen_stats::connect(&format!("sqlite:{}?mode=rwc", db.display()))
        .await
        .unwrap();
    let usage = lumen_stats::get_usage(&pool).await.unwrap();
    assert_eq!(usage.all_time.turns, 2, "both turns are real spend");
    assert_eq!(usage.all_time.input, 17);

    let sessions = lumen_stats::get_sessions(&pool).await.unwrap();
    assert_eq!(sessions.len(), 1, "a subagent is not a separate session");
    assert_eq!(
        sessions[0].peak_cache_read, 300_000,
        "the gauge follows the main agent, not the subagent's 4,000"
    );
}

#[tokio::test]
async fn a_snapshot_labels_the_session_with_its_project() {
    // With two editor windows open the gauge follows whichever session is newest,
    // so it has to say which project that is. The transcript records no cwd, so
    // the label comes from the encoded project directory.
    let fx = start_daemon().await;
    let db = fx.db.clone();
    append(
        &fx.projects.join("session.jsonl"),
        &[transcript_line(
            "p1",
            "sess-p",
            "2026-01-01T10:00:00Z",
            (1, 1, 1_000, 1),
        )],
    );
    wait_until("the turn", || turn_count(&db) == 1).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(&fx.ws_url).await.unwrap();
    let first = ws.next().await.expect("frame").expect("ok");
    let snap: serde_json::Value = serde_json::from_str(&first.into_text().unwrap()).unwrap();

    let session = &snap["sessions"][0];
    // The fixture writes under ".../projects/test-project/...", so the label is
    // derived from that directory name.
    assert_eq!(
        session["project"].as_str(),
        Some("test-project"),
        "got {session}"
    );
    assert_eq!(session["fill"], 1_000);
}
