//! The pieces the migration is built from: the exclusive probe, the byte-exact copy, the
//! stage and lock names, and the set-aside of a file in the way.
//!
//! The orchestration (M1-M11) is [`super::commit`]. L's files are only ever read, with one
//! exception: probing a ledger that is still in WAL mode switches it to rollback-journal mode,
//! a change to its header bytes with every row intact.
//!
//! The ledger is never read through the file system. Our own connection holds it with an
//! exclusive lock, and on Windows SQLite's locks are mandatory byte-range locks (at
//! 0x40000000, 1 GiB), so another handle reading a larger ledger gets ERROR_LOCK_VIOLATION.
//! The staged ledger is a `VACUUM INTO` through that same connection, verified by
//! [`super::retire::verify_snapshot`]. Every other file is checked by SHA-256 of its bytes.

use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the copy is built, inside N.
pub const STAGE_DIR: &str = ".legacy-import";
/// The mutex file, inside N.
pub const LOCK_NAME: &str = ".lumen-migrate.lock";
pub(super) const LEDGER: &str = "lumen.db";

/// A point in the run, passed to the debug-build hook in [`super::commit::CommitOptions`].
#[derive(Debug)]
pub enum Phase<'a> {
    /// One file was copied and synced into the stage.
    Copied { rel: &'a Path },
    /// Every file is staged; verification is next.
    Staged { dir: &'a Path },
    /// One staged file was renamed into N (commit, step M8). Used by [`super::commit`].
    Commit { rel: &'a Path },
    /// The marker is written (M9). Used by [`super::commit`].
    Committed,
    /// L was renamed to `dir`, `B/data` (M11). On Unix the old ledger's exclusive lock is
    /// still held at this point. Used by [`super::commit`].
    Moved { dir: &'a Path },
}

impl Phase<'_> {
    /// The phase's name, as `LUMEN_TEST_MIGRATE_PAUSE` gives it in the app's debug builds.
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Copied { .. } => "copied",
            Phase::Staged { .. } => "staged",
            Phase::Commit { .. } => "commit",
            Phase::Committed => "committed",
            Phase::Moved { .. } => "moved",
        }
    }
}

pub(super) fn is_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || matches!(e.raw_os_error(), Some(28 | 39 | 112))
}

pub(super) fn sha256_file(p: &Path) -> io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Copy `src` to `dst`, sync it, keep its permission bits, and return the SHA-256 of the bytes
/// that were written.
pub(super) fn copy_hashed(src: &Path, dst: &Path) -> io::Result<String> {
    let mut r = File::open(src)?;
    let mut w = File::create(dst)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        w.write_all(&buf[..n])?;
    }
    w.sync_all()?;
    drop(w);
    fs::set_permissions(dst, r.metadata()?.permissions())?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub(super) fn sync_dir(_p: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(_p) {
        let _ = d.sync_all();
    }
}

pub(super) enum Probe {
    Held(String),
    Err(String),
}

/// Take the old ledger exclusively. Leaving WAL needs a lock no other open
/// connection allows, so "still wal" or BUSY means someone has it.
pub(super) fn probe(db: &Path, tries: u32, delay: Duration) -> Result<Connection, Probe> {
    let mut last = String::new();
    for i in 0..tries.max(1) {
        if i > 0 {
            std::thread::sleep(delay);
        }
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| Probe::Err(e.to_string()))?;
        let _ = conn.busy_timeout(Duration::ZERO);
        let attempt = (|| -> rusqlite::Result<Option<String>> {
            let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Ok(Some(format!("journal mode stays {mode}")));
            }
            conn.execute_batch("PRAGMA main.locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
            Ok(None)
        })();
        match attempt {
            Ok(None) => return Ok(conn),
            Ok(Some(why)) => last = why,
            Err(e) => match e.sqlite_error_code() {
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => last = e.to_string(),
                _ => return Err(Probe::Err(e.to_string())),
            },
        }
    }
    Err(Probe::Held(last))
}

pub(super) fn tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    st.query_map([], |r| r.get(0))?.collect()
}

pub(super) fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Highest rowid per table; a WITHOUT ROWID table has none and is left out.
pub(super) fn high_water(conn: &Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
    let mut m = BTreeMap::new();
    for t in tables(conn)? {
        let r = conn.query_row(
            &format!("SELECT coalesce(max(rowid),0) FROM {}", q(&t)),
            [],
            |r| r.get::<_, i64>(0),
        );
        if let Ok(v) = r {
            m.insert(t, v);
        }
    }
    Ok(m)
}

/// Move the file in the way aside. For a ledger its `-wal`, `-shm` and `-journal` go with it,
/// under the same name, because SQLite pairs a WAL with a database by name alone: left behind,
/// they would be replayed onto the ledger that takes its place and the rows in them lost.
///
/// No failed rename leaves the database and its WAL under different names: the moves already
/// made are undone, newest first. (A process killed between two renames is not covered: the
/// sidecars go first, so the database stays whole under its own name.)
pub(super) fn move_aside(dst: &Path, ledger: bool) -> io::Result<()> {
    move_aside_with(dst, ledger, &|from, to| fs::rename(from, to))
}

/// [`move_aside`] with the rename as a parameter, so a test can fail any one of them.
pub(super) fn move_aside_with(
    dst: &Path,
    ledger: bool,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let to = aside(dst, ledger);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut run = || -> io::Result<()> {
        if ledger {
            // The sidecars first, the database last: it is the one that, once moved, ends it.
            for sfx in ["-wal", "-shm", "-journal"] {
                let from = sidecar(dst, sfx);
                if from.symlink_metadata().is_ok() {
                    let target = sidecar(&to, sfx);
                    rename(&from, &target)?;
                    moved.push((from, target));
                }
            }
        }
        rename(dst, &to)
    };
    match run() {
        Ok(()) => Ok(()),
        Err(e) => {
            // Put back what was moved, newest first, so the database is whole again under its name.
            let mut stuck = Vec::new();
            for (from, target) in moved.iter().rev() {
                if let Err(u) = fs::rename(target, from) {
                    stuck.push(format!("{}: {u}", target.display()));
                }
            }
            if stuck.is_empty() {
                Err(e)
            } else {
                Err(io::Error::new(
                    e.kind(),
                    format!("{e}; and could not put back {}", stuck.join(", ")),
                ))
            }
        }
    }
}

pub(super) fn sidecar(p: &Path, sfx: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(sfx);
    PathBuf::from(s)
}

/// A free name beside `p` for a file already in the way; never overwrites. With `ledger`, the
/// names of its sidecars must be free too.
pub(super) fn aside(p: &Path, ledger: bool) -> PathBuf {
    let Some(name) = p.file_name() else {
        return p.with_extension("pre-legacy");
    };
    let base = name.to_string_lossy().into_owned();
    for n in 0u32.. {
        let tag = if n == 0 {
            String::new()
        } else {
            format!("-{n}")
        };
        let cand = p.with_file_name(format!("{base}.pre-legacy{tag}"));
        let free = |c: &Path| c.symlink_metadata().is_err();
        if free(&cand)
            && (!ledger
                || ["-wal", "-shm", "-journal"]
                    .iter()
                    .all(|s| free(&sidecar(&cand, s))))
        {
            return cand;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names the app's debug-build pause reads from its environment: one per phase, all
    /// distinct, so the upgrade workflow can stop the start at any of them.
    #[test]
    fn every_phase_has_its_own_name() {
        let p = Path::new("x");
        let names: Vec<&str> = [
            Phase::Copied { rel: p },
            Phase::Staged { dir: p },
            Phase::Commit { rel: p },
            Phase::Committed,
            Phase::Moved { dir: p },
        ]
        .iter()
        .map(Phase::name)
        .collect();
        assert_eq!(names, ["copied", "staged", "commit", "committed", "moved"]);
    }
    use tempfile::TempDir;

    /// A ledger in WAL mode whose last row is only in the WAL; the connection stays open so the
    /// WAL is not folded in. On Windows SQLite's handles forbid renaming the WAL under it.
    fn ledger_with_wal(dir: &Path) -> (Connection, PathBuf) {
        let p = dir.join("lumen.db");
        let c = Connection::open(&p).unwrap();
        c.pragma_update(None, "journal_mode", "wal").unwrap();
        c.execute_batch(
            "PRAGMA wal_autocheckpoint = 0; CREATE TABLE t (x); INSERT INTO t VALUES (1)",
        )
        .unwrap();
        (c, p)
    }

    fn rows(p: &Path) -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap()
    }

    /// A database and sidecars as plain files, no handle open: what is on disk after a crash. The
    /// bytes say which file is which.
    fn plain_ledger(dir: &Path, with_journal: bool) -> PathBuf {
        let db = dir.join("lumen.db");
        fs::write(&db, b"db").unwrap();
        fs::write(sidecar(&db, "-wal"), b"wal").unwrap();
        fs::write(sidecar(&db, "-shm"), b"shm").unwrap();
        if with_journal {
            fs::write(sidecar(&db, "-journal"), b"journal").unwrap();
        }
        db
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn a_failed_database_rename_keeps_the_database_and_its_wal_together() {
        let t = TempDir::new().unwrap();
        let db = plain_ledger(t.path(), false);
        // The database cannot be renamed (on Windows: another process holds it open); the
        // sidecars before it have been moved by then.
        let fail_db = |from: &Path, to: &Path| {
            if from == db {
                Err(io::Error::other("in use"))
            } else {
                fs::rename(from, to)
            }
        };
        let err = move_aside_with(&db, true, &fail_db).unwrap_err();
        assert!(err.to_string().contains("in use"), "{err}");
        assert_eq!(fs::read(sidecar(&db, "-wal")).unwrap(), b"wal");
        assert_eq!(fs::read(sidecar(&db, "-shm")).unwrap(), b"shm");
        assert_eq!(fs::read(&db).unwrap(), b"db");
        assert_eq!(
            names(t.path()),
            ["lumen.db", "lumen.db-shm", "lumen.db-wal"],
            "nothing is stranded under another name"
        );
    }

    #[test]
    fn a_failed_sidecar_rename_puts_the_ones_already_moved_back() {
        let t = TempDir::new().unwrap();
        let db = plain_ledger(t.path(), true);
        let fail_journal = |from: &Path, to: &Path| {
            if from == sidecar(&db, "-journal") {
                Err(io::Error::other("in use"))
            } else {
                fs::rename(from, to)
            }
        };
        move_aside_with(&db, true, &fail_journal).unwrap_err();
        assert_eq!(
            names(t.path()),
            [
                "lumen.db",
                "lumen.db-journal",
                "lumen.db-shm",
                "lumen.db-wal"
            ]
        );
        assert_eq!(fs::read(sidecar(&db, "-wal")).unwrap(), b"wal");
    }

    /// A real WAL whose ledger is open: Windows refuses to rename the WAL under SQLite's handles,
    /// Unix lets it. Either way the database and its WAL end up together under one name.
    #[test]
    fn a_ledger_held_open_is_never_split_from_its_wal() {
        let t = TempDir::new().unwrap();
        let (conn, db) = ledger_with_wal(t.path());
        let moved = move_aside(&db, true);
        let wal = sidecar(&db, "-wal");
        if moved.is_err() {
            assert!(db.is_file() && wal.is_file(), "nothing moved");
            assert_eq!(rows(&db), 1);
        } else {
            let aside_db = t.path().join("lumen.db.pre-legacy");
            assert!(!db.exists() && !wal.exists());
            assert!(sidecar(&aside_db, "-wal").is_file());
        }
        drop(conn);
    }

    #[test]
    fn a_ledger_moves_aside_with_its_wal_and_nothing_is_left_behind() {
        let t = TempDir::new().unwrap();
        let (conn, db) = ledger_with_wal(t.path());
        drop(conn);
        move_aside(&db, true).unwrap();
        assert!(!db.exists());
        assert!(!sidecar(&db, "-wal").exists());
        let aside_db = t.path().join("lumen.db.pre-legacy");
        assert_eq!(rows(&aside_db), 1);
    }
}
