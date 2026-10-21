//! Merge from the backup, then freeze (§8 of the design).
//!
//! Once the previous folder (L) has been moved into the backup (`B/data`), an old binary can still
//! have written into it for a short while: on Windows between the connection closing and the rename,
//! and on every OS in the window where an old app ran against L before the move succeeded. So the
//! rows of the backup's ledger are merged into the new ledger at every start until Setup has
//! recorded the new data folder; then the backup is frozen and is never read for merging again.
//!
//! - **[`merge_backup`]** takes rows only, by position, and never the fault spool. Progress per table
//!   lives in the new ledger's `legacy_merge` (seeded from the marker's `hwm` and `hwm_sig` on the
//!   first run), in the same transaction as the rows. A positional table (`read_events`, `faults`)
//!   whose backup no longer has the recorded row at its mark has been *rewound*, that is replaced by
//!   someone: the merge stops and says so, and reconciles nothing by content.
//! - **[`freeze_if_ready`]** runs the same row pass and then takes the backup's fault spool
//!   (`faults.jsonl.draining` and `faults.jsonl`, no line cap) exactly once, in the same transaction as
//!   a `$frozen` row. Every fault of the previous install is therefore one of: a row at or below the
//!   marker's mark (in the snapshot), a row above it (taken by a merge), or a spool line (taken here).
//!   Nothing is copied into N's live spool, so there is no torn append to resume.
//! - After the commit the backup's files are made read-only and a stamp (`B/.frozen`) records the
//!   ledger's size and mtime, so a later change to the backup is noticed and reported.
//!
//! The gate is: the marker is `migrated`; the backup is in place (or there is no previous folder any
//! more); the Setup record names N; and the migration lock is free. Everything after the `$frozen`
//! row is repeatable: a run killed before the commit changed nothing, a run killed after it only has
//! the stamp and the read-only bits left to redo.

use super::commit::{DATA_DIR, Lock, LockError, backup_dir, take_lock};
use super::faultset::{
    admit_fresh, counts_in_n, read_spool_files, record_digests, row_digests, tally,
};
use super::import::{prefix_in_list, record_rows, recorded_lists};
use super::marker::MarkerV2;
use super::merge::{
    KEYED_TABLES, POSITIONAL_TABLES, columns, progress, quote, ro_uri, row_sig_in, set_progress,
};
use super::migrate::LEDGER;
use super::same_dir;
use crate::faults;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Written into the backup (beside `data/`) at the freeze: the ledger's size and mtime.
pub const FROZEN_STAMP: &str = ".frozen";

/// The `tbl` of the `legacy_merge` row that ends the merge: `hwm` is the number of fault lines
/// taken from the backup's spool, `sig` the SHA-256 of the bytes they were taken from.
pub const FROZEN_TBL: &str = "$frozen";

/// The `legacy_merge.source` of a backup: `backup:<its folder name>`.
pub fn source_key(b: &Path) -> String {
    let name = b
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("backup:{name}")
}

/// What one pass over the backup did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// Rows inserted, per table.
    pub rows: BTreeMap<String, usize>,
    /// Things worth reporting that did not stop the pass (a column the new ledger lacks).
    pub notes: Vec<String>,
    /// Positional tables whose backup was rewound. Non-empty: nothing was merged or frozen.
    pub rewound: Vec<String>,
    /// The `$frozen` row was already there: nothing was read from the backup.
    pub was_frozen: bool,
    /// This pass wrote the `$frozen` row.
    pub frozen_now: bool,
    /// Fault records taken from the backup's spool by this pass (the freeze only).
    pub spool_records: usize,
    /// A test seam stopped the pass.
    pub stopped: bool,
}

impl MergeReport {
    pub fn total_rows(&self) -> usize {
        self.rows.values().sum()
    }
}

/// Where a pass is cut short, for the tests that stand in for a killed process.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Never,
    /// After the rows and the spool are inserted, before the commit: the transaction rolls back.
    BeforeCommit,
    /// Right after the commit: no stamp, no read-only bits.
    AfterCommit,
}

/// Merge the rows the backup has and N lacks (§8.2). Never reads the backup's spool, and does
/// nothing at all once the freeze has happened.
///
/// `Err` only when N or the backup's path cannot be used; a rewound backup is a report, not an
/// error.
pub fn merge_backup(n_db: &Path, b: &Path, marker: &MarkerV2) -> Result<MergeReport, String> {
    run(n_db, b, marker, false, Stop::Never)
}

fn run(
    n_db: &Path,
    b: &Path,
    marker: &MarkerV2,
    freeze: bool,
    stop: Stop,
) -> Result<MergeReport, String> {
    let data = b.join(DATA_DIR);
    let b_db = data.join(LEDGER);
    let key = source_key(b);
    let has_ledger = b_db.is_file();
    if has_ledger {
        recover_if_hot(&b_db)?;
    }
    // N through the shared opener: the current schema, `legacy_merge` included, even when
    // nothing has opened the migrated ledger since the copy.
    let mut conn = crate::meter::connect_db(n_db)
        .map_err(|e| format!("cannot open {}: {e}", n_db.display()))?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let mut report = MergeReport::default();
    if progress(&conn, &key, FROZEN_TBL)?.is_some() {
        report.was_frozen = true;
        return Ok(report);
    }
    if has_ledger {
        let uri = ro_uri(&b_db.to_string_lossy())?;
        conn.execute("ATTACH DATABASE ?1 AS src", params![uri])
            .map_err(|e| format!("cannot attach the backup: {e}"))?;
    }
    let outcome = pass(
        &mut conn,
        &key,
        marker,
        &data,
        n_db.parent().unwrap_or(Path::new(".")),
        has_ledger,
        freeze,
        stop,
        &mut report,
    );
    if has_ledger && let Err(e) = conn.execute_batch("DETACH DATABASE src") {
        // A failed detach leaves the attachment on a connection that is dropped next.
        return outcome.and_then(|()| Err(format!("cannot detach the backup: {e}")));
    }
    outcome.map(|()| report)
}

/// One transaction: the rows and their progress, and at the freeze the spool and the `$frozen` row.
#[allow(clippy::too_many_arguments)]
fn pass(
    conn: &mut Connection,
    key: &str,
    marker: &MarkerV2,
    data: &Path,
    n_dir: &Path,
    has_ledger: bool,
    freeze: bool,
    stop: Stop,
    report: &mut MergeReport,
) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("cannot start the merge: {e}"))?;
    if has_ledger {
        // Look before writing anything: a replaced backup merges nothing, not part of it.
        for table in POSITIONAL_TABLES {
            if rewound(&tx, key, marker, table)? {
                report.rewound.push(table.to_string());
            }
        }
        if !report.rewound.is_empty() {
            return Ok(()); // the transaction rolls back
        }
        rows(&tx, key, marker, report, data, n_dir)?;
    }
    if freeze {
        // The spool lines come in as a multiset against N (see `faultset`): a line goes in while
        // the backup (its rows and its spool) holds more of its digest than N does, so one an
        // import, an earlier pass or N's own drain already brought is not added again.
        let (recs, bytes) = read_spool_files(&data.join("faults.jsonl"))?;
        let keys = record_digests(&recs)?;
        let mut held_by_b = tally(keys.iter());
        if has_ledger && let Some(rows) = row_digests(&tx, "src")? {
            for (_, k) in &rows {
                *held_by_b.entry(k.clone()).or_insert(0) += 1;
            }
        }
        let in_n = counts_in_n(&tx, "main", n_dir)?;
        let admitted = admit_fresh(&held_by_b, &in_n, &keys);
        let fresh: Vec<_> = recs
            .iter()
            .zip(&admitted)
            .filter(|(_, ok)| **ok)
            .map(|(r, _)| r.clone())
            .collect();
        faults::insert_in(&tx, &fresh).map_err(|e| format!("faults from the spool: {e}"))?;
        set_progress(&tx, key, FROZEN_TBL, fresh.len() as i64, Some(&hex(&bytes)))?;
        report.spool_records = fresh.len();
    }
    if stop == Stop::BeforeCommit {
        report.stopped = true;
        return Ok(());
    }
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    report.frozen_now = freeze;
    report.stopped = stop == Stop::AfterCommit;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where the merge stands for `table`: rows up to `held` are in N, `sig` is the digest of the
/// backup's row there. The marker seeds the first run.
fn held(
    conn: &Connection,
    key: &str,
    marker: &MarkerV2,
    table: &str,
) -> Result<(i64, Option<String>), String> {
    Ok(match progress(conn, key, table)? {
        Some(p) => p,
        None => (
            marker.hwm.get(table).copied().unwrap_or(0),
            marker.hwm_sig.get(table).cloned(),
        ),
    })
}

/// Whether the backup's row at the mark is the row recorded there.
///
/// The digest covers the backup's columns. A column an old binary added to the backup after the
/// mark was recorded (`ALTER ... ADD COLUMN` goes last, and old rows take its default) would change
/// that digest without the row having changed, so the shorter column lists, from the end, are tried
/// too before the row is called different.
fn row_is_recorded(
    conn: &Connection,
    table: &str,
    cols: &[String],
    id: i64,
    sig: Option<&str>,
) -> Result<bool, String> {
    let Some(sig) = sig else {
        return Ok(row_sig_in(conn, "src", table, cols, id)?.is_some());
    };
    for k in (1..=cols.len()).rev() {
        if row_sig_in(conn, "src", table, &cols[..k], id)?.as_deref() == Some(sig) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn top_of(conn: &Connection, table: &str) -> Result<i64, String> {
    conn.query_row(
        &format!("SELECT coalesce(max(rowid), 0) FROM src.{}", quote(table)),
        [],
        |r| r.get(0),
    )
    .map_err(|e| format!("{table}: {e}"))
}

/// A positional table is rewound when the backup holds less than was merged, or not the row that
/// was recorded at the mark: someone replaced the backup.
fn rewound(conn: &Connection, key: &str, marker: &MarkerV2, table: &str) -> Result<bool, String> {
    let src_cols = columns(conn, "src", table).map_err(|e| e.to_string())?;
    if src_cols.is_empty() {
        return Ok(false);
    }
    let (mark, sig) = held(conn, key, marker, table)?;
    if mark <= 0 {
        return Ok(false);
    }
    if top_of(conn, table)? < mark {
        return Ok(true);
    }
    Ok(!row_is_recorded(
        conn,
        table,
        &src_cols,
        mark,
        sig.as_deref(),
    )?)
}

/// ` AND rowid NOT IN (...)` for the backup's fault rows in `(mark, top]` that N already holds
/// enough of (empty when none, or when the table has no digest). N is counted as it is now.
fn skipped_fault_rows(
    conn: &Connection,
    data: &Path,
    n_dir: &Path,
    mark: i64,
    top: i64,
) -> Result<String, String> {
    let Some(b_rows) = row_digests(conn, "src")? else {
        return Ok(String::new());
    };
    let (recs, _) = read_spool_files(&data.join("faults.jsonl"))?;
    let mut held_by_b = tally(record_digests(&recs)?.iter());
    for (_, k) in &b_rows {
        *held_by_b.entry(k.clone()).or_insert(0) += 1;
    }
    let in_n = counts_in_n(conn, "main", n_dir)?;
    let cands: Vec<(i64, String)> = b_rows
        .into_iter()
        .filter(|(id, _)| *id > mark && *id <= top)
        .collect();
    let keys: Vec<String> = cands.iter().map(|(_, k)| k.clone()).collect();
    let skip: Vec<String> = cands
        .iter()
        .zip(admit_fresh(&held_by_b, &in_n, &keys))
        .filter(|(_, ok)| !ok)
        .map(|((id, _), _)| id.to_string())
        .collect();
    Ok(if skip.is_empty() {
        String::new()
    } else {
        format!(" AND rowid NOT IN ({})", skip.join(","))
    })
}

/// How many leading rows of the backup's `table` a row list in N already holds: an import of a
/// copy of the previous folder put them there, so the merge must not add them again.
fn listed_prefix(conn: &Connection, src_cols: &[String], table: &str) -> Result<i64, String> {
    let mut best = 0;
    for list in recorded_lists(conn, "main", table)? {
        if list.cols.iter().any(|c| !src_cols.contains(c)) {
            continue;
        }
        best = best.max(prefix_in_list(conn, "src", table, &list)?);
    }
    Ok(best)
}

/// The row pass: every table of both kinds, on the columns both ledgers have.
fn rows(
    conn: &Connection,
    key: &str,
    marker: &MarkerV2,
    report: &mut MergeReport,
    data: &Path,
    n_dir: &Path,
) -> Result<(), String> {
    let tables = KEYED_TABLES
        .iter()
        .map(|t| (*t, false))
        .chain(POSITIONAL_TABLES.iter().map(|t| (*t, true)));
    for (table, positional) in tables {
        let new_cols = columns(conn, "main", table).map_err(|e| e.to_string())?;
        let src_cols = columns(conn, "src", table).map_err(|e| e.to_string())?;
        if new_cols.is_empty() || src_cols.is_empty() {
            continue;
        }
        let extra: Vec<&str> = src_cols
            .iter()
            .filter(|c| !new_cols.contains(c))
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            report.notes.push(format!(
                "{table}: the backup is on a newer schema, columns not carried: {}",
                extra.join(", ")
            ));
        }
        let common: Vec<String> = new_cols
            .iter()
            .filter(|c| src_cols.contains(c))
            .map(|c| quote(c))
            .collect();
        if common.is_empty() {
            continue;
        }
        let list = common.join(", ");
        let top = top_of(conn, table)?;
        let (mut mark, _) = held(conn, key, marker, table)?;
        if !positional && top < mark {
            mark = 0; // a keyed table that shrank: one full pass, the keys keep it exact
        }
        if table == "read_events" {
            mark = mark.max(listed_prefix(conn, &src_cols, table)?);
        }
        let t = quote(table);
        let verb = if positional {
            "INSERT"
        } else {
            "INSERT OR IGNORE"
        };
        // Faults: the backup is one source (its rows, its spool and its `.draining` batch) and
        // adds only what it holds above N's count of a digest (see `faultset`).
        let not_in = if table == "faults" {
            skipped_fault_rows(conn, data, n_dir, mark, top)?
        } else {
            String::new()
        };
        let n = conn
            .execute(
                &format!(
                    "{verb} INTO main.{t} ({list}) SELECT {list} FROM src.{t} \
                     WHERE rowid > ?1 AND rowid <= ?2{not_in} ORDER BY rowid"
                ),
                params![mark, top],
            )
            .map_err(|e| format!("{table}: {e}"))?;
        if n > 0 {
            report.rows.insert(table.to_string(), n);
            if table == "read_events" {
                // Keep the backup's rows, one by one, in N: an import of a copy of the previous
                // folder is then told what N holds even after the backup is deleted.
                record_rows(conn, "src", "main", table)?;
            }
        }
        let sig = if positional && top > 0 {
            row_sig_in(conn, "src", table, &src_cols, top)?
        } else {
            None
        };
        set_progress(conn, key, table, top, sig.as_deref())?;
    }
    Ok(())
}

/// A hot journal (or a WAL) beside the backup's ledger belongs to a write an old binary did not
/// finish. A read-only open cannot roll it back and would either fail or read half a write, so the
/// ledger is opened for writing once, which is what any opener does. Only ever before the freeze.
fn recover_if_hot(b_db: &Path) -> Result<(), String> {
    let side = |suffix: &str| {
        let mut s = b_db.as_os_str().to_os_string();
        s.push(suffix);
        fs::metadata(PathBuf::from(s)).is_ok_and(|m| m.len() > 0)
    };
    if !(side("-journal") || side("-wal")) {
        return Ok(());
    }
    let c = Connection::open_with_flags(
        b_db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("cannot recover the backup's ledger: {e}"))?;
    c.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map(|_| ())
    .map_err(|e| format!("cannot recover the backup's ledger: {e}"))
}

// ---- the freeze ----

/// What [`freeze_if_ready`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freeze {
    /// A condition of the gate does not hold yet; nothing was changed.
    NotReady(String),
    /// This call froze the backup.
    Frozen(MergeReport),
    /// It was frozen before. The stamp and the read-only bits are made sure of, and `changed`
    /// says the backup's ledger is not what the stamp recorded: nothing from it was merged.
    Already { changed: bool },
    /// The backup was rewound (replaced): nothing was merged or frozen.
    Rewound(Vec<String>),
    /// Nothing was changed; the next start or Setup tries again.
    Failed(String),
}

/// Whether the Setup record `setup_done` (`data_dir=<folder>` lines) names `n`, folding case where
/// the file system does. An empty or missing record (an install from before the migration) names
/// nothing.
pub fn setup_names(setup_done: &Path, n: &Path) -> bool {
    let Ok(text) = fs::read_to_string(setup_done) else {
        return false;
    };
    text.lines().any(|line| {
        line.trim_end_matches('\r')
            .strip_prefix("data_dir=")
            .is_some_and(|dir| !dir.is_empty() && same_dir(Path::new(dir), n))
    })
}

/// Freeze the backup when the gate allows it, at every start after the move and after Setup.
/// Takes the migration lock; a held lock is "not ready", not an error.
pub fn freeze_if_ready(n: &Path, setup_done: &Path, marker: &MarkerV2) -> Freeze {
    match take_lock(n) {
        Ok(lock) => {
            let out = freeze_if_ready_under(&lock, n, setup_done, marker);
            drop(lock);
            out
        }
        Err(LockError::Held) => Freeze::NotReady("another instance holds the lock".into()),
        Err(LockError::Io(e)) => Freeze::Failed(format!("lock: {e}")),
    }
}

/// [`freeze_if_ready`] for a caller that already holds the lock.
pub fn freeze_if_ready_under(
    lock: &Lock,
    n: &Path,
    setup_done: &Path,
    marker: &MarkerV2,
) -> Freeze {
    freeze_with(lock, n, setup_done, marker, Stop::Never)
}

#[doc(hidden)]
pub fn freeze_with(
    _lock: &Lock,
    n: &Path,
    setup_done: &Path,
    marker: &MarkerV2,
    stop: Stop,
) -> Freeze {
    let Some(b) = backup_dir(n, marker) else {
        return Freeze::NotReady("the marker names no backup".into());
    };
    let data = b.join(DATA_DIR);
    let l = Path::new(&marker.source_dir);
    if !data.is_dir() && fs::symlink_metadata(l).is_ok_and(|m| m.file_type().is_dir()) {
        return Freeze::NotReady("the previous folder has not been moved yet".into());
    }
    if b.join(FROZEN_STAMP).exists() {
        return already(&b);
    }
    if !setup_names(setup_done, n) {
        return Freeze::NotReady("Setup has not recorded this data folder".into());
    }
    match run(&n.join(LEDGER), &b, marker, true, stop) {
        Err(e) => Freeze::Failed(e),
        Ok(r) if !r.rewound.is_empty() => Freeze::Rewound(r.rewound),
        Ok(r) if r.was_frozen => already(&b),
        Ok(r) if r.stopped => Freeze::Frozen(r),
        Ok(r) => match finish_frozen(&b) {
            Ok(_) => Freeze::Frozen(r),
            Err(e) => Freeze::Failed(format!("the backup is frozen but not yet read-only: {e}")),
        },
    }
}

fn already(b: &Path) -> Freeze {
    match finish_frozen(b) {
        Ok(changed) => Freeze::Already { changed },
        Err(e) => Freeze::Failed(format!("the backup is frozen but not yet read-only: {e}")),
    }
}

/// What the stamp records of the backup's ledger.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    size: Option<u64>,
    mtime_ns: Option<u64>,
}

fn stamp_of(ledger: &Path) -> Stamp {
    match fs::metadata(ledger) {
        Ok(m) => Stamp {
            size: Some(m.len()),
            mtime_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_nanos()).ok()),
        },
        Err(_) => Stamp {
            size: None,
            mtime_ns: None,
        },
    }
}

/// F2 and F3, safe to repeat: write the stamp if it is missing, otherwise compare with it; then
/// make the files under `B/data` read-only. Returns whether the ledger differs from the stamp.
///
/// A backup that is not there (no previous folder, so nothing was ever moved) has nothing to stamp.
pub fn finish_frozen(b: &Path) -> io::Result<bool> {
    let data = b.join(DATA_DIR);
    if !data.is_dir() {
        return Ok(false);
    }
    let now = stamp_of(&data.join(LEDGER));
    let stamp = b.join(FROZEN_STAMP);
    let before = fs::read(&stamp)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Stamp>(&bytes).ok());
    let changed = match before {
        Some(prev) => prev != now,
        None => {
            fs::write(&stamp, serde_json::to_vec(&now).map_err(io::Error::other)?)?;
            false
        }
    };
    make_read_only(&data)?;
    Ok(changed)
}

/// Every file under `dir` read-only: mode 0444 on Unix, the read-only attribute on Windows. The
/// directories stay writable, so `rm -rf` and the uninstaller work. Links are left alone, not
/// followed. Idempotent. Tries every file and returns the first error.
pub fn make_read_only(dir: &Path) -> io::Result<()> {
    let mut first = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let res = match entry.file_type() {
            Ok(t) if t.is_dir() => make_read_only(&path),
            Ok(t) if t.is_file() => read_only_file(&path),
            _ => Ok(()),
        };
        if let Err(e) = res
            && first.is_none()
        {
            first = Some(e);
        }
    }
    first.map_or(Ok(()), Err)
}

#[cfg(unix)]
fn read_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode == 0o444 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
}

#[cfg(not(unix))]
fn read_only_file(path: &Path) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    if perms.readonly() {
        return Ok(());
    }
    perms.set_readonly(true);
    fs::set_permissions(path, perms)
}

/// Whether N's ledger holds the `$frozen` row for `b`. A ledger that is missing, unreadable or
/// without the table is "not frozen".
pub fn is_frozen(n_db: &Path, b: &Path) -> bool {
    let Ok(conn) = crate::meter::open_read_only(n_db) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM legacy_merge WHERE source = ?1 AND tbl = ?2",
        params![source_key(b), FROZEN_TBL],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faults::FaultRecord;
    use crate::legacy::commit::{self, CommitOptions, Moved, Outcome};
    use crate::legacy::retire::Native;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
    const V190: &str = include_str!("../../tests/fixtures/ledger_schema_v1.9.0.sql");

    fn q(p: &Path, sql: &str) {
        Connection::open(p).unwrap().execute_batch(sql).unwrap();
    }

    fn count(p: &Path, table: &str) -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn has_table(p: &Path, table: &str) -> bool {
        Connection::open(p)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    }

    fn kinds(p: &Path) -> Vec<String> {
        let c = Connection::open(p).unwrap();
        let mut st = c.prepare("SELECT kind FROM faults ORDER BY kind").unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn line(kind: &str) -> String {
        serde_json::to_string(&FaultRecord::now(kind, "v")).unwrap()
    }

    /// Rows for every table both schemas have, tagged so each row is told apart.
    fn rows_for(tag: &str, v190: bool) -> String {
        let mut s = format!(
            "INSERT INTO turns (message_id, session_id, ts, input_tokens, output_tokens, \
               cache_read_input_tokens, cache_creation_input_tokens) \
               VALUES ('m-{tag}', 's', '2026-01-01T00:00:00Z', 1, 2, 3, 4); \
             INSERT INTO calibration VALUES ('m-{tag}', 10, 12); \
             INSERT INTO session_events (session_id, kind, uuid, ts_ms) \
               VALUES ('s', 'compaction', 'u-{tag}', 5); \
             INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
               routed_via) VALUES ('t-{tag}', 'Read', '/{tag}', 1, 2, 1, 'builtin_read'); \
             INSERT INTO faults (ts, kind, session_id) VALUES ('t-{tag}', 'row-{tag}', 's');"
        );
        if v190 {
            s += &format!(
                "INSERT INTO intercept_blocks (tool_use_id, session_id, path, kind, ts_ms, uuid) \
                   VALUES ('tu-{tag}', 's', '/{tag}', 'redirect', 1, 'u'); \
                 INSERT INTO transcript_scans (source_file, session_id, matcher, through_ms) \
                   VALUES ('f-{tag}', 's', 1, 1);"
            );
        }
        s
    }

    const TABLES: [&str; 7] = [
        "turns",
        "calibration",
        "session_events",
        "intercept_blocks",
        "transcript_scans",
        "read_events",
        "faults",
    ];

    /// A migrated and moved install, built by the real migration: N, the backup, the marker.
    struct Install {
        _t: TempDir,
        n: PathBuf,
        b: PathBuf,
        m: MarkerV2,
        home: PathBuf,
    }

    impl Install {
        fn n_db(&self) -> PathBuf {
            self.n.join("lumen.db")
        }
        fn b_db(&self) -> PathBuf {
            self.b.join(DATA_DIR).join("lumen.db")
        }
        fn spool(&self) -> PathBuf {
            self.b.join(DATA_DIR).join("faults.jsonl")
        }
        fn draining(&self) -> PathBuf {
            self.b.join(DATA_DIR).join("faults.jsonl.draining")
        }
        fn setup_done(&self) -> PathBuf {
            self.home.join(".setup_done")
        }
        fn record_setup(&self) {
            fs::write(
                self.setup_done(),
                format!("data_dir={}\n", self.n.display()),
            )
            .unwrap();
        }
        fn merge(&self) -> MergeReport {
            merge_backup(&self.n_db(), &self.b, &self.m).unwrap()
        }
        fn freeze(&self) -> Freeze {
            freeze_if_ready(&self.n, &self.setup_done(), &self.m)
        }
        /// Open the backup's ledger for writing again, as an old binary would have before the
        /// freeze (or a person after it).
        fn write_to_backup(&self, sql: &str) {
            make_writable(&self.b.join(DATA_DIR));
            q(&self.b_db(), sql);
        }
    }

    fn make_writable(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                make_writable(&p);
            } else {
                let mut perms = fs::metadata(&p).unwrap().permissions();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    perms.set_mode(0o644);
                }
                #[cfg(not(unix))]
                {
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                }
                fs::set_permissions(&p, perms).unwrap();
            }
        }
    }

    fn install(ddl: &str, v190: bool) -> Install {
        let t = TempDir::new().unwrap();
        let root = t.path().join("data");
        let l = root.join("example.previous.app");
        let n = root.join("example.current.app");
        fs::create_dir_all(l.join("bin")).unwrap();
        fs::write(l.join("bin").join("lumen-mcp"), b"#!/bin/sh\n").unwrap();
        let ledger = l.join("lumen.db");
        Connection::open(&ledger)
            .unwrap()
            .execute_batch(ddl)
            .unwrap();
        q(&ledger, &rows_for("snap", v190));
        q(&ledger, &rows_for("snap2", v190));
        let home = t.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
        o.probe_tries = 1;
        o.probe_delay = Duration::from_millis(10);
        let m = match commit::migrate(&l, &n, &o, &Native) {
            Outcome::Migrated { marker, moved } => {
                assert_eq!(moved, Moved::Done);
                marker
            }
            other => panic!("expected Migrated, got {other:?}"),
        };
        let b = backup_dir(&n, &m).unwrap();
        Install {
            _t: t,
            n,
            b,
            m,
            home,
        }
    }

    // ---- merges before the freeze ----

    #[test]
    fn merges_before_the_freeze_take_rows_only_and_the_spool_waits() {
        let i = install(V190, true);
        fs::write(i.spool(), format!("{}\n", line("waiting"))).unwrap();
        fs::write(i.draining(), format!("{}\n", line("drain-waiting"))).unwrap();
        let spool = fs::read(i.spool()).unwrap();
        let before = count(&i.n_db(), "faults");
        i.write_to_backup(&rows_for("late", true));

        let r = i.merge();
        assert_eq!(r.rows.get("faults"), Some(&1), "{r:?}");
        assert_eq!(count(&i.n_db(), "faults"), before + 1);
        assert!(!kinds(&i.n_db()).contains(&"waiting".to_string()));
        assert!(!kinds(&i.n_db()).contains(&"drain-waiting".to_string()));
        assert_eq!(
            fs::read(i.spool()).unwrap(),
            spool,
            "the spool is not touched"
        );
        assert!(
            !i.n.join("faults.jsonl").exists(),
            "nothing reaches N's live spool"
        );
        assert!(!is_frozen(&i.n_db(), &i.b));
        assert!(!i.b.join(FROZEN_STAMP).exists());
    }

    #[test]
    fn merging_twice_adds_nothing() {
        for (ddl, v190) in [(V180, false), (V190, true)] {
            let i = install(ddl, v190);
            let late = rows_for("late", v190);
            i.write_to_backup(&late);

            let first = i.merge();
            let late_tables: BTreeSet<&str> = first.rows.keys().map(String::as_str).collect();
            let expect: BTreeSet<&str> = TABLES
                .iter()
                .copied()
                .filter(|t| v190 || !matches!(*t, "intercept_blocks" | "transcript_scans"))
                .collect();
            assert_eq!(late_tables, expect, "v190={v190} {first:?}");
            assert!(first.rows.values().all(|n| *n == 1), "{first:?}");

            let n_counts: Vec<i64> = TABLES
                .iter()
                .map(|t| {
                    if has_table(&i.n_db(), t) {
                        count(&i.n_db(), t)
                    } else {
                        -1
                    }
                })
                .collect();
            for (t, n) in TABLES.iter().zip(&n_counts) {
                if has_table(&i.b_db(), t) {
                    assert_eq!(
                        *n,
                        count(&i.b_db(), t),
                        "{t}: every backup row is in N once"
                    );
                }
            }
            let second = i.merge();
            assert_eq!(second.total_rows(), 0, "v190={v190} {second:?}");
            let after: Vec<i64> = TABLES
                .iter()
                .map(|t| {
                    if has_table(&i.n_db(), t) {
                        count(&i.n_db(), t)
                    } else {
                        -1
                    }
                })
                .collect();
            assert_eq!(after, n_counts, "v190={v190}");
        }
    }

    #[test]
    fn a_backup_on_the_1_8_0_schema_takes_the_columns_it_lacks_as_defaults() {
        let i = install(V180, false);
        i.write_to_backup(&rows_for("late", false));
        let r = i.merge();
        assert!(r.notes.is_empty(), "{r:?}");
        let c = Connection::open(i.n_db()).unwrap();
        let geo: Option<String> = c
            .query_row(
                "SELECT inference_geo FROM turns WHERE message_id = 'm-late'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(geo, None);
        // The tables 1.8.0 did not have are there, empty, and nothing was invented for them.
        assert_eq!(count(&i.n_db(), "intercept_blocks"), 0);
        assert_eq!(count(&i.n_db(), "transcript_scans"), 0);
    }

    #[test]
    fn the_first_merge_after_a_real_upgrade_brings_the_ledger_to_the_current_schema() {
        let i = install(V190, true);
        // As the app has it: nothing has opened the migrated ledger since the copy.
        assert!(
            !has_table(&i.n_db(), "legacy_merge"),
            "the copy is on the old schema"
        );
        i.write_to_backup(&rows_for("late", true));
        let r = i.merge();
        assert_eq!(r.rows.get("read_events"), Some(&1), "{r:?}");
        assert!(has_table(&i.n_db(), "legacy_merge"));
        assert!(!has_table(&i.n_db(), "legacy_spool_intent"));
        let c = Connection::open(i.n_db()).unwrap();
        assert!(!crate::meter::live_shape(&c).unwrap().lacks_something());
        assert_eq!(count(&i.n_db(), "turns"), 3);
    }

    #[test]
    fn the_ledger_has_one_set_of_tables_through_every_opener() {
        let t = TempDir::new().unwrap();
        let via_rusqlite = t.path().join("a.db");
        drop(crate::meter::connect_db(&via_rusqlite).unwrap());
        let via_sqlx = t.path().join("b.db");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let pool = crate::schema::open_pool(&format!("sqlite:{}?mode=rwc", via_sqlx.display()))
                .await
                .unwrap();
            pool.close().await;
        });
        let names = |p: &Path| -> BTreeSet<String> {
            let c = Connection::open(p).unwrap();
            let mut st = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
                .unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let a = names(&via_rusqlite);
        assert_eq!(a, names(&via_sqlx));
        assert!(a.contains("legacy_merge"));
        assert!(
            !a.contains("legacy_spool_intent"),
            "the freeze keeps no spool intent"
        );
    }

    // ---- faults ----

    /// Faults, old binary's view: some drained into backup rows past the mark, some still in the
    /// spool, some in a `.draining` batch a drain was killed on.
    fn faulty() -> Install {
        let i = install(V190, true);
        i.write_to_backup(
            "INSERT INTO faults (ts, kind, session_id) VALUES ('t', 'drained-1', 's'); \
             INSERT INTO faults (ts, kind, session_id) VALUES ('t', 'drained-2', 's');",
        );
        fs::write(
            i.spool(),
            format!("{}\n{}\n", line("spool-1"), line("spool-2")),
        )
        .unwrap();
        fs::write(i.draining(), format!("{}\n", line("draining-1"))).unwrap();
        i
    }

    #[test]
    fn a_fault_reaches_the_new_ledger_once() {
        let i = faulty();
        // Merges at every start before Setup, as many as the user has starts.
        i.merge();
        i.merge();
        i.record_setup();
        match i.freeze() {
            Freeze::Frozen(r) => assert_eq!(r.spool_records, 3, "{r:?}"),
            other => panic!("{other:?}"),
        }
        // Later starts, and a second freeze, add nothing.
        i.merge();
        assert!(matches!(i.freeze(), Freeze::Already { .. }));

        let mut want = vec![
            "draining-1".to_string(),
            "drained-1".into(),
            "drained-2".into(),
            "row-snap".into(),
            "row-snap2".into(),
            "spool-1".into(),
            "spool-2".into(),
        ];
        want.sort();
        assert_eq!(kinds(&i.n_db()), want, "each fault is in N exactly once");
        assert!(
            !i.n.join("faults.jsonl").exists(),
            "N's live spool is not appended to"
        );
    }

    #[test]
    fn the_spool_is_taken_once_at_the_freeze_with_draining_and_past_the_drain_cap() {
        let i = install(V190, true);
        let total = faults::DRAIN_LINE_CAP + 7;
        let mut spool = String::new();
        for k in 0..total {
            spool += &format!("{}\n", line(&format!("s{k:06}")));
        }
        let drain = format!("{}\n{}\n", line("d-1"), line("d-2"));
        fs::write(i.spool(), &spool).unwrap();
        fs::write(i.draining(), &drain).unwrap();
        let before = count(&i.n_db(), "faults");
        i.record_setup();

        let Freeze::Frozen(r) = i.freeze() else {
            panic!("expected Frozen");
        };
        assert_eq!(r.spool_records, total + 2);
        assert_eq!(count(&i.n_db(), "faults"), before + (total + 2) as i64);
        let c = Connection::open(i.n_db()).unwrap();
        let distinct: i64 = c
            .query_row(
                "SELECT count(DISTINCT kind) FROM faults WHERE kind LIKE 's0%' OR kind LIKE 'd-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            distinct,
            (total + 2) as i64,
            "no line past the cap is left behind"
        );
        // What the freeze says it took: the line count and a digest of the bytes.
        let (lines, sig): (i64, String) = c
            .query_row(
                "SELECT hwm, sig FROM legacy_merge WHERE source = ?1 AND tbl = '$frozen'",
                [source_key(&i.b)],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(lines as usize, total + 2);
        assert_eq!(sig, hex(format!("{drain}{spool}").as_bytes()));
        // Once: the spool does not come again.
        assert!(matches!(i.freeze(), Freeze::Already { .. }));
        assert_eq!(count(&i.n_db(), "faults"), before + (total + 2) as i64);
    }

    #[test]
    fn a_partial_last_line_stays_in_the_backup() {
        let i = install(V190, true);
        let spool = format!("{}\n{{\"ts\":\"t\",\"kind\":\"cut", line("whole"));
        fs::write(i.spool(), &spool).unwrap();
        i.record_setup();
        let Freeze::Frozen(r) = i.freeze() else {
            panic!("expected Frozen");
        };
        assert_eq!(r.spool_records, 1);
        assert!(kinds(&i.n_db()).contains(&"whole".to_string()));
        assert!(!kinds(&i.n_db()).iter().any(|k| k.starts_with("cut")));
        assert_eq!(
            fs::read_to_string(i.spool()).unwrap(),
            spool,
            "the backup keeps the bytes, partial line included"
        );
    }

    // ---- after the freeze ----

    #[test]
    fn nothing_is_merged_after_the_freeze() {
        let i = faulty();
        i.record_setup();
        assert!(matches!(i.freeze(), Freeze::Frozen(_)));
        let snapshot: Vec<i64> = TABLES.iter().map(|t| count(&i.n_db(), t)).collect();

        // Anything that lands in the backup now, by whatever means, is not carried.
        i.write_to_backup(&rows_for("after", true));
        fs::write(i.spool(), format!("{}\n", line("after-spool"))).unwrap();
        let r = i.merge();
        assert!(r.was_frozen, "{r:?}");
        assert_eq!(r.total_rows(), 0);
        let again: Vec<i64> = TABLES.iter().map(|t| count(&i.n_db(), t)).collect();
        assert_eq!(again, snapshot);
        // The freeze is noticed, and says the backup changed afterwards.
        assert_eq!(i.freeze(), Freeze::Already { changed: true });
        let again: Vec<i64> = TABLES.iter().map(|t| count(&i.n_db(), t)).collect();
        assert_eq!(again, snapshot);
    }

    #[test]
    fn a_freeze_killed_before_its_commit_redoes_it_once() {
        let i = faulty();
        i.merge();
        i.record_setup();
        let rows = |i: &Install| count(&i.n_db(), "faults");
        let before = rows(&i);
        {
            let lock = take_lock(&i.n).unwrap();
            let out = freeze_with(&lock, &i.n, &i.setup_done(), &i.m, Stop::BeforeCommit);
            let Freeze::Frozen(r) = out else {
                panic!("{out:?}")
            };
            assert!(r.stopped && r.spool_records == 3, "{r:?}");
        }
        assert_eq!(rows(&i), before, "nothing was committed");
        assert!(!is_frozen(&i.n_db(), &i.b));
        assert!(!i.b.join(FROZEN_STAMP).exists());

        let Freeze::Frozen(r) = i.freeze() else {
            panic!("expected Frozen");
        };
        assert_eq!(r.spool_records, 3);
        assert_eq!(rows(&i), before + 3, "each spool line once, not twice");
        assert!(i.b.join(FROZEN_STAMP).exists());
    }

    #[test]
    fn a_freeze_killed_after_its_commit_merges_nothing_more() {
        let i = faulty();
        i.record_setup();
        {
            let lock = take_lock(&i.n).unwrap();
            let out = freeze_with(&lock, &i.n, &i.setup_done(), &i.m, Stop::AfterCommit);
            let Freeze::Frozen(r) = out else {
                panic!("{out:?}")
            };
            assert!(r.stopped && r.frozen_now, "{r:?}");
        }
        assert!(is_frozen(&i.n_db(), &i.b));
        assert!(!i.b.join(FROZEN_STAMP).exists(), "killed before the stamp");
        let faults_in_n = count(&i.n_db(), "faults");

        // The next start, with the backup changed in between.
        i.write_to_backup(&rows_for("between", true));
        fs::write(i.spool(), format!("{}\n", line("between-spool"))).unwrap();
        assert!(i.merge().was_frozen);
        assert!(matches!(i.freeze(), Freeze::Already { .. }));
        assert_eq!(
            count(&i.n_db(), "faults"),
            faults_in_n,
            "nothing more was merged"
        );
        assert!(i.b.join(FROZEN_STAMP).exists(), "the stamp is made good");
        assert!(
            read_only_all(&i.b.join(DATA_DIR)),
            "and so are the read-only bits"
        );
    }

    fn read_only_all(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().all(|e| {
            let p = e.unwrap().path();
            if p.is_dir() {
                return read_only_all(&p);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&p).unwrap().permissions().mode() & 0o777 == 0o444
            }
            #[cfg(not(unix))]
            {
                fs::metadata(&p).unwrap().permissions().readonly()
            }
        })
    }

    #[test]
    fn the_backup_is_read_only_after_the_freeze() {
        let i = install(V190, true);
        fs::write(i.spool(), format!("{}\n", line("x"))).unwrap();
        i.record_setup();
        assert!(!read_only_all(&i.b.join(DATA_DIR)), "writable before");
        assert!(matches!(i.freeze(), Freeze::Frozen(_)));
        assert!(read_only_all(&i.b.join(DATA_DIR)));
        assert!(fs::metadata(i.b.join(DATA_DIR).join("bin").join("lumen-mcp")).is_ok());
        // Directories stay writable, so the backup can be removed.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(i.b.join(DATA_DIR))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o200, 0, "directories keep their write bit");
        }
        fs::remove_dir_all(&i.b).unwrap();
    }

    // ---- a replaced or interrupted backup ----

    #[test]
    fn a_rewound_backup_stops_the_merge_and_says_so() {
        // Replaced by a shorter ledger.
        let i = install(V190, true);
        i.write_to_backup(&rows_for("late", true));
        assert_eq!(i.merge().rows.get("read_events"), Some(&1));
        let n_before = count(&i.n_db(), "read_events");
        fs::remove_file(i.b_db()).unwrap();
        let fresh = i.b_db();
        Connection::open(&fresh)
            .unwrap()
            .execute_batch(V190)
            .unwrap();
        q(&fresh, &rows_for("other", true));
        let r = i.merge();
        assert_eq!(
            r.rewound,
            vec!["read_events".to_string(), "faults".to_string()],
            "{r:?}"
        );
        assert_eq!(r.total_rows(), 0);
        assert_eq!(count(&i.n_db(), "read_events"), n_before);
        i.record_setup();
        assert_eq!(
            i.freeze(),
            Freeze::Rewound(vec!["read_events".into(), "faults".into()])
        );
        assert!(
            !is_frozen(&i.n_db(), &i.b),
            "a replaced backup is not frozen"
        );

        // Replaced by a ledger as long as the old one, with other rows.
        let i = install(V190, true);
        i.merge();
        q(&i.b_db(), "UPDATE read_events SET path = '/replaced'");
        let r = i.merge();
        assert_eq!(r.rewound, vec!["read_events".to_string()], "{r:?}");
        assert_eq!(r.total_rows(), 0);
    }

    #[test]
    fn a_column_the_backup_gained_after_the_mark_is_not_a_rewind() {
        let i = install(V180, false);
        // An old binary of a newer schema opened the folder before the move succeeded.
        i.write_to_backup("ALTER TABLE read_events ADD COLUMN extra_after_mark TEXT");
        i.write_to_backup(&rows_for("late", false));
        let r = i.merge();
        assert!(r.rewound.is_empty(), "{r:?}");
        assert_eq!(r.rows.get("read_events"), Some(&1));
        assert!(
            r.notes.iter().any(|n| n.contains("extra_after_mark")),
            "{r:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hot_journal_in_the_backup_is_rolled_back_before_the_merge() {
        let i = install(V190, true);
        i.write_to_backup(&rows_for("committed", true));
        // An old binary dies mid-write: a journal and a half-written ledger are left behind.
        let scratch = i._t.path().join("hot");
        fs::create_dir_all(&scratch).unwrap();
        let c = Connection::open(i.b_db()).unwrap();
        c.execute_batch("PRAGMA synchronous=OFF; PRAGMA cache_size=1; BEGIN")
            .unwrap();
        for k in 0..400 {
            c.execute(
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                 saved_tokens, routed_via) VALUES (?1, 'Read', ?2, 1, 2, 1, 'builtin_read')",
                params![format!("hot{k}"), "x".repeat(900)],
            )
            .unwrap();
        }
        let journal = {
            let mut s = i.b_db().into_os_string();
            s.push("-journal");
            PathBuf::from(s)
        };
        assert!(journal.is_file() && fs::metadata(&journal).unwrap().len() > 0);
        fs::copy(i.b_db(), scratch.join("lumen.db")).unwrap();
        fs::copy(&journal, scratch.join("lumen.db-journal")).unwrap();
        c.execute_batch("ROLLBACK").unwrap();
        drop(c);
        fs::copy(scratch.join("lumen.db"), i.b_db()).unwrap();
        fs::copy(scratch.join("lumen.db-journal"), &journal).unwrap();
        assert!(journal.is_file(), "the backup holds a hot journal");

        let before = count(&i.n_db(), "read_events");
        let r = i.merge();
        assert_eq!(
            r.rows.get("read_events"),
            Some(&1),
            "only what was committed is merged: {r:?}"
        );
        assert_eq!(count(&i.n_db(), "read_events"), before + 1);
        assert!(!journal.exists() || fs::metadata(&journal).unwrap().len() == 0);
    }

    // ---- the gate ----

    #[test]
    fn the_freeze_waits_for_setup_to_name_the_new_folder() {
        let i = install(V190, true);
        let not_ready = |i: &Install| matches!(i.freeze(), Freeze::NotReady(_));
        assert!(not_ready(&i), "no Setup record");
        fs::write(i.setup_done(), "").unwrap();
        assert!(
            not_ready(&i),
            "an install from before the migration records nothing"
        );
        fs::write(i.setup_done(), "data_dir=/somewhere/else\n").unwrap();
        assert!(not_ready(&i), "another folder");
        assert!(!is_frozen(&i.n_db(), &i.b));
        i.record_setup();
        assert!(matches!(i.freeze(), Freeze::Frozen(_)));
    }

    #[test]
    fn the_freeze_waits_for_the_move() {
        let i = install(V190, true);
        i.record_setup();
        // The state the move leaves when it is refused: the previous folder is where it was.
        let l = PathBuf::from(&i.m.source_dir);
        fs::rename(i.b.join(DATA_DIR), &l).unwrap();
        assert!(matches!(i.freeze(), Freeze::NotReady(why) if why.contains("moved")));
        assert!(!is_frozen(&i.n_db(), &i.b));
    }

    #[test]
    fn a_previous_folder_that_is_gone_freezes_with_nothing_to_take() {
        let i = install(V190, true);
        i.record_setup();
        fs::remove_dir_all(&i.b).unwrap();
        let rows = count(&i.n_db(), "read_events");
        let Freeze::Frozen(r) = i.freeze() else {
            panic!("expected Frozen");
        };
        assert_eq!((r.total_rows(), r.spool_records), (0, 0));
        assert!(is_frozen(&i.n_db(), &i.b));
        assert_eq!(count(&i.n_db(), "read_events"), rows);
        assert!(!i.b.exists(), "no backup is invented");
        assert!(matches!(i.freeze(), Freeze::Already { changed: false }));
    }

    #[test]
    fn a_held_lock_is_not_ready_and_changes_nothing() {
        let i = install(V190, true);
        i.record_setup();
        let lock = take_lock(&i.n).unwrap();
        assert!(matches!(i.freeze(), Freeze::NotReady(_)));
        assert!(!is_frozen(&i.n_db(), &i.b));
        assert!(matches!(
            freeze_if_ready_under(&lock, &i.n, &i.setup_done(), &i.m),
            Freeze::Frozen(_)
        ));
    }

    #[test]
    fn the_setup_record_is_read_by_folder_not_by_text() {
        let t = TempDir::new().unwrap();
        let n = t.path().join("example.current.app");
        let rec = t.path().join(".setup_done");
        for (text, want) in [
            (format!("data_dir={}\n", n.display()), true),
            (format!("data_dir={}/\n", n.display()), true),
            (format!("other=1\r\ndata_dir={}\r\n", n.display()), true),
            (format!("data_dir={}-2\n", n.display()), false),
            ("data_dir=\n".to_string(), false),
            (String::new(), false),
        ] {
            fs::write(&rec, &text).unwrap();
            assert_eq!(setup_names(&rec, &n), want, "{text:?}");
        }
        assert!(!setup_names(&t.path().join("missing"), &n));
    }
}
