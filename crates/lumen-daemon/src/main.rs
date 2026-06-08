use lumen_core::record::Record;
use lumen_core::schema::DDL;
use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;

/// Worst-case lag before a missed notify-event is caught and new session
/// files are discovered. notify handles the common fast path; polling is
/// the correctness guarantee.
const POLL_SECS: u64 = 3;

#[derive(Serialize, Clone)]
struct TurnMsg {
    message_id: String,
    session_id: String,
    ts: String,
    model: String,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
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
        let ws_tx   = tx.clone();
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

    let base    = dirs::home_dir().unwrap().join(".claude/projects");
    let offsets: Offsets = Arc::new(Mutex::new(HashMap::new()));

    // ── Initial pass ──────────────────────────────────────────────────────
    // Errors are logged per-file and skipped; a single bad file must never
    // abort the startup pass.  This is the only place that starts from 0.
    for path in walk_jsonl(&base) {
        match ingest_from(&pool, &path, 0, &tx, false).await {
            Ok(n)  => { offsets.lock().unwrap().insert(path, n); }
            Err(e) => { eprintln!("init ingest {:?}: {e}", path); }
        }
    }
    println!("initial import done, watching for changes...");

    // ── Notify watcher task (latency path) ────────────────────────────────
    // Runs in its own task with an inner restart loop.  If the watcher
    // thread dies for any reason, it is recreated after a 2 s pause.  The
    // polling loop below remains the correctness backbone; a dead watcher
    // does not affect what ultimately ends up in the DB.
    tokio::spawn({
        let pool    = pool.clone();
        let tx      = tx.clone();
        let offsets = offsets.clone();
        let base    = base.clone();
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
    pool:    &SqlitePool,
    base:    &Path,
    tx:      &broadcast::Sender<TurnMsg>,
    offsets: &Offsets,
) {
    for path in walk_jsonl(base) {
        let start = *offsets.lock().unwrap().get(&path).unwrap_or(&0);
        match ingest_from(pool, &path, start, tx, true).await {
            Ok(end) => advance_offset(offsets, path, end),
            Err(e)  => eprintln!("poll ingest {:?}: {e}", path),
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
    pool:    &SqlitePool,
    base:    &Path,
    tx:      &broadcast::Sender<TurnMsg>,
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
        let mut watcher = match notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res { let _ = cb_tx.send(event); }
            },
        ) {
            Ok(w)  => w,
            Err(e) => { eprintln!("watcher create error: {e}"); return; }
        };

        use notify::Watcher;
        if let Err(e) = watcher.watch(&base_owned, notify::RecursiveMode::Recursive) {
            eprintln!("watcher watch error: {e}"); return;
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
            if path.extension().map_or(false, |x| x == "jsonl") {
                let start = *offsets.lock().unwrap().get(&path).unwrap_or(&0);
                match ingest_from(pool, &path, start, tx, true).await {
                    Ok(end) => advance_offset(offsets, path, end),
                    Err(e)  => eprintln!("notify ingest {:?}: {e}", path),
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
    pool:           &SqlitePool,
    path:           &Path,
    start:          u64,
    tx:             &broadcast::Sender<TurnMsg>,
    broadcast_live: bool,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    let last_nl = match buf.rfind('\n') {
        Some(i) => i,
        None    => return Ok(start), // no complete line yet
    };
    let complete = &buf[..=last_nl];
    let fname    = path.to_string_lossy().to_string();
    let mut new  = 0u64;

    for line in complete.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: Record = match serde_json::from_str(line) {
            Ok(r)  => r,
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
              cache_read_input_tokens, cache_creation_input_tokens, source_file)
             VALUES (?,?,?,?,?,?,?,?,?)",
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
                    ts:         rec.timestamp.clone(),
                    model:      rec.message.model.clone().unwrap_or_default(),
                    input_tokens:                u.input_tokens,
                    output_tokens:               u.output_tokens,
                    cache_read_input_tokens:     u.cache_read_input_tokens,
                    cache_creation_input_tokens: u.cache_creation_input_tokens,
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
async fn ws_server(pool: SqlitePool, tx: broadcast::Sender<TurnMsg>) -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:9999").await
        .map_err(|e| { eprintln!("ws bind failed (port 9999 in use?): {e}"); e })?;
    println!("WebSocket server listening on ws://127.0.0.1:9999");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e)   => { eprintln!("ws accept error: {e}"); continue; }
        };
        let pool = pool.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(w)  => w,
                Err(_) => return,
            };
            let (mut sink, _read) = ws.split();

            // 1. Snapshot on connect.
            // fill = latest turn's cache_read (not session MAX) so the gauge
            // reflects the real current context fill, including drops after /compact.
            if let Ok(rows) = sqlx::query_as::<_, (String, i64, i64, i64, i64, i64, String)>(
                "SELECT t.session_id,
                        SUM(t.input_tokens),
                        SUM(t.output_tokens),
                        SUM(t.cache_read_input_tokens),
                        SUM(t.cache_creation_input_tokens),
                        (SELECT cache_read_input_tokens FROM turns
                          WHERE session_id = t.session_id ORDER BY ts DESC LIMIT 1),
                        MAX(t.ts)
                 FROM turns t GROUP BY t.session_id",
            )
            .fetch_all(&pool)
            .await
            {
                let snapshot = serde_json::json!({
                    "type": "snapshot",
                    "sessions": rows.iter().map(|(s, i, o, cr, cw, fill, ts)| {
                        serde_json::json!({
                            "session_id": s,
                            "input": i, "output": o,
                            "cache_read": cr, "cache_write": cw,
                            "fill": fill, "ts": ts
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
            } else if p.extension().map_or(false, |x| x == "jsonl") {
                out.push(p);
            }
        }
    }
    out
}
