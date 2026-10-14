use futures_util::{SinkExt, StreamExt};
use lumen_core::record::Record;
use serde::Serialize;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Write a line to stderr, ignoring failure.
///
/// Never `println!` or `eprintln!` in this process — use `logline!`. BOTH pipes belong
/// to the GUI, so either macro panics once it dies. The rule named only stderr, and four
/// `println!` calls survived it; one ran on the main thread during startup, so a GUI that
/// died mid-startup took the daemon with it.
///
/// Never `eprintln!` in this process. Its stderr is a pipe whose read end belongs to
/// the GUI, so the moment the GUI dies every `eprintln!` becomes a panic — and one of
/// them fires every two seconds from the WebSocket restart loop. A daemon must not die
/// because it could not describe itself, and it must not be *kept alive* by a panic
/// either: that is precisely how the orphan survived its own watchdog, which noticed
/// the supervisor was gone, panicked announcing it, and unwound only its own thread.
macro_rules! logline {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

// ─────────────────────────────────────────────────────────────────────────────
// WS restart backoff and log suppression
//
// The restart loop used to retry every 2 seconds and log two lines each time, for as long as the
// failure persisted. Measured against a held port: 434 KB/hour. The GUI forwards this process's
// stderr into its own log, which is capped at 256 KB with KeepOne — so a sustained bind failure
// rotated the file every ~35 minutes and took the startup and tray diagnostics with it. That is
// the opposite of what release logging was added for: the evidence disappears precisely when the
// user finally reports the problem.
//
// Two changes, both bounded. The retry interval backs off, because a held port does not become
// free faster for being asked every 2 seconds; and an unchanged failure stops being logged in
// full, with the suppressed count reported when it next speaks.
// ─────────────────────────────────────────────────────────────────────────────

/// First retry delay after a failure.
const WS_RETRY_BASE_SECS: u64 = 2;

/// Ceiling on the retry delay.
///
/// 30s rather than something larger: an orphaned daemon from an upgrade usually dies within
/// seconds once its stdin closes, and this bounds how long the replacement waits before taking the
/// port. The GUI retries its own connection every 2s, so recovery is this plus at most 2s.
const WS_RETRY_CAP_SECS: u64 = 30;

/// How long to wait before restart attempt `attempt` (1-based).
///
/// Doubles from the base and clamps at the cap.
fn ws_retry_delay(attempt: u32) -> std::time::Duration {
    let secs = WS_RETRY_BASE_SECS
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(16))
        .min(WS_RETRY_CAP_SECS);
    std::time::Duration::from_secs(secs)
}

/// Should attempt `attempt` (1-based) be logged?
///
/// `changed` means the failure text differs from the last one logged — always worth saying, since
/// a new error is new information. Otherwise: the first three, then one in ten. `verbose` comes
/// from `LUMEN_LOG=debug|trace` and reports everything, for someone actively debugging.
fn should_log_ws_retry(attempt: u32, changed: bool, verbose: bool) -> bool {
    verbose || changed || attempt <= 3 || attempt.is_multiple_of(10)
}

/// Whether every retry should be logged, from `LUMEN_LOG`.
///
/// The same variable the GUI uses for its own level, so there is one thing to learn. This process
/// has no level filter of its own — `logline!` writes straight to stderr — so it is read here
/// rather than plumbed through a logger.
fn ws_retry_verbose() -> bool {
    matches!(
        std::env::var("LUMEN_LOG")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "debug" | "trace"
    )
}

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

/// Exit when the process supervising this daemon goes away.
///
/// The GUI spawns the daemon as a sidecar with a piped stdin and sets
/// `LUMEN_SUPERVISED=1`. When the GUI dies — Quit, a crash, or the app being
/// replaced by `brew upgrade` — the write end of that pipe closes and the read
/// below returns 0 bytes.
///
/// Without this the daemon is reparented to launchd and keeps holding
/// 127.0.0.1:9999. The *next* app launch then cannot bind: its own daemon spins
/// in the supervised restart loop below forever while the GUI talks to the
/// orphan from the previous build. That is a silent version skew on every
/// upgrade, and it is silent precisely because the retry loop was designed to
/// recover from a *transient* collision and cannot tell one from a permanent
/// squatter.
///
/// Gated on the env var, not applied unconditionally: an unsupervised run has no
/// such pipe. `lumen-daemon < /dev/null` would see an immediate EOF and exit
/// instantly, which would break running the daemon by hand.
fn exit_when_supervisor_does() {
    if std::env::var("LUMEN_SUPERVISED").as_deref() != Ok("1") {
        return;
    }
    std::thread::spawn(|| {
        let mut buf = [0u8; 64];
        loop {
            // Anything actually sent on stdin is ignored; only EOF matters.
            match std::io::stdin().lock().read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        // Exit FIRST. The logline! macro cannot panic, but the ordering still matters.
        //
        // The supervisor owns the read end of this process's stderr as well as the
        // write end of its stdin, so both die together. `eprintln!` panics when the
        // write fails, and a panic on a spawned thread unwinds only that thread —
        // so logging before exiting meant the watchdog noticed the EOF, panicked
        // trying to announce it, and left the process running. The orphan survived
        // for exactly the reason the log line was added: to explain itself.
        //
        // Verified on a real install: after SIGKILL of the app the daemon's fd 0 had
        // no peer — the pipe was at EOF and the read had returned — yet the process
        // was still holding 127.0.0.1:9999 twenty-four seconds later.
        logline!("lumen-daemon: supervisor exited, shutting down to free the port");
        std::process::exit(0);
    });
}

/// Record a daemon fault to the JSONL spool.
///
/// Spool, not SQLite: these are error paths, and the pool they would write to is often
/// exactly what is unhealthy. Throttled because both callers sit in retry loops that can
/// fire indefinitely.
fn note_fault(kind: &str, variant: &str, path: Option<&std::path::Path>, detail: String) {
    use lumen_core::faults::{FaultRecord, record_throttled};

    let mut rec = FaultRecord::now(kind, variant).with_detail(detail);
    if let Some(p) = path {
        // Into `path`, never into `detail`: the reporter redacts the path field, so a
        // transcript path under $HOME does not reach a public issue verbatim.
        rec = rec.with_path(p.display().to_string());
    }
    record_throttled(&rec, std::time::Duration::from_secs(60));
}

/// Resolve the directory of Claude Code transcripts to watch.
///
/// Overridable via `LUMEN_PROJECTS_DIR` so the e2e tests can point the daemon at
/// a tempdir. Setting `HOME` is not enough: `dirs::home_dir()` reads `HOME` only
/// on Unix, and on Windows resolves `%USERPROFILE%` — so a test that exported
/// `HOME` still had the daemon watching the real user profile and ingesting
/// nothing. An explicit override behaves the same on every platform, and leaves
/// production resolution untouched: `%USERPROFILE%` is where Claude Code
/// actually writes transcripts on Windows.
fn projects_dir() -> Option<PathBuf> {
    resolve_projects_dir(
        std::env::var("LUMEN_PROJECTS_DIR").ok().as_deref(),
        dirs::home_dir().as_deref(),
    )
}

/// The precedence policy behind [`projects_dir`], with the environment passed in.
///
/// Separated for the same reason as `lumen_core::meter::resolve_db_path`: mutating
/// the environment in a test is racy across threads and `unsafe` in edition 2024.
fn resolve_projects_dir(override_dir: Option<&str>, home: Option<&Path>) -> Option<PathBuf> {
    match override_dir {
        Some(d) if !d.trim().is_empty() => Some(PathBuf::from(d)),
        // No `unwrap()`: a machine where the home directory cannot be resolved
        // should report that, not panic inside an unrelated startup step.
        _ => home.map(|h| h.join(".claude/projects")),
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
    /// The part of `cache_creation_input_tokens` written to the 1-hour cache, which costs
    /// more than a 5-minute write. Null when the transcript recorded no split by duration:
    /// then none of the write's duration is known.
    cache_creation_1h_input_tokens: Option<i64>,
    /// `usage.speed`, "standard" or "fast"; null when the transcript did not record it.
    speed: Option<String>,
    /// `usage.inference_geo`, "us", "global" or "not_available"; null when the transcript did
    /// not record it. US-only inference costs more on the 4.6 and later models.
    inference_geo: Option<String>,
    /// True when this turn came from a subagent transcript. Consumers must keep
    /// its tokens in cost but exclude its cache_read from the context gauge.
    is_subagent: bool,
    /// Short project label, so a client watching several concurrent sessions can
    /// say which one it is showing.
    project: Option<String>,
    /// The turn's rowid in the ledger. Not sent: `serve_ws` uses it to hold back a live turn
    /// the snapshot it just sent already counted.
    #[serde(skip)]
    rowid: i64,
}

/// Per-file byte offsets shared between the notify path and the poll path.
/// Value = first byte NOT yet consumed (= end of last complete line ingested).
type Offsets = Arc<Mutex<HashMap<PathBuf, u64>>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    exit_when_supervisor_does();

    // Resolve exactly as every other writer does. The old fallback was the string
    // "lumen.db" — relative to the working directory — which is the bug that split
    // the ledger in two: a daemon started with LUMEN_DB unset created a second
    // database wherever it happened to be launched from, and both files then
    // accumulated real events. Sharing meter::resolve_db_path is what keeps the
    // GUI, the MCP server, the hook and the daemon pointed at one file by
    // construction rather than by four copies of the same convention.
    let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string());
    let db_path = match lumen_core::meter::resolve_db_path(
        std::env::var("LUMEN_DB").ok().as_deref(),
        home.as_deref(),
    ) {
        Some(p) => p.to_string_lossy().to_string(),
        None => {
            // Failing loudly beats inventing a path. A daemon that cannot name its
            // database would otherwise start a third ledger nobody reads.
            logline!("lumen-daemon: cannot resolve a database path; set LUMEN_DB");
            return Ok(());
        }
    };
    if let Some(parent) = Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    lumen_core::meter::restore_sidecar_write_bits(Path::new(&db_path));
    let conn = format!("sqlite:{db_path}?mode=rwc");
    logline!("lumen-daemon using db: {db_path}");

    // open_pool runs init_schema, not raw DDL: DDL alone is CREATE TABLE IF NOT EXISTS, so on an
    // existing database it is a no-op and the is_subagent column added in 1.1.0
    // never appeared — making every insert below fail on an unknown column.
    let pool = lumen_core::schema::open_pool(&conn).await?;

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
            let verbose = ws_retry_verbose();
            // Attempt count and last failure, so an unchanged error can be collapsed and a new one
            // always reported. Reset on success, so a daemon that recovers and later fails again
            // starts loud rather than inheriting a suppressed state.
            let mut attempt: u32 = 0;
            let mut last: Option<String> = None;
            let mut suppressed: u32 = 0;
            loop {
                let outcome = match ws_server(ws_pool.clone(), ws_tx.clone()).await {
                    Err(e) => {
                        note_fault("ws_restart", "error", None, e.to_string());
                        format!("exited: {e}")
                    }
                    Ok(()) => {
                        note_fault(
                            "ws_restart",
                            "clean_return",
                            None,
                            "ws_server returned Ok; the accept loop should never end".into(),
                        );
                        "returned Ok; the accept loop should never end".to_string()
                    }
                };

                attempt = attempt.saturating_add(1);
                let changed = last.as_deref() != Some(outcome.as_str());
                if changed {
                    attempt = 1;
                }
                let delay = ws_retry_delay(attempt);

                if should_log_ws_retry(attempt, changed, verbose) {
                    let also = if suppressed > 0 {
                        format!(" ({suppressed} identical since the last message)")
                    } else {
                        String::new()
                    };
                    logline!(
                        "ws_server {outcome}{also}; attempt {attempt}, retrying in {}s",
                        delay.as_secs()
                    );
                    suppressed = 0;
                } else {
                    suppressed = suppressed.saturating_add(1);
                }
                last = Some(outcome);

                tokio::time::sleep(delay).await;
            }
        }
    });

    let base = match projects_dir() {
        Some(b) => b,
        None => {
            logline!("cannot resolve a home directory; set LUMEN_PROJECTS_DIR");
            return Ok(());
        }
    };
    logline!("lumen-daemon watching: {}", base.display());
    let offsets: Offsets = Arc::new(Mutex::new(HashMap::new()));

    // What the ledger already holds of blocked Reads and of transcripts read for them, so the
    // pass below writes only what moved.
    if KNOWN.set(load_known(&pool).await).is_err() {
        logline!("the blocks already recorded were read twice; the first reading stands");
    }

    // ── Initial pass ──────────────────────────────────────────────────────
    // Errors are logged per-file and skipped; a single bad file must never
    // abort the startup pass.  This is the only place that starts from 0.
    //
    // It is also the backfill of what a newer daemon reads in a transcript: every start reads
    // every transcript on disk from its first line, so the first start of a build that records
    // blocked Reads (`intercept_blocks`) records those of transcripts written before it, and the
    // primary keys keep every later start from adding a row twice. Off the app's UI thread: this
    // runs in the daemon, a process of its own.
    for path in walk_jsonl(&base) {
        match ingest_from(&pool, &path, 0, &tx, false).await {
            Ok(n) => {
                offsets.lock().unwrap().insert(path, n);
            }
            Err(e) => {
                logline!("init ingest {:?}: {e}", path);
                note_fault("ingest_failed", "init", Some(&path), e.to_string());
            }
        }
    }
    // Reports the total, which is the information the suppressed per-turn lines carried in
    // aggregate — and the only line a fresh install needs about the backfill.
    //
    // Read from the ledger rather than counted in the loop: `ingest_from` returns a byte offset,
    // and threading a second value out of it would change three call sites for one log line.
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM turns")
        .fetch_one(&pool)
        .await
        .unwrap_or(-1);
    logline!("initial import done ({total} turns in the ledger), watching for changes...");

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
                logline!("notify watcher channel closed; recreating in 2s...");
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
            Err(e) => {
                logline!("poll ingest {:?}: {e}", path);
                note_fault("ingest_failed", "poll", Some(&path), e.to_string());
            }
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
                    logline!("watcher create error: {e}");
                    return;
                }
            };

        use notify::Watcher;
        if let Err(e) = watcher.watch(&base_owned, notify::RecursiveMode::Recursive) {
            logline!("watcher watch error: {e}");
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
                    Err(e) => {
                        logline!("notify ingest {:?}: {e}", path);
                        note_fault("ingest_failed", "notify", Some(&path), e.to_string());
                    }
                }
            }
        }
    }
    // fwd_rx drained (bridge thread exited) → return → caller recreates.
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared helper
// ─────────────────────────────────────────────────────────────────────────────

/// Write the new offset for `path`, never moving it backwards, but for a transcript rewritten in
/// place: that one was read again from its first line, and its offset is where that read ended.
///
/// The notify path and the poll path can race on the same file (both read
/// from the same start offset, both ingest the same bytes, both return the
/// same end offset).  INSERT OR IGNORE deduplicates in the DB; this max()
/// ensures the stored offset is the high-water mark regardless of ordering.
#[inline]
fn advance_offset(offsets: &Offsets, path: PathBuf, end: u64) {
    let rewound = trackers()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(path.to_string_lossy().as_ref())
        .is_some_and(|t| std::mem::take(&mut t.rewound));
    let mut map = offsets.lock().unwrap();
    let prev = *map.get(&path).unwrap_or(&0);
    map.insert(path, if rewound { end } else { end.max(prev) });
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
///   - A transcript that is gone when it is opened has nothing to ingest:
///     `start` is returned and no state is touched.  Every other file-open /
///     seek / read error is returned to the caller, who logs and skips.  This
///     keeps error handling at the outermost loop boundary.
async fn ingest_from(
    pool: &SqlitePool,
    path: &Path,
    start: u64,
    tx: &broadcast::Sender<TurnMsg>,
    broadcast_live: bool,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        // Deleted between being noticed and being opened: nothing left to ingest.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(start),
        Err(e) => return Err(e.into()),
    };
    let fname = path.to_string_lossy().to_string();
    let len = file.metadata()?.len();
    // Rewritten in place since the last read: shorter than the offset, or holding other bytes
    // before where its tracker stopped than the tracker read there. The offset then points into
    // lines no tracker saw, so the file is read again from its first line. Its bytes decide, not
    // whether it is the same file: one deleted and written again with the same lines and more is
    // the same transcript, tailed as if it never left.
    let was = start;
    let rewritten = start > 0
        && (len < start || {
            let last = trackers()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&fname)
                .map(|t| (t.end, t.tail.clone()));
            match last {
                Some((end, tail)) => len < end || !bytes_before(&mut file, end, &tail)?,
                None => false,
            }
        });
    let start = if rewritten {
        put_tracker(
            fname.clone(),
            FileState {
                rewound: true,
                ..Default::default()
            },
        );
        0
    } else {
        start
    };
    file.seek(SeekFrom::Start(start))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    let last_nl = match buf.rfind('\n') {
        Some(i) => i,
        None => return Ok(start), // no complete line yet
    };
    let complete = &buf[..=last_nl];
    // All three are properties of the path, so resolve once per file, not per line.
    let is_subagent = lumen_core::project::is_subagent_path(&fname);
    let agent = lumen_core::project::subagent_of(&fname);
    let project = lumen_core::project::label_for_transcript(&fname);
    let mut new = 0u64;
    // One tracker per transcript, kept across polls: a rewind is a line that branches back past
    // lines read earlier, and a blocked Read's result can arrive in a later read than its call. A
    // restart rebuilds it from offset 0, and the rows it re-derives are ignored by their primary
    // key.
    let (mut state, lost) = take_tracker(&fname, start);
    // A read that started before another one moved the offset on finds lines its tracker has
    // already read: they were ingested then, and are not fed to it twice.
    let seen = state.end.saturating_sub(start) as usize;
    let Some(complete) = complete.get(seen..).filter(|rest| !rest.is_empty()) else {
        let end = state.end;
        put_tracker(fname, state);
        return Ok(end);
    };
    // How far into the transcript the blocks are known, by each session's latest turn in it: only
    // on a read that has missed nothing since the file's first line.
    let mut through: Vec<(String, i64)> = Vec::new();
    if lost || rewritten {
        let session = transcript_session(complete, path);
        let ev = lumen_core::events::Event::resync(session, lumen_core::repeat::now_ms(), was);
        if let Err(e) = record_event(pool, &ev, &fname).await {
            logline!("! could not record a resync in {fname}: {e}");
        }
    }

    for line in complete.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(ev) = state.events.feed(line)
            && let Err(e) = record_event(pool, &ev, &fname).await
        {
            logline!("! could not record a {} in {fname}: {e}", ev.kind.as_str());
        }
        // Not `?`, for the reason the calibration row is not: the file's offset would stay put and
        // every poll would fail on the same block.
        for block in state.blocks.results(line) {
            if let Err(e) = record_block(pool, &block, agent, &fname).await {
                logline!("! could not record a blocked Read in {fname}: {e}");
            }
        }
        let rec: Record = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // The Reads it asks for, from the parse every assistant line gets anyway: a result in a
        // later line is matched to them.
        for (id, file_path) in rec.message.read_calls() {
            state.blocks.read_called(id, file_path, rec.cwd.as_deref());
        }
        if !rec.is_billable() {
            continue;
        }
        // Not unwrapped. `is_billable` is what guarantees the usage, and it lives in
        // lumen-core: an edit there would otherwise panic the daemon here.
        let Some(u) = rec.message.usage.as_ref() else {
            continue;
        };
        if state.whole
            && let Some(t) = lumen_core::events::parse_ts_ms(&rec.timestamp)
        {
            match through.iter_mut().find(|(s, _)| *s == rec.session_id) {
                Some((_, at)) => *at = (*at).max(t),
                None => through.push((rec.session_id.clone(), t)),
            }
        }

        // The cache write's split by duration, where the transcript recorded one. A 1-hour
        // write costs 1.6 times a 5-minute one, so pricing needs it. NULL, not 0, when it was
        // not recorded: the two columns have been in the schema from the start, unfilled
        // until 1.9.0, and an absent split is not a split of nothing.
        let (cache_5m, cache_1h) = u.cache_creation.as_ref().map_or((None, None), |c| {
            (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
        });

        let res = sqlx::query(
            "INSERT OR IGNORE INTO turns
             (message_id, session_id, ts, model,
              input_tokens, output_tokens,
              cache_read_input_tokens, cache_creation_input_tokens,
              cache_5m, cache_1h, source_file,
              is_subagent, speed, inference_geo)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(&rec.message.id)
        .bind(&rec.session_id)
        .bind(&rec.timestamp)
        .bind(&rec.message.model)
        .bind(u.input_tokens)
        .bind(u.output_tokens)
        .bind(u.cache_read_input_tokens)
        .bind(u.cache_creation_input_tokens)
        .bind(cache_5m)
        .bind(cache_1h)
        .bind(&fname)
        .bind(is_subagent as i64)
        .bind(&u.speed)
        .bind(&u.inference_geo)
        .execute(pool)
        .await?;

        if res.rows_affected() > 0 {
            new += 1;
            // Per-turn only for live turns, not for the initial import.
            //
            // `broadcast_live` is false exactly during the startup backfill, which on a fresh
            // database replays every transcript on the machine: measured at 16,252 lines and 876 KB
            // in 60 seconds here. The GUI forwards this stderr into a 256 KB log with KeepOne, so a
            // first run rotated the file three times over and destroyed the startup and tray
            // diagnostics — on precisely the launch where a new user is most likely to hit a
            // problem and be asked for a log.
            //
            // A live turn arrives every few seconds at most, and seeing them is the point of the
            // tool, so those stay. `LUMEN_LOG=debug` restores the per-turn line during import for
            // anyone debugging ingestion.
            if broadcast_live || ws_retry_verbose() {
                logline!(
                    "+ turn {} ({} in / {} out)",
                    rec.message.id,
                    u.input_tokens,
                    u.output_tokens
                );
            }
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
                    cache_creation_1h_input_tokens: cache_1h,
                    speed: u.speed.clone(),
                    inference_geo: u.inference_geo.clone(),
                    is_subagent,
                    project: project.clone(),
                    rowid: res.last_insert_rowid(),
                });
            }
            // Calibration: real vs estimated output tokens.
            //
            // Not `?`. The turn above is already in the ledger, and failing the file here
            // would leave its offset where it was, so every poll would re-read it and fail on
            // the same row: one bad calibration insert would stop ingest for that transcript.
            let est = lumen_core::tokenizer::count_tokens(&rec.message.text_output()) as i64;
            if est > 0
                && let Err(e) = sqlx::query(
                    "INSERT OR IGNORE INTO calibration (message_id, real_output, est_output)
                     VALUES (?,?,?)",
                )
                .bind(&rec.message.id)
                .bind(u.output_tokens)
                .bind(est)
                .execute(pool)
                .await
            {
                calibration_lost(&e);
            }
        }
    }
    state.end = start + (last_nl as u64) + 1;
    let read = &buf.as_bytes()[..=last_nl];
    state.tail = read[read.len().saturating_sub(TAIL_BYTES)..].to_vec();
    state.rewound = rewritten;
    put_tracker(fname.clone(), state);
    for (session, at) in &through {
        if let Err(e) = record_scan(pool, &fname, session, agent, *at).await {
            logline!("! could not record {fname} as read for blocks: {e}");
        }
    }
    if new > 0 {
        logline!("  ({new} new from {fname})");
    }

    Ok(start + (last_nl as u64) + 1)
}

/// What the daemon keeps of a transcript between reads of it.
#[derive(Default)]
struct FileState {
    /// Compactions and rewinds.
    events: lumen_core::events::Tracker,
    /// The Reads asked for whose result has not been read yet.
    blocks: lumen_core::intercept::Scanner,
    /// Read from its first line with nothing lost since, so a blocked Read it does not record
    /// did not happen in it (`transcript_scans`). A tail read that lost its state is not, until
    /// the file is read from its first line again.
    whole: bool,
    /// The byte offset it has read the file to: the end of the last line fed to it.
    end: u64,
    /// The last bytes it read before `end`: what a file rewritten in place differs in.
    tail: Vec<u8>,
    /// Read again from its first line because it was rewritten: its offset goes back, once
    /// ([`advance_offset`]).
    rewound: bool,
}

/// How many bytes before its offset a transcript is checked against on the next read.
const TAIL_BYTES: usize = 64;

/// Whether the bytes of `file` just before `at` are `tail`.
fn bytes_before(file: &mut std::fs::File, at: u64, tail: &[u8]) -> std::io::Result<bool> {
    let n = tail.len() as u64;
    if n > at {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(at - n))?;
    let mut got = vec![0u8; tail.len()];
    file.read_exact(&mut got)?;
    Ok(got == tail)
}

fn trackers() -> &'static std::sync::Mutex<std::collections::HashMap<String, FileState>> {
    static T: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, FileState>>> =
        std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}

/// The tracker for `file`, and whether it was lost. From byte 0 a fresh one is the true
/// starting state, and it sees the whole file. A tail read that finds none has missed whatever
/// came before `start`: a rewind in the lines it is about to read has no parent to branch from,
/// and a blocked Read no call to match.
///
/// So has one whose tracker stops short of `start`. The notify and poll paths can read one file
/// at once: the second finds no tracker, reads on, and the first puts its tracker back after the
/// offset has moved past what that tracker saw. Going on with it would read on as if nothing were
/// missed, and record the file as read for blocks through lines it never saw.
fn take_tracker(file: &str, start: u64) -> (FileState, bool) {
    if start == 0 {
        return (
            FileState {
                whole: true,
                ..Default::default()
            },
            false,
        );
    }
    let mut g = trackers().lock().unwrap_or_else(|p| p.into_inner());
    match g.remove(file) {
        Some(t) if t.end >= start => (t, false),
        Some(_) | None => (Default::default(), true),
    }
}

/// The session a transcript's lines belong to: the first `sessionId` among them, else the
/// file's name, which is the session id for a Claude Code transcript.
fn transcript_session(lines: &str, path: &Path) -> String {
    lines
        .lines()
        .find_map(|l| {
            serde_json::from_str::<serde_json::Value>(l)
                .ok()?
                .get("sessionId")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned()
        })
}

fn put_tracker(file: String, t: FileState) {
    let mut g = trackers().lock().unwrap_or_else(|p| p.into_inner());
    g.insert(file, t);
}

/// What the ledger held when this daemon started: the blocks recorded, by their call's id, and how
/// far each transcript had been read for them by this matcher. The startup pass reads every
/// transcript again, and a row it would write as it already is is left alone: a statement per
/// transcript and per block, on every start, for nothing. Set once by `main`, before that pass,
/// and never changed: anywhere else (the tests) it is unset and every row is written, which is
/// what a row missing from it costs too.
#[derive(Default)]
struct Known {
    blocks: std::collections::HashSet<String>,
    scans: HashMap<(String, String), i64>,
}

static KNOWN: std::sync::OnceLock<Known> = std::sync::OnceLock::new();

/// [`Known`], from the ledger. What cannot be read is not known, and is written again.
async fn load_known(pool: &SqlitePool) -> Known {
    let mut known = Known::default();
    match sqlx::query_scalar::<_, String>("SELECT tool_use_id FROM intercept_blocks")
        .fetch_all(pool)
        .await
    {
        Ok(ids) => known.blocks.extend(ids),
        Err(e) => logline!("! could not read the blocks already recorded: {e}"),
    }
    match sqlx::query_as::<_, (String, String, i64)>(
        "SELECT source_file, session_id, through_ms FROM transcript_scans WHERE matcher = ?",
    )
    .bind(lumen_core::intercept::MATCHER)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => known
            .scans
            .extend(rows.into_iter().map(|(f, s, t)| ((f, s), t))),
        Err(e) => logline!("! could not read which transcripts were read for blocks: {e}"),
    }
    known
}

/// One row per blocked Read, keyed by its call's id: a replay of the file records it once.
async fn record_block(
    pool: &SqlitePool,
    b: &lumen_core::intercept::RecordedBlock,
    agent: Option<&str>,
    file: &str,
) -> Result<(), sqlx::Error> {
    if KNOWN
        .get()
        .is_some_and(|k| k.blocks.contains(&b.tool_use_id))
    {
        return Ok(());
    }
    sqlx::query(
        "INSERT OR IGNORE INTO intercept_blocks \
         (tool_use_id, session_id, agent_id, path, lines, kind, ts_ms, uuid, source_file) \
         VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(&b.tool_use_id)
    .bind(&b.session_id)
    .bind(agent)
    .bind(&b.path)
    .bind(b.lines)
    .bind(b.kind.as_str())
    .bind(b.ts_ms)
    .bind(&b.uuid)
    .bind(file)
    .execute(pool)
    .await?;
    Ok(())
}

/// `file` has been read for blocks from its first line through its turn at `through_ms`: the
/// latest such, by the matcher that read it. A row that already says as much is left alone, so
/// the startup pass, which reads every transcript again, writes nothing where nothing moved.
async fn record_scan(
    pool: &SqlitePool,
    file: &str,
    session: &str,
    agent: Option<&str>,
    through_ms: i64,
) -> Result<(), sqlx::Error> {
    if KNOWN.get().is_some_and(|k| {
        k.scans
            .get(&(file.to_string(), session.to_string()))
            .is_some_and(|&t| t >= through_ms)
    }) {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO transcript_scans (source_file, session_id, agent_id, matcher, through_ms) \
         VALUES (?,?,?,?,?) \
         ON CONFLICT (source_file, session_id) DO UPDATE SET agent_id = excluded.agent_id, \
           matcher = excluded.matcher, through_ms = MAX(through_ms, excluded.through_ms) \
         WHERE excluded.through_ms > through_ms OR excluded.matcher <> matcher",
    )
    .bind(file)
    .bind(session)
    .bind(agent)
    .bind(lumen_core::intercept::MATCHER)
    .bind(through_ms)
    .execute(pool)
    .await?;
    Ok(())
}

/// One row per compaction, rewind or resync. An event with no usable timestamp is stamped now, which
/// is the safe direction: it invalidates every earlier read rather than none.
async fn record_event(
    pool: &SqlitePool,
    ev: &lumen_core::events::Event,
    file: &str,
) -> Result<(), sqlx::Error> {
    let ts = ev.ts_ms.unwrap_or_else(lumen_core::repeat::now_ms);
    sqlx::query(
        "INSERT OR IGNORE INTO session_events (session_id, kind, uuid, ts_ms, source_file)
         VALUES (?,?,?,?,?)",
    )
    .bind(&ev.session_id)
    .bind(ev.kind.as_str())
    .bind(&ev.uuid)
    .bind(ts)
    .bind(file)
    .execute(pool)
    .await?;
    Ok(())
}

/// Set once a calibration row has been refused and logged.
static CALIBRATION_LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Log a calibration row the ledger refused, the first time only.
///
/// These rows were dropped with `let _ =` until 1.6.0, so a broken `calibration` table
/// cost every row with no trace. Logged rather than filed as a fault: the rows feed only
/// `correction_factor`, which nothing in the app or the CLI displays, so no figure the user
/// sees is wrong because of them. Once per process because this runs per turn, and the
/// startup backfill replays every transcript on the machine — the per-turn line that
/// rotated the log away (see `broadcast_live` above).
fn calibration_lost(e: &sqlx::Error) {
    if !CALIBRATION_LOST.swap(true, std::sync::atomic::Ordering::Relaxed) {
        logline!("calibration row not written: {e} (logged once; later failures are not)");
    }
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
    // Deliberately silent. This used to log "ws bind failed" here while the restart loop logged
    // "ws_server exited" for the same event — two lines per failed cycle, and the throttle state
    // could only live in the loop. The loop now formats the single message, and the error carries
    // enough to identify a bind failure.
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("could not bind {addr} ({e}); is another daemon running?"))?;
    logline!("WebSocket server listening on ws://{addr}");
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
                logline!("ws accept error: {e}");
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

            // 1. Snapshot on connect, read in one transaction so its parts agree with each
            //    other and with the live turns that follow.
            //
            // The subscription above was taken before the read, so a turn ingested in between
            // is both in the snapshot and waiting in `rx`. Sending it again would count it
            // twice, so a live turn the snapshot already holds, by rowid, is held back.
            let mut sent_through = None;
            if let Ok(snapshot) = read_snapshot(&pool).await {
                sent_through = snapshot.last_rowid;
                let _ = sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(
                        snapshot.json.to_string().into(),
                    ))
                    .await;
            }

            // 2. Live turns.
            while let Ok(turn) = rx.recv().await {
                if sent_through.is_some_and(|last| turn.rowid <= last) {
                    continue;
                }
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

/// The snapshot a client gets on connect, and the newest turn it counts.
struct Snapshot {
    json: serde_json::Value,
    /// The highest rowid in the ledger when it was read; None for an empty ledger. Rowids
    /// only grow, so every turn at or below it is in the snapshot and every later one is not.
    last_rowid: Option<i64>,
}

/// Read the snapshot in one transaction.
///
/// Each session's sums come with their split by model, speed and region (`by_model`), which is
/// what a client prices: a session can mix models, a subagent on a cheaper one. The session
/// totals are the split added up, not a second query, so a turn that lands while the snapshot is
/// read can never be in one and not the other.
async fn read_snapshot(pool: &SqlitePool) -> Result<Snapshot, sqlx::Error> {
    let mut tx = pool.begin().await?;

    // fill = latest turn's cache_read (not session MAX) so the gauge reflects the real current
    // context fill, including drops after /compact.
    #[allow(clippy::type_complexity)]
    let sessions = sqlx::query_as::<
        _,
        (
            String,
            Option<i64>,
            i64,
            String,
            Option<String>,
            Option<String>,
        ),
    >(
        // fill / peak_fill deliberately exclude subagents: their transcripts reuse the
        // parent's sessionId but carry a separate, fresh context, so counting them made the
        // gauge dip whenever a subagent ran. Cost covers every turn, subagents included, below.
        "SELECT t.session_id,
                (SELECT cache_read_input_tokens FROM turns
                  WHERE session_id = t.session_id AND is_subagent = 0
                  ORDER BY ts DESC LIMIT 1),
                -- Peak fill: the window is derived from the session's high-water mark,
                -- never the momentary fill, so /compact cannot shrink the reported window
                -- mid-session.
                (SELECT COALESCE(MAX(cache_read_input_tokens),0) FROM turns
                  WHERE session_id = t.session_id AND is_subagent = 0),
                MAX(t.ts),
                (SELECT t2.model FROM turns t2
                  WHERE t2.session_id = t.session_id AND t2.model IS NOT NULL
                  ORDER BY t2.ts DESC LIMIT 1),
                -- Project identity: the transcript records no cwd, so the encoded project
                -- directory is all we have.
                (SELECT t3.source_file FROM turns t3
                  WHERE t3.session_id = t.session_id AND t3.source_file IS NOT NULL
                  ORDER BY t3.ts DESC LIMIT 1)
         FROM turns t GROUP BY t.session_id",
    )
    .fetch_all(&mut *tx)
    .await?;

    #[allow(clippy::type_complexity)]
    let parts = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ),
    >(
        "SELECT session_id, model, speed, inference_geo,
                SUM(input_tokens), SUM(output_tokens),
                SUM(cache_read_input_tokens), SUM(cache_creation_input_tokens),
                COALESCE(SUM(cache_1h),0),
                -- A turn that recorded no split has a write of unknown duration, priced at
                -- the 5-minute rate as a floor.
                SUM(CASE WHEN cache_1h IS NULL THEN cache_creation_input_tokens ELSE 0 END)
         FROM turns GROUP BY session_id, model, speed, inference_geo
         ORDER BY session_id, model, speed, inference_geo",
    )
    .fetch_all(&mut *tx)
    .await?;

    let (last_rowid,): (Option<i64>,) = sqlx::query_as("SELECT MAX(rowid) FROM turns")
        .fetch_one(&mut *tx)
        .await?;
    tx.rollback().await?;

    let mut by_session: HashMap<String, (lumen_core::rates::Tokens, Vec<serde_json::Value>)> =
        HashMap::new();
    for (session, model, speed, geo, i, o, cr, cw, cw_1h, cw_unknown) in parts {
        let entry = by_session.entry(session).or_default();
        entry.0.add(&lumen_core::rates::Tokens {
            input: i,
            output: o,
            cache_read: cr,
            cache_write: cw,
            cache_write_1h: cw_1h,
            cache_write_unknown: cw_unknown,
        });
        entry.1.push(serde_json::json!({
            "model": model, "speed": speed, "inference_geo": geo,
            "input": i, "output": o,
            "cache_read": cr, "cache_write": cw,
            "cache_write_1h": cw_1h, "cache_write_unknown": cw_unknown
        }));
    }

    let json = serde_json::json!({
        "type": "snapshot",
        "sessions": sessions.iter().map(|(s, fill, peak, ts, model, src)| {
            let (totals, by_model) = by_session.remove(s).unwrap_or_default();
            // A session made up entirely of subagent turns has no main-agent fill to
            // report; 0 is the honest answer.
            let project = src
                .as_deref()
                .and_then(lumen_core::project::label_for_transcript);
            serde_json::json!({
                "session_id": s,
                "input": totals.input, "output": totals.output,
                "cache_read": totals.cache_read, "cache_write": totals.cache_write,
                "by_model": by_model,
                "fill": fill.unwrap_or(0), "peak_fill": peak, "ts": ts,
                "model": model, "project": project
            })
        }).collect::<Vec<_>>()
    });
    Ok(Snapshot { json, last_rowid })
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

    // ── WS restart backoff and log suppression (issue #7) ────────────────────
    //
    // The volume, not the correctness, was the defect: 434 KB/hour against the GUI's 256 KB log cap
    // rotated the tray diagnostics away in ~35 minutes. These pin the two decisions that bound it.

    #[test]
    fn the_retry_delay_backs_off_and_then_stops_growing() {
        let secs = |a: u32| ws_retry_delay(a).as_secs();
        assert_eq!(secs(1), 2, "the first retry stays prompt");
        assert_eq!(secs(2), 4);
        assert_eq!(secs(3), 8);
        assert_eq!(secs(4), 16);
        // Capped: a held port does not free up faster for being asked more often, but recovery
        // must still be bounded.
        assert_eq!(secs(5), 30);
        assert_eq!(secs(6), 30);
        assert_eq!(secs(1000), 30);
    }

    #[test]
    fn the_retry_delay_never_overflows_or_reaches_zero() {
        // `1 << attempt` on a large attempt count is the obvious way to write this and the obvious
        // way to panic in release-mode arithmetic. A zero delay would spin the loop.
        for a in [0, 1, 31, 32, 33, 64, 1_000_000, u32::MAX] {
            let s = ws_retry_delay(a).as_secs();
            assert!(
                (WS_RETRY_BASE_SECS..=WS_RETRY_CAP_SECS).contains(&s),
                "attempt {a} gave {s}s, outside {WS_RETRY_BASE_SECS}..={WS_RETRY_CAP_SECS}"
            );
        }
    }

    #[test]
    fn the_first_few_failures_are_always_reported() {
        // Silence on the first failure would be worse than the noise: this is the line that says
        // the daemon cannot serve.
        for a in 1..=3 {
            assert!(should_log_ws_retry(a, false, false), "attempt {a}");
        }
    }

    #[test]
    fn an_unchanged_failure_is_collapsed_to_one_in_ten() {
        for a in 4..=9 {
            assert!(
                !should_log_ws_retry(a, false, false),
                "attempt {a} should be quiet"
            );
        }
        assert!(should_log_ws_retry(10, false, false));
        assert!(should_log_ws_retry(20, false, false));
        assert!(!should_log_ws_retry(11, false, false));
    }

    #[test]
    fn a_changed_failure_is_always_reported_however_deep_the_run() {
        // A new error is new information, and suppressing it would hide a bind failure turning
        // into something else entirely.
        assert!(should_log_ws_retry(7, true, false));
        assert!(should_log_ws_retry(9_999, true, false));
    }

    #[test]
    fn verbose_reports_every_attempt() {
        for a in [4, 5, 7, 11, 99] {
            assert!(
                should_log_ws_retry(a, false, true),
                "attempt {a} under LUMEN_LOG=debug"
            );
        }
    }

    #[test]
    fn the_suppression_and_backoff_together_bound_the_hourly_volume() {
        // The claim the issue is about, computed rather than asserted by hand: one hour of an
        // unchanged failure must produce a small number of lines, against the ~1,800 cycles and
        // 434 KB/hour measured before the fix.
        let mut elapsed = 0u64;
        let mut attempt = 0u32;
        let mut logged = 0u32;
        while elapsed < 3600 {
            attempt += 1;
            if should_log_ws_retry(attempt, attempt == 1, false) {
                logged += 1;
            }
            elapsed += ws_retry_delay(attempt).as_secs();
        }
        assert!(
            logged <= 20,
            "an hour of one unchanged failure logged {logged} lines; that is not bounded"
        );
        // And it must not be silent, or a persistent failure leaves no trace at all.
        assert!(logged >= 3, "only {logged} lines in an hour is too quiet");
    }

    #[test]
    fn verbose_is_read_from_the_same_variable_the_gui_uses() {
        // Not a new env var: LUMEN_LOG is what a user already sets for the app's own level.
        // Serialised implicitly by being the only test that touches it.
        let prev = std::env::var("LUMEN_LOG").ok();
        for (v, want) in [
            ("debug", true),
            ("DEBUG", true),
            (" trace ", true),
            ("warn", false),
            ("", false),
            ("nonsense", false),
        ] {
            unsafe { std::env::set_var("LUMEN_LOG", v) };
            assert_eq!(ws_retry_verbose(), want, "for LUMEN_LOG={v:?}");
        }
        match prev {
            Some(p) => unsafe { std::env::set_var("LUMEN_LOG", p) },
            None => unsafe { std::env::remove_var("LUMEN_LOG") },
        }
    }
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::io::Write;
    use tempfile::TempDir;

    const BILLABLE_LINE: &str = r#"{"sessionId":"s1","timestamp":"2025-01-01T00:00:00Z","message":{"id":"__ID__","model":"claude-sonnet-4-6","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;

    async fn make_pool(dir: &TempDir) -> SqlitePool {
        let db = dir.path().join("test.db");
        let url = format!("sqlite:{}?mode=rwc", db.display());
        let pool = SqlitePoolOptions::new().connect(&url).await.unwrap();
        lumen_core::schema::init_schema(&pool).await.unwrap();
        pool
    }

    fn write_jsonl(path: &Path, lines: &[&str]) {
        let mut f = std::fs::File::create(path).unwrap();
        for line in lines {
            writeln!(f, "{}", line).unwrap();
        }
    }

    // ── resolve_projects_dir ───────────────────────────────────────────────────

    #[test]
    fn the_watch_root_defaults_to_dot_claude_projects_under_home() {
        assert_eq!(
            resolve_projects_dir(None, Some(Path::new("/Users/me"))),
            Some(PathBuf::from("/Users/me/.claude/projects"))
        );
    }

    #[test]
    fn an_override_wins_over_the_home_derived_path() {
        // This is what makes the e2e tests hermetic on Windows, where
        // dirs::home_dir() ignores HOME and resolves %USERPROFILE% instead.
        assert_eq!(
            resolve_projects_dir(Some("/tmp/fixture/projects"), Some(Path::new("/Users/me"))),
            Some(PathBuf::from("/tmp/fixture/projects"))
        );
    }

    #[test]
    fn a_blank_override_falls_back_rather_than_watching_an_empty_path() {
        // An exported-but-empty variable is a common shell accident; watching ""
        // would silently ingest nothing.
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_projects_dir(Some(blank), Some(Path::new("/Users/me"))),
                Some(PathBuf::from("/Users/me/.claude/projects")),
                "blank override {blank:?} should fall back"
            );
        }
    }

    #[test]
    fn no_home_and_no_override_resolves_to_nothing_instead_of_panicking() {
        // The old code called dirs::home_dir().unwrap() here.
        assert_eq!(resolve_projects_dir(None, None), None);
    }

    #[test]
    fn an_override_still_works_with_no_resolvable_home() {
        assert_eq!(
            resolve_projects_dir(Some("/tmp/p"), None),
            Some(PathBuf::from("/tmp/p"))
        );
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

    // ── Compaction and rewind are persisted, once each, across polls and restarts ──

    #[tokio::test]
    async fn a_compaction_and_a_rewind_are_each_recorded_once() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        let rec = |ty: &str, u: &str, p: &str, extra: &str| {
            let p = if p.is_empty() {
                "null".to_string()
            } else {
                format!("\"{p}\"")
            };
            format!(
                r#"{{"type":"{ty}","uuid":"{u}","parentUuid":{p},"sessionId":"sess","timestamp":"2026-06-01T16:14:05.157Z"{extra}}}"#
            )
        };
        let prompt = |u: &str, p: &str| rec("user", u, p, r#","message":{"content":"hi"}"#);
        let asst = |u: &str, p: &str| rec("assistant", u, p, r#","message":{"content":[]}"#);
        let first = [
            prompt("u1", ""),
            asst("a1", "u1"),
            prompt("u2", "a1"),
            asst("a2", "u2"),
            rec("system", "c1", "a2", r#","subtype":"compact_boundary""#),
        ];
        let second = [prompt("u3", "a1")]; // branches back past u2: a rewind
        write_jsonl(
            &jsonl,
            &first.iter().map(String::as_str).collect::<Vec<_>>(),
        );

        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&jsonl)
                .unwrap();
            writeln!(f, "{}", second[0]).unwrap();
        }
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        // A restart replays the file from zero: the same two rows, not four.
        ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();

        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT session_id, kind, uuid, ts_ms FROM session_events ORDER BY kind",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "sess".into(),
                    "compaction".into(),
                    "c1".into(),
                    1_780_330_445_157
                ),
                (
                    "sess".into(),
                    "rewind".into(),
                    "u3".into(),
                    1_780_330_445_157
                ),
            ]
        );
    }

    /// A subagent's transcript (`<session>/subagents/agent-<id>.jsonl`, every record a
    /// sidechain) records its compaction, under its own kind and its own file, so the savings
    /// summary can end that subagent's rounds there. Until 1.9.0 sidechains were skipped whole.
    #[tokio::test]
    async fn a_subagent_transcripts_compaction_is_recorded_under_its_own_kind_and_file() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let subdir = dir.path().join("sess").join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let jsonl = subdir.join("agent-a0223eddc41f13056.jsonl");
        let side = |ty: &str, u: &str, p: &str, extra: &str| {
            let p = if p.is_empty() {
                "null".to_string()
            } else {
                format!("\"{p}\"")
            };
            format!(
                r#"{{"type":"{ty}","uuid":"{u}","parentUuid":{p},"isSidechain":true,"agentId":"a0223eddc41f13056","sessionId":"sess","timestamp":"2026-06-01T16:14:05.157Z"{extra}}}"#
            )
        };
        let lines = [
            side("user", "s1", "", r#","message":{"content":"task"}"#),
            side("assistant", "s2", "s1", r#","message":{"content":[]}"#),
            side("system", "sc", "s2", r#","subtype":"compact_boundary""#),
        ];
        write_jsonl(
            &jsonl,
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let (tx, _rx) = broadcast::channel(4);
        ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT session_id, kind, source_file FROM session_events")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![(
                "sess".to_string(),
                "subagent_compaction".to_string(),
                jsonl.to_string_lossy().to_string()
            )]
        );
    }

    // ── A transcript tailed without its tracker is resynced, not trusted ───────

    fn tree_line(ty: &str, u: &str, p: &str, extra: &str) -> String {
        let p = if p.is_empty() {
            "null".to_string()
        } else {
            format!("\"{p}\"")
        };
        format!(
            r#"{{"type":"{ty}","uuid":"{u}","parentUuid":{p},"sessionId":"sess","timestamp":"2026-06-01T16:14:05.157Z"{extra}}}"#
        )
    }
    fn tree_prompt(u: &str, p: &str) -> String {
        tree_line("user", u, p, r#","message":{"content":"hi"}"#)
    }
    fn tree_asst(u: &str, p: &str) -> String {
        tree_line("assistant", u, p, r#","message":{"content":[]}"#)
    }
    fn append_line(path: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    }
    fn lose_tracker(path: &Path) {
        trackers()
            .lock()
            .unwrap()
            .remove(&path.to_string_lossy().to_string());
    }
    async fn event_kinds(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT kind FROM session_events ORDER BY kind")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_rewind_after_the_tracker_was_lost_still_reaches_the_gate() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(
            &jsonl,
            &[
                &tree_prompt("u1", ""),
                &tree_asst("a1", "u1"),
                &tree_prompt("u2", "a1"),
                &tree_asst("a2", "u2"),
            ],
        );
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        // The model read a file here; the gate asks about everything after this instant.
        let read_at = lumen_core::repeat::now_ms();
        std::thread::sleep(std::time::Duration::from_millis(5));

        lose_tracker(&jsonl);
        // Esc-Esc back past u2: the tracker that could have seen it is gone.
        append_line(&jsonl, &tree_prompt("u3", "a1"));
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();

        let seen =
            lumen_core::repeat::event_since(&dir.path().join("test.db"), "sess", read_at).unwrap();
        assert_eq!(seen.as_deref(), Some("resync"));
    }

    #[tokio::test]
    async fn a_tail_read_that_lost_its_tracker_records_one_resync_stamped_now() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(&jsonl, &[&tree_prompt("u1", ""), &tree_asst("a1", "u1")]);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        lose_tracker(&jsonl);

        let before = lumen_core::repeat::now_ms();
        append_line(&jsonl, &tree_prompt("u2", "a1"));
        let off = ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        let after = lumen_core::repeat::now_ms();

        let rows: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT session_id, kind, ts_ms FROM session_events")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), ("sess", "resync"));
        assert!((before..=after).contains(&rows[0].2), "{rows:?}");

        // The tracker is back: the next tail read is trusted and adds nothing.
        append_line(&jsonl, &tree_asst("a2", "u2"));
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        assert_eq!(event_kinds(&pool).await, ["resync"]);
    }

    #[tokio::test]
    async fn losing_the_tracker_twice_is_two_resyncs() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(&jsonl, &[&tree_prompt("u1", ""), &tree_asst("a1", "u1")]);
        let (tx, _rx) = broadcast::channel(4);
        let mut off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        for (u, p) in [("u2", "a1"), ("u3", "u2")] {
            lose_tracker(&jsonl);
            append_line(&jsonl, &tree_prompt(u, p));
            off = ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(event_kinds(&pool).await, ["resync", "resync"]);
    }

    #[tokio::test]
    async fn a_new_transcript_and_a_replay_from_zero_record_no_resync() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(&jsonl, &[&tree_prompt("u1", ""), &tree_asst("a1", "u1")]);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        append_line(&jsonl, &tree_prompt("u2", "a1"));
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        // A restart's initial pass starts every transcript at zero, with or without a tracker.
        ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        lose_tracker(&jsonl);
        ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        assert_eq!(event_kinds(&pool).await, Vec::<String>::new());
    }

    // ── Blocked Reads, as the transcripts record them ──────────────────────────

    /// 2026-10-13T09:00:00Z.
    const T0: i64 = 1_791_882_000_000;

    fn at_secs(secs: i64) -> String {
        lumen_core::savings::iso_ms(T0 + secs * 1000)
    }

    /// An assistant record that calls `tool` on `input`, with usage: a turn of its own.
    fn tool_call(id: &str, secs: i64, side: bool, tool: &str, input: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant", "uuid": format!("a-{id}"), "isSidechain": side,
            "sessionId": "sess", "cwd": "/r", "timestamp": at_secs(secs),
            "message": {
                "id": format!("msg-{id}"), "model": "claude-sonnet-4-6", "role": "assistant",
                "content": [{"type": "tool_use", "id": id, "name": tool, "input": input}],
                "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100,
                          "cache_creation_input_tokens": 0}
            }
        })
        .to_string()
    }

    fn read_call(id: &str, secs: i64, side: bool, path: &str) -> String {
        tool_call(
            id,
            secs,
            side,
            "Read",
            serde_json::json!({"file_path": path}),
        )
    }

    /// The user record holding `id`'s result.
    fn tool_result(
        id: &str,
        secs: i64,
        side: bool,
        content: serde_json::Value,
        is_error: bool,
    ) -> String {
        serde_json::json!({
            "type": "user", "uuid": format!("r-{id}"), "isSidechain": side,
            "sessionId": "sess", "cwd": "/r", "timestamp": at_secs(secs),
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}
            ]}
        })
        .to_string()
    }

    /// What Claude Code hands the model when the intercept blocks `path`: its own words, then the
    /// hook's stderr, with `nl` for a newline.
    fn block_text(path: &str, nl: &str) -> String {
        format!(
            "PreToolUse:Read hook error: [lumen-mcp hook intercept]: Lumen intercept: {path} is \
             400 lines.{nl}Instead of reading the full file, call:{nl}  1. lumen:smart_read(path=\
             \"{path}\"){nl}"
        )
    }

    type BlockRow = (String, String, Option<String>, String, String, i64, String);

    async fn block_rows(pool: &SqlitePool) -> Vec<BlockRow> {
        sqlx::query_as(
            "SELECT tool_use_id, session_id, agent_id, path, kind, ts_ms, uuid \
             FROM intercept_blocks ORDER BY ts_ms, tool_use_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// A main transcript and a subagent's: a block in each, as a string and as parts, one with
    /// CRLF; a Read retried after its block and let through; and three errors that are no block.
    /// One row per block, with whose transcript it was; the same rows after a tail read and a
    /// restart.
    #[tokio::test]
    async fn a_blocked_read_in_a_main_or_subagent_transcript_is_recorded_once() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let main = dir.path().join("sess.jsonl");
        write_jsonl(
            &main,
            &[
                &read_call("t1", 0, false, "/r/big.rs"),
                &tool_result("t1", 1, false, block_text("/r/big.rs", "\r\n").into(), true),
                // The retry, let through by the escape valve.
                &read_call("t2", 5, false, "/r/big.rs"),
                &tool_result("t2", 6, false, "     1\tfn main() {}".into(), false),
                // A Read that failed for another reason.
                &read_call("t3", 8, false, "/r/gone.rs"),
                &tool_result("t3", 9, false, "File does not exist.".into(), true),
                // A failing command whose output quotes a block: not a Read.
                &tool_call(
                    "b1",
                    10,
                    false,
                    "Bash",
                    serde_json::json!({"command": "cargo test"}),
                ),
                &tool_result("b1", 11, false, block_text("/r/big.rs", "\n").into(), true),
                // A Read that went through, of a file that holds a block message.
                &read_call("t4", 12, false, "/r/big.rs"),
                &tool_result("t4", 13, false, block_text("/r/big.rs", "\n").into(), false),
            ],
        );
        let subdir = dir.path().join("sess").join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let sub = subdir.join("agent-a77.jsonl");
        write_jsonl(
            &sub,
            &[
                &read_call("s1", 20, true, "/r/big.rs"),
                &tool_result(
                    "s1",
                    21,
                    true,
                    serde_json::json!([{"type": "text", "text": block_text("/r/big.rs", "\n")}]),
                    true,
                ),
            ],
        );

        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        ingest_from(&pool, &sub, 0, &tx, false).await.unwrap();
        // A block that arrives in a later read of the file, its call in the earlier one.
        append_line(&main, &read_call("t5", 30, false, "/r/other.rs"));
        let off = ingest_from(&pool, &main, off, &tx, false).await.unwrap();
        append_line(
            &main,
            &tool_result(
                "t5",
                31,
                false,
                block_text("/r/other.rs", "\n").into(),
                true,
            ),
        );
        ingest_from(&pool, &main, off, &tx, false).await.unwrap();

        let ms = |secs: i64| T0 + secs * 1000;
        let want: Vec<BlockRow> = vec![
            (
                "t1".into(),
                "sess".into(),
                None,
                "/r/big.rs".into(),
                "redirect".into(),
                ms(1),
                "r-t1".into(),
            ),
            (
                "s1".into(),
                "sess".into(),
                Some("a77".into()),
                "/r/big.rs".into(),
                "redirect".into(),
                ms(21),
                "r-s1".into(),
            ),
            (
                "t5".into(),
                "sess".into(),
                None,
                "/r/other.rs".into(),
                "redirect".into(),
                ms(31),
                "r-t5".into(),
            ),
        ];
        assert_eq!(block_rows(&pool).await, want);

        // A restart replays both files from their first line: the same rows.
        lose_tracker(&main);
        lose_tracker(&sub);
        ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        ingest_from(&pool, &sub, 0, &tx, false).await.unwrap();
        assert_eq!(block_rows(&pool).await, want);
    }

    type ScanRow = (String, String, Option<String>, i64, i64);

    async fn scan_rows(pool: &SqlitePool) -> Vec<ScanRow> {
        sqlx::query_as(
            "SELECT source_file, session_id, agent_id, matcher, through_ms \
             FROM transcript_scans ORDER BY source_file",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// A transcript read from its first line is recorded as read for blocks, through its latest
    /// turn, and each later read moves that on. A tail read that lost its tracker has lost the
    /// Reads waiting for their result, so it moves nothing until the file is read from its first
    /// line again.
    #[tokio::test]
    async fn a_transcript_read_from_its_first_line_is_recorded_as_scanned_through_its_latest_turn()
    {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let main = dir.path().join("sess.jsonl");
        write_jsonl(
            &main,
            &[
                &read_call("t1", 0, false, "/r/a.rs"),
                &read_call("t2", 4, false, "/r/b.rs"),
            ],
        );
        let subdir = dir.path().join("sess").join("subagents");
        std::fs::create_dir_all(&subdir).unwrap();
        let sub = subdir.join("agent-a77.jsonl");
        write_jsonl(&sub, &[&read_call("s1", 2, true, "/r/a.rs")]);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        ingest_from(&pool, &sub, 0, &tx, false).await.unwrap();
        let file = |p: &Path| p.to_string_lossy().to_string();
        let matcher = lumen_core::intercept::MATCHER;
        assert_eq!(
            scan_rows(&pool).await,
            vec![
                (file(&main), "sess".into(), None, matcher, T0 + 4_000),
                (
                    file(&sub),
                    "sess".into(),
                    Some("a77".into()),
                    matcher,
                    T0 + 2_000
                ),
            ]
        );

        append_line(&main, &read_call("t3", 9, false, "/r/c.rs"));
        let off = ingest_from(&pool, &main, off, &tx, false).await.unwrap();
        assert_eq!(scan_rows(&pool).await[0].4, T0 + 9_000);

        lose_tracker(&main);
        append_line(&main, &read_call("t4", 15, false, "/r/d.rs"));
        let off = ingest_from(&pool, &main, off, &tx, false).await.unwrap();
        append_line(&main, &read_call("t5", 20, false, "/r/e.rs"));
        ingest_from(&pool, &main, off, &tx, false).await.unwrap();
        assert_eq!(
            scan_rows(&pool).await[0].4,
            T0 + 9_000,
            "a gap moves nothing"
        );

        ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        assert_eq!(scan_rows(&pool).await[0].4, T0 + 20_000);
    }

    /// The notify and poll paths read one transcript at once. The reader holding the tracker read
    /// fewer bytes and puts it back last, so the offset is past what that tracker saw: the next
    /// tail read has missed lines, and must not go on saying the file was read whole.
    #[tokio::test]
    async fn a_tracker_put_back_behind_the_offset_is_lost_not_trusted() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let main = dir.path().join("sess.jsonl");
        let fname = main.to_string_lossy().to_string();
        write_jsonl(
            &main,
            &[
                &read_call("t1", 0, false, "/r/a.rs"),
                &tool_result("t1", 1, false, block_text("/r/a.rs", "\n").into(), true),
                &read_call("t2", 2, false, "/r/x.rs"),
            ],
        );
        let (tx, _rx) = broadcast::channel(4);
        let s = ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        // R1 reads on to e1 with the tracker, and holds it.
        append_line(&main, &read_call("t3", 5, false, "/r/y.rs"));
        let e1 = ingest_from(&pool, &main, s, &tx, false).await.unwrap();
        let (held, lost) = take_tracker(&fname, e1);
        assert!(!lost);
        // R2 starts at the same offset and finds no tracker; the file has grown by the Read
        // that is about to be blocked.
        append_line(&main, &read_call("t4", 8, false, "/r/big.rs"));
        let e2 = ingest_from(&pool, &main, s, &tx, false).await.unwrap();
        // R1 puts its tracker back last; the offset is the larger end.
        put_tracker(fname.clone(), held);
        append_line(
            &main,
            &tool_result("t4", 9, false, block_text("/r/big.rs", "\n").into(), true),
        );
        append_line(
            &main,
            &tool_call(
                "t5",
                12,
                false,
                "mcp__lumen__smart_read",
                serde_json::json!({"path": "/r/big.rs"}),
            ),
        );
        ingest_from(&pool, &main, e2.max(e1), &tx, false)
            .await
            .unwrap();
        let through = scan_rows(&pool).await[0].4;
        let ids: Vec<String> = block_rows(&pool).await.into_iter().map(|r| r.0).collect();
        assert!(
            ids.contains(&"t4".to_string()) || through < T0 + 9_000,
            "read through T0+{}ms, and the block at 9s is not recorded: {ids:?}",
            through - T0
        );
    }

    /// A read that starts behind its tracker (it took the offset before another read moved it on)
    /// feeds the tracker nothing it has seen: a prompt fed twice reads as a rewind.
    #[tokio::test]
    async fn a_read_from_behind_its_tracker_feeds_it_nothing_twice() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(&jsonl, &[&tree_prompt("u1", ""), &tree_asst("a1", "u1")]);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();
        append_line(&jsonl, &tree_prompt("u2", "a1"));
        let end = ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        assert_eq!(
            ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap(),
            end
        );
        assert_eq!(event_kinds(&pool).await, Vec::<String>::new());
        // And it reads on from where the tracker stopped.
        append_line(&jsonl, &tree_asst("a2", "u2"));
        append_line(&jsonl, &tree_prompt("u3", "a1"));
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        assert_eq!(event_kinds(&pool).await, ["rewind"]);
    }

    /// A transcript rewritten in place (truncated, then written again) is read again from its
    /// first line, with a resync, whether the daemon saw it shorter than its offset or only after
    /// it grew past it: going on from the old offset would skip the new head, and record the file
    /// as read for blocks over lines no tracker saw.
    #[tokio::test]
    async fn a_transcript_rewritten_in_place_is_read_again_from_its_first_line() {
        for seen_short in [true, false] {
            let dir = TempDir::new().unwrap();
            let pool = make_pool(&dir).await;
            let main = dir.path().join("sess.jsonl");
            write_jsonl(
                &main,
                &[
                    &read_call("o1", 0, false, "/r/a.rs"),
                    &read_call("o2", 1, false, "/r/b.rs"),
                    &read_call("o3", 2, false, "/r/c.rs"),
                ],
            );
            let (tx, _rx) = broadcast::channel(4);
            let mut off = ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
            write_jsonl(&main, &[&read_call("n1", 10, false, "/r/big.rs")]);
            if seen_short {
                off = ingest_from(&pool, &main, off, &tx, false).await.unwrap();
            }
            append_line(
                &main,
                &tool_result("n1", 11, false, block_text("/r/big.rs", "\n").into(), true),
            );
            for i in 0..4 {
                append_line(
                    &main,
                    &read_call(&format!("n{}", i + 2), 20 + i, false, "/r/z.rs"),
                );
            }
            let end = ingest_from(&pool, &main, off, &tx, false).await.unwrap();
            // The saved offset goes back to where the new read ended, once.
            let offsets: Offsets = Arc::new(Mutex::new(HashMap::from([(main.clone(), off)])));
            advance_offset(&offsets, main.clone(), end);
            advance_offset(&offsets, main.clone(), end - 1);
            assert_eq!(offsets.lock().unwrap()[&main], end, "{seen_short}");
            let ids: Vec<String> = block_rows(&pool).await.into_iter().map(|r| r.0).collect();
            assert_eq!(ids, ["n1"], "seen short: {seen_short}");
            assert_eq!(scan_rows(&pool).await[0].4, T0 + 23_000, "{seen_short}");
            assert_eq!(event_kinds(&pool).await, ["resync"], "{seen_short}");
        }
    }

    /// A transcript holding two sessions' lines (resumed or forked) is read through each session's
    /// own latest turn, not the file's.
    #[tokio::test]
    async fn each_session_in_a_transcript_is_read_through_its_own_latest_turn() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let f = dir.path().join("b.jsonl");
        let a_line = read_call("ta", 2, false, "/r/a.rs");
        let b_line = read_call("tb", 100, false, "/r/b.rs").replace("\"sess\"", "\"other\"");
        write_jsonl(&f, &[&a_line, &b_line]);
        let (tx, _rx) = broadcast::channel(4);
        ingest_from(&pool, &f, 0, &tx, false).await.unwrap();
        let through: Vec<(String, i64)> = scan_rows(&pool)
            .await
            .into_iter()
            .map(|r| (r.1, r.4))
            .collect();
        assert_eq!(
            through,
            [
                ("other".to_string(), T0 + 100_000),
                ("sess".to_string(), T0 + 2_000)
            ]
        );
    }

    /// What the ledger held when the daemon started is not written again: the startup pass reads
    /// every transcript, and rewriting each row as it is costs a statement apiece on every start.
    /// A scan row moves on once the transcript is read past it. The ids and files are this test's
    /// own: what `main` sets once is shared by every test in the process.
    #[tokio::test]
    async fn rows_the_ledger_held_at_start_are_not_written_again() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let main = dir.path().join("sess.jsonl");
        let file = main.to_string_lossy().to_string();
        let mut known = Known::default();
        known.blocks.insert("known-1".into());
        known
            .scans
            .insert((file.clone(), "sess".into()), T0 + 10_000);
        assert!(KNOWN.set(known).is_ok(), "no other test sets it");
        write_jsonl(
            &main,
            &[
                &read_call("known-1", 0, false, "/r/big.rs"),
                &tool_result(
                    "known-1",
                    1,
                    false,
                    block_text("/r/big.rs", "\n").into(),
                    true,
                ),
                &read_call("unknown-1", 5, false, "/r/big2.rs"),
                &tool_result(
                    "unknown-1",
                    6,
                    false,
                    block_text("/r/big2.rs", "\n").into(),
                    true,
                ),
            ],
        );
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &main, 0, &tx, false).await.unwrap();
        let ids: Vec<String> = block_rows(&pool).await.into_iter().map(|r| r.0).collect();
        assert_eq!(ids, ["unknown-1"], "known-1 was in the ledger at start");
        assert!(scan_rows(&pool).await.is_empty(), "read through 5s of 10s");

        append_line(&main, &read_call("later", 12, false, "/r/c.rs"));
        ingest_from(&pool, &main, off, &tx, false).await.unwrap();
        assert_eq!(
            scan_rows(&pool)
                .await
                .into_iter()
                .map(|r| r.4)
                .collect::<Vec<_>>(),
            [T0 + 12_000]
        );
    }

    // ── An assistant record with no usage is skipped, not fatal ────────────────

    #[tokio::test]
    async fn an_assistant_record_with_null_usage_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;

        let jsonl = dir.path().join("session.jsonl");
        let no_usage = BILLABLE_LINE
            .replace("__ID__", "msg-no-usage")
            .replace(
                r#""usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}"#,
                r#""usage":null"#,
            );
        assert!(no_usage.contains(r#""usage":null"#), "{no_usage}");
        let good = BILLABLE_LINE.replace("__ID__", "msg-after");
        write_jsonl(&jsonl, &[&no_usage, &good]);

        let (tx, _rx) = broadcast::channel(4);
        let result = ingest_from(&pool, &jsonl, 0, &tx, false).await;
        assert!(result.is_ok(), "ingest_from failed: {result:?}");

        let ids: Vec<String> = sqlx::query_scalar("SELECT message_id FROM turns")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(ids, vec!["msg-after".to_string()]);
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

    // ── A transcript deleted before it is opened is not an ingest error ────────

    /// The `io::ErrorKind` behind an `ingest_from` error, when it is an I/O error.
    fn io_kind(e: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
        e.downcast_ref::<std::io::Error>().map(std::io::Error::kind)
    }

    #[tokio::test]
    async fn a_transcript_deleted_before_it_is_opened_is_not_an_error() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let (tx, _rx) = broadcast::channel(4);

        // A path never read, one read up to a saved offset, and one whose directory went with
        // it all hand the offset back.
        let gone = dir.path().join("gone.jsonl");
        assert_eq!(ingest_from(&pool, &gone, 0, &tx, true).await.unwrap(), 0);
        assert_eq!(
            ingest_from(&pool, &gone, 137, &tx, true).await.unwrap(),
            137
        );
        let in_gone_dir = dir.path().join("session/gone.jsonl");
        assert_eq!(
            ingest_from(&pool, &in_gone_dir, 42, &tx, true)
                .await
                .unwrap(),
            42
        );
        assert_eq!(event_kinds(&pool).await, Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_transcript_that_vanishes_and_returns_is_tailed_as_if_it_never_left() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        let read = [
            tree_prompt("u1", ""),
            tree_asst("a1", "u1"),
            tree_prompt("u2", "a1"),
            tree_asst("a2", "u2"),
        ];
        let mut lines: Vec<&str> = read.iter().map(String::as_str).collect();
        write_jsonl(&jsonl, &lines);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();

        std::fs::remove_file(&jsonl).unwrap();
        assert_eq!(
            ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap(),
            off
        );

        // Back at the same path with a line that branches back past u2: the tracker that read
        // the first four is still there to call it a rewind, not a resync.
        let rewind = tree_prompt("u3", "a1");
        lines.push(&rewind);
        write_jsonl(&jsonl, &lines);
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        assert_eq!(event_kinds(&pool).await, ["rewind"]);
    }

    #[tokio::test]
    async fn a_transcript_that_vanishes_without_its_tracker_resyncs_when_it_returns() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let jsonl = dir.path().join("s.jsonl");
        write_jsonl(&jsonl, &[&tree_prompt("u1", ""), &tree_asst("a1", "u1")]);
        let (tx, _rx) = broadcast::channel(4);
        let off = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();

        lose_tracker(&jsonl);
        std::fs::remove_file(&jsonl).unwrap();
        assert_eq!(
            ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap(),
            off
        );

        write_jsonl(
            &jsonl,
            &[
                &tree_prompt("u1", ""),
                &tree_asst("a1", "u1"),
                &tree_prompt("u2", "a1"),
            ],
        );
        ingest_from(&pool, &jsonl, off, &tx, false).await.unwrap();
        assert_eq!(event_kinds(&pool).await, ["resync"]);
    }

    #[tokio::test]
    async fn a_transcript_that_cannot_be_read_for_any_other_reason_is_still_an_error() {
        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let (tx, _rx) = broadcast::channel(4);

        // A directory with a transcript's name: refused at open on Windows, at read elsewhere.
        let a_dir = dir.path().join("dir.jsonl");
        std::fs::create_dir(&a_dir).unwrap();
        assert!(ingest_from(&pool, &a_dir, 0, &tx, true).await.is_err());

        // Bytes that are not text fail the read after the open has succeeded.
        let binary = dir.path().join("binary.jsonl");
        std::fs::write(&binary, [0xff, 0xfe, b'\n']).unwrap();
        let err = ingest_from(&pool, &binary, 0, &tx, true).await.unwrap_err();
        assert_eq!(io_kind(&*err), Some(std::io::ErrorKind::InvalidData));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_transcript_that_cannot_be_opened_for_any_other_reason_is_still_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let pool = make_pool(&dir).await;
        let (tx, _rx) = broadcast::channel(4);
        let locked = dir.path().join("locked.jsonl");
        write_jsonl(&locked, &[&BILLABLE_LINE.replace("__ID__", "msg-locked")]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root opens it whatever its mode, which leaves nothing to refuse.
        if std::fs::File::open(&locked).is_ok() {
            return;
        }

        let err = ingest_from(&pool, &locked, 0, &tx, true).await.unwrap_err();
        assert_eq!(io_kind(&*err), Some(std::io::ErrorKind::PermissionDenied));
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
        lumen_core::schema::init_schema(&pool).await.unwrap();
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
            cache_creation_1h_input_tokens: None,
            speed: None,
            inference_geo: None,
            // Not in the ledger, so after any snapshot.
            rowid: i64::MAX,
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
            cache_creation_1h_input_tokens: None,
            speed: None,
            inference_geo: None,
            // Not in the ledger, so after any snapshot.
            rowid: i64::MAX,
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
                cache_creation_1h_input_tokens: None,
                speed: None,
                inference_geo: None,
                // Not in the ledger, so after any snapshot.
                rowid: i64::MAX,
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
            cache_creation_1h_input_tokens: None,
            speed: None,
            inference_geo: None,
            // Not in the ledger, so after any snapshot.
            rowid: i64::MAX,
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

    #[tokio::test]
    async fn the_snapshot_splits_each_sessions_sums_by_model_speed_and_region() {
        // A session can mix models, a subagent on a cheaper one, speeds and regions, and its
        // sums can only be priced per model, speed and region.
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        add_turn(&pool, "m1", "s1", "2026-01-01T10:00:00Z", 1_000).await;
        add_turn(&pool, "m2", "s1", "2026-01-01T11:00:00Z", 2_000).await;
        sqlx::query(
            "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                               cache_read_input_tokens,cache_creation_input_tokens,cache_1h,speed,
                               inference_geo)
             VALUES('h1','s1','2026-01-01T11:30:00Z','claude-haiku-4-5-20251001',1,2,3,40,30,
                    'standard',NULL),
                   ('f1','s1','2026-01-01T11:40:00Z','claude-opus-4-8',0,7,0,0,0,'fast','global'),
                   ('u1','s1','2026-01-01T11:50:00Z','claude-opus-4-8',0,5,0,0,0,'fast','us')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (url, _tx) = start(pool).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let snap = next_json(&mut ws).await;
        let session = &snap["sessions"][0];
        assert_eq!(session["input"], 21, "the totals still cover every model");
        assert_eq!(session["output"], 54);
        assert_eq!(
            session["by_model"],
            serde_json::json!([
                {"model": "claude-haiku-4-5-20251001", "speed": "standard",
                 "inference_geo": null, "input": 1, "output": 2, "cache_read": 3,
                 "cache_write": 40, "cache_write_1h": 30, "cache_write_unknown": 0},
                {"model": "claude-opus-4-8", "speed": "fast", "inference_geo": "global",
                 "input": 0, "output": 7, "cache_read": 0, "cache_write": 0,
                 "cache_write_1h": 0, "cache_write_unknown": 0},
                {"model": "claude-opus-4-8", "speed": "fast", "inference_geo": "us",
                 "input": 0, "output": 5, "cache_read": 0, "cache_write": 0,
                 "cache_write_1h": 0, "cache_write_unknown": 0},
                // add_turn records no speed, region or duration split: all unknown.
                {"model": "claude-sonnet-4", "speed": null, "inference_geo": null,
                 "input": 20, "output": 40, "cache_read": 3_000, "cache_write": 10,
                 "cache_write_1h": 0, "cache_write_unknown": 10},
            ])
        );
    }

    #[tokio::test]
    async fn ingest_records_the_cache_writes_duration_speed_and_region_and_broadcasts_them() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool.clone()).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _ = next_json(&mut ws).await;

        // The usage shape Claude Code writes: the write total, its split by duration, the speed
        // and the region.
        let split = r#"{"sessionId":"s-ttl","timestamp":"2026-01-01T12:00:00Z","message":{"id":"msg-ttl","model":"claude-opus-4-8","role":"assistant","content":[],"usage":{"input_tokens":7,"output_tokens":9,"cache_read_input_tokens":50,"cache_creation_input_tokens":2799,"cache_creation":{"ephemeral_1h_input_tokens":2000,"ephemeral_5m_input_tokens":799},"inference_geo":"us","speed":"fast"}}}"#;
        // An older line, with neither.
        let bare = r#"{"sessionId":"s-ttl","timestamp":"2026-01-01T12:01:00Z","message":{"id":"msg-bare","model":"claude-opus-4-8","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":100}}}"#;
        // What Claude Code writes when it has no region to report: kept as written, not as null.
        let na = r#"{"sessionId":"s-ttl","timestamp":"2026-01-01T12:02:00Z","message":{"id":"msg-na","model":"claude-opus-4-8","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":1,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"inference_geo":"not_available","speed":"standard"}}}"#;
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(&jsonl, format!("{split}\n{bare}\n{na}\n")).unwrap();
        ingest_from(&pool, &jsonl, 0, &tx, true).await.unwrap();

        let event = next_json(&mut ws).await;
        assert_eq!(event["turn"]["cache_creation_input_tokens"], 2799);
        assert_eq!(event["turn"]["cache_creation_1h_input_tokens"], 2000);
        assert_eq!(event["turn"]["speed"], "fast");
        assert_eq!(event["turn"]["inference_geo"], "us");
        let event = next_json(&mut ws).await;
        assert!(
            event["turn"]["cache_creation_1h_input_tokens"].is_null(),
            "no recorded split: the duration of the whole write is unknown, not zero"
        );
        assert!(event["turn"]["speed"].is_null());
        assert!(event["turn"]["inference_geo"].is_null());
        let event = next_json(&mut ws).await;
        assert_eq!(event["turn"]["inference_geo"], "not_available");

        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT message_id, cache_5m, cache_1h, speed, inference_geo FROM turns
             ORDER BY message_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("msg-bare".to_string(), None, None, None, None),
                (
                    "msg-na".to_string(),
                    None,
                    None,
                    Some("standard".to_string()),
                    Some("not_available".to_string())
                ),
                (
                    "msg-ttl".to_string(),
                    Some(799),
                    Some(2000),
                    Some("fast".to_string()),
                    Some("us".to_string())
                ),
            ],
            "what was not recorded is stored as unknown, not as zero or standard"
        );
    }

    #[tokio::test]
    async fn a_turn_ingested_while_a_client_connects_is_counted_once() {
        // The client is subscribed to live turns before its snapshot is read, so a turn that
        // lands in between is in both. It used to be sent twice: in the snapshot's sums, then
        // again as a live event.
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        let (url, tx) = start(pool.clone()).await;

        // Open the TCP connection, wait until the server has subscribed, and only then ingest
        // a turn and complete the WebSocket handshake: the snapshot is read after the turn.
        let addr = url.trim_start_matches("ws://").to_string();
        let tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
        while tx.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }
        let line = |id: &str, out: i64| {
            format!(
                r#"{{"sessionId":"s-race","timestamp":"2026-01-01T12:00:00Z","message":{{"id":"{id}","model":"claude-opus-4-8","role":"assistant","content":[],"usage":{{"input_tokens":0,"output_tokens":{out},"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}}}}"#
            )
        };
        let jsonl = dir.path().join("race.jsonl");
        std::fs::write(&jsonl, format!("{}\n", line("msg-1", 100))).unwrap();
        let end = ingest_from(&pool, &jsonl, 0, &tx, true).await.unwrap();

        let (mut ws, _) = tokio_tungstenite::client_async(&url, tcp).await.unwrap();
        let snap = next_json(&mut ws).await;
        assert_eq!(
            snap["sessions"][0]["output"], 100,
            "the snapshot holds msg-1"
        );

        // A later turn, so the next frame is whatever the client is sent next.
        std::fs::write(
            &jsonl,
            format!("{}\n{}\n", line("msg-1", 100), line("msg-2", 7)),
        )
        .unwrap();
        ingest_from(&pool, &jsonl, end, &tx, true).await.unwrap();
        let next = next_json(&mut ws).await;
        assert_eq!(
            next["turn"]["message_id"], "msg-2",
            "msg-1 is already in the snapshot and must not be sent again"
        );
    }

    #[tokio::test]
    async fn a_refused_calibration_row_is_reported_and_costs_neither_the_turn_nor_the_file() {
        let dir = TempDir::new().unwrap();
        let pool = pool_with_schema(&dir).await;
        sqlx::query(
            "CREATE TRIGGER refuse BEFORE INSERT ON calibration
             BEGIN SELECT RAISE(ABORT, 'calibration refused by the test'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (tx, _rx) = broadcast::channel::<TurnMsg>(4);
        // Text output, so the estimate is non-zero and the calibration insert is attempted.
        let line = r#"{"sessionId":"s-cal","timestamp":"2026-01-01T12:00:00Z","message":{"id":"msg-cal","model":"claude-sonnet-4-6","role":"assistant","content":[{"type":"text","text":"a reply long enough to count"}],"usage":{"input_tokens":7,"output_tokens":9,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#;
        let jsonl = dir.path().join("session.jsonl");
        std::fs::write(&jsonl, format!("{line}\n")).unwrap();

        let end = ingest_from(&pool, &jsonl, 0, &tx, false).await.unwrap();

        assert_eq!(end, line.len() as u64 + 1, "the file must be consumed");
        let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM turns")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(turns, 1, "the turn must land");
        assert!(
            CALIBRATION_LOST.load(std::sync::atomic::Ordering::Relaxed),
            "the refused calibration row must be reported"
        );
    }
}
