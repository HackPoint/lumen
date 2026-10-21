//! The seam for renames, the backup's name, and the `com.tauri.dev` source.
//!
//! - [`Ops`]: the one filesystem operation the moves go through, so a test can refuse it. [`Native`]
//!   is the real one.
//! - [`backup_name`]: where a migration's backup goes.
//! - [`copy_ledger_verified`] and [`import_ledger_only`]: the ledger-only source (`com.tauri.dev`, macOS).

use std::io;
use std::path::{Path, PathBuf};

/// The filesystem operations the moves need, as a seam.
pub trait Ops {
    /// Rename a file or directory. A failure (on Windows: it is in use) defers the move.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    /// A guess whether a rename of `from` to somewhere under `to` would cross volumes. Advisory:
    /// the manual import words a dry run's report by it. It never decides whether a source is
    /// written (a bind mount shows one device and still answers EXDEV); the rename does.
    fn crosses_volumes(&self, from: &Path, to: &Path) -> bool {
        on_another_volume(from, to)
    }
}

/// The nearest ancestor of `p` (or `p`) that exists.
fn existing_ancestor(p: &Path) -> Option<&Path> {
    p.ancestors().find(|a| a.symlink_metadata().is_ok())
}

#[cfg(unix)]
fn on_another_volume(from: &Path, to: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| {
        existing_ancestor(p)
            .and_then(|a| a.metadata().ok())
            .map(|m| m.dev())
    };
    matches!((dev(from), dev(to)), (Some(a), Some(b)) if a != b)
}

#[cfg(windows)]
fn on_another_volume(from: &Path, to: &Path) -> bool {
    use std::path::Component;
    let root = |p: &Path| {
        existing_ancestor(p)
            .and_then(|a| std::fs::canonicalize(a).ok())
            .and_then(|c| match c.components().next() {
                Some(Component::Prefix(x)) => Some(x.as_os_str().to_ascii_lowercase()),
                _ => None,
            })
    };
    matches!((root(from), root(to)), (Some(a), Some(b)) if a != b)
}

#[cfg(not(any(unix, windows)))]
fn on_another_volume(_from: &Path, _to: &Path) -> bool {
    false
}

/// The real filesystem: every operation is the default.
pub struct Native;

impl Ops for Native {}

/// The backup folder for a migration: `<holder>/legacy-backup-<date>`, then `-2`, `-3`, ...
/// until unused. `holder` is the new identifier's folder beside the one being moved (usually
/// N itself); `date` is `YYYY-MM-DD`. Call it once, before the commit, and store the name in
/// the marker.
pub fn backup_name(holder: &Path, date: &str) -> PathBuf {
    unused_name(holder, &format!("legacy-backup-{date}"))
}

pub(super) fn unused_name(parent: &Path, base: &str) -> PathBuf {
    let mut p = parent.join(base);
    let mut n = 2u32;
    while std::fs::symlink_metadata(&p).is_ok() {
        p = parent.join(format!("{base}-{n}"));
        n += 1;
    }
    p
}

/// Whether `source` is for this system.
pub fn applies(source: &super::Source) -> bool {
    match source.os {
        super::Os::MacOs => cfg!(target_os = "macos"),
    }
}

/// What [`copy_ledger_verified`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum LedgerCopy {
    Copied {
        rows: i64,
    },
    /// `to` already exists; it is never overwritten.
    DestinationExists,
    /// There is no ledger at `from`.
    NoSource,
    /// A writer holds the source. Nothing was copied; treat it as a deferral.
    Busy,
}

/// How long to wait on a locked source before reporting [`LedgerCopy::Busy`].
const BUSY_MS: u64 = 200;

enum Fail {
    Busy,
    Other(String),
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

fn step(what: &str, e: rusqlite::Error) -> Fail {
    if is_busy(&e) {
        Fail::Busy
    } else {
        Fail::Other(format!("{what}: {e}"))
    }
}

/// SHA-256 over every row of `table` (all columns, in rowid order, or in column
/// order for a WITHOUT ROWID table), each value tagged with its type.
fn table_digest(c: &rusqlite::Connection, table: &str) -> Result<String, String> {
    use rusqlite::types::ValueRef;
    use sha2::{Digest, Sha256};
    let ident = format!("\"{}\"", table.replace('"', "\"\""));
    let cols = c
        .prepare(&format!("SELECT * FROM {ident}"))
        .map_err(|e| format!("digest {table}: {e}"))?
        .column_count();
    let by_rowid = format!("SELECT * FROM {ident} ORDER BY rowid");
    let by_cols = {
        let order: Vec<String> = (1..=cols).map(|i| i.to_string()).collect();
        format!("SELECT * FROM {ident} ORDER BY {}", order.join(","))
    };
    let mut st = match c.prepare(&by_rowid) {
        Ok(s) => s,
        Err(_) => c
            .prepare(&by_cols)
            .map_err(|e| format!("digest {table}: {e}"))?,
    };
    let mut rows = st.query([]).map_err(|e| format!("digest {table}: {e}"))?;
    let mut h = Sha256::new();
    while let Some(row) = rows.next().map_err(|e| format!("digest {table}: {e}"))? {
        for i in 0..cols {
            match row.get_ref(i).map_err(|e| format!("digest {table}: {e}"))? {
                ValueRef::Null => h.update([0u8]),
                ValueRef::Integer(v) => {
                    h.update([1u8]);
                    h.update(v.to_le_bytes());
                }
                ValueRef::Real(v) => {
                    h.update([2u8]);
                    h.update(v.to_bits().to_le_bytes());
                }
                ValueRef::Text(b) => {
                    h.update([3u8]);
                    h.update((b.len() as u64).to_le_bytes());
                    h.update(b);
                }
                ValueRef::Blob(b) => {
                    h.update([4u8]);
                    h.update((b.len() as u64).to_le_bytes());
                    h.update(b);
                }
            }
        }
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Check a snapshot against its source: `integrity_check` is `ok`, and every
/// table has the same row count and the same content digest. Returns the
/// total rows.
pub(super) fn verify_snapshot(
    src: &rusqlite::Connection,
    snapshot: &rusqlite::Connection,
) -> Result<i64, String> {
    let ok: String = snapshot
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("integrity_check: {e}"))?;
    if ok != "ok" {
        return Err(format!("integrity_check: {ok}"));
    }
    let tables: Vec<String> = src
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .map_err(|e| format!("tables: {e}"))?;
    let mut total = 0;
    for t in tables {
        let q = format!("SELECT count(*) FROM \"{}\"", t.replace('"', "\"\""));
        let a: i64 = src
            .query_row(&q, [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let b: i64 = snapshot
            .query_row(&q, [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if a != b {
            return Err(format!("table {t}: {a} rows at the source, {b} copied"));
        }
        if table_digest(src, &t)? != table_digest(snapshot, &t)? {
            return Err(format!("table {t}: the copied rows differ from the source"));
        }
        total += b;
    }
    Ok(total)
}

/// Copy the ledger `from` to `to` as a consistent snapshot, rows still in
/// `-wal` included, and verify the result before it appears at `to`.
///
/// Interface for T2: it can replace this body with its probe + staged copy as
/// long as the signature and the "never overwrite, never leave a partial `to`"
/// behaviour stay. The source is opened read-only and not modified.
///
/// A snapshot via `VACUUM INTO` into `<to>.import-tmp`, then [`verify_snapshot`]
/// (retried if a writer moved the source in between), then a no-replace
/// publish: `hard_link` fails if `to` exists on every OS and needs no
/// dependency, so a `to` created in the meantime (by the daemon, say) is never
/// replaced. Where hard links are unsupported it falls back to a checked
/// rename, which is then only protected by the caller's migration lock.
/// A locked source gives [`LedgerCopy::Busy`] after ~200 ms, not an error.
pub fn copy_ledger_verified(from: &Path, to: &Path) -> Result<LedgerCopy, String> {
    use rusqlite::{Connection, OpenFlags};
    if std::fs::symlink_metadata(from).is_err() {
        return Ok(LedgerCopy::NoSource);
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Ok(LedgerCopy::DestinationExists);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = PathBuf::from(format!("{}.import-tmp", to.display()));
    let attempt = |tmp: &Path| -> Result<i64, Fail> {
        clear(tmp).map_err(Fail::Other)?; // our own scratch file from a killed run
        let src = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| step(&format!("open {}", from.display()), e))?;
        src.busy_timeout(std::time::Duration::from_millis(BUSY_MS))
            .map_err(|e| step("busy timeout", e))?;
        src.execute("VACUUM INTO ?1", [tmp.to_string_lossy().as_ref()])
            .map_err(|e| step("snapshot", e))?;
        let dst = Connection::open_with_flags(tmp, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| step("open snapshot", e))?;
        verify_snapshot(&src, &dst).map_err(Fail::Other)
    };
    let mut last = String::new();
    for _ in 0..3 {
        match attempt(&tmp) {
            Ok(rows) => return publish(&tmp, to).map(|r| r.unwrap_or(LedgerCopy::Copied { rows })),
            Err(Fail::Busy) => {
                clear(&tmp)?;
                return Ok(LedgerCopy::Busy);
            }
            Err(Fail::Other(e)) => last = e,
        }
    }
    match clear(&tmp) {
        Ok(()) => Err(last),
        Err(e) => Err(format!("{last}; {e}")),
    }
}

/// Move the verified snapshot to `to` without replacing anything.
/// `Ok(Some(DestinationExists))` when `to` appeared meanwhile.
fn publish(tmp: &Path, to: &Path) -> Result<Option<LedgerCopy>, String> {
    match std::fs::hard_link(tmp, to) {
        Ok(()) => {
            clear(tmp)?;
            Ok(None)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            clear(tmp)?;
            Ok(Some(LedgerCopy::DestinationExists))
        }
        Err(_) => {
            // No hard links here: checked rename.
            if std::fs::symlink_metadata(to).is_ok() {
                clear(tmp)?;
                return Ok(Some(LedgerCopy::DestinationExists));
            }
            std::fs::rename(tmp, to).map_err(|e| format!("{}: {e}", to.display()))?;
            Ok(None)
        }
    }
}

/// Remove our own scratch file; absent is fine, anything else is reported.
fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            Err(format!("remove {}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Import a ledger-only source: `<root>/<source.id>/lumen.db` into `new_db`.
/// Leaves the source where it is (`Retire::LeaveInPlace`). The caller decides
/// when to run it (the new ledger is missing) and checks [`applies`].
pub fn import_ledger_only(
    source: &super::Source,
    root: &Path,
    new_db: &Path,
) -> Result<LedgerCopy, String> {
    debug_assert!(matches!(source.take, super::Take::LedgerOnly));
    copy_ledger_verified(&root.join(source.id).join("lumen.db"), new_db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn ledger_with_wal_rows(dir: &Path) -> (Connection, PathBuf) {
        std::fs::create_dir_all(dir).unwrap();
        let db = dir.join("lumen.db");
        let c = Connection::open(&db).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        c.execute_batch("CREATE TABLE turns(id INTEGER PRIMARY KEY, v TEXT);")
            .unwrap();
        c.execute("INSERT INTO turns(v) VALUES ('checkpointed')", [])
            .unwrap();
        c.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
        for i in 0..5 {
            c.execute("INSERT INTO turns(v) VALUES (?1)", [format!("wal{i}")])
                .unwrap();
        }
        assert!(db.with_extension("db-wal").metadata().unwrap().len() > 0);
        (c, db)
    }

    fn count(db: &Path) -> i64 {
        Connection::open(db)
            .unwrap()
            .query_row("SELECT count(*) FROM turns", [], |r| r.get(0))
            .unwrap()
    }

    /// The old lib.rs block: a bare copy of the main file.
    fn old_block_copy(from: &Path, to: &Path) {
        std::fs::copy(from, to).unwrap();
    }

    #[test]
    fn the_old_bare_copy_loses_rows_still_in_the_wal() {
        let root = TempDir::new().unwrap();
        let (_held, db) = ledger_with_wal_rows(&root.path().join("src"));
        let to = root.path().join("old-style.db");
        old_block_copy(&db, &to);
        assert!(count(&to) < 6, "red: the WAL rows are not in the copy");
    }

    #[test]
    fn the_wal_rows_come_with_the_new_copy_and_the_source_stays() {
        let root = TempDir::new().unwrap();
        let src = root.path().join("com.tauri.dev");
        let (_held, db) = ledger_with_wal_rows(&src);
        let before: Vec<_> = ["lumen.db", "lumen.db-wal"]
            .iter()
            .map(|n| std::fs::read(src.join(n)).ok())
            .collect();
        let new_db = root.path().join("new/lumen.db");
        let source = super::super::SOURCES
            .iter()
            .find(|s| s.id == "com.tauri.dev")
            .unwrap();
        let r = import_ledger_only(source, root.path(), &new_db).unwrap();
        assert_eq!(r, LedgerCopy::Copied { rows: 6 });
        assert_eq!(count(&new_db), 6, "green: nothing lost");
        assert!(!new_db.with_extension("db.import-tmp").exists());
        // Left in place and unchanged.
        let after: Vec<_> = ["lumen.db", "lumen.db-wal"]
            .iter()
            .map(|n| std::fs::read(src.join(n)).ok())
            .collect();
        assert_eq!(before, after);
        assert_eq!(count(&db), 6);
    }

    #[test]
    fn an_existing_ledger_is_never_overwritten_and_a_missing_source_is_quiet() {
        let root = TempDir::new().unwrap();
        let (_held, db) = ledger_with_wal_rows(&root.path().join("src"));
        let to = root.path().join("lumen.db");
        std::fs::write(&to, "keep").unwrap();
        assert_eq!(
            copy_ledger_verified(&db, &to),
            Ok(LedgerCopy::DestinationExists)
        );
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "keep");
        assert_eq!(
            copy_ledger_verified(&root.path().join("nope.db"), &root.path().join("x.db")),
            Ok(LedgerCopy::NoSource)
        );
    }

    #[test]
    fn a_source_that_is_not_a_database_leaves_no_partial_file() {
        let root = TempDir::new().unwrap();
        let from = root.path().join("lumen.db");
        std::fs::write(&from, vec![7u8; 4096]).unwrap();
        let to = root.path().join("new.db");
        assert!(copy_ledger_verified(&from, &to).is_err());
        assert!(!to.exists() && !root.path().join("new.db.import-tmp").exists());
        assert_eq!(std::fs::read(&from).unwrap(), vec![7u8; 4096]);
    }

    #[test]
    fn a_snapshot_that_fails_integrity_is_refused() {
        // An index that disagrees with its table: integrity_check reports it
        // without a query error, so only the check itself can catch it.
        let root = TempDir::new().unwrap();
        let p = root.path().join("bad.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE t(a,b); INSERT INTO t VALUES (1,3),(2,2),(3,1);
                 CREATE INDEX i ON t(a);
                 PRAGMA writable_schema=ON;
                 UPDATE sqlite_master SET sql='CREATE INDEX i ON t(b)' WHERE name='i';",
            )
            .unwrap();
        }
        let snap = Connection::open(&p).unwrap();
        let ok: String = snap
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_ne!(ok, "ok", "fixture must be inconsistent");
        let src = Connection::open_in_memory().unwrap();
        let e = verify_snapshot(&src, &snap).unwrap_err();
        assert!(e.starts_with("integrity_check"), "{e}");
    }

    #[test]
    fn a_snapshot_with_a_different_row_count_is_refused() {
        let root = TempDir::new().unwrap();
        let mk = |n: &str, rows: i64| {
            let c = Connection::open(root.path().join(n)).unwrap();
            c.execute_batch("CREATE TABLE turns(id INTEGER PRIMARY KEY);")
                .unwrap();
            for i in 0..rows {
                c.execute("INSERT INTO turns(id) VALUES (?1)", [i]).unwrap();
            }
            c
        };
        let (src, snap) = (mk("s.db", 3), mk("c.db", 2));
        let e = verify_snapshot(&src, &snap).unwrap_err();
        assert!(e.contains("3 rows at the source, 2 copied"), "{e}");
        assert_eq!(verify_snapshot(&src, &mk("d.db", 3)), Ok(3));
    }

    #[test]
    fn a_locked_source_is_busy_quickly_and_leaves_nothing() {
        let root = TempDir::new().unwrap();
        let from = root.path().join("lumen.db");
        let c = Connection::open(&from).unwrap(); // rollback journal: readers are locked out
        c.execute_batch("CREATE TABLE turns(id); INSERT INTO turns VALUES (1); BEGIN EXCLUSIVE;")
            .unwrap();
        let to = root.path().join("new.db");
        let t = std::time::Instant::now();
        assert_eq!(copy_ledger_verified(&from, &to), Ok(LedgerCopy::Busy));
        assert!(t.elapsed() < std::time::Duration::from_secs(3));
        assert!(!to.exists() && !root.path().join("new.db.import-tmp").exists());
        c.execute_batch("COMMIT;").unwrap();
        assert!(matches!(
            copy_ledger_verified(&from, &to),
            Ok(LedgerCopy::Copied { .. })
        ));
    }

    #[test]
    fn a_destination_that_appears_late_is_not_replaced() {
        let root = TempDir::new().unwrap();
        let tmp = root.path().join("x.import-tmp");
        let to = root.path().join("x.db");
        std::fs::write(&tmp, "snapshot").unwrap();
        std::fs::write(&to, "daemon's").unwrap();
        assert_eq!(publish(&tmp, &to), Ok(Some(LedgerCopy::DestinationExists)));
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "daemon's");
        assert!(!tmp.exists());
    }

    #[test]
    fn sources_apply_by_system() {
        for s in super::super::SOURCES.iter() {
            let expect = cfg!(target_os = "macos");
            assert_eq!(applies(s), expect, "{}", s.id);
        }
    }
}
