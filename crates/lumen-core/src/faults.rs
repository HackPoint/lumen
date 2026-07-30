//! Fault capture: a JSONL spool, and the drain that folds it into the `faults` table.
//!
//! Every writer appends to the spool; nobody writes to SQLite directly. That is not a
//! style preference — the highest-signal fault is the Read intercept's fail-open guard,
//! and that hook sits on the path that decides whether the model may read a file at
//! all. A SQLite write there would put a lock acquisition on a blocking path that must
//! never stall, so writers get an `O_APPEND` line and `lumen report` does the database
//! work later.
//!
//! Appending a single line under `O_APPEND` is atomic for writes below `PIPE_BUF`, so
//! concurrent hooks and daemons interleave whole lines rather than corrupting each
//! other. [`drain_spool`] renames before reading, so a writer racing the drain lands in
//! the fresh spool instead of being lost.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Spool lines read in one drain. A runaway writer must not turn `lumen report` into an
/// unbounded allocation; the cap is far above any plausible real fault volume.
pub const DRAIN_LINE_CAP: usize = 20_000;

/// The sentinel for a kind with no sub-kind. Must match the renderer's, because it is
/// part of the dedupe fingerprint.
pub const NO_VARIANT: &str = "-";

/// One fault occurrence, as it appears on a spool line and as a `faults` row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultRecord {
    /// ISO-8601 UTC. Supplied by the caller so this module needs no clock.
    pub ts: String,
    pub kind: String,
    #[serde(default = "no_variant")]
    pub variant: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub lines: Option<i64>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "unknown_channel")]
    pub channel: String,
}

fn no_variant() -> String {
    NO_VARIANT.to_string()
}

fn unknown_channel() -> String {
    "unknown".to_string()
}

impl FaultRecord {
    /// A record stamped with this build's version and the current UTC second.
    pub fn now(kind: &str, variant: &str) -> Self {
        Self {
            ts: utc_now(),
            kind: kind.to_string(),
            variant: if variant.is_empty() {
                NO_VARIANT.to_string()
            } else {
                variant.to_string()
            },
            path: None,
            lines: None,
            detail: None,
            session_id: std::env::var("LUMEN_SESSION_ID").ok(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            channel: std::env::var("LUMEN_CHANNEL").unwrap_or_else(|_| "unknown".into()),
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// ISO-8601 UTC to the second, without pulling in a date library.
///
/// Days-from-civil is the inverse of Howard Hinnant's civil-from-days, which is the
/// algorithm `chrono` uses; it is exact for every date this will ever see.
fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days, era-based (Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Where the spool lives: `LUMEN_FAULT_SPOOL`, else `faults.jsonl` beside the database.
///
/// Beside the database on purpose — that path is already resolved consistently by every
/// component (env var, pointer file, binary-relative), so the spool cannot end up
/// somewhere `lumen report` will not look.
pub fn spool_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("LUMEN_FAULT_SPOOL") {
        return Some(PathBuf::from(p));
    }
    let db = crate::meter::db_path()?;
    Some(db.parent()?.join("faults.jsonl"))
}

/// Append one fault to the resolved spool. Best-effort by contract: callers are on
/// paths where failing to record a fault must never become a second fault, so every
/// error is dropped.
pub fn record(rec: &FaultRecord) {
    let Some(path) = spool_path() else { return };
    record_at(&path, rec);
}

/// [`record`] against an explicit spool. Every entry point has this pairing so callers
/// — tests especially — never have to mutate process-global environment to redirect the
/// spool, which cannot be done safely from tests running in parallel.
pub fn record_at(path: &Path, rec: &FaultRecord) {
    let _ = append_line(path, rec);
}

/// Last recorded instant per `(kind, variant)`, for [`record_throttled`].
static LAST_RECORDED: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, String), std::time::Instant>>,
> = std::sync::OnceLock::new();

/// Record at most one occurrence of a `(kind, variant)` per `min_interval`.
///
/// For callers inside retry loops. The daemon's WS supervisor retries every 2s, so a
/// permanently failed bind would append ~43k lines a day and drown every other fault in
/// the spool; the count in a report is not worth that. Returns whether it recorded.
///
/// Process-local: a restarted daemon records again immediately, which is correct — that
/// is a new occurrence of the condition, not the same one still going.
pub fn record_throttled(rec: &FaultRecord, min_interval: std::time::Duration) -> bool {
    let key = (rec.kind.clone(), rec.variant.clone());
    let now = std::time::Instant::now();

    let map = LAST_RECORDED.get_or_init(Default::default);
    {
        let Ok(mut seen) = map.lock() else {
            // A poisoned mutex must not silence capture, and must not panic a caller
            // that is already on an error path.
            record(rec);
            return true;
        };
        match seen.get(&key) {
            Some(prev) if now.duration_since(*prev) < min_interval => return false,
            _ => seen.insert(key, now),
        };
    }
    record(rec);
    true
}

fn append_line(path: &Path, rec: &FaultRecord) -> std::io::Result<()> {
    let mut line = serde_json::to_string(rec).map_err(std::io::Error::other)?;
    line.push('\n');

    // One `write` of one line: O_APPEND makes it atomic below PIPE_BUF, so parallel
    // writers interleave whole lines. Buffering or two writes would not.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Move the spool aside and parse it. Renaming first is what makes the drain safe: a
/// writer that opens the old path mid-drain still appends to a file nobody will delete
/// out from under it, and the next writer creates a fresh spool.
///
/// Returns the records and the moved-aside path, which the caller deletes once the rows
/// are committed — losing the file before the insert would lose the faults.
pub fn take_spool() -> Option<(Vec<FaultRecord>, PathBuf)> {
    take_spool_at(&spool_path()?)
}

/// How many records are waiting in the spool, without draining it.
///
/// Read-only on purpose: this feeds a badge that refreshes on navigation, and a count
/// that silently moved rows into the database every time a screen opened would make
/// looking at the UI a write.
pub fn spool_len() -> usize {
    spool_path().map(|p| spool_len_at(&p)).unwrap_or(0)
}

/// [`spool_len`] against an explicit spool path.
pub fn spool_len_at(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// [`take_spool`] against an explicit spool path.
pub fn take_spool_at(path: &Path) -> Option<(Vec<FaultRecord>, PathBuf)> {
    if !path.is_file() {
        return None;
    }
    let taken = path.with_extension("jsonl.draining");
    // A leftover .draining from an interrupted run is folded into the live spool and
    // then removed — not merged the other way. The rename below replaces `taken`, so
    // anything merged *into* it would be clobbered by the very next line.
    if taken.exists() {
        let _ = merge_into(path, &taken);
        let _ = std::fs::remove_file(&taken);
    }
    std::fs::rename(path, &taken).ok()?;

    let text = std::fs::read_to_string(&taken).ok()?;
    let mut out = Vec::new();
    for line in text.lines().take(DRAIN_LINE_CAP) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A truncated tail line is skipped, not fatal: one bad line must not discard
        // every good fault in the spool.
        if let Ok(rec) = serde_json::from_str::<FaultRecord>(line) {
            out.push(rec);
        }
    }
    Some((out, taken))
}

fn merge_into(dst: &Path, src: &Path) -> std::io::Result<()> {
    let extra = std::fs::read_to_string(src)?;
    let mut f = std::fs::OpenOptions::new().append(true).open(dst)?;
    f.write_all(extra.as_bytes())
}

/// Insert drained records into `faults`. Returns how many rows landed.
///
/// One transaction: on failure the caller keeps the `.draining` file, so a crashed
/// drain is retried rather than silently dropping the batch.
pub fn insert(conn: &rusqlite::Connection, recs: &[FaultRecord]) -> rusqlite::Result<usize> {
    if recs.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?;
        for r in recs {
            stmt.execute(rusqlite::params![
                r.ts,
                r.kind,
                r.variant,
                r.path,
                r.lines,
                r.detail,
                r.session_id,
                r.version,
                r.channel,
            ])?;
        }
    }
    tx.commit()?;
    Ok(recs.len())
}

/// Drain the spool into the database: take, insert, then delete the moved-aside file.
///
/// The delete is last and conditional on the insert, so an error leaves the batch on
/// disk for the next run.
pub fn drain_spool(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
    match spool_path() {
        Some(p) => drain_spool_at(conn, &p),
        None => Ok(0),
    }
}

/// [`drain_spool`] against an explicit spool path.
pub fn drain_spool_at(conn: &rusqlite::Connection, path: &Path) -> rusqlite::Result<usize> {
    let Some((recs, taken)) = take_spool_at(path) else {
        return Ok(0);
    };
    let n = insert(conn, &recs)?;
    let _ = std::fs::remove_file(&taken);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A spool private to this test. Deliberately not via `LUMEN_FAULT_SPOOL`: the
    /// environment is process-global, so tests sharing it through env would race under
    /// the default parallel test runner — which is exactly what they did.
    fn spool_in(dir: &TempDir) -> PathBuf {
        dir.path().join("faults.jsonl")
    }

    fn conn_with_schema() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(crate::schema::DDL).unwrap();
        c
    }

    #[test]
    fn utc_now_is_iso8601_to_the_second() {
        let ts = utc_now();
        assert_eq!(ts.len(), 20, "unexpected shape: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        // Sanity-check the civil-from-days arithmetic against a known instant.
        assert!(ts.as_str() > "2026-01-01T00:00:00Z", "{ts}");
        assert!(ts.as_str() < "2100-01-01T00:00:00Z", "{ts}");
    }

    #[test]
    fn record_appends_one_line_per_fault() {
        let dir = TempDir::new().unwrap();
        let spool = spool_in(&dir);

        record_at(
            &spool,
            &FaultRecord::now("hook_fail_open", "retry_escape_valve"),
        );
        record_at(&spool, &FaultRecord::now("ingest_failed", "poll"));

        let text = std::fs::read_to_string(&spool).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|l| l.starts_with('{') && l.ends_with('}')));
    }

    #[test]
    fn drain_moves_rows_into_the_table_and_clears_the_spool() {
        let dir = TempDir::new().unwrap();
        let spool = spool_in(&dir);
        let conn = conn_with_schema();

        record_at(
            &spool,
            &FaultRecord::now("hook_fail_open", "retry_escape_valve").with_path("a.rs"),
        );
        record_at(
            &spool,
            &FaultRecord::now("hook_fail_open", "retry_escape_valve").with_path("b.rs"),
        );

        assert_eq!(drain_spool_at(&conn, &spool).unwrap(), 2);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM faults", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        assert!(!spool.exists(), "spool should be gone after a clean drain");
        assert_eq!(
            drain_spool_at(&conn, &spool).unwrap(),
            0,
            "second drain is a no-op"
        );
    }

    /// A fault written while the drain is in flight must survive.
    #[test]
    fn a_write_racing_the_drain_is_not_lost() {
        let dir = TempDir::new().unwrap();
        let spool = spool_in(&dir);
        let conn = conn_with_schema();

        record_at(&spool, &FaultRecord::now("ingest_failed", "init"));
        let (recs, taken) = take_spool_at(&spool).expect("spool taken");
        assert_eq!(recs.len(), 1);

        // Writer arrives after the rename: lands in a fresh spool, untouched by us.
        record_at(&spool, &FaultRecord::now("ingest_failed", "notify"));
        insert(&conn, &recs).unwrap();
        std::fs::remove_file(&taken).unwrap();

        assert!(spool.exists(), "the racing write must still be on disk");
        assert_eq!(drain_spool_at(&conn, &spool).unwrap(), 1);
        let kinds: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT variant FROM faults ORDER BY variant")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(kinds, vec!["init", "notify"]);
    }

    /// An interrupted drain leaves `.draining` behind; the next run must fold it in.
    #[test]
    fn a_leftover_draining_file_is_recovered_not_clobbered() {
        let dir = TempDir::new().unwrap();
        let spool = spool_in(&dir);
        let conn = conn_with_schema();

        record_at(&spool, &FaultRecord::now("schema_drift", "-"));
        let (_recs, taken) = take_spool_at(&spool).expect("taken");
        assert!(taken.exists());

        // A later fault, then a drain that finds the orphaned .draining still there.
        record_at(&spool, &FaultRecord::now("ws_restart", "-"));
        assert_eq!(
            drain_spool_at(&conn, &spool).unwrap(),
            2,
            "both batches must land"
        );
        assert!(!spool.exists());
    }

    #[test]
    fn a_corrupt_line_does_not_discard_the_good_ones() {
        let dir = TempDir::new().unwrap();
        let spool = spool_in(&dir);

        record_at(&spool, &FaultRecord::now("ingest_failed", "init"));
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&spool)
                .unwrap();
            f.write_all(b"{\"kind\": truncated\n").unwrap();
        }
        record_at(&spool, &FaultRecord::now("ingest_failed", "poll"));

        let (recs, _) = take_spool_at(&spool).expect("taken");
        assert_eq!(recs.len(), 2, "the two valid lines must survive");
    }

    #[test]
    fn missing_fields_fall_back_to_the_sentinels() {
        let rec: FaultRecord = serde_json::from_str(r#"{"ts":"2026-07-30T00:00:00Z","kind":"x"}"#)
            .expect("minimal line parses");
        assert_eq!(rec.variant, NO_VARIANT);
        assert_eq!(rec.channel, "unknown");
        assert!(rec.path.is_none());
    }

    #[test]
    fn an_empty_variant_becomes_the_sentinel() {
        assert_eq!(FaultRecord::now("k", "").variant, NO_VARIANT);
    }

    /// The throttle is what keeps a 2s retry loop from flooding the spool.
    #[test]
    fn throttling_admits_the_first_and_suppresses_the_burst() {
        let dir = TempDir::new().unwrap();
        // SAFETY: record_throttled goes through spool_path(), and this is the only test
        // that touches the var. It is set once and never cleared, so no other test can
        // observe a transition.
        unsafe { std::env::set_var("LUMEN_FAULT_SPOOL", dir.path().join("throttle.jsonl")) };

        let rec = FaultRecord::now("ws_restart", "error");
        assert!(record_throttled(&rec, std::time::Duration::from_secs(60)));
        for _ in 0..50 {
            assert!(
                !record_throttled(&rec, std::time::Duration::from_secs(60)),
                "a burst inside the interval must be suppressed"
            );
        }

        // A distinct (kind, variant) has its own budget.
        let other = FaultRecord::now("ws_restart", "clean_return");
        assert!(record_throttled(&other, std::time::Duration::from_secs(60)));

        // A zero interval always admits — the throttle must be opt-in, not a cap.
        assert!(record_throttled(&rec, std::time::Duration::ZERO));

        let text = std::fs::read_to_string(dir.path().join("throttle.jsonl")).unwrap();
        assert_eq!(
            text.lines().count(),
            3,
            "expected first + other + unthrottled"
        );
    }

    /// The faults table has to exist on a database created from DDL alone, and from
    /// MIGRATIONS alone — the two arms are independent on purpose.
    #[test]
    fn both_schema_arms_create_the_faults_table() {
        for build in ["ddl", "migrations"] {
            let c = rusqlite::Connection::open_in_memory().unwrap();
            match build {
                "ddl" => c.execute_batch(crate::schema::DDL).unwrap(),
                _ => {
                    for m in crate::schema::MIGRATIONS {
                        let _ = c.execute_batch(m);
                    }
                }
            }
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='faults'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{build} arm did not create faults");
        }
    }
}
