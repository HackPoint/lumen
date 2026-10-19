//! Steps 6 and 7 of the migration, and the `com.tauri.dev` source.
//!
//! - [`retire`]: after the marker exists, rename the old directory to a dated
//!   backup and link the old path to it. Never deletes. Every call checks what
//!   is already done, so a start after a kill or a failed rename just continues.
//! - [`migrate_logs`]: best effort; a failure is reported, never fatal.
//! - [`import_ledger_only`]: the ledger-only source (`com.tauri.dev`, macOS).
//!
//! The Windows directory junction needs the `junction` crate, which lives in the
//! app crate only (it must not enter lumen-core's runtime dependencies or the
//! hook path). So the link goes through the [`Ops`] trait: the app implements
//! `link_dir` with a junction on Windows and inherits the default `rename`.
//! [`Native`] covers macOS and Linux (a symlink) and refuses on Windows rather
//! than silently skipping the link.

use std::io;
use std::path::{Path, PathBuf};

/// The two filesystem operations retirement needs, as a seam.
pub trait Ops {
    /// Rename a directory. A failure (on Windows: the directory is in use) defers retirement.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    /// Make `link` resolve to the directory `target`: a symlink, or a junction on Windows.
    fn link_dir(&self, link: &Path, target: &Path) -> io::Result<()>;
}

/// Symlink on unix; on Windows it returns `Unsupported` (the app supplies a junction).
pub struct Native;

impl Ops for Native {
    #[cfg(unix)]
    fn link_dir(&self, link: &Path, target: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    fn link_dir(&self, _link: &Path, _target: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory links on this system need the app's junction implementation",
        ))
    }
}

/// `<parent>/<id>.<date>.migrated`, then `...migrated-2`, `-3`, ... until unused.
/// `date` is `YYYY-MM-DD`. Call it once, store the name in the marker, and pass it to [`retire`].
pub fn backup_path(parent: &Path, id: &str, date: &str) -> PathBuf {
    unused_name(parent, &format!("{id}.{date}.migrated"))
}

/// The backup folder for a migration: `<holder>/legacy-backup-<date>`, then `-2`, `-3`, ...
/// until unused. `holder` is the new identifier's folder beside the one being moved (usually
/// N itself); `date` is `YYYY-MM-DD`. Call it once, before the commit, and store the name in
/// the marker.
pub fn backup_name(holder: &Path, date: &str) -> PathBuf {
    unused_name(holder, &format!("legacy-backup-{date}"))
}

fn unused_name(parent: &Path, base: &str) -> PathBuf {
    let mut p = parent.join(base);
    let mut n = 2u32;
    while std::fs::symlink_metadata(&p).is_ok() {
        p = parent.join(format!("{base}-{n}"));
        n += 1;
    }
    p
}

/// What [`retire`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Retired {
    /// Renamed and linked.
    Done,
    /// The old path is already a link; nothing to do.
    AlreadyDone,
    /// Neither the old directory nor a backup to link to exists.
    Nothing,
    /// The directory could not be renamed (in use) or must not be touched; try again next start.
    Deferred(String),
    /// Renamed, but the link is missing; the next start creates it.
    LinkPending(String),
}

/// Retire `old` into `backup` and link `old` -> `backup`. Call only after the
/// marker is committed.
///
/// - `old` is a link: done already.
/// - `old` is a real directory and `backup` is free: rename, then link.
/// - `old` is a real directory and `backup` exists: an old binary re-created it
///   (or a rename was interrupted oddly). It is left alone for the late-row
///   merge, never renamed over the backup.
/// - `old` is missing and `backup` exists: only the link is missing; create it.
///
/// Nothing here deletes.
pub fn retire(old: &Path, backup: &Path, ops: &dyn Ops) -> Retired {
    let old_meta = std::fs::symlink_metadata(old).ok();
    let backup_is_dir = std::fs::symlink_metadata(backup)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false);
    match old_meta {
        Some(m) if m.file_type().is_symlink() => {
            // Done only when the link really reaches the backup. A dangling or
            // misdirected link is not repaired (that would mean removing it).
            match (std::fs::canonicalize(old), std::fs::canonicalize(backup)) {
                (Ok(a), Ok(b)) if a == b => Retired::AlreadyDone,
                (Err(e), _) => Retired::Deferred(format!(
                    "{} is a link that does not resolve: {e}",
                    old.display()
                )),
                _ => Retired::Deferred(format!(
                    "{} is a link, but not to {}",
                    old.display(),
                    backup.display()
                )),
            }
        }
        Some(m) if m.file_type().is_dir() => {
            if std::fs::symlink_metadata(backup).is_ok() {
                return Retired::Deferred(format!(
                    "{} exists again next to its backup {}; left for the merge",
                    old.display(),
                    backup.display()
                ));
            }
            if let Err(e) = ops.rename(old, backup) {
                return Retired::Deferred(format!(
                    "could not rename {} to {}: {e}",
                    old.display(),
                    backup.display()
                ));
            }
            link(old, backup, ops)
        }
        Some(_) => Retired::Deferred(format!("{} is not a directory", old.display())),
        None if backup_is_dir => link(old, backup, ops),
        None => Retired::Nothing,
    }
}

fn link(old: &Path, backup: &Path, ops: &dyn Ops) -> Retired {
    match ops.link_dir(old, backup) {
        Ok(()) => Retired::Done,
        Err(e) => Retired::LinkPending(format!(
            "could not link {} to {}: {e}",
            old.display(),
            backup.display()
        )),
    }
}

/// The old log directory for `id`, where Tauri put it. `None` on Linux, where
/// logs live inside the data directory and travel with it.
pub fn log_dir(id: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        dirs::home_dir().map(|h| h.join("Library/Logs").join(id))
    } else if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join(id).join("logs"))
    } else {
        None
    }
}

/// What [`migrate_logs`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LogReport {
    pub copied: usize,
    pub skipped_existing: usize,
    /// Why the old directory was left where it is, if it was.
    pub not_renamed: Option<String>,
    /// First copy error; the old directory is then kept as it is.
    pub error: Option<String>,
}

/// Copy every file of `old_logs` into `new_logs` (never overwriting), then
/// rename `old_logs` to `<old_logs>.<date>.migrated`. Best effort: errors are
/// in the report. A directory that does not exist is an empty report.
pub fn migrate_logs(old_logs: &Path, new_logs: &Path, date: &str, ops: &dyn Ops) -> LogReport {
    let mut r = LogReport::default();
    match std::fs::symlink_metadata(old_logs) {
        Ok(m) if m.file_type().is_dir() => {}
        _ => return r,
    }
    if let Err(e) = copy_tree(old_logs, new_logs, &mut r) {
        r.error = Some(format!("{}: {e}", old_logs.display()));
        return r;
    }
    let (Some(parent), Some(name)) = (old_logs.parent(), old_logs.file_name()) else {
        return r;
    };
    let target = unused_name(
        parent,
        &format!("{}.{date}.migrated", name.to_string_lossy()),
    );
    if let Err(e) = ops.rename(old_logs, &target) {
        r.not_renamed = Some(format!("{}: {e}", old_logs.display()));
    }
    r
}

fn copy_tree(from: &Path, to: &Path, r: &mut LogReport) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_tree(&src, &dst, r)?;
        } else if ft.is_file() {
            if std::fs::symlink_metadata(&dst).is_ok() {
                r.skipped_existing += 1;
            } else {
                std::fs::copy(&src, &dst)?;
                r.copied += 1;
            }
        }
    }
    Ok(())
}

/// Whether `source` is for this system.
pub fn applies(source: &super::Source) -> bool {
    match source.os {
        super::Os::All => true,
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
    use std::cell::RefCell;
    use tempfile::TempDir;

    const OLD: &str = "fake.old.app";
    const DATE: &str = "2026-10-18";

    fn mkdir_with(root: &Path, files: &[(&str, &str)]) -> PathBuf {
        let d = root.join(OLD);
        for (n, c) in files {
            let p = d.join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        d
    }

    /// Records calls; can fail the rename like a directory that is in use.
    struct Fake {
        fail_rename: bool,
        fail_link: RefCell<bool>,
        calls: RefCell<Vec<&'static str>>,
    }
    impl Fake {
        fn new(fail_rename: bool, fail_link: bool) -> Self {
            Fake {
                fail_rename,
                fail_link: RefCell::new(fail_link),
                calls: RefCell::new(vec![]),
            }
        }
    }
    impl Ops for Fake {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("rename");
            if self.fail_rename {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "in use"));
            }
            std::fs::rename(from, to)
        }
        fn link_dir(&self, link: &Path, target: &Path) -> io::Result<()> {
            self.calls.borrow_mut().push("link");
            if *self.fail_link.borrow() {
                return Err(io::Error::other("no link"));
            }
            Native.link_dir(link, target)
        }
    }

    #[cfg(unix)]
    #[test]
    fn rename_then_link_resolves_to_the_backup() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(
            root.path(),
            &[("lumen.db", "ledger"), ("bin/lumen-mcp", "x")],
        );
        let b = backup_path(root.path(), OLD, DATE);
        assert_eq!(
            b.file_name().unwrap(),
            format!("{OLD}.{DATE}.migrated").as_str()
        );
        assert_eq!(retire(&old, &b, &Native), Retired::Done);
        assert!(
            std::fs::symlink_metadata(&old)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(old.join("lumen.db")).unwrap(),
            "ledger"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("bin/lumen-mcp")).unwrap(),
            "x"
        );
        assert_eq!(
            std::fs::canonicalize(&old).unwrap(),
            std::fs::canonicalize(&b).unwrap()
        );
    }

    #[test]
    fn a_taken_name_gets_a_suffix() {
        let root = TempDir::new().unwrap();
        let first = backup_path(root.path(), OLD, DATE);
        std::fs::create_dir_all(&first).unwrap();
        let second = backup_path(root.path(), OLD, DATE);
        assert_eq!(
            second.file_name().unwrap(),
            format!("{OLD}.{DATE}.migrated-2").as_str()
        );
        std::fs::create_dir_all(&second).unwrap();
        assert!(backup_path(root.path(), OLD, DATE).ends_with(format!("{OLD}.{DATE}.migrated-3")));
    }

    #[cfg(unix)]
    #[test]
    fn a_rerun_with_the_link_in_place_does_nothing() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "l")]);
        let b = backup_path(root.path(), OLD, DATE);
        assert_eq!(retire(&old, &b, &Native), Retired::Done);
        let ops = Fake::new(false, false);
        assert_eq!(retire(&old, &b, &ops), Retired::AlreadyDone);
        assert!(ops.calls.borrow().is_empty(), "no rename, no link");
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_link_is_recreated_next_start() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "l")]);
        let b = backup_path(root.path(), OLD, DATE);
        // First run: rename works, link fails (killed or refused).
        let r = retire(&old, &b, &Fake::new(false, true));
        assert!(matches!(r, Retired::LinkPending(_)), "{r:?}");
        assert!(!old.exists() && b.join("lumen.db").is_file());
        // Next start.
        assert_eq!(retire(&old, &b, &Native), Retired::Done);
        assert_eq!(std::fs::read_to_string(old.join("lumen.db")).unwrap(), "l");
    }

    #[test]
    fn a_directory_in_use_defers_and_keeps_everything_where_it_was() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "l")]);
        let b = backup_path(root.path(), OLD, DATE);
        let ops = Fake::new(true, false);
        let r = retire(&old, &b, &ops);
        assert!(matches!(r, Retired::Deferred(_)), "{r:?}");
        assert_eq!(
            *ops.calls.borrow(),
            vec!["rename"],
            "no link after a failed rename"
        );
        assert!(old.join("lumen.db").is_file() && !b.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_deferred_retire_completes_once_the_directory_is_free() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "l")]);
        let b = backup_path(root.path(), OLD, DATE);
        assert!(matches!(
            retire(&old, &b, &Fake::new(true, false)),
            Retired::Deferred(_)
        ));
        assert_eq!(retire(&old, &b, &Native), Retired::Done);
        assert!(b.join("lumen.db").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_real_refused_rename_defers() {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "l")]);
        let b = backup_path(root.path(), OLD, DATE);
        let ro = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(root.path(), ro).unwrap();
        let r = retire(&old, &b, &Native);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root ignores the mode; only assert when the rename was really refused.
        if matches!(r, Retired::Deferred(_)) {
            assert!(old.join("lumen.db").is_file());
        }
    }

    #[test]
    fn a_recreated_old_directory_is_not_renamed_over_the_backup() {
        let root = TempDir::new().unwrap();
        let old = mkdir_with(root.path(), &[("lumen.db", "late")]);
        let b = root.path().join("b.migrated");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("lumen.db"), "first").unwrap();
        let ops = Fake::new(false, false);
        assert!(matches!(retire(&old, &b, &ops), Retired::Deferred(_)));
        assert!(ops.calls.borrow().is_empty());
        assert_eq!(
            std::fs::read_to_string(old.join("lumen.db")).unwrap(),
            "late"
        );
        assert_eq!(
            std::fs::read_to_string(b.join("lumen.db")).unwrap(),
            "first"
        );
    }

    #[test]
    fn nothing_to_retire_when_neither_exists() {
        let root = TempDir::new().unwrap();
        let ops = Fake::new(false, false);
        assert_eq!(
            retire(&root.path().join(OLD), &root.path().join("b"), &ops),
            Retired::Nothing
        );
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_ever_deleted() {
        let root = TempDir::new().unwrap();
        let files = [
            ("lumen.db", "a"),
            ("lumen.db-wal", "w"),
            ("bin/x", "b"),
            ("logs/l.log", "c"),
        ];
        let old = mkdir_with(root.path(), &files);
        let b = backup_path(root.path(), OLD, DATE);
        let ops = Fake::new(false, true);
        retire(&old, &b, &ops);
        retire(&old, &b, &Native);
        retire(&old, &b, &Native);
        for (n, c) in files {
            assert_eq!(std::fs::read_to_string(b.join(n)).unwrap(), c, "{n}");
            assert_eq!(
                std::fs::read_to_string(old.join(n)).unwrap(),
                c,
                "{n} via the link"
            );
        }
    }

    #[test]
    fn logs_are_copied_and_the_old_directory_renamed() {
        let root = TempDir::new().unwrap();
        let old = root.path().join("Logs").join(OLD);
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("a.log"), "one").unwrap();
        std::fs::write(old.join("sub/b.log"), "two").unwrap();
        let new = root.path().join("Logs").join("fake.new.app");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("a.log"), "already new").unwrap();

        let r = migrate_logs(&old, &new, DATE, &Native);
        assert_eq!((r.copied, r.skipped_existing), (1, 1));
        assert!(r.error.is_none() && r.not_renamed.is_none(), "{r:?}");
        assert_eq!(
            std::fs::read_to_string(new.join("a.log")).unwrap(),
            "already new"
        );
        assert_eq!(
            std::fs::read_to_string(new.join("sub/b.log")).unwrap(),
            "two"
        );
        assert!(!old.exists());
        let kept = root
            .path()
            .join("Logs")
            .join(format!("{OLD}.{DATE}.migrated"));
        assert_eq!(std::fs::read_to_string(kept.join("a.log")).unwrap(), "one");
    }

    #[test]
    fn a_log_failure_is_reported_not_fatal() {
        let root = TempDir::new().unwrap();
        let old = root.path().join("logs");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.log"), "x").unwrap();
        let new = root.path().join("new");
        // Rename refused: copied, kept in place, reported.
        let r = migrate_logs(&old, &new, DATE, &Fake::new(true, false));
        assert_eq!(r.copied, 1);
        assert!(r.not_renamed.is_some() && old.join("a.log").is_file());
        // Copy impossible (the destination's parent is a file): reported, old untouched.
        let blocker = root.path().join("blocker");
        std::fs::write(&blocker, "f").unwrap();
        let r = migrate_logs(&old, &blocker.join("x"), DATE, &Native);
        assert!(r.error.is_some() && old.join("a.log").is_file());
        // No old directory: nothing to say.
        assert_eq!(
            migrate_logs(&root.path().join("none"), &new, DATE, &Native),
            LogReport::default()
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn a_dangling_or_misdirected_link_is_not_success() {
        let root = TempDir::new().unwrap();
        let b = root.path().join("b.migrated");
        std::fs::create_dir_all(&b).unwrap();
        let old = root.path().join(OLD);
        // Dangling.
        std::os::unix::fs::symlink(root.path().join("gone"), &old).unwrap();
        assert!(matches!(retire(&old, &b, &Native), Retired::Deferred(_)));
        std::fs::remove_file(&old).unwrap();
        // Somewhere else.
        let other = root.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, &old).unwrap();
        assert!(matches!(retire(&old, &b, &Native), Retired::Deferred(_)));
        // Untouched.
        assert_eq!(std::fs::read_link(&old).unwrap(), other);
    }

    #[test]
    fn sources_apply_by_system() {
        for s in super::super::SOURCES.iter() {
            let expect = s.os == super::super::Os::All || cfg!(target_os = "macos");
            assert_eq!(applies(s), expect, "{}", s.id);
        }
    }

    #[test]
    fn log_dirs_follow_tauri() {
        let p = log_dir("x.y");
        if cfg!(target_os = "macos") {
            assert!(p.unwrap().ends_with("Library/Logs/x.y"));
        } else if cfg!(windows) {
            assert!(p.unwrap().ends_with("x.y\\logs"));
        } else {
            assert!(p.is_none());
        }
    }
}
