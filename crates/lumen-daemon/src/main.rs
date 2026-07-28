use futures_util::{SinkExt, StreamExt};
use lumen_core::record::Record;
use lumen_core::schema::DDL;
use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Where the GUI and CLI expect to find the daemon.
const DEFAULT_WS_ADDR: &str = "127.0.0.1:9999";

/// Resolve the WebSocket bind address.
///
/// Overridable via `LUMEN_WS_ADDR` for two reasons: a user whose port 9999 is
/// already taken can move the daemon rather than being stuck (the bind-failure
/// path already anticipates this), and the e2e tests can bind an ephemeral port
/// instead of colliding with a daemon the developer has running.
fn ws_addr() -> String {
    match std::env::var("LUMEN_WS_ADDR") {
        Ok(a) if !a.trim().is_empty() => a,
        _ => DEFAULT_WS_ADDR.to_string(),
    }
}

/// Worst-case lag before a missed notify-event is caught and new session
/// files are discovered. notify handles the common fast path; polling is
/// the correctness guarantee.
const POLL_SECS: u64 = 3;

#[derive(Serialize, Clone, Debug)]
struct TurnMsg {
    message_id: String,
    session_id: String,
    ts: String,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
    /// True when this turn came from a subagent transcript. Consumers must keep
    /// its tokens in cost but exclude its cache_read from the context gauge.
    is_subagent: bool,
    /// Short project label, so a client watching several concurrent sessions can
    /// say which one it is showing.
    project: Option<String>,
}

/// Per-file byte offsets shared between the notify path and the poll path.
/// Value = first byte NOT yet consumed (= end of last complete line ingested).
type Offsets = Arc<Mutex<HashMap<PathBuf, u64>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::var("LUMEN_DB").unwrap_or_else(|_| "lumen.db".to_string());
    let conn = format!("sqlite:{db_path}?mode=rwc");
    let pool = SqlitePoolOptions::new().connect(&conn).await?;
    eprintln!("lumen-daemon using db: {db_path}");

    sqlx::raw_sql(DDL).execute(&pool).await?;

    let (tx, _rx) = broadcast::channel::<TurnMsg>(1000);

    // ── WS server (supervised restart loop) ──────────────────────────────
    // If ws_server returns (bind failure or unrecoverable accept error),
    // recreate it after a 2s pause — same pattern as notify_watch_loop.
    // A transient "address in use" at startup now recovers instead of
    // killing the WS stream for the process lifetime.
    tokio::spawn({
        let ws_pool = pool.clone();
        let ws_tx = tx.clone();
        async move {
            loop {
                if let Err(e) = ws_server(ws_pool.clone(), ws_tx.clone()).await {
                    eprintln!("ws_server exited: {e}; restarting in 2s");
                } else {
                    eprintln!("ws_server returned; restarting in 2s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    });

    let base = dirs::home_dir().unwrap().join(".claude/projects");
    let offsets: Offsets = Arc::new(Mutex::new(HashMap::new()));

    // ── Initial pass ──────────────────────────────────────────────────────
    // Errors are logged per-file and skipped; a single bad file must never
    // abort the startup pass.  This is the only place that starts from 0.
    for path in walk_jsonl(&base) {
        match ingest_from(&pool, &path, 0, &tx, false).await {
            Ok(n) => {
                offsets.lock().unwrap().insert(path, n);
            }
            Err(e) => {
                eprintln!("init ingest {:?}: {e}", path);
            }
        }
    }
    println!("initial import done, watching for changes...");

    // ── Notify watcher task (latency path) ────────────────────────────────
    // Runs in its own task with an inner restart loop.  If the watcher
    // thread dies for any reason, it is recreated after a 2 s pause.  The
    // polling loop below remains the correctness backbone; a dead watcher
    // does not affect what ultimately ends up in the DB.
    tokio::spawn({
        let pool = pool.clone();
        let tx = tx.clone();
        let offsets = offsets.clone();
        let base = base.clone();
        async move {
            loop {
                notify_watch_loop(&pool, &base, &tx, &offsets).await;
                eprintln!("notify watcher channel closed; recreating in 2s...");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    });

    // ── Polling loop (correctness backbone) ───────────────────────────────
    // Re-walks every POLL_SECS seconds.  Picks up new session files created
    // after startup (offset starts at 0 for files not yet in the map).
    // Per-file errors are logged; the loop never terminates.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(POLL_SECS)).await;
        poll_all(&pool, &base, &tx, &offsets).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// poll_all
// ─────────────────────────────────────────────────────────────────────────────

/// Walk all .jsonl under `base`, ingest each file from its saved offset.
/// New files (not yet in offsets) start from byte 0.
/// Per-file errors are logged; the loop always continues.
async fn poll_all(
    pool: &SqlitePool,
    base: &Path,
    tx: &broadcast::Sender<TurnMsg>,
    offsets: &Offsets,
) {
    for path in walk_jsonl(base) {
        let start = *offsets.lock().unwrap().get(&path).unwrap_or(&0);
        match ingest_from(pool, &path, start, tx, true).await {
            Ok(end) => advance_offset(offsets, path, end),
            Err(e) => eprintln!("poll ingest {:?}: {e}", path),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// notify_watch_loop
// ─────────────────────────────────────────────────────────────────────────────

/// Runs a notify watcher and processes its events until the internal channel
/// closes (watcher thread exited).  The outer loop in the spawned task
/// recreates it automatically on return.
///
/// Design: a `spawn_blocking` thread owns the `RecommendedWatcher` for its
/// full lifetime and bridges the synchronous mpsc → tokio unbounded_channel.
/// The async side reads from the tokio channel without blocking the executor.
async fn notify_watch_loop(
    pool: &SqlitePool,
    base: &Path,
    tx: &broadcast::Sender<TurnMsg>,
    offsets: &Offsets,
) {
    let (fwd_tx, mut fwd_rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();
    let base_owned = base.to_path_buf();

    // The blocking thread owns the watcher.  When the async receiver is
    // dropped (this function returns), fwd_tx.send returns Err, the loop
    // breaks, the thread returns, and the watcher is dropped — stopping
    // the OS-level watch cleanly.
    tokio::task::spawn_blocking(move || {
        let (evt_tx, evt_rx) = std::sync::mpsc::channel::<notify::Event>();

        // Move a clone into the callback; drop the original so that only
        // the callback holds a sender.  When the watcher drops the callback,
        // evt_tx is dropped and evt_rx drains cleanly.
        let cb_tx = evt_tx;
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    let _ = cb_tx.send(event);
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("watcher create error: {e}");
                    return;
                }
            };

        use notify::Watcher;
        if let Err(e) = watcher.watch(&base_owned, notify::RecursiveMode::Recursive) {
            eprintln!("watcher watch error: {e}");
            return;
        }

        // Bridge: sync Receiver<Event> → async UnboundedSender<Event>.
        for event in evt_rx {
            if fwd_tx.send(event).is_err() {
                break; // async receiver dropped — exit cleanly
            }
        }
        // `watcher` dropped here → OS watch stops.
    });

    // Async event consumer.  Never uses `?`.
    // A per-file error is logged and the loop continues to the next path.
    while let Some(event) = fwd_rx.recv().await {
        for path in event.paths {
            if path.extension().is_some_and(|x| x == "jsonl") {
                let start = *offsets.lock().unwrap().get(&path).unwrap_or(&0);
                match ingest_from(pool, &path, start, tx, true).await {
                    Ok(end) => advance_offset(offsets, path, end),
                    Err(e) => eprintln!("notify ingest {:?}: {e}", path),
                }
            }
        }
    }
    // fwd_rx drained (bridge thread exited) → return → caller recreates.
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared helper
// ─────────────────────────────────────────────────────────────────────────────

/// Write the new offset for `path`, never moving it backwards.
///
/// The notify path and the poll path can race on the same file (both read
/// from the same start offset, both ingest the same bytes, both return the
/// same end offset).  INSERT OR IGNORE deduplicates in the DB; this max()
/// ensures the stored offset is the high-water mark regardless of ordering.
#[inline]
fn advance_offset(offsets: &Offsets, path: PathBuf, end: u64) {
    let mut map = offsets.lock().unwrap();
    let prev = *map.get(&path).unwrap_or(&0);
    map.insert(path, end.max(prev));
}

// ─────────────────────────────────────────────────────────────────────────────
// ingest_from — core ingestion (logic unchanged from original)
// ─────────────────────────────────────────────────────────────────────────────

/// Read the file from byte `start`, ingest all complete lines, return the
/// new end offset.
///
/// Invariants:
///   - Trailing incomplete line (no \n yet) is never consumed; offset is
///     not advanced past the last complete newline.  Safe to call repeatedly
///     as the writer appends.
///   - Per-line serde errors skip that line and continue (malformed JSON
///     must not stop the rest of the file).
///   - INSERT OR IGNORE on message_id: same bytes ingested twice is safe.
///   - File-open / seek / read errors are returned to the caller, who logs
///     and skips.  This keeps error handling at the outermost loop boundary.
async fn ingest_from(
    pool: &SqlitePool,
    path: &Path,
    start: u64,
    tx: &broadcast::Sender<TurnMsg>,
    broadcast_live: bool,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    let last_nl = match buf.rfind('\n') {
        Some(i) => i,
        None => return Ok(start), // no complete line yet
    };
    let complete = &buf[..=last_nl];
    let fname = path.to_string_lossy().to_string();
    // Both are properties of the path, so resolve once per file, not per line.
    let is_subagent = lumen_core::project::is_subagent_path(&fname);
    let project = lumen_core::project::label_for_transcript(&fname);
    let mut new = 0u64;

    for line in complete.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !rec.is_billable() {
            continue;
        }
        let u = rec.message.usage.as_ref().unwrap();

        let res = sqlx::query(
            "INSERT OR IGNORE INTO turns
             (message_id, session_id, ts, model,
              input_tokens, output_tokens,
              cache_read_input_tokens, cache_creation_input_tokens, source_file,
              is_subagent)
             VALUES (?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&rec.message.id)
        .bind(&rec.session_id)
        .bind(&rec.timestamp)
        .bind(&rec.message.model)
        .bind(u.input_tokens)
        .bind(u.output_tokens)
        .bind(u.cache_read_input_tokens)
        .bind(u.cache_creation_input_tokens)
        .bind(&fname)
        .bind(is_subagent as i64)
        .execute(pool)
        .await?;

        if res.rows_affected() > 0 {
            new += 1;
            println!(
                "+ turn {} ({} in / {} out)",
                rec.message.id, u.input_tokens, u.output_tokens
            );
            if broadcast_live {
                let _ = tx.send(TurnMsg {
                    message_id: rec.message.id.clone(),
                    session_id: rec.session_id.clone(),
                    ts: rec.timestamp.clone(),
                    model: rec.message.model.clone().unwrap_or_default(),
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_read_input_tokens: u.cache_read_input_tokens,
                    cache_creation_input_tokens: u.cache_creation_input_tokens,
                    is_subagent,
                    project: project.clone(),
                });
            }
            // Calibration: real vs estimated output tokens (unchanged).
            let est = lumen_core::tokenizer::count_tokens(&rec.message.text_output()) as i64;
            if est > 0 {
                let _ = sqlx::query(
                    "INSERT OR IGNORE INTO calibration (message_id, real_output, est_output)
                     VALUES (?,?,?)",
                )
                .bind(&rec.message.id)
                .bind(u.output_tokens)
                .bind(est)
                .execute(pool)
                .await;
            }
        }
    }
    if new > 0 {
        println!("  ({new} new from {fname})");
    }

    Ok(start + (last_nl as u64) + 1)
}

// ─────────────────────────────────────────────────────────────────────────────
// ws_server
// ─────────────────────────────────────────────────────────────────────────────

/// Returns Err on bind failure (triggers the restart loop in main).
/// Individual accept/connection errors are logged and skipped; only a bind
/// failure kills this invocation and lets the caller retry.
async fn ws_server(
    pool: SqlitePool,
    tx: broadcast::Sender<TurnMsg>,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = ws_addr();
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
        eprintln!("ws bind failed ({addr} in use?): {e}");
        e
    })?;
    println!("WebSocket server listening on ws://{addr}");
    serve_ws(listener, pool, tx).await
}

/// The accept loop, taking an already-bound listener.
///
/// Split from `ws_server` so tests can bind port 0, learn the ephemeral port and
/// drive a real client — binding the fixed 9999 in a test would collide with a
/// running daemon and with other tests.
async fn serve_ws(
    listener: tokio::net::TcpListener,
    pool: SqlitePool,
    tx: broadcast::Sender<TurnMsg>,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("ws accept error: {e}");
                continue;
            }
        };
        let pool = pool.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(w) => w,
                Err(_) => return,
            };
            let (mut sink, _read) = ws.split();

            // 1. Snapshot on connect.
            // fill = latest turn's cache_read (not session MAX) so the gauge
            // reflects the real current context fill, including drops after /compact.
            #[allow(clippy::type_complexity)]
            let snapshot_rows = sqlx::query_as::<
                _,
                (
                    String,
                    i64,
                    i64,
                    i64,
                    i64,
                    Option<i64>,
                    i64,
                    String,
                    Option<String>,
                    Option<String>,
                ),
            >(
                // Cost SUMs cover every turn including subagents — that is real
                // spend. fill / peak_fill deliberately exclude subagents: their
                // transcripts reuse the parent's sessionId but carry a separate,
                // fresh context, so counting them made the gauge dip whenever a
                // subagent ran.
                "SELECT t.session_id,
                        SUM(t.input_tokens),
                        SUM(t.output_tokens),
                        SUM(t.cache_read_input_tokens),
                        SUM(t.cache_creation_input_tokens),
                        (SELECT cache_read_input_tokens FROM turns
                          WHERE session_id = t.session_id AND is_subagent = 0
                          ORDER BY ts DESC LIMIT 1),
                        -- Peak fill: the window is derived from the session's
                        -- high-water mark, never the momentary fill, so /compact
                        -- cannot shrink the reported window mid-session.
                        (SELECT COALESCE(MAX(cache_read_input_tokens),0) FROM turns
                          WHERE session_id = t.session_id AND is_subagent = 0),
                        MAX(t.ts),
                        (SELECT t2.model FROM turns t2
                          WHERE t2.session_id = t.session_id AND t2.model IS NOT NULL
                          ORDER BY t2.ts DESC LIMIT 1),
                        -- Project identity: the transcript records no cwd, so the
                        -- encoded project directory is all we have.
                        (SELECT t3.source_file FROM turns t3
                          WHERE t3.session_id = t.session_id AND t3.source_file IS NOT NULL
                          ORDER BY t3.ts DESC LIMIT 1)
                 FROM turns t GROUP BY t.session_id",
            )
            .fetch_all(&pool)
            .await;

            if let Ok(rows) = snapshot_rows {
                let snapshot = serde_json::json!({
                    "type": "snapshot",
                    "sessions": rows.iter().map(|(s, i, o, cr, cw, fill, peak, ts, model, src)| {
                        // A session made up entirely of subagent turns has no
                        // main-agent fill to report; 0 is the honest answer.
                        let project = src
                            .as_deref()
                            .and_then(lumen_core::project::label_for_transcript);
                        serde_json::json!({
                            "session_id": s,
                            "input": i, "output": o,
                            "cache_read": cr, "cache_write": cw,
                            "fill": fill.unwrap_or(0), "peak_fill": peak, "ts": ts,
                            "model": model, "project": project
                        })
                    }).collect::<Vec<_>>()
                });
                let _ = sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        snapshot.to_string().into(),
                    ))
                    .await;
            }

            // 2. Live turns.
            while let Ok(turn) = rx.recv().await {
                let msg = serde_json::json!({ "type": "event", "turn": turn });
                if sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        msg.to_string().into(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// walk_jsonl — unchanged
// ─────────────────────────────────────────────────────────────────────────────

fn walk_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk_jsonl(&p));
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::io::Write;
    use tempfile::TempDir;

    const BILLABLE_LINE: &str = r#"{"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z","message":{"id":"__ID__","model":"claude-sonnet-4-6","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;

    async fn make_pool(dir: &TempDir) -> SqlitePool {
        let db = dir.path().join("test.db");
        let url = format!("sqlite:{}?mode=rwc", db.display());
        let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
        sqlx::raw_sql(DDL).execute(&pool).await.unwrap();
        pool
    }

    fn write_jsonl(path: &Path, lines: &[&str]) {
        let mut f = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
    }

    // ── Regression: bad JSON line must not kill ingest ─────────────────────────

    #[tokio::test]
    async fn bad_json_line_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;

        let jsonl = dir.path().join("session.jsonl");
        let good = BILLABLE_LINE.replace("__ID__", "msg-good");
        write_jsonl(&jsonl, &["this line is not json at all !@#$", &good]);

        let (tx, _rx) = broadcast::channel(4);
        let result = ingest_from(&pool, &jsonl, 0, &tx, false).await;
        assert!(
            result.is_ok(),
            "ingest_from must succeed despite bad JSON line"
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM turns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "the one good line must still be inserted");
    }

    // ── Regression: duplicate message_id → single DB row ─────────────────────

    #[tokio::test]
    async fn duplicate_message_id_yields_one_row() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;

        let jsonl = dir.path().join("session.jsonl");
        let line = BILLABLE_LINE.replace("__ID__", "msg-dup");
        write_jsonl(&jsonl, &[&line, &line]);

        let (tx, _rx) = broadcast::channel(4);
        ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM turns WHERE message_id='msg-dup'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1, "INSERT OR IGNORE must deduplicate on message_id");
    }

    // ── Tailing: resume from saved byte offset ────────────────────────────────

    #[tokio::test]
    async fn tailing_offset_resumes_correctly() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("session.jsonl");

        let line_a = BILLABLE_LINE.replace("__ID__", "msg-tail-a");
        let line_b = BILLABLE_LINE.replace("__ID__", "msg-tail-b");

        // First pass: write and ingest only line_a
        write_jsonl(&jsonl, &[&line_a]);
        let (tx, _rx) = broadcast::channel(4);
        let offset = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        assert!(offset > 0, "offset must advance past line_a");

        // Append line_b
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&jsonl)
                .unwrap();
            writeln!(f, "{}", line_b).unwrap();
        }

        // Second pass: resume from saved offset — only line_b is new
        let (tx2, _rx2) = broadcast::channel(4);
        ingest_from(&pool, &jsonl, offset, &tx2, false)
            .await
            .unwrap();

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM turns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            count, 2,
            "both turns must be in DB; no duplicates from re-read"
        );
    }

    // ── walk_jsonl: finds only .jsonl files ──────────────────────────────────

    #[test]
    fn walk_jsonl_filters_by_extension() {
        let dir = TempDir::new().unwrap();
        std::fs::File::create(dir.path().join("a.jsonl")).unwrap();
        std::fs::File::create(dir.path().join("b.jsonl")).unwrap();
        std::fs::File::create(dir.path().join("c.txt")).unwrap();

        let found = walk_jsonl(dir.path());
        assert_eq!(found.len(), 2, "should find exactly the two .jsonl files");
        assert!(found.iter().all(|p| p.extension().unwrap() == "jsonl"));
    }
}

#[cfg(test)]
mod ws_tests {
    //! End-to-end tests for the WebSocket surface the GUI and CLI consume.
    //!
    //! These bind an ephemeral port, run the real `serve_ws` accept loop and
    //! connect a real client, so they cover the snapshot query, the live
    //! broadcast, and the `Utf8Bytes` framing that the tungstenite 0.30 upgrade
    //! changed.

    use super::*;
    use futures_util::StreamExt;
    use sqlx::sqlite::SqlitePoolOptions;
    use tempfile::TempDir;
    use tokio_tungstenite::tungstenite::Message;

    async fn pool_with_schema(dir: &TempDir) -> SqlitePool {
        let url = format!("sqlite:{}?mode=rwc", dir.path().join("ws.db").display());
        let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
        sqlx::raw_sql(DDL).execute(&pool).await.unwrap();
        pool
    }

    async fn add_turn(pool: &SqlitePool, id: &str, session: &str, ts: &str, fill: i64) {
        sqlx::query(
            "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                               cache_read_input_tokens,cache_creation_input_tokens)
             VALUES(?,?,?,'claude-sonnet-4',10,20,?,5)",
        )
        .bind(id)
        .bind(session)
        .bind(ts)
        .bind(fill)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Start `serve_ws` on an ephemeral port; returns the ws:// URL and the sender.
    async fn start(pool: SqlitePool) -> (String, broadcast::Sender<TurnMsg>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _rx) = broadcast::channel::<TurnMsg>(64);
        let server_tx = tx.clone();
        tokio::spawn(async move {
            let _ = serve_ws(listener, pool, server_tx).await;
        });
        (format!("ws://{addr}"), tx)
    }

    /// Read the next text frame, failing the test rather than hanging forever.
    async fn next_json<S>(stream: &mut S) -> serde_json::Value
    where
        S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("stream ended")
            .expect("websocket error");
        let text = msg.into_text().expect("frame must be text");
        serde_json::from_str(&text).expect("frame must be JSON")
    }

    #[tokio::test]
    async fn a_client_receives_a_snapshot_on_connect() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        add_turn(&pool, "m1", "s1", "2026-01-01T10:00:00Z", 1_000).await;
        add_turn(&pool, "m2", "s1", "2026-01-01T11:00:00Z", 7_000).await;

        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let snap = next_json(&mut ws).await;

        assert_eq!(snap["type"], "snapshot");
        let sessions = snap["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["session_id"], "s1");
        assert_eq!(sessions[0]["input"], 20, "summed across both turns");
        assert_eq!(sessions[0]["output"], 40);
    }

    #[tokio::test]
    async fn the_snapshot_fill_is_the_latest_turn_not_the_session_max() {
        // After /compact the fill DROPS. Reporting MAX would leave the gauge
        // stuck at the pre-compaction high-water mark.
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        add_turn(&pool, "m1", "s1", "2026-01-01T10:00:00Z", 190_000).await;
        add_turn(&pool, "m2", "s1", "2026-01-01T11:00:00Z", 12_000).await;

        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let snap = next_json(&mut ws).await;
        assert_eq!(
            snap["sessions"][0]["fill"], 12_000,
            "must follow the newest turn, not the peak"
        );
    }

    #[tokio::test]
    async fn an_empty_db_still_produces_a_snapshot_frame() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let snap = next_json(&mut ws).await;
        assert_eq!(snap["type"], "snapshot");
        assert!(snap["sessions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_row_per_session_is_reported() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        add_turn(&pool, "a", "s1", "2026-01-01T10:00:00Z", 100).await;
        add_turn(&pool, "b", "s2", "2026-01-01T11:00:00Z", 200).await;

        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let snap = next_json(&mut ws).await;
        assert_eq!(snap["sessions"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_broadcast_turn_reaches_a_connected_client() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _snapshot = next_json(&mut ws).await;

        tx.send(TurnMsg {
            message_id: "msg-live".into(),
            session_id: "s1".into(),
            ts: "2026-01-01T12:00:00Z".into(),
            model: "claude-opus-4".into(),
            input_tokens: 11,
            output_tokens: 22,
            cache_read_input_tokens: 33,
            cache_creation_input_tokens: 44,
            is_subagent: false,
            project: None,
        })
        .unwrap();

        let event = next_json(&mut ws).await;
        assert_eq!(event["type"], "event");
        assert_eq!(event["turn"]["message_id"], "msg-live");
        assert_eq!(event["turn"]["model"], "claude-opus-4");
        assert_eq!(event["turn"]["cache_read_input_tokens"], 33);
    }

    #[tokio::test]
    async fn every_connected_client_gets_the_same_live_turn() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool).await;

        let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _ = next_json(&mut a).await;
        let _ = next_json(&mut b).await;

        tx.send(TurnMsg {
            message_id: "fanout".into(),
            session_id: "s1".into(),
            ts: "2026-01-01T12:00:00Z".into(),
            model: "m".into(),
            input_tokens: 1,
            output_tokens: 1,
            cache_read_input_tokens: 1,
            cache_creation_input_tokens: 1,
            is_subagent: false,
            project: None,
        })
        .unwrap();

        assert_eq!(next_json(&mut a).await["turn"]["message_id"], "fanout");
        assert_eq!(next_json(&mut b).await["turn"]["message_id"], "fanout");
    }

    #[tokio::test]
    async fn turn_frames_arrive_in_order() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _ = next_json(&mut ws).await;

        for i in 0..5 {
            tx.send(TurnMsg {
                message_id: format!("m{i}"),
                session_id: "s1".into(),
                ts: "2026-01-01T12:00:00Z".into(),
                model: "m".into(),
                input_tokens: i,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                is_subagent: false,
                project: None,
            })
            .unwrap();
        }
        for i in 0..5 {
            assert_eq!(
                next_json(&mut ws).await["turn"]["message_id"],
                format!("m{i}")
            );
        }
    }

    #[tokio::test]
    async fn a_disconnecting_client_does_not_stop_the_server() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool).await;

        {
            let (mut doomed, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let _ = next_json(&mut doomed).await;
        } // dropped mid-stream

        // A turn sent to nobody must not poison the broadcast channel.
        let _ = tx.send(TurnMsg {
            message_id: "orphan".into(),
            session_id: "s1".into(),
            ts: "2026-01-01T12:00:00Z".into(),
            model: "m".into(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            is_subagent: false,
            project: None,
        });

        // A fresh client must still be served.
        let (mut fresh, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        assert_eq!(next_json(&mut fresh).await["type"], "snapshot");
    }

    #[tokio::test]
    async fn frames_are_text_not_binary() {
        // The CLI checks msg.is_text() and the GUI parses event.payload as a
        // string; a binary frame would be silently dropped by both.
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(msg.is_text(), "got {msg:?}");
    }

    #[tokio::test]
    async fn ingest_then_broadcast_reaches_the_client_end_to_end() {
        // The real pipeline: a JSONL line lands on disk, ingest_from parses and
        // stores it, and the resulting broadcast reaches a websocket client.
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool.clone()).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _ = next_json(&mut ws).await;

        let line = r#"{"sessionId":"s-e2e","timestamp":"2026-01-01T12:00:00Z","message":{"id":"msg-e2e","model":"claude-sonnet-4-6","role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":9,"cache_read_input_tokens":5000,"cache_creation_input_tokens":0}}}"#;
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(&jsonl, format!("{line}\n")).unwrap();

        let end = ingest_from(&pool, &jsonl, 0, &tx, true).await.unwrap();
        assert!(end > 0, "offset must advance past the consumed line");

        let event = next_json(&mut ws).await;
        assert_eq!(event["type"], "event");
        assert_eq!(event["turn"]["message_id"], "msg-e2e");
        assert_eq!(event["turn"]["session_id"], "s-e2e");
        assert_eq!(event["turn"]["cache_read_input_tokens"], 5000);

        // And it is durable, not just broadcast.
        let stored: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM turns WHERE message_id='msg-e2e'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, 1);
    }
}
