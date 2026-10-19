//! Migrate and move (steps M1-M11 of the design).
//!
//! Lumen's own files are copied out of the previous folder (L) into the new one (N), checked,
//! and committed with a marker; then L is renamed whole into a backup inside N. Nothing is
//! deleted, and nothing but the ledger and Lumen's own small files is copied:
//!
//! - **Staged:** `lumen.db` by `VACUUM INTO` through the held connection, then `lumen_settings.json`,
//!   `update_check.json` and `lumen_call_*.json` by [`copy_hashed`]. The fault spool, `bin/`,
//!   `logs/` and the webview stores stay in L and travel with it into the backup.
//! - **The commit intent** (`N/.legacy-import/.commit`, a map of file to SHA-256) is written before
//!   any file moves into N. A run killed halfway through the renames leaves N files that are ours;
//!   the next run recognises them by hash, takes them back into the stage and starts over, so the
//!   ledger it staged is never set aside as if it were someone else's.
//! - **The marker** ([`super::marker::write_marker`]) is the commit point, written once.
//! - **The move** (M11) renames L to `B/data` (B is `legacy-backup-<date>` beside N). On Unix it
//!   happens while the probe's exclusive lock on L's ledger is still held, so no connection can be
//!   writing at that moment, and a connection opened before the move gets `SQLITE_READONLY_DBMOVED`
//!   on its next write. On Windows the connection is dropped first, because an open handle blocks
//!   the rename. A refused rename leaves the commit in place; [`retry_move`] tries again at the next
//!   start, after re-probing the ledger on Unix, so an old app that has it open is never moved under.
//!
//! The lock is `N/.lumen-migrate.lock`, dropped by the OS when the process dies.

use super::marker::{self, MarkerV2, Pending};
use super::merge::{POSITIONAL_TABLES, columns, row_sig_in};
use super::migrate::{
    LEDGER, LOCK_NAME, Phase, Probe, STAGE_DIR, aside, copy_hashed, high_water, is_full,
    move_aside, probe, sha256_file, sidecar, sync_dir,
};
use super::retire::{Ops, backup_name};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The commit intent, inside the stage.
pub const INTENT_NAME: &str = ".commit";
/// Written into the backup when L has been moved into it.
pub const MOVED_STAMP: &str = ".moved_at";
/// Where L lands inside the backup.
pub const DATA_DIR: &str = "data";

/// Lumen's own small files that are staged besides the ledger.
const OWN_FILES: [&str; 2] = ["lumen_settings.json", "update_check.json"];

#[derive(Clone)]
pub struct CommitOptions {
    /// The current time, RFC 3339 UTC (`2026-10-19T08:00:00Z`). Its first ten characters name
    /// the backup; the marker and the move stamp record it.
    pub now: String,
    /// Other Lumen folders discovery found and did not take, for the marker.
    pub others: Vec<String>,
    /// Tries at taking the old ledger before deferring.
    pub probe_tries: u32,
    pub probe_delay: Duration,
    /// Tries at renaming L on Windows, where a handle opened by anything else refuses it.
    pub move_tries: u32,
    pub move_delay: Duration,
    /// Test seam: pause, or alter the stage, at a [`Phase`]. Not in release builds.
    #[cfg(debug_assertions)]
    pub hook: Option<fn(&Phase<'_>)>,
}

impl CommitOptions {
    pub fn new(now: &str) -> Self {
        CommitOptions {
            now: now.into(),
            others: Vec::new(),
            probe_tries: 5,
            probe_delay: Duration::from_millis(200),
            move_tries: 3,
            move_delay: Duration::from_millis(200),
            #[cfg(debug_assertions)]
            hook: None,
        }
    }

    fn date(&self) -> &str {
        self.now.get(..10).unwrap_or(&self.now)
    }
}

#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn fire(opts: &CommitOptions, phase: &Phase<'_>) {
    #[cfg(debug_assertions)]
    if let Some(h) = opts.hook {
        h(phase);
    }
}

/// What happened to L after the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// L was renamed to `B/data` in this call.
    Done,
    /// `B/data` is there already and L is not: nothing to do (the stamp is made sure of).
    Already,
    /// L was not moved and stays in use as a source for the merge; try again at the next start.
    Deferred(String),
    /// There is no L and no `B/data`: the previous folder is gone, nothing to move.
    Gone,
    /// `B/data` is there and L is a real directory again: an old binary re-created it.
    Recreated,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The copy committed; the marker is written, and `moved` says what became of L.
    Migrated { marker: MarkerV2, moved: Moved },
    /// Nothing was changed; keep using L and try again later.
    Deferred { reason: String },
    /// No previous folder, or the marker is already there.
    NothingToDo,
    /// Nothing was committed; L's rows are untouched.
    Failed { step: &'static str, error: String },
}

fn failed(step: &'static str, e: impl std::fmt::Display) -> Outcome {
    Outcome::Failed {
        step,
        error: e.to_string(),
    }
}

/// `N/.lumen-migrate.lock`, held while this value lives.
pub struct Lock(#[allow(dead_code)] File);

#[derive(Debug)]
pub enum LockError {
    /// Another process (or another holder in this one) has it.
    Held,
    Io(io::Error),
}

/// M1: create N and take the lock. The OS drops it when the process dies.
pub fn take_lock(n: &Path) -> Result<Lock, LockError> {
    fs::create_dir_all(n).map_err(LockError::Io)?;
    let f = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(n.join(LOCK_NAME))
        .map_err(LockError::Io)?;
    match f.try_lock() {
        Ok(()) => Ok(Lock(f)),
        Err(TryLockError::WouldBlock) => Err(LockError::Held),
        Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
    }
}

/// Where the backup lives: beside L, in the folder named like N (N itself, unless L sits in
/// another data root).
fn holder(l: &Path, n: &Path) -> PathBuf {
    match (l.parent(), n.file_name()) {
        (Some(p), Some(id)) => p.join(id),
        _ => n.to_path_buf(),
    }
}

/// The backup folder `m` names, for the install whose data folder is `n`.
pub fn backup_dir(n: &Path, m: &MarkerV2) -> Option<PathBuf> {
    if m.backup.is_empty() || m.source_dir.is_empty() {
        return None;
    }
    Some(holder(Path::new(&m.source_dir), n).join(&m.backup))
}

/// Run M1-M11 for `l` into `n`. Safe to call again after any outcome.
pub fn migrate(l: &Path, n: &Path, opts: &CommitOptions, ops: &dyn Ops) -> Outcome {
    let lock = match take_lock(n) {
        Ok(l) => l,
        Err(LockError::Held) => {
            return Outcome::Deferred {
                reason: "another migration is running".into(),
            };
        }
        Err(LockError::Io(e)) => return failed("lock", e),
    };
    let out = migrate_under(&lock, l, n, opts, ops);
    drop(lock);
    out
}

/// [`migrate`] for a caller that already holds the lock.
pub fn migrate_under(
    _lock: &Lock,
    l: &Path,
    n: &Path,
    opts: &CommitOptions,
    ops: &dyn Ops,
) -> Outcome {
    // A real directory only: a link is not a previous folder.
    match fs::symlink_metadata(l) {
        Ok(m) if m.file_type().is_dir() => {}
        _ => return Outcome::NothingToDo,
    }
    if marker::read(n).is_present() {
        return Outcome::NothingToDo;
    }
    if let Err(e) = undo_half_commit(n) {
        return failed("commit", e);
    }
    let Some(x) = l.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        return Outcome::NothingToDo;
    };
    let abs = std::path::absolute(l).unwrap_or_else(|_| l.to_path_buf());
    // M3. Harmless if the run dies right after: resolution trusts it only while no marker exists.
    if let Err(e) = marker::write_pending(
        n,
        &Pending {
            source: x.clone(),
            dir: abs.clone(),
        },
    ) {
        return failed("pending", e);
    }
    let stage = n.join(STAGE_DIR);
    if let Err(e) = fs::create_dir_all(&stage) {
        return if is_full(&e) {
            Outcome::Deferred {
                reason: format!("no space for the copy: {e}"),
            }
        } else {
            failed("stage", e)
        };
    }
    let out = copy_verify_commit(l, &abs, &x, n, &stage, opts, ops);
    if !matches!(out, Outcome::Migrated { .. }) && !marker::read(n).is_present() {
        // A failure that returned (not a kill) may have renamed some staged files into N.
        // Take them back with the intent still in hand, instead of deleting it: if this
        // fails too, the stage and its intent stay for the next run's undo.
        let _ = undo_half_commit(n);
    }
    out
}

/// A run killed between the first and the last rename into N leaves some staged files in N.
/// The intent says what they are: a file in N whose SHA-256 is the intent's is ours, and is
/// taken back into the stage, which is then discarded. Files that were set aside stay aside.
/// Without this, the staged ledger would be set aside by the rerun as a ledger that was in N
/// already, and L's rows would sit in two places.
fn undo_half_commit(n: &Path) -> io::Result<()> {
    let stage = n.join(STAGE_DIR);
    match fs::symlink_metadata(&stage) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let intent = fs::read(stage.join(INTENT_NAME))
        .ok()
        .and_then(|b| serde_json::from_slice::<BTreeMap<String, String>>(&b).ok());
    for (rel, sum) in intent.into_iter().flatten() {
        if rel.is_empty() || rel == ".." || rel.contains(['/', '\\']) {
            continue; // never ours: only plain names are staged
        }
        let dst = n.join(&rel);
        let ours = fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_file())
            && sha256_file(&dst).is_ok_and(|got| got == sum);
        if ours {
            let back = stage.join(&rel);
            let _ = fs::remove_file(&back);
            fs::rename(&dst, &back)?;
        }
    }
    fs::remove_dir_all(&stage)
}

/// The names of Lumen's own small files in L, besides the ledger.
fn own_files(l: &Path) -> io::Result<Vec<String>> {
    let mut v = Vec::new();
    for e in fs::read_dir(l)? {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let call = name.starts_with("lumen_call_") && name.ends_with(".json");
        if OWN_FILES.contains(&name.as_str()) || call {
            v.push(name);
        }
    }
    v.sort();
    Ok(v)
}

fn copy_verify_commit(
    l: &Path,
    abs_l: &Path,
    x: &str,
    n: &Path,
    stage: &Path,
    opts: &CommitOptions,
    ops: &dyn Ops,
) -> Outcome {
    // M4. Probe: take the ledger exclusively, or defer.
    let ledger = l.join(LEDGER);
    let held = if ledger.is_file() {
        match probe(&ledger, opts.probe_tries, opts.probe_delay) {
            Ok(c) => Some(c),
            Err(Probe::Held(why)) => {
                return Outcome::Deferred {
                    reason: format!("the previous ledger is in use ({why})"),
                };
            }
            Err(Probe::Err(e)) => return failed("probe", e),
        }
    } else {
        None
    };

    // M5. Stage, while the exclusive lock holds the ledger still.
    let mut names = match own_files(l) {
        Ok(v) => v,
        Err(e) => return failed("stage", e),
    };
    let mut want: BTreeMap<String, String> = BTreeMap::new();
    for name in &names {
        match copy_hashed(&l.join(name), &stage.join(name), false) {
            Ok(h) => {
                want.insert(name.clone(), h);
                fire(
                    opts,
                    &Phase::Copied {
                        rel: Path::new(name),
                    },
                );
            }
            Err(e) if is_full(&e) => {
                return Outcome::Deferred {
                    reason: format!("no space for the copy: {e}"),
                };
            }
            Err(e) => return failed("stage", format!("{name}: {e}")),
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
        // Write access: on Windows FlushFileBuffers refuses a read-only handle.
        if let Err(e) = File::options()
            .write(true)
            .open(&target)
            .and_then(|f| f.sync_all())
        {
            return failed("stage", e);
        }
        names.push(LEDGER.to_string());
        names.sort();
        fire(
            opts,
            &Phase::Copied {
                rel: Path::new(LEDGER),
            },
        );
    }
    sync_dir(stage);
    fire(opts, &Phase::Staged { dir: stage });

    // M6. Verify.
    for (name, sum) in &want {
        match sha256_file(&stage.join(name)) {
            Ok(got) if got == *sum => {}
            Ok(_) => return failed("verify", format!("{name} differs from its source")),
            Err(e) => return failed("verify", format!("{name}: {e}")),
        }
    }
    let mut hwm = BTreeMap::new();
    let mut hwm_sig = BTreeMap::new();
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
        // What says, later, that a ledger is this one: the row at each positional mark.
        for table in POSITIONAL_TABLES {
            let Some(&mark) = hwm.get(table) else {
                continue;
            };
            if mark <= 0 {
                continue;
            }
            let sig = columns(src, "main", table)
                .map_err(|e| e.to_string())
                .and_then(|cols| row_sig_in(src, "main", table, &cols, mark));
            match sig {
                Ok(Some(s)) => {
                    hwm_sig.insert(table.to_string(), s);
                }
                Ok(None) => {}
                Err(e) => return failed("verify", e),
            }
        }
    }
    let mut intent: BTreeMap<String, String> = BTreeMap::new();
    for name in &names {
        match sha256_file(&stage.join(name)) {
            Ok(h) => {
                intent.insert(name.clone(), h);
            }
            Err(e) => return failed("verify", format!("{name}: {e}")),
        }
    }

    // M7. The commit intent, whole and synced before any file moves into N.
    if let Err(e) = write_intent(stage, &intent) {
        return failed("commit", e);
    }

    // The backup's name is computed once, here, and stored in the marker.
    let b = backup_name(&holder(abs_l, n), opts.date());
    let Some(backup) = b.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        return failed("commit", "no name for the backup folder");
    };

    // M8. Files into N, anything in the way set aside.
    for name in &names {
        let dst = n.join(name);
        let ledger_file = name == LEDGER;
        if let Err(e) = set_aside_in_the_way(&dst, ledger_file) {
            return failed("commit", format!("{name}: {e}"));
        }
        if let Err(e) = fs::rename(stage.join(name), &dst) {
            return failed("commit", format!("{name}: {e}"));
        }
        fire(
            opts,
            &Phase::Commit {
                rel: Path::new(name),
            },
        );
    }
    sync_dir(n);

    // M9. The commit point.
    let marker = MarkerV2 {
        format: marker::FORMAT,
        state: marker::STATE_MIGRATED.into(),
        source: x.to_string(),
        source_dir: abs_l.to_string_lossy().into_owned(),
        backup,
        committed_at: opts.now.clone(),
        hwm,
        hwm_sig,
        others: opts.others.clone(),
    };
    if let Err(e) = marker::write_marker(n, &marker) {
        return failed("commit", e);
    }
    fire(opts, &Phase::Committed);

    // M10. Tidy.
    let _ = marker::remove_pending(n);
    let _ = fs::remove_dir_all(stage);

    // M11. Move L into the backup, still holding the ledger on Unix.
    let moved = do_move(abs_l, &b, held, opts, ops);
    Outcome::Migrated { marker, moved }
}

fn write_intent(stage: &Path, intent: &BTreeMap<String, String>) -> io::Result<()> {
    let mut f = File::create(stage.join(INTENT_NAME))?;
    f.write_all(&serde_json::to_vec(intent).map_err(io::Error::other)?)?;
    f.sync_all()?;
    drop(f);
    sync_dir(stage);
    Ok(())
}

/// Whatever sits at `dst` goes aside under a free name, never overwritten. A ledger takes its
/// `-wal`, `-shm` and `-journal` with it; sidecars with no ledger beside them are set aside too,
/// because SQLite would replay them onto the ledger that takes its place.
fn set_aside_in_the_way(dst: &Path, ledger: bool) -> io::Result<()> {
    if dst.symlink_metadata().is_ok() {
        return move_aside(dst, ledger);
    }
    if ledger {
        let to = aside(dst, true);
        for sfx in ["-wal", "-shm", "-journal"] {
            let from = sidecar(dst, sfx);
            if from.symlink_metadata().is_ok() {
                fs::rename(&from, sidecar(&to, sfx))?;
            }
        }
    }
    Ok(())
}

/// M11: rename L to `B/data`. `held` is the probe's connection, if there is one.
///
/// Unix: the rename happens while the connection still holds the ledger's exclusive lock, so
/// nothing can be mid-write. Windows: the connection is dropped first, because an open handle
/// refuses the rename, and the rename is tried a few times against a transient holder.
fn do_move(
    l: &Path,
    b: &Path,
    held: Option<Connection>,
    opts: &CommitOptions,
    ops: &dyn Ops,
) -> Moved {
    let windows = cfg!(windows);
    let mut held = held;
    if windows {
        held = None; // an open handle in L refuses the rename
    }
    if let Err(e) = fs::create_dir_all(b) {
        return Moved::Deferred(format!("could not create {}: {e}", b.display()));
    }
    let data = b.join(DATA_DIR);
    let tries = if windows { opts.move_tries.max(1) } else { 1 };
    let mut last = None;
    for i in 0..tries {
        if i > 0 {
            std::thread::sleep(opts.move_delay);
        }
        match ops.rename(l, &data) {
            Ok(()) => {
                last = None;
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    if let Some(e) = last {
        return Moved::Deferred(format!(
            "could not rename {} to {}: {e}",
            l.display(),
            data.display()
        ));
    }
    sync_dir(b);
    let _ = fs::write(b.join(MOVED_STAMP), &opts.now);
    fire(opts, &Phase::Moved { dir: &data });
    drop(held.take());
    Moved::Done
}

/// Retry the move of a committed migration whose L was not moved (state S2), or finish a move
/// that was killed before its stamp. Takes the lock.
pub fn retry_move(n: &Path, m: &MarkerV2, opts: &CommitOptions, ops: &dyn Ops) -> Moved {
    match take_lock(n) {
        Ok(lock) => {
            let out = retry_move_under(&lock, n, m, opts, ops);
            drop(lock);
            out
        }
        Err(LockError::Held) => Moved::Deferred("another migration is running".into()),
        Err(LockError::Io(e)) => Moved::Deferred(format!("lock: {e}")),
    }
}

/// [`retry_move`] for a caller that already holds the lock.
///
/// - Unix: the ledger is probed again, so a long-lived holder (an old app) keeps L in place and
///   never ends up writing into the backup. The rename then happens under the probe's lock.
/// - Windows: a plain rename; it succeeds only when no handle is open inside L.
pub fn retry_move_under(
    _lock: &Lock,
    n: &Path,
    m: &MarkerV2,
    opts: &CommitOptions,
    ops: &dyn Ops,
) -> Moved {
    let Some(b) = backup_dir(n, m) else {
        return Moved::Gone;
    };
    let l = PathBuf::from(&m.source_dir);
    let data = b.join(DATA_DIR);
    let l_is_dir = fs::symlink_metadata(&l).is_ok_and(|md| md.file_type().is_dir());
    if data.is_dir() {
        if l_is_dir {
            return Moved::Recreated;
        }
        if !b.join(MOVED_STAMP).exists() {
            let _ = fs::write(b.join(MOVED_STAMP), &opts.now);
        }
        return Moved::Already;
    }
    if !l_is_dir {
        return Moved::Gone;
    }
    let ledger = l.join(LEDGER);
    let held = if !cfg!(windows) && ledger.is_file() {
        match probe(&ledger, opts.probe_tries, opts.probe_delay) {
            Ok(c) => Some(c),
            Err(Probe::Held(why)) => {
                return Moved::Deferred(format!("the previous ledger is in use ({why})"));
            }
            Err(Probe::Err(e)) => return Moved::Deferred(format!("probe: {e}")),
        }
    } else {
        None
    };
    do_move(&l, &b, held, opts, ops)
}
