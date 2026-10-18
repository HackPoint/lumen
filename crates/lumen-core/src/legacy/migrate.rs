//! Steps 1-5 of the migration: detect, probe, stage, verify, commit.
//!
//! L's files are only ever read, with one exception: probing a ledger that is
//! still in WAL mode switches it to rollback-journal mode, a change to its header
//! bytes with every row intact. Everything is copied into `N/.legacy-import/`,
//! checked there, and moved into N; the marker written last is the commit point,
//! and [`super::effective`] keys on it. A run that dies before the marker leaves
//! L's rows whole and N unresolved, so a rerun starts over.
//!
//! The ledger is never read through the file system. Our own connection holds it
//! with an exclusive lock, and on Windows SQLite's locks are mandatory byte-range
//! locks (at 0x40000000, 1 GiB), so another handle reading a larger ledger gets
//! ERROR_LOCK_VIOLATION. The staged ledger is a `VACUUM INTO` through that same
//! connection, verified by [`super::retire::verify_snapshot`]: `integrity_check`,
//! per-table counts and per-table content digests. Every other file is checked by
//! SHA-256 of its bytes.
//!
//! Retiring L (rename, link), the late-row merge and the app wiring are not here.

use super::{MARKER_NAME, Marker, Take, read_marker};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the copy is built, inside N.
pub const STAGE_DIR: &str = ".legacy-import";
/// The mutex file, in the directory that holds both L and N.
pub const LOCK_NAME: &str = ".lumen-migrate.lock";
const LEDGER: &str = "lumen.db";
const SPOOL: &str = "faults.jsonl";

/// A point in the run, passed to the debug-build hook in [`Options`].
#[derive(Debug)]
pub enum Phase<'a> {
    /// One file was copied and synced into the stage.
    Copied { rel: &'a Path },
    /// Every file is staged; verification is next.
    Staged { dir: &'a Path },
}

#[derive(Clone)]
pub struct Options {
    /// Identifier of L, written to the marker.
    pub source_id: String,
    /// Name the retire step will give L, written to the marker.
    pub backup: String,
    pub take: Take,
    /// Tries at taking the old ledger before deferring.
    pub probe_tries: u32,
    pub probe_delay: Duration,
    /// Test seam: pause, or alter the stage, at a [`Phase`]. Not in release builds.
    #[cfg(debug_assertions)]
    pub hook: Option<fn(&Phase<'_>)>,
}

impl Options {
    pub fn new(source_id: &str, backup: &str, take: Take) -> Self {
        Options {
            source_id: source_id.into(),
            backup: backup.into(),
            take,
            probe_tries: 5,
            probe_delay: Duration::from_millis(200),
            #[cfg(debug_assertions)]
            hook: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The copy committed; the marker is written.
    Migrated(Marker),
    /// Nothing was changed; keep using L and try again later.
    Deferred { reason: String },
    /// No old directory, or the marker is already there.
    NothingToDo,
    /// Nothing was committed; L's rows are untouched.
    Failed { step: &'static str, error: String },
}

#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn fire(opts: &Options, phase: &Phase<'_>) {
    #[cfg(debug_assertions)]
    if let Some(h) = opts.hook {
        h(phase);
    }
}

fn failed(step: &'static str, e: impl std::fmt::Display) -> Outcome {
    Outcome::Failed {
        step,
        error: e.to_string(),
    }
}

fn is_full(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::StorageFull || matches!(e.raw_os_error(), Some(28 | 39 | 112))
}

/// Run steps 1-5 for `old` (L) into `new` (N). Safe to call again after any outcome.
pub fn migrate(old: &Path, new: &Path, opts: &Options) -> Outcome {
    // Mutex: the OS drops it if we die. It lives beside both directories.
    let root = new.parent().unwrap_or(Path::new("."));
    if let Err(e) = fs::create_dir_all(root) {
        return failed("lock", e);
    }
    let lock = match File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(LOCK_NAME))
    {
        Ok(f) => f,
        Err(e) => return failed("lock", e),
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Outcome::Deferred {
                reason: "another migration is running".into(),
            };
        }
        Err(TryLockError::Error(e)) => return failed("lock", e),
    }
    let out = run(old, new, opts);
    drop(lock);
    out
}

fn run(old: &Path, new: &Path, opts: &Options) -> Outcome {
    // 1. Detect. A symlink is a retired L, not a pending one.
    match fs::symlink_metadata(old) {
        Ok(m) if m.file_type().is_dir() => {}
        _ => return Outcome::NothingToDo,
    }
    if read_marker(new).is_some() || new.join(MARKER_NAME).exists() {
        return Outcome::NothingToDo;
    }
    let stage = new.join(STAGE_DIR);
    // A killed run leaves a stage behind; it is never trusted.
    if stage.exists()
        && let Err(e) = fs::remove_dir_all(&stage)
    {
        return failed("stage", e);
    }
    if let Err(e) = fs::create_dir_all(&stage) {
        return if is_full(&e) {
            Outcome::Deferred {
                reason: format!("no space for the copy: {e}"),
            }
        } else {
            failed("stage", e)
        };
    }
    let out = copy_verify_commit(old, new, &stage, opts);
    if !matches!(out, Outcome::Migrated(_)) {
        let _ = fs::remove_dir_all(&stage);
    }
    out
}

struct Entry {
    rel: PathBuf,
    dir: bool,
}

fn list(old: &Path, take: Take) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    match take {
        Take::LedgerOnly => out.push(Entry {
            rel: LEDGER.into(),
            dir: false,
        }),
        Take::Dir => walk(old, Path::new(""), &mut out)?,
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk(base: &Path, rel: &Path, out: &mut Vec<Entry>) -> io::Result<()> {
    for e in fs::read_dir(base.join(rel))? {
        let e = e?;
        let r = rel.join(e.file_name());
        let t = e.file_type()?;
        if t.is_dir() {
            out.push(Entry {
                rel: r.clone(),
                dir: true,
            });
            walk(base, &r, out)?;
        } else if t.is_file() {
            out.push(Entry { rel: r, dir: false });
        }
        // Symlinks and specials are not Lumen's own files; they stay in L.
    }
    Ok(())
}

fn sha256_file(p: &Path) -> io::Result<String> {
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

/// Copy `src` to `dst`, sync it, keep its permission bits, and return the
/// SHA-256 of the bytes that were written. With `whole_lines` only the bytes up
/// to the last newline are taken: an appender may be mid-line at the end.
fn copy_hashed(src: &Path, dst: &Path, whole_lines: bool) -> io::Result<String> {
    let mut r = File::open(src)?;
    let mut w = File::create(dst)?;
    let mut h = Sha256::new();
    if whole_lines {
        let mut all = Vec::new();
        r.read_to_end(&mut all)?;
        let keep = match all.iter().rposition(|b| *b == b'\n') {
            Some(i) => i + 1,
            None => 0, // no complete line yet: nothing to stage
        };
        h.update(&all[..keep]);
        w.write_all(&all[..keep])?;
    } else {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            w.write_all(&buf[..n])?;
        }
    }
    w.sync_all()?;
    drop(w);
    fs::set_permissions(dst, r.metadata()?.permissions())?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn sync_dir(_p: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(_p) {
        let _ = d.sync_all();
    }
}

enum Probe {
    Held(String),
    Err(String),
}

/// Take the old ledger exclusively. Leaving WAL needs a lock no other open
/// connection allows, so "still wal" or BUSY means someone has it.
fn probe(db: &Path, opts: &Options) -> Result<Connection, Probe> {
    let mut last = String::new();
    for i in 0..opts.probe_tries.max(1) {
        if i > 0 {
            std::thread::sleep(opts.probe_delay);
        }
        let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| Probe::Err(e.to_string()))?;
        let _ = conn.busy_timeout(Duration::ZERO);
        let attempt = (|| -> rusqlite::Result<Option<String>> {
            let mode: String = conn.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
            if !mode.eq_ignore_ascii_case("delete") {
                return Ok(Some(format!("journal mode stays {mode}")));
            }
            conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
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

fn tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    st.query_map([], |r| r.get(0))?.collect()
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Highest rowid per table; a WITHOUT ROWID table has none and is left out.
fn high_water(conn: &Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
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

fn copy_verify_commit(old: &Path, new: &Path, stage: &Path, opts: &Options) -> Outcome {
    // 2. Probe.
    let ledger = old.join(LEDGER);
    let held = if ledger.is_file() {
        match probe(&ledger, opts) {
            Ok(c) => Some(c),
            Err(Probe::Held(why)) => {
                return Outcome::Deferred {
                    reason: format!("the old ledger is in use ({why})"),
                };
            }
            Err(Probe::Err(e)) => return failed("probe", e),
        }
    } else {
        None
    };

    // 3. Stage, while the exclusive lock holds the ledger still.
    let mut entries = match list(old, opts.take) {
        Ok(v) => v,
        Err(e) => return failed("stage", e),
    };
    if held.is_some() {
        // The ledger and its sidecars are staged by SQLite, below, not by file copy.
        entries.retain(|e| {
            let n = e.rel.to_string_lossy();
            !["", "-wal", "-shm", "-journal"]
                .iter()
                .any(|sfx| n == format!("{LEDGER}{sfx}"))
        });
    }
    // The sum is of the bytes read from L, so a file a hook is appending to (the
    // spool) is not a false alarm: the stage must equal what was read.
    let mut want: BTreeMap<PathBuf, String> = BTreeMap::new();
    for e in &entries {
        let dst = stage.join(&e.rel);
        let step = (|| -> io::Result<Option<String>> {
            if e.dir {
                fs::create_dir_all(&dst)?;
                return Ok(None);
            }
            if let Some(p) = dst.parent() {
                fs::create_dir_all(p)?;
            }
            copy_hashed(&old.join(&e.rel), &dst, e.rel == Path::new(SPOOL)).map(Some)
        })();
        match step {
            Ok(Some(h)) => {
                want.insert(e.rel.clone(), h);
                fire(opts, &Phase::Copied { rel: &e.rel });
            }
            Ok(None) => {}
            Err(err) if is_full(&err) => {
                return Outcome::Deferred {
                    reason: format!("no space for the copy: {err}"),
                };
            }
            Err(err) => return failed("stage", format!("{}: {err}", e.rel.display())),
        }
    }
    if let Some(src) = &held {
        let target = stage.join(LEDGER);
        match src.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()]) {
            Ok(_) => {}
            Err(e) if e.sqlite_error_code() == Some(ErrorCode::DiskFull) => {
                return Outcome::Deferred {
                    reason: format!("no space for the copy: {e}"),
                };
            }
            Err(e) => return failed("stage", format!("snapshot of the ledger: {e}")),
        }
        match File::open(&target).and_then(|f| f.sync_all()) {
            Ok(()) => {}
            Err(e) => return failed("stage", e),
        }
        entries.push(Entry {
            rel: LEDGER.into(),
            dir: false,
        });
        entries.sort_by(|a, b| a.rel.cmp(&b.rel));
        fire(
            opts,
            &Phase::Copied {
                rel: Path::new(LEDGER),
            },
        );
    }
    sync_dir(stage);
    fire(opts, &Phase::Staged { dir: stage });

    // 4. Verify.
    for (rel, sum) in &want {
        match sha256_file(&stage.join(rel)) {
            Ok(got) if got == *sum => {}
            Ok(_) => {
                return failed(
                    "verify",
                    format!("{} differs from its source", rel.display()),
                );
            }
            Err(e) => return failed("verify", format!("{}: {e}", rel.display())),
        }
    }
    let mut hwm = BTreeMap::new();
    if let Some(src) = &held {
        let staged = stage.join(LEDGER);
        let check = Connection::open_with_flags(&staged, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())
            .and_then(|c| super::retire::verify_snapshot(src, &c));
        if let Err(e) = check {
            return failed("verify", e);
        }
        hwm = match high_water(src) {
            Ok(h) => h,
            Err(e) => return failed("verify", e),
        };
    }
    let spool_offset = match fs::metadata(stage.join(SPOOL)) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return failed("verify", e),
    };

    // 5. Commit: files first, the marker last.
    for e in entries.iter().filter(|e| !e.dir) {
        let dst = new.join(&e.rel);
        if let Some(p) = dst.parent()
            && let Err(err) = fs::create_dir_all(p)
        {
            return failed("commit", err);
        }
        if dst.symlink_metadata().is_ok()
            && let Err(err) = fs::rename(&dst, aside(&dst))
        {
            return failed("commit", err);
        }
        if let Err(err) = fs::rename(stage.join(&e.rel), &dst) {
            return failed("commit", format!("{}: {err}", e.rel.display()));
        }
    }
    sync_dir(new);
    let marker = Marker {
        source: opts.source_id.clone(),
        backup: opts.backup.clone(),
        state: "copied".into(),
        hwm,
        spool_offset,
    };
    if let Err(e) = write_marker(new, &marker) {
        return failed("commit", e);
    }
    drop(held);
    let _ = fs::remove_dir_all(stage);
    Outcome::Migrated(marker)
}

/// A free name beside `p` for a file already in the way; never overwrites.
fn aside(p: &Path) -> PathBuf {
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
        if cand.symlink_metadata().is_err() {
            return cand;
        }
    }
    unreachable!()
}

fn write_marker(new: &Path, m: &Marker) -> io::Result<()> {
    let tmp = new.join(format!("{MARKER_NAME}.tmp"));
    let mut f = File::create(&tmp)?;
    f.write_all(&serde_json::to_vec_pretty(m).map_err(io::Error::other)?)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, new.join(MARKER_NAME))?;
    sync_dir(new);
    Ok(())
}
