//! Manual import: `lumen import-ledger <dir|lumen.db> [--dry-run]` (§9 of the design).
//!
//! The merge and the freeze carry the one backup the migration made. Everything else that holds a
//! Lumen ledger and is not that backup comes in by hand: a folder an old app re-created after the
//! move, a copy of the previous folder restored from a backup tool, a ledger N already had that the
//! migration set aside (`lumen.db.pre-legacy`). This module is that command, in lumen-core, so the
//! CLI is a thin shell around it and the app can name the same command in its reports.
//!
//! In order:
//!
//! 1. **Refuse** what must not be imported: a start that has neither committed nor settled, the
//!    live ledger, anything under the backup, something that is not a Lumen ledger, a folder that
//!    was imported already.
//! 2. **Lock, then probe.** The lock is `N/.lumen-migrate.lock`. The source is taken exclusively
//!    ([`super::migrate::probe`]); one that is held is refused and nothing changes.
//! 3. **Move it out of the way.** A folder outside N becomes `N/legacy-imports/<date>-<k>`; a
//!    set-aside ledger goes, with its sidecars, to `N/legacy-imports/<date>-<k>/lumen.db*`. On Unix
//!    the rename happens under the held lock, on Windows the connection is closed first and the
//!    ledger is probed again at its new place. A rename across volumes cannot move it, so the
//!    ledger set and the spool are copied byte for byte into `legacy-imports/<date>-<k>` (the source
//!    is never opened through SQLite, so never written), and that copy is the folder's original;
//!    the user's folder stays and is never read again. No user path is ever an original or a `$done` key: a folder is judged by
//!    its rows alone.
//! 4. **Find what is new.** For each positional table (`read_events`, `faults`), the longest
//!    rowid-aligned prefix the source shares with each original already merged (`B/data/lumen.db`
//!    and every `legacy-imports/<k>` with a `$done` row): rows are compared on the columns both
//!    ledgers have, in rowid order, until the first mismatch. Rows above the longest prefix are new.
//!    A re-created ledger has prefix 0; a restored copy of the previous folder is all prefix; a
//!    restored copy an old app wrote to afterwards is prefix up to where it diverged.
//!    Every import also records, per positional table, the rowid and a digest of every row it held
//!    (in `legacy_merge`, with the columns they were taken over), and the merge records the row it
//!    reached: a source is compared with those lists the way it is with a live original, so a copy
//!    that is older or newer than what was imported, or whose schema an app upgraded since, shares
//!    its prefix even when the original moved, was removed or sits on a volume that is absent.
//!    The lists are kept twice, over all the source's columns and over the core columns every
//!    released schema has (`CORE_READ_EVENTS`, `CORE_FAULTS`), so a copy on another schema is still
//!    recognised. Faults do not use the prefix alone: they merge as a multiset by core-content digest against N
//!    (`faultset`), whatever route the others took.
//! 5. **One transaction** from the held connection, with N attached: the keyed tables with
//!    `INSERT OR IGNORE`, the new rows of the positional tables, the folder's spool and `.draining`
//!    lines (only when every prefix is 0: a folder that shares history keeps its spool), each fault
//!    row or spool line only while the source holds more of its digest than N does, and the
//!    `('import:<k>', '$done', ..)` row that makes a second run say "already imported".
//! 6. **Finish:** the moved folder's files are made read-only.
//!
//! A run killed before step 3 changed nothing. Killed after it, `legacy-imports/<k>` has no `$done`
//! row, so a rerun on that path imports. Killed after step 5, the rerun says "already imported".
//! `--dry-run` runs the refusals, the probe and the prefix search, takes the same transaction and
//! rolls it back: it moves nothing and writes nothing.

use super::commit::{LockError, backup_dir, take_lock};
use super::discover::{Verdict, is_lumen_ledger};
use super::faultset::{
    CORE_FAULTS, admit_fresh, counts_in_n, read_spool_files, record_digests, row_digests, tally,
};
use super::freeze::{FROZEN_TBL, make_read_only, source_key};
use super::marker::{self, Seen};
use super::merge::{KEYED_TABLES, POSITIONAL_TABLES, columns, key_expr, quote, row_sig_in};
use super::migrate::{LEDGER, Probe, probe, sidecar, sync_dir};
use super::retire::Ops;
use super::same_dir;
use crate::faults;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where imported folders live, inside N.
pub const IMPORTS_DIR: &str = "legacy-imports";

/// The `tbl` of the `legacy_merge` row that says a source was imported: `hwm` is the number of rows
/// it added, `sig` the SHA-256 of the spool bytes it took.
pub const DONE_TBL: &str = "$done";

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Where a run is cut short, for the tests that stand in for a killed process.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Never,
    /// Right after the move: nothing is imported, no `$done` row.
    AfterMove,
    /// Everything inserted, before the commit: the transaction rolls back.
    BeforeCommit,
    /// Right after the commit: no read-only bits yet.
    AfterCommit,
}

#[derive(Clone)]
pub struct ImportOptions {
    /// The current time, RFC 3339 UTC. Its first ten characters name the folder in
    /// `legacy-imports`.
    pub now: String,
    /// Run everything except the move and the writes.
    pub dry_run: bool,
    /// Tries at taking the source exclusively before refusing it as in use.
    pub probe_tries: u32,
    pub probe_delay: Duration,
    #[doc(hidden)]
    pub stop: Stop,
}

impl ImportOptions {
    pub fn new(now: &str, dry_run: bool) -> Self {
        ImportOptions {
            now: now.into(),
            dry_run,
            probe_tries: 5,
            probe_delay: Duration::from_millis(200),
            stop: Stop::Never,
        }
    }

    fn date(&self) -> &str {
        self.now.get(..10).unwrap_or(&self.now)
    }
}

/// Why nothing was imported. Each says what to do next; the command exits non-zero on all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// N has no record of a committed or settled start.
    NotStarted(String),
    /// The path does not exist.
    Missing(PathBuf),
    /// The path is a link; a link is never moved or followed.
    Link(PathBuf),
    /// The live ledger of this install, or its folder.
    Live,
    /// The backup of the previous folder: its rows come only through the merge and the freeze.
    InBackup(PathBuf),
    /// Not a Lumen ledger (or not one that can be told yet).
    NotLumen(String),
    /// A `$done` row says it came in already.
    AlreadyImported(String),
    /// Another process holds the ledger, or the folder cannot be renamed.
    InUse(String),
    /// A start, a freeze or another import holds the migration lock.
    LockHeld,
    /// Something else went wrong; nothing was committed.
    Failed(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NotStarted(why) => write!(f, "start Lumen first: {why}"),
            ImportError::Missing(p) => write!(f, "{} does not exist", p.display()),
            ImportError::Link(p) => write!(
                f,
                "{} is a link; give the folder or the ledger it points to",
                p.display()
            ),
            ImportError::Live => write!(
                f,
                "that is the live ledger of this install; there is nothing to import from it"
            ),
            ImportError::InBackup(p) => write!(
                f,
                "{} is inside the backup of the previous folder; its rows reach the live ledger \
                 through the merge and the freeze, never by import",
                p.display()
            ),
            ImportError::NotLumen(why) => write!(f, "not a Lumen ledger: {why}"),
            ImportError::AlreadyImported(k) => write!(f, "already imported ({k})"),
            ImportError::InUse(why) => {
                write!(f, "in use: quit the older Lumen, then retry ({why})")
            }
            ImportError::LockHeld => write!(
                f,
                "another Lumen is starting or importing right now; wait for it, then retry"
            ),
            ImportError::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// What became of the source's place on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placed {
    /// Renamed into `legacy-imports` (a dry run: would be).
    Moved,
    /// Already under `legacy-imports`.
    Already,
    /// A rename across volumes: the folder was copied into `legacy-imports`, verified, and that
    /// copy is its original; the user's folder stays where it is. The reason is the OS's.
    InPlace(String),
}

/// What happened to the source's fault spool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spool {
    /// The folder's `.draining` and live spool were taken: this many records.
    Taken(usize),
    /// The source shares history with a ledger already imported, so its spool, which that ledger's
    /// owner already drained or kept, was left where it is.
    Kept,
    /// A ledger on its own has no spool.
    NoneHere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The folder under `legacy-imports` (or the source's own, when it stayed).
    pub name: String,
    pub dir: PathBuf,
    pub placed: Placed,
    /// Per positional table, the rows it shares with an original already merged.
    pub prefix: BTreeMap<String, i64>,
    /// Rows added, per table (a dry run: that would be added).
    pub rows: BTreeMap<String, usize>,
    pub spool: Spool,
    pub dry_run: bool,
    /// A test seam stopped the run.
    pub stopped: bool,
}

impl ImportReport {
    pub fn total_rows(&self) -> usize {
        self.rows.values().sum()
    }

    /// The lines the command prints.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        let (verb, moved) = if self.dry_run {
            (
                "Dry run, nothing was moved or written. It would add",
                "move",
            )
        } else {
            ("Added", "moved")
        };
        match &self.placed {
            Placed::Moved => out.push(format!("The folder {moved} to {}", self.dir.display())),
            Placed::Already => out.push(format!("Reading {}", self.dir.display())),
            Placed::InPlace(why) => out.push(format!(
                "The folder is on another volume, so it was copied to {} and the original stays where it is ({why})",
                self.dir.display()
            )),
        }
        if self.rows.is_empty() {
            out.push(format!("{verb} no rows."));
        } else {
            out.push(format!("{verb}:"));
            for (t, n) in &self.rows {
                out.push(format!("  {t}: {n}"));
            }
        }
        for (t, p) in &self.prefix {
            if *p > 0 {
                out.push(format!(
                    "{t}: the first {p} rowids are already in the live ledger and were skipped"
                ));
            }
        }
        match &self.spool {
            Spool::Taken(0) | Spool::NoneHere => {}
            Spool::Taken(n) => out.push(format!("Fault records taken from its spool: {n}")),
            Spool::Kept => out.push(
                "Its fault spool was left alone: this folder shares history with a ledger that \
                 was already merged, so the same faults are not taken twice"
                    .into(),
            ),
        }
        out
    }
}

// ---- report texts that name the command ----

/// The command that imports `path`, as the reports print it.
pub fn command_for(path: &Path) -> String {
    format!("lumen import-ledger \"{}\"", path.display())
}

/// §8.5: the previous folder was re-created by an old binary and holds a ledger.
pub fn recreated_folder_text(dir: &Path) -> String {
    format!(
        "An older Lumen wrote to {} after the move. Nothing from it was merged. Quit it, then run: {}",
        dir.display(),
        command_for(dir)
    )
}

/// §3.1: another folder in the data root holds a Lumen ledger that discovery did not take.
pub fn other_ledger_text(dir: &Path) -> String {
    format!(
        "Another Lumen ledger was found in {} and left alone. To bring it in, run: {}",
        dir.display(),
        command_for(dir)
    )
}

/// §6: a ledger N already had was set aside at the commit.
pub fn set_aside_text(ledger: &Path) -> String {
    format!(
        "A ledger that was already here was set aside as {}. To bring its rows in, run: {}",
        ledger.display(),
        command_for(ledger)
    )
}

// ---- the source ----

enum Kind {
    /// A folder with `lumen.db` in it (and maybe a spool) that holds only Lumen's own files, or
    /// sits directly in a data root: moves whole.
    Folder,
    /// A folder with `lumen.db` in it that holds other files too (a user's `Documents`): only the
    /// ledger set and the spool move; the rest is never touched.
    Shared,
    /// A single ledger file with its sidecars, such as `lumen.db.pre-legacy`.
    Ledger,
}

struct Source {
    kind: Kind,
    /// The folder, or the ledger file.
    path: PathBuf,
    /// The ledger file.
    ledger: PathBuf,
}

fn resolve(arg: &Path) -> Result<Source, ImportError> {
    let md = match fs::symlink_metadata(arg) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ImportError::Missing(arg.to_path_buf()));
        }
        Err(e) => return Err(ImportError::Failed(format!("{}: {e}", arg.display()))),
    };
    if md.file_type().is_symlink() {
        return Err(ImportError::Link(arg.to_path_buf()));
    }
    if md.is_dir() {
        let ledger = arg.join(LEDGER);
        if !fs::symlink_metadata(&ledger).is_ok_and(|m| m.file_type().is_file()) {
            return Err(ImportError::NotLumen(format!(
                "there is no {LEDGER} in {}",
                arg.display()
            )));
        }
        return Ok(Source {
            kind: Kind::Folder,
            path: arg.to_path_buf(),
            ledger,
        });
    }
    if !md.is_file() {
        return Err(ImportError::NotLumen(format!(
            "{} is neither a folder nor a file",
            arg.display()
        )));
    }
    let named_ledger = arg
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(LEDGER));
    match (named_ledger, arg.parent()) {
        (true, Some(dir)) => Ok(Source {
            kind: Kind::Folder,
            path: dir.to_path_buf(),
            ledger: arg.to_path_buf(),
        }),
        _ => Ok(Source {
            kind: Kind::Ledger,
            path: arg.to_path_buf(),
            ledger: arg.to_path_buf(),
        }),
    }
}

/// The path as the file system names it, so two spellings of one place compare equal.
fn real(p: &Path) -> PathBuf {
    match fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()),
    }
}

fn within(p: &Path, root: &Path) -> bool {
    p.ancestors().any(|a| same_dir(a, root))
}

/// Whether `name` is a file or folder Lumen itself keeps in its data folder.
fn is_lumen_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with(LEDGER)
        || n.starts_with("faults.jsonl")
        || n == "lumen_settings.json"
        || n == "update_check.json"
        || (n.starts_with("lumen_call_") && n.ends_with(".json"))
        || matches!(n.as_str(), "bin" | "logs" | "ebwebview" | "webkit")
}

/// Whether the whole of `folder` may move into `legacy-imports` and be made read-only: it sits
/// directly in a data root (next to N) or under N, or everything in it is a Lumen name. Any other
/// folder holds somebody's files, and those stay where they are.
fn owns_folder(n_real: &Path, folder: &Path) -> bool {
    if let Some(parent) = folder.parent() {
        if within(parent, n_real) {
            return true;
        }
        if n_real.parent().is_some_and(|root| same_dir(parent, root)) {
            return true;
        }
    }
    match fs::read_dir(folder) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .all(|e| is_lumen_name(&e.file_name().to_string_lossy())),
        Err(_) => false,
    }
}

/// The folder name under `legacy-imports` that `p` is directly in or is, when it is.
fn import_name(imports: &Path, p: &Path, kind: &Kind) -> Option<String> {
    let folder = match kind {
        Kind::Folder | Kind::Shared => p,
        Kind::Ledger => p.parent()?,
    };
    let parent = folder.parent()?;
    if !same_dir(parent, imports) {
        return None;
    }
    Some(folder.file_name()?.to_string_lossy().into_owned())
}

fn done_source(k: &str) -> String {
    format!("import:{k}")
}

/// Whether N's ledger holds a `$done` row for `source`. A ledger that is missing, unreadable or
/// without the table has none.
fn is_done(n_db: &Path, source: &str) -> bool {
    if !n_db.is_file() {
        return false;
    }
    let Ok(conn) = Connection::open_with_flags(n_db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    conn.query_row(
        "SELECT 1 FROM legacy_merge WHERE source = ?1 AND tbl = ?2",
        params![source, DONE_TBL],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

fn failed(what: &str, e: impl fmt::Display) -> ImportError {
    ImportError::Failed(format!("{what}: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Import the Lumen ledger at `arg` (a folder, its `lumen.db`, or a set-aside ledger file) into
/// the install whose data folder is `n`.
pub fn import(
    n: &Path,
    arg: &Path,
    opts: &ImportOptions,
    ops: &dyn Ops,
) -> Result<ImportReport, ImportError> {
    // A relative argument would be read from the filesystem root by the read-only URI.
    let abs = std::path::absolute(arg).map_err(|e| failed("cannot resolve the path", e))?;
    let arg = abs.as_path();
    // 1. Refuse.
    let mut pending_move = false;
    let backup = match marker::read(n) {
        Seen::Settled => None,
        Seen::Migrated(m) => {
            let b = backup_dir(n, &m);
            // The previous folder was not moved into the backup yet (the OS refused the rename, or
            // the start was killed before it): there is no original to compare against, and the
            // next start moves and merges it. Importing now would add its rows a second time.
            let data_db = b
                .as_ref()
                .map(|b| b.join(super::commit::DATA_DIR).join(LEDGER));
            pending_move = !m.source_dir.is_empty()
                && data_db.is_some_and(|d| !d.is_file())
                && fs::symlink_metadata(Path::new(&m.source_dir).join(LEDGER))
                    .is_ok_and(|md| md.file_type().is_file());
            b
        }
        Seen::Absent => {
            return Err(ImportError::NotStarted(
                "this install has not finished its first start, so there is nothing to import into"
                    .into(),
            ));
        }
        Seen::V1 { .. } | Seen::Bad => {
            return Err(ImportError::NotStarted(
                "the migration record in the data folder is not one this command understands"
                    .into(),
            ));
        }
    };
    if pending_move {
        return Err(ImportError::NotStarted(
            "the previous folder has not moved into the backup yet and its rows are not all \
             merged; quit the older Lumen, start Lumen again, then retry"
                .into(),
        ));
    }
    let mut src = resolve(arg)?;
    let n_real = real(n);
    if matches!(src.kind, Kind::Folder) && !owns_folder(&n_real, &real(&src.path)) {
        src.kind = Kind::Shared;
    }
    let n_db = n.join(LEDGER);
    let src_real = real(&src.path);
    let ledger_real = real(&src.ledger);
    if same_dir(&src_real, &n_real) || same_dir(&ledger_real, &real(&n_db)) {
        return Err(ImportError::Live);
    }
    if let Some(b) = &backup
        && within(&src_real, &real(b))
    {
        return Err(ImportError::InBackup(arg.to_path_buf()));
    }
    let imports = n_real.join(IMPORTS_DIR);
    // Only a folder under `legacy-imports` (ours, never the user's) is known by its name. A user's
    // path is never a key: what sits there now may be anything, so it is judged by its rows.
    let in_place_name = import_name(&imports, &src_real, &src.kind);
    if let Some(k) = &in_place_name
        && is_done(&n_db, &done_source(k))
    {
        return Err(ImportError::AlreadyImported(k.clone()));
    }
    match is_lumen_ledger(&src.ledger) {
        Verdict::Lumen => {}
        Verdict::NotLumen => {
            return Err(ImportError::NotLumen(format!(
                "{} has no Lumen tables",
                src.ledger.display()
            )));
        }
        Verdict::Unknown(why) => {
            return Err(ImportError::NotLumen(format!(
                "{why}; quit the older Lumen and retry"
            )));
        }
    }

    // 2. Lock, then probe.
    let lock = match take_lock(n) {
        Ok(l) => l,
        Err(LockError::Held) => return Err(ImportError::LockHeld),
        Err(LockError::Io(e)) => return Err(failed("lock", e)),
    };
    let out = import_under(n, &n_real, &src, in_place_name, opts, ops);
    drop(lock);
    out
}

/// Where the work is, for [`run_import`].
struct Place<'a> {
    n: &'a Path,
    n_db: PathBuf,
    /// The ledger file now.
    ledger: PathBuf,
    /// The folder now (or the one holding a set-aside ledger's new home); in a dry run, where it
    /// would be.
    dir: PathBuf,
    /// Where the folder's files really are right now.
    actual: PathBuf,
    name: String,
    placed: Placed,
    /// The `legacy_merge.source` of the `$done` row.
    done_key: String,
}

fn import_under(
    n: &Path,
    n_real: &Path,
    src: &Source,
    in_place_name: Option<String>,
    opts: &ImportOptions,
    ops: &dyn Ops,
) -> Result<ImportReport, ImportError> {
    let n_db = n.join(LEDGER);
    // A source on another volume is never opened for writing (taking it would change the journal
    // mode in its header): it is copied byte for byte, and the copy is what gets taken.
    let crossing = in_place_name.is_none() && ops.crosses_volumes(&src.path, n_real);
    let mut held = if crossing {
        None
    } else {
        Some(take(&src.ledger, opts)?)
    };
    let mut scratch: Option<PathBuf> = None;

    // 3. Move it out of the way.
    let imports = n_real.join(IMPORTS_DIR);
    let mut ledger = src.ledger.clone();
    let mut dir = match src.kind {
        Kind::Folder | Kind::Shared => src.path.clone(),
        Kind::Ledger => src
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| src.path.clone()),
    };
    let mut actual = dir.clone();
    let (name, placed) = match in_place_name {
        Some(k) => (k, Placed::Already),
        None if opts.dry_run => {
            let k = free_name(&imports, &n_db, opts.date());
            dir = imports.join(&k);
            if crossing {
                // Looked at through a copy in a scratch folder that goes when the run ends.
                static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let tmp = std::env::temp_dir().join(format!(
                    "lumen-import-dry-{}-{}",
                    std::process::id(),
                    RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                held = Some(copy_aside(src, &tmp, None, opts)?);
                ledger = tmp.join(LEDGER);
                actual = tmp.clone();
                scratch = Some(tmp);
                (k, Placed::InPlace("another volume".into()))
            } else {
                (k, Placed::Moved)
            }
        }
        None => {
            let k = free_name(&imports, &n_db, opts.date());
            let target = imports.join(&k);
            let made_imports = !imports.exists();
            fs::create_dir_all(&imports).map_err(|e| failed("legacy-imports", e))?;
            if cfg!(windows) {
                held = None; // an open handle in the folder refuses the rename
            }
            let moved = if crossing {
                Err(io::Error::new(
                    io::ErrorKind::CrossesDevices,
                    "on another volume",
                ))
            } else {
                match src.kind {
                    Kind::Folder => ops.rename(&src.path, &target),
                    Kind::Shared => move_ledger_set(ops, &src.ledger, &target, true),
                    Kind::Ledger => move_ledger_set(ops, &src.path, &target, false),
                }
            };
            let placed = match moved {
                Ok(()) => {
                    sync_dir(&imports);
                    ledger = target.join(LEDGER);
                    dir = target;
                    actual = dir.clone();
                    Placed::Moved
                }
                // Another volume: the folder is copied, verified and kept under `legacy-imports`,
                // and that copy is its original. The user's folder stays where it is.
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    // The failed move may have made `target` (a set-aside ledger's); the copy
                    // makes its own folder.
                    fs::remove_dir(&target).ok();
                    match copy_aside(src, &target, held.take(), opts) {
                        Ok(c) => {
                            held = Some(c);
                            sync_dir(&imports);
                            ledger = target.join(LEDGER);
                            dir = target;
                            actual = dir.clone();
                            Placed::InPlace(e.to_string())
                        }
                        Err(err) => {
                            if made_imports {
                                fs::remove_dir(&imports).ok();
                            }
                            return Err(err);
                        }
                    }
                }
                // Nothing changed, but for the folder made to receive it, if it was ours.
                Err(e) => {
                    if made_imports {
                        // Empty, so non-recursive; one that is not is somebody's and stays.
                        fs::remove_dir(&imports).ok();
                    }
                    return Err(ImportError::InUse(format!(
                        "could not move {} to {}: {e}",
                        src.path.display(),
                        target.display()
                    )));
                }
            };
            (k, placed)
        }
    };
    // On Unix the rename left the connection holding the (moved) file under its lock. On Windows it
    // was closed first, so the ledger is taken again where it is now.
    let held = match held {
        Some(c) => c,
        None => take(&ledger, opts)?,
    };
    if opts.stop == Stop::AfterMove {
        drop(held);
        if let Some(t) = &scratch {
            clear_dir(t).ok();
        }
        return Ok(ImportReport {
            stopped: true,
            ..empty_report(name, dir, placed, opts)
        });
    }
    // After the move, so a refusal above leaves N as it was: the current schema, `legacy_merge`
    // included, whatever the install last ran.
    drop(crate::meter::connect_db(&n_db).map_err(|e| failed("cannot open the live ledger", e))?);
    let done_key = done_source(&name);
    let place = Place {
        n,
        n_db,
        ledger,
        dir,
        actual,
        name,
        placed,
        done_key,
    };
    let landed = place.dir.clone();
    let moved = matches!(place.placed, Placed::Moved) && !opts.dry_run;
    let out = run_import(&place, src, held, opts);
    if let Some(t) = &scratch {
        clear_dir(t).ok();
    }
    out.map_err(|e| match e {
        // The source already left its place: say where it is, and that a rerun on it imports.
        ImportError::Failed(why) if moved => ImportError::Failed(format!(
            "{why}; the source is now at {}, and this finishes it: {}",
            landed.display(),
            command_for(&landed)
        )),
        other => other,
    })
}

fn empty_report(name: String, dir: PathBuf, placed: Placed, opts: &ImportOptions) -> ImportReport {
    ImportReport {
        name,
        dir,
        placed,
        prefix: BTreeMap::new(),
        rows: BTreeMap::new(),
        spool: Spool::NoneHere,
        dry_run: opts.dry_run,
        stopped: false,
    }
}

/// Take the source's ledger exclusively, or say why not.
fn take(ledger: &Path, opts: &ImportOptions) -> Result<Connection, ImportError> {
    match probe(ledger, opts.probe_tries, opts.probe_delay) {
        Ok(c) => Ok(c),
        Err(Probe::Held(why)) => Err(ImportError::InUse(why)),
        Err(Probe::Err(e)) => Err(failed(
            &format!(
                "cannot take {} exclusively (it must be writable for the import)",
                ledger.display()
            ),
            e,
        )),
    }
}

/// `<date>-1`, `<date>-2`, ... the first name not yet in `imports` and with no `$done` row in N.
/// A folder the user deleted leaves its `$done` row behind; a new folder given that name would
/// inherit it, and a run cut short after the move could never be finished (refused as "already
/// imported", while its rows were never taken).
fn free_name(imports: &Path, n_db: &Path, date: &str) -> String {
    let mut k = 1u32;
    loop {
        let name = format!("{date}-{k}");
        if fs::symlink_metadata(imports.join(&name)).is_err() && !is_done(n_db, &done_source(&name))
        {
            return name;
        }
        k += 1;
    }
}

/// A set-aside ledger goes, sidecars first, to `<target>/lumen.db*`; with `spool`, the folder's
/// fault spool and its `.draining` file go too, ahead of the ledger. A failed rename puts back
/// what was moved, newest first, so the ledger is whole under its own name again, and removes the
/// folder it made.
fn move_ledger_set(ops: &dyn Ops, from_db: &Path, target: &Path, spool: bool) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let to_db = target.join(LEDGER);
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut run = || -> io::Result<()> {
        if spool && let Some(dir) = from_db.parent() {
            let live = dir.join("faults.jsonl");
            for from in [faults::draining_path(&live), live] {
                if from
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_file())
                    && let Some(name) = from.file_name()
                {
                    let to = target.join(name);
                    ops.rename(&from, &to)?;
                    moved.push((from, to));
                }
            }
        }
        for sfx in SIDECARS {
            let from = sidecar(from_db, sfx);
            if from.symlink_metadata().is_ok() {
                let to = sidecar(&to_db, sfx);
                ops.rename(&from, &to)?;
                moved.push((from, to));
            }
        }
        ops.rename(from_db, &to_db)
    };
    match run() {
        Ok(()) => Ok(()),
        Err(e) => {
            let mut stuck = Vec::new();
            for (from, to) in moved.iter().rev() {
                if let Err(u) = fs::rename(to, from) {
                    stuck.push(format!("{}: {u}", to.display()));
                }
            }
            if stuck.is_empty() {
                // Empty by now; a folder that is not empty is somebody's and stays.
                if let Err(u) = fs::remove_dir(target) {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("{e}; and could not remove {}: {u}", target.display()),
                    ));
                }
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

/// The source is on another volume, so it cannot be renamed: copy its ledger set (and, for a
/// folder, its spool) into `target`, byte for byte, and give back the copy, taken. Nothing is read
/// from the source's place afterwards. The copy is built in a hidden folder next to `target` and
/// renamed into place only once every file of it is equal to its source, so a run that dies
/// half-way leaves no `legacy-imports/<k>` that looks whole, and a source that changed while it was
/// copied is refused.
///
/// The source is never opened through SQLite and never written: a connection that can checkpoint
/// or change the journal mode would rewrite the user's file. `held` is a connection the caller took
/// on it before it knew the move would cross volumes (a rename that failed on its own); it is
/// dropped once the copy is checked.
fn copy_aside(
    src: &Source,
    target: &Path,
    held: Option<Connection>,
    opts: &ImportOptions,
) -> Result<Connection, ImportError> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| ImportError::Failed(format!("{} has no name", target.display())))?;
    let tmp = target.with_file_name(format!(".{name}.copying"));
    clear_dir(&tmp)?;
    let run = || -> Result<Connection, ImportError> {
        fs::create_dir_all(&tmp).map_err(|e| failed("cannot copy the folder", e))?;
        let to_db = tmp.join(LEDGER);
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        for sfx in SIDECARS {
            let from = sidecar(&src.ledger, sfx);
            if from
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_file())
            {
                files.push((from, sidecar(&to_db, sfx)));
            }
        }
        if !matches!(src.kind, Kind::Ledger)
            && let Some(dir) = src.ledger.parent()
        {
            let live = dir.join("faults.jsonl");
            for from in [faults::draining_path(&live), live] {
                if from
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_file())
                    && let Some(f) = from.file_name()
                {
                    let to = tmp.join(f);
                    files.push((from, to));
                }
            }
        }
        files.push((src.ledger.clone(), to_db.clone()));
        for (from, to) in &files {
            fs::copy(from, to).map_err(|e| {
                failed(
                    &format!("cannot copy {} to {}", from.display(), to.display()),
                    e,
                )
            })?;
            let same = same_bytes(from, to)
                .map_err(|e| failed(&format!("cannot check the copy of {}", from.display()), e))?;
            if !same {
                return Err(ImportError::Failed(format!(
                    "{} changed while it was copied (or the copy does not match it); nothing was \
                     imported. Quit the older Lumen and retry",
                    from.display()
                )));
            }
        }
        drop(held);
        // The copy is ours: taking it replays a WAL the source left, and proves it opens.
        drop(take(&to_db, opts)?);
        fs::rename(&tmp, target).map_err(|e| failed("cannot place the copy", e))?;
        take(&target.join(LEDGER), opts)
    };
    match run() {
        Ok(c) => Ok(c),
        Err(e) => match clear_dir(&tmp) {
            Ok(()) => Err(e),
            Err(left) => Err(ImportError::Failed(format!("{e}; and {left}"))),
        },
    }
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    use std::io::Read;
    let (mut fa, mut fb) = (fs::File::open(a)?, fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let n = fa.read(&mut x)?;
        if n == 0 {
            return Ok(fb.read(&mut y)? == 0);
        }
        fb.read_exact(&mut y[..n])?;
        if x[..n] != y[..n] {
            return Ok(false);
        }
    }
}

/// Remove `dir` and what is in it; one that is not there is already clear.
fn clear_dir(dir: &Path) -> Result<(), ImportError> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(failed(&format!("cannot clear {}", dir.display()), e)),
    }
}

/// The originals already merged: N's own ledger, `B/data/lumen.db` and every `legacy-imports/<k>`
/// with a `$done` row (a moved folder, or the copy of one from another volume). Read-only connections, one per ledger, opened on demand.
fn originals(n: &Path, conn: &Connection, current: &Path) -> Result<Vec<PathBuf>, ImportError> {
    // N's own ledger is an original too: a copy of it (a Finder duplicate, a restored backup)
    // holds only rows N already has, so they are prefix, not new.
    let mut found = Vec::new();
    let own = n.join(LEDGER);
    if own.is_file() && !same_dir(&real(&own), &real(current)) {
        found.push(own);
    }
    if let Seen::Migrated(m) = marker::read(n)
        && let Some(b) = backup_dir(n, &m)
    {
        let db = b.join(super::commit::DATA_DIR).join(LEDGER);
        if db.is_file() {
            found.push(db);
        }
    }
    let mut st = conn
        .prepare("SELECT source FROM n.legacy_merge WHERE tbl = ?1 AND source LIKE 'import:%'")
        .map_err(|e| failed("legacy_merge", e))?;
    let sources = st
        .query_map(params![DONE_TBL], |r| r.get::<_, String>(0))
        .map_err(|e| failed("legacy_merge", e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| failed("legacy_merge", e))?;
    for s in sources {
        // Only a folder of ours: no user path is ever an original, whatever a `$done` row says.
        let k = s.strip_prefix("import:").unwrap_or(&s);
        if Path::new(k).components().count() != 1 || Path::new(k).is_absolute() {
            continue;
        }
        let db = n.join(IMPORTS_DIR).join(k).join(LEDGER);
        if db.is_file() && !same_dir(&real(&db), &real(current)) {
            found.push(db);
        }
    }
    Ok(found)
}

/// The `(table, rowid, signature)` marks N's `legacy_merge` holds for the positional tables: from
/// each import and from the merge from the backup.
fn recorded_marks(conn: &Connection) -> Result<Vec<(String, i64, String)>, ImportError> {
    let mut st = conn
        .prepare(
            "SELECT tbl, hwm, sig FROM n.legacy_merge \
             WHERE tbl IN ('read_events', 'faults') AND sig IS NOT NULL AND hwm > 0",
        )
        .map_err(|e| failed("legacy_merge", e))?;
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| failed("legacy_merge", e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| failed("legacy_merge", e))
}

/// How many leading rowids of `table` the two ledgers share, row for row, on the columns they both
/// have. Stops at the first row that differs, or at the end of either.
fn common_prefix(src: &Connection, orig: &Connection, table: &str) -> Result<i64, String> {
    let s_cols = columns(src, "main", table).map_err(|e| e.to_string())?;
    let o_cols = columns(orig, "main", table).map_err(|e| e.to_string())?;
    let common: Vec<&str> = s_cols
        .iter()
        .filter(|c| o_cols.contains(c))
        .map(String::as_str)
        .collect();
    if common.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "SELECT rowid, {} FROM {} ORDER BY rowid",
        key_expr("", &common),
        quote(table)
    );
    let mut a = src.prepare(&sql).map_err(|e| format!("{table}: {e}"))?;
    let mut b = orig.prepare(&sql).map_err(|e| format!("{table}: {e}"))?;
    let mut ra = a.query([]).map_err(|e| format!("{table}: {e}"))?;
    let mut rb = b.query([]).map_err(|e| format!("{table}: {e}"))?;
    let mut prefix = 0i64;
    while let (Some(x), Some(y)) = (
        ra.next().map_err(|e| format!("{table}: {e}"))?,
        rb.next().map_err(|e| format!("{table}: {e}"))?,
    ) {
        let (xi, yi): (i64, i64) = (
            x.get(0).map_err(|e| e.to_string())?,
            y.get(0).map_err(|e| e.to_string())?,
        );
        let (xk, yk): (String, String) = (
            x.get(1).map_err(|e| e.to_string())?,
            y.get(1).map_err(|e| e.to_string())?,
        );
        if xi != yi || xk != yk {
            break;
        }
        prefix = xi;
    }
    Ok(prefix)
}

/// Does `src` hold, at rowid `mark`, the row whose signature `sig` was recorded? The signature
/// depends on the column list it was taken over, and an old app that opened the ledger since may
/// have added columns at the end, so the shorter lists, from the end, are tried too.
fn mark_matches(src: &Connection, table: &str, mark: i64, sig: &str) -> bool {
    if mark <= 0 {
        return false;
    }
    let Ok(cols) = columns(src, "main", table) else {
        return false;
    };
    let hit = |list: &[String]| {
        row_sig_in(src, "main", table, list, mark)
            .ok()
            .flatten()
            .as_deref()
            == Some(sig)
    };
    if (1..=cols.len()).rev().any(|k| hit(&cols[..k])) {
        return true;
    }
    // Last resort: a schema change that took a column out of the middle leaves no prefix of the
    // columns that was recorded; the core set is what every release has.
    let core = core_in(table, &cols);
    !core.is_empty() && hit(&core)
}

/// The columns of each positional table that every released schema has and never dropped, so a
/// row can be recognised across schema changes (1.9.0 added `faults.agent_id` and `read_events`
/// ones, 1.6.0 dropped `read_events.is_subagent`). `read_events` is the 1.0.0 table, whose nine
/// columns every later one keeps in its first nine places; `faults` is the table as 1.5.0
/// created it. The `core_columns_are_in_every_released_schema` test checks them against the
/// fixtures of 1.4.0, 1.8.0 and 1.9.0.
const CORE_READ_EVENTS: [&str; 9] = [
    "ts",
    "tool",
    "path",
    "lines",
    "tokens_returned",
    "full_tokens",
    "saved_tokens",
    "routed_via",
    "channel",
];

fn core_columns(table: &str) -> &'static [&'static str] {
    match table {
        "read_events" => &CORE_READ_EVENTS,
        "faults" => &CORE_FAULTS,
        _ => &[],
    }
}

/// The core columns of `table`, in the ledger's order, when `cols` has all of them; else none.
fn core_in(table: &str, cols: &[String]) -> Vec<String> {
    let core = core_columns(table);
    let have: Vec<String> = cols
        .iter()
        .filter(|c| core.contains(&c.as_str()))
        .cloned()
        .collect();
    if have.len() == core.len() {
        have
    } else {
        Vec::new()
    }
}

/// Rows per stored chunk of a row list.
const LIST_CHUNK: usize = 4096;
/// Bytes of one list entry: the rowid, then the first 8 bytes of the row's digest.
const ENTRY: usize = 16;

/// The `legacy_merge` source of the row list of `table` with this content.
fn list_source(table: &str, id: &str) -> String {
    format!("rows:{table}:{id}")
}

/// Keep, in N, the rowid and a digest of every row the source holds in `table` (over all the
/// source's columns, which are stored too), in chunks, under a key made of the content so two
/// imports never replace each other's list.
///
/// Twice: over all of the source's columns, and over the core set (see [`core_columns`]), which a
/// copy on another schema can still be compared on.
fn record_rows(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<(), ImportError> {
    let cols = columns(tx, "main", table).map_err(|e| failed(table, e))?;
    if cols.is_empty() {
        return Ok(());
    }
    record_list(tx, table, &cols)?;
    let core = core_in(table, &cols);
    if !core.is_empty() && core != cols {
        record_list(tx, table, &core)?;
    }
    Ok(())
}

fn record_list(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    cols: &[String],
) -> Result<(), ImportError> {
    let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
    let sql = format!(
        "SELECT rowid, {} FROM main.{} ORDER BY rowid",
        key_expr("", &refs),
        quote(table)
    );
    let mut all: Vec<u8> = Vec::new();
    {
        let mut st = tx.prepare(&sql).map_err(|e| failed(table, e))?;
        let mut rs = st.query([]).map_err(|e| failed(table, e))?;
        while let Some(r) = rs.next().map_err(|e| failed(table, e))? {
            let id: i64 = r.get(0).map_err(|e| failed(table, e))?;
            let key: String = r.get(1).map_err(|e| failed(table, e))?;
            all.extend_from_slice(&id.to_le_bytes());
            all.extend_from_slice(&Sha256::digest(key.as_bytes())[..8]);
        }
    }
    if all.is_empty() {
        return Ok(());
    }
    let joined = cols.join("\t");
    let id = hex(&Sha256::digest([joined.as_bytes(), &all].concat())[..8]);
    let source = list_source(table, &id);
    let put =
        "INSERT OR REPLACE INTO n.legacy_merge (source, tbl, hwm, sig) VALUES (?1, ?2, ?3, ?4)";
    tx.execute(
        put,
        params![source, "cols", (all.len() / ENTRY) as i64, joined],
    )
    .map_err(|e| failed("legacy_merge", e))?;
    for (i, chunk) in all.chunks(LIST_CHUNK * ENTRY).enumerate() {
        tx.execute(
            put,
            params![
                source,
                format!("{i:08}"),
                (chunk.len() / ENTRY) as i64,
                chunk
            ],
        )
        .map_err(|e| failed("legacy_merge", e))?;
    }
    Ok(())
}

/// A row list an earlier import recorded: the columns it was taken over and the entries.
struct RowList {
    cols: Vec<String>,
    entries: Vec<u8>,
}

fn recorded_lists(conn: &Connection, table: &str) -> Result<Vec<RowList>, ImportError> {
    let mut st = conn
        .prepare("SELECT source, sig FROM n.legacy_merge WHERE tbl = 'cols' AND source LIKE ?1")
        .map_err(|e| failed("legacy_merge", e))?;
    let heads = st
        .query_map(params![format!("rows:{table}:%")], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| failed("legacy_merge", e))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| failed("legacy_merge", e))?;
    let mut out = Vec::new();
    for (source, cols) in heads {
        let mut st = conn
            .prepare(
                "SELECT sig FROM n.legacy_merge WHERE source = ?1 AND tbl GLOB '[0-9]*' \
                 ORDER BY tbl",
            )
            .map_err(|e| failed("legacy_merge", e))?;
        let chunks = st
            .query_map(params![source], |r| r.get::<_, Vec<u8>>(0))
            .map_err(|e| failed("legacy_merge", e))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| failed("legacy_merge", e))?;
        out.push(RowList {
            cols: cols.split('\t').map(str::to_string).collect(),
            entries: chunks.concat(),
        });
    }
    Ok(out)
}

/// How many leading rowids the source shares with a recorded list, row for row, over the list's
/// columns: the same walk as [`common_prefix`], against a list instead of a live ledger.
fn prefix_in_list(src: &Connection, table: &str, list: &RowList) -> Result<i64, String> {
    let refs: Vec<&str> = list.cols.iter().map(String::as_str).collect();
    let sql = format!(
        "SELECT rowid, {} FROM main.{} ORDER BY rowid",
        key_expr("", &refs),
        quote(table)
    );
    let mut st = src.prepare(&sql).map_err(|e| format!("{table}: {e}"))?;
    let mut rs = st.query([]).map_err(|e| format!("{table}: {e}"))?;
    let mut prefix = 0i64;
    for entry in list.entries.chunks(ENTRY) {
        if entry.len() < ENTRY {
            break;
        }
        let Some(r) = rs.next().map_err(|e| format!("{table}: {e}"))? else {
            break;
        };
        let id: i64 = r.get(0).map_err(|e| e.to_string())?;
        let key: String = r.get(1).map_err(|e| e.to_string())?;
        let mut want = [0u8; 8];
        want.copy_from_slice(&entry[..8]);
        if id != i64::from_le_bytes(want) || Sha256::digest(key.as_bytes())[..8] != entry[8..] {
            break;
        }
        prefix = id;
    }
    Ok(prefix)
}

/// The previous folder's `data/` while the freeze has not taken its spool (those lines are N's
/// to come, so a source holding the same ones adds nothing); none once it has, or with no backup.
fn unfrozen_backup_data(conn: &Connection, n: &Path) -> Result<Option<PathBuf>, ImportError> {
    let Seen::Migrated(m) = marker::read(n) else {
        return Ok(None);
    };
    let Some(b) = backup_dir(n, &m) else {
        return Ok(None);
    };
    let frozen = conn
        .query_row(
            "SELECT 1 FROM n.legacy_merge WHERE source = ?1 AND tbl = ?2",
            params![source_key(&b), FROZEN_TBL],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| failed("legacy_merge", e))?
        .is_some();
    Ok(if frozen {
        None
    } else {
        Some(b.join(super::commit::DATA_DIR))
    })
}

fn run_import(
    p: &Place<'_>,
    src: &Source,
    mut conn: Connection,
    opts: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| failed("busy timeout", e))?;
    // N is attached by plain path. The source's exclusive locking mode is the default for
    // anything attached after it, and N is shared with the running app, so say normal.
    conn.execute("ATTACH DATABASE ?1 AS n", params![p.n_db.to_string_lossy()])
        .map_err(|e| failed("cannot attach the live ledger", e))?;
    conn.pragma_update(Some("n"), "locking_mode", "NORMAL")
        .map_err(|e| failed("cannot attach the live ledger", e))?;

    // 4. What is new.
    let mut prefix: BTreeMap<String, i64> = POSITIONAL_TABLES
        .iter()
        .map(|t| (t.to_string(), 0))
        .collect();
    for db in originals(p.n, &conn, &p.ledger)? {
        let orig = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| failed(&format!("cannot read {}", db.display()), e))?;
        for table in POSITIONAL_TABLES {
            let k = common_prefix(&conn, &orig, table).map_err(|e| failed("prefix", e))?;
            let slot = prefix.entry(table.to_string()).or_insert(0);
            *slot = (*slot).max(k);
        }
    }
    // With no copy of the previous folder left to compare against (the backup was deleted), the
    // marker still knows the row at each mark: a source that has it holds everything up to there.
    if let Seen::Migrated(m) = marker::read(p.n) {
        let have_copy = backup_dir(p.n, &m)
            .is_some_and(|b| b.join(super::commit::DATA_DIR).join(LEDGER).is_file());
        if !have_copy {
            for table in POSITIONAL_TABLES {
                let (Some(&mark), Some(sig)) = (m.hwm.get(table), m.hwm_sig.get(table)) else {
                    continue;
                };
                if mark_matches(&conn, table, mark, sig) {
                    let slot = prefix.entry(table.to_string()).or_insert(0);
                    *slot = (*slot).max(mark);
                }
            }
        }
    }
    // Identity by content, part one: the rows the merge from the backup reached, each a rowid and
    // the signature of the row there. A source with that row at that rowid shares everything up to it.
    for (table, mark, sig) in recorded_marks(&conn)? {
        if mark_matches(&conn, &table, mark, &sig) {
            let slot = prefix.entry(table).or_insert(0);
            *slot = (*slot).max(mark);
        }
    }
    // Part two: every earlier import left the whole row list of each positional table (a hash per
    // row, with its rowid), so a copy of that source, older or newer than the one imported, shares
    // the rows it has in common with the list, in order, whether or not the ledger it came from
    // still exists.
    for table in POSITIONAL_TABLES {
        let s_cols = columns(&conn, "main", table).map_err(|e| failed(table, e))?;
        for list in recorded_lists(&conn, table)? {
            if list.cols.iter().any(|c| !s_cols.contains(c)) {
                continue;
            }
            let k = prefix_in_list(&conn, table, &list).map_err(|e| failed("prefix", e))?;
            let slot = prefix.entry(table.to_string()).or_insert(0);
            *slot = (*slot).max(k);
        }
    }
    let shares_history = prefix.values().any(|&v| v > 0);

    // 5. One transaction.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| failed("cannot start the import", e))?;
    // Faults merge as a multiset by core-content digest against N (see `faultset`): the rows
    // above the prefix and the spool lines come in only while the source holds more of a digest
    // than N does, whatever route the others took.
    let spool_active = !matches!(src.kind, Kind::Ledger) && !shares_history;
    let (spool_recs, spool_bytes) = if spool_active {
        read_spool_files(&p.actual.join("faults.jsonl")).map_err(|e| failed("the spool", e))?
    } else {
        (Vec::new(), Vec::new())
    };
    let spool_keys = record_digests(&spool_recs).map_err(|e| failed("faults", e))?;
    let src_rows = row_digests(&tx, "main").map_err(|e| failed("faults", e))?;
    let mut src_counts = tally(spool_keys.iter());
    for (_, k) in src_rows.iter().flatten() {
        *src_counts.entry(k.clone()).or_insert(0) += 1;
    }
    let backup_data = unfrozen_backup_data(&tx, p.n)?;
    let n_counts =
        counts_in_n(&tx, "n", p.n, backup_data.as_deref()).map_err(|e| failed("faults", e))?;
    let Some(&faults_from) = prefix.get("faults") else {
        return Err(failed("faults", "no prefix was computed"));
    };
    let row_cands: Vec<(i64, String)> = src_rows
        .iter()
        .flatten()
        .filter(|(id, _)| *id > faults_from)
        .cloned()
        .collect();
    let cands: Vec<String> = row_cands
        .iter()
        .map(|(_, k)| k.clone())
        .chain(spool_keys.iter().cloned())
        .collect();
    let admitted = admit_fresh(&src_counts, &n_counts, &cands);
    let skip: Vec<i64> = row_cands
        .iter()
        .zip(&admitted)
        .filter(|(_, ok)| !**ok)
        .map(|((id, _), _)| *id)
        .collect();
    let (_, spool_admitted) = admitted.split_at(row_cands.len());

    let mut rows: BTreeMap<String, usize> = BTreeMap::new();
    let tables = KEYED_TABLES
        .iter()
        .map(|t| (*t, false))
        .chain(POSITIONAL_TABLES.iter().map(|t| (*t, true)));
    for (table, positional) in tables {
        let n_cols = columns(&tx, "n", table).map_err(|e| failed(table, e))?;
        let s_cols = columns(&tx, "main", table).map_err(|e| failed(table, e))?;
        let list = n_cols
            .iter()
            .filter(|c| s_cols.contains(c))
            .map(|c| quote(c))
            .collect::<Vec<_>>()
            .join(", ");
        if n_cols.is_empty() || s_cols.is_empty() || list.is_empty() {
            continue;
        }
        let t = quote(table);
        let from: i64 = match prefix.get(table) {
            Some(&v) if positional => v,
            _ => 0,
        };
        let verb = if positional {
            "INSERT"
        } else {
            "INSERT OR IGNORE"
        };
        let skipping = table == "faults" && !skip.is_empty();
        if skipping {
            tx.execute_batch(
                "CREATE TEMP TABLE IF NOT EXISTS lumen_skip (id INTEGER PRIMARY KEY); \
                 DELETE FROM temp.lumen_skip",
            )
            .map_err(|e| failed(table, e))?;
            for id in &skip {
                tx.execute("INSERT INTO temp.lumen_skip (id) VALUES (?1)", params![id])
                    .map_err(|e| failed(table, e))?;
            }
        }
        let not_in = if skipping {
            " AND rowid NOT IN (SELECT id FROM temp.lumen_skip)"
        } else {
            ""
        };
        let added = tx
            .execute(
                &format!(
                    "{verb} INTO n.{t} ({list}) SELECT {list} FROM main.{t} \
                     WHERE rowid > ?1{not_in} ORDER BY rowid"
                ),
                params![from],
            )
            .map_err(|e| failed(table, e))?;
        if skipping {
            tx.execute_batch("DROP TABLE temp.lumen_skip")
                .map_err(|e| failed(table, e))?;
        }
        if added > 0 {
            rows.insert(table.to_string(), added);
        }
    }
    let (spool, spool_sig) = match src.kind {
        Kind::Ledger => (Spool::NoneHere, None),
        // A folder that shares history keeps its spool: it is the old one's.
        Kind::Folder | Kind::Shared if shares_history => (Spool::Kept, None),
        Kind::Folder | Kind::Shared => {
            let fresh: Vec<faults::FaultRecord> = spool_recs
                .iter()
                .zip(spool_admitted)
                .filter(|(_, ok)| **ok)
                .map(|(r, _)| r.clone())
                .collect();
            if fresh.is_empty() && !spool_recs.is_empty() {
                (Spool::Kept, None)
            } else {
                faults::insert_into(&tx, "n", &fresh)
                    .map_err(|e| failed("faults from the spool", e))?;
                (Spool::Taken(fresh.len()), Some(hex(&spool_bytes)))
            }
        }
    };
    // What this source held in each positional table, row by row, so a later copy of it is
    // recognised by content alone.
    for table in POSITIONAL_TABLES {
        record_rows(&tx, table)?;
    }
    let total: usize = rows.values().sum::<usize>()
        + match &spool {
            Spool::Taken(k) => *k,
            _ => 0,
        };
    tx.execute(
        "INSERT INTO n.legacy_merge (source, tbl, hwm, sig) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (source, tbl) DO UPDATE SET hwm = excluded.hwm, sig = excluded.sig",
        params![p.done_key, DONE_TBL, total as i64, spool_sig],
    )
    .map_err(|e| failed("legacy_merge", e))?;
    let mut rep = ImportReport {
        prefix,
        rows,
        spool,
        ..empty_report(p.name.clone(), p.dir.clone(), p.placed.clone(), opts)
    };
    if opts.dry_run || opts.stop == Stop::BeforeCommit {
        // Dropped: the transaction rolls back.
        rep.stopped = opts.stop == Stop::BeforeCommit;
        drop(tx);
        drop(conn);
        return Ok(rep);
    }
    tx.commit().map_err(|e| failed("commit", e))?;
    drop(conn);
    if opts.stop == Stop::AfterCommit {
        rep.stopped = true;
        return Ok(rep);
    }

    // 6. Finish: what moved, or was copied, into `legacy-imports` is read-only. The user's own
    // folder, when it was copied from another volume, is never touched.
    if let Err(e) = make_read_only(&p.actual) {
        return Err(failed(
            "imported, but the folder could not be made read-only",
            e,
        ));
    }
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faults::FaultRecord;
    use crate::legacy::commit::{self, CommitOptions, DATA_DIR, Moved, Outcome};
    use crate::legacy::marker::MarkerV2;
    use crate::legacy::retire::Native;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    const V190: &str = include_str!("../../tests/fixtures/ledger_schema_v1.9.0.sql");
    const NOW: &str = "2026-10-20T08:00:00Z";
    const TABLES: [&str; 7] = [
        "turns",
        "calibration",
        "session_events",
        "intercept_blocks",
        "transcript_scans",
        "read_events",
        "faults",
    ];

    fn q(p: &Path, sql: &str) {
        Connection::open(p).unwrap().execute_batch(sql).unwrap();
    }

    fn count(p: &Path, table: &str) -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    fn counts(p: &Path) -> BTreeMap<String, i64> {
        TABLES
            .iter()
            .map(|t| (t.to_string(), count(p, t)))
            .collect()
    }

    fn kinds(p: &Path) -> BTreeSet<String> {
        let c = Connection::open(p).unwrap();
        let mut st = c.prepare("SELECT kind FROM faults").unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn line(kind: &str) -> String {
        serde_json::to_string(&FaultRecord::now(kind, "v")).unwrap()
    }

    /// One row in every table of the 1.9.0 schema, tagged so each is told apart.
    fn rows_for(tag: &str) -> String {
        format!(
            "INSERT INTO turns (message_id, session_id, ts, input_tokens, output_tokens, \
               cache_read_input_tokens, cache_creation_input_tokens) \
               VALUES ('m-{tag}', 's', '2026-01-01T00:00:00Z', 1, 2, 3, 4); \
             INSERT INTO calibration VALUES ('m-{tag}', 10, 12); \
             INSERT INTO session_events (session_id, kind, uuid, ts_ms) \
               VALUES ('s', 'compaction', 'u-{tag}', 5); \
             INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
               routed_via) VALUES ('t-{tag}', 'Read', '/{tag}', 1, 2, 1, 'builtin_read'); \
             INSERT INTO faults (ts, kind, session_id) VALUES ('t-{tag}', 'row-{tag}', 's'); \
             INSERT INTO intercept_blocks (tool_use_id, session_id, path, kind, ts_ms, uuid) \
               VALUES ('tu-{tag}', 's', '/{tag}', 'redirect', 1, 'u'); \
             INSERT INTO transcript_scans (source_file, session_id, matcher, through_ms) \
               VALUES ('f-{tag}', 's', 1, 1);"
        )
    }

    fn ledger(path: &Path, tags: &[&str]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.execute_batch(V190).unwrap();
        for t in tags {
            c.execute_batch(&rows_for(t)).unwrap();
        }
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let target = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &target);
            } else {
                fs::copy(e.path(), &target).unwrap();
            }
        }
    }

    /// Names, sizes, bytes and mtimes of everything under `dir`, but the `-shm` files SQLite makes
    /// and removes on its own.
    fn snap(dir: &Path) -> BTreeMap<String, (Vec<u8>, Option<std::time::SystemTime>)> {
        fn walk(
            root: &Path,
            dir: &Path,
            out: &mut BTreeMap<String, (Vec<u8>, Option<std::time::SystemTime>)>,
        ) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                if p.is_dir() {
                    out.insert(format!("{rel}/"), (Vec::new(), None));
                    walk(root, &p, out);
                } else if !rel.ends_with("-shm") {
                    let md = fs::metadata(&p).unwrap();
                    out.insert(rel, (fs::read(&p).unwrap(), md.modified().ok()));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    fn opts() -> ImportOptions {
        let mut o = ImportOptions::new(NOW, false);
        o.probe_tries = 1;
        o.probe_delay = Duration::from_millis(10);
        o
    }

    /// A migrated and moved install, built by the real migration: N, the backup, the marker.
    struct Install {
        _t: TempDir,
        /// The data root the previous folder lived in.
        root: PathBuf,
        n: PathBuf,
        b: PathBuf,
    }

    impl Install {
        fn n_db(&self) -> PathBuf {
            self.n.join("lumen.db")
        }
        fn b_data(&self) -> PathBuf {
            self.b.join(DATA_DIR)
        }
        /// The place the previous folder used to have: free again after the move.
        fn old_place(&self) -> PathBuf {
            self.root.join("example.previous.app")
        }
        fn imports(&self) -> PathBuf {
            self.n.join(IMPORTS_DIR)
        }
        fn first_import(&self) -> PathBuf {
            self.imports().join("2026-10-20-1")
        }
        fn import(&self, arg: &Path) -> Result<ImportReport, ImportError> {
            import(&self.n, arg, &opts(), &Native)
        }
    }

    fn install() -> Install {
        let t = TempDir::new().unwrap();
        let root = t.path().join("data");
        let l = root.join("example.previous.app");
        let n = root.join("example.current.app");
        ledger(&l.join("lumen.db"), &["snap", "snap2"]);
        fs::write(
            l.join("faults.jsonl"),
            format!("{}\n", line("backup-spool")),
        )
        .unwrap();
        let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
        o.probe_tries = 1;
        o.probe_delay = Duration::from_millis(10);
        let m: MarkerV2 = match commit::migrate(&l, &n, &o, &Native) {
            Outcome::Migrated { marker, moved } => {
                assert_eq!(moved, Moved::Done);
                marker
            }
            other => panic!("expected Migrated, got {other:?}"),
        };
        let b = backup_dir(&n, &m).unwrap();
        Install { _t: t, root, n, b }
    }

    /// A folder an old binary re-created at the previous place, with one row of its own in
    /// every table and a spool line.
    fn recreated(i: &Install) -> PathBuf {
        let d = i.old_place();
        ledger(&d.join("lumen.db"), &["fresh"]);
        fs::write(d.join("faults.jsonl"), format!("{}\n", line("fresh-spool"))).unwrap();
        d
    }

    /// A copy of the previous folder as a backup tool would restore it.
    fn restored(i: &Install) -> PathBuf {
        let d = i.old_place();
        copy_dir(&i.b_data(), &d);
        d
    }

    // ---- what is new ----

    #[test]
    fn a_restored_copy_of_the_old_folder_adds_no_row() {
        let i = install();
        let before = counts(&i.n_db());
        let kinds_before = kinds(&i.n_db());
        let copy = restored(&i);
        assert!(
            copy.join("faults.jsonl").is_file(),
            "the copy has its spool"
        );

        let r = i.import(&copy).unwrap();

        assert_eq!(r.total_rows(), 0, "{r:?}");
        assert_eq!(counts(&i.n_db()), before, "not one row came in twice");
        assert_eq!(kinds(&i.n_db()), kinds_before);
        assert_eq!(
            r.spool,
            Spool::Kept,
            "a folder that shares history keeps its spool"
        );
        assert!(
            r.prefix["read_events"] > 0 && r.prefix["faults"] > 0,
            "{r:?}"
        );
        assert!(!copy.exists(), "the old name is gone");
        assert!(i.first_import().join("lumen.db").is_file());
    }

    #[test]
    fn a_recreated_folder_is_moved_into_legacy_imports_and_imported_once() {
        let i = install();
        let before = counts(&i.n_db());
        let d = recreated(&i);

        let r = i.import(&d).unwrap();

        assert_eq!(r.placed, Placed::Moved);
        assert!(!d.exists(), "nothing keeps the old name");
        assert!(i.first_import().join("lumen.db").is_file());
        assert!(i.first_import().join("faults.jsonl").is_file());
        let after = counts(&i.n_db());
        for t in TABLES {
            // Every table gains the one row the folder had; faults also gains its spool line.
            let want = before[t] + if t == "faults" { 2 } else { 1 };
            assert_eq!(after[t], want, "{t}: {r:?}");
        }
        assert_eq!(
            r.prefix["read_events"], 0,
            "a re-created ledger shares no history"
        );
        assert_eq!(r.spool, Spool::Taken(1));
        assert!(kinds(&i.n_db()).contains("fresh-spool"));
        assert!(kinds(&i.n_db()).contains("row-fresh"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(i.first_import().join("lumen.db"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o444, "the moved folder is read-only");
        }
    }

    #[test]
    fn importing_it_again_says_already_imported() {
        let i = install();
        let d = recreated(&i);
        i.import(&d).unwrap();
        let after = counts(&i.n_db());

        let moved = i.first_import();
        let err = i.import(&moved).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyImported(_)), "{err:?}");
        assert!(err.to_string().contains("already imported"));
        let err = i.import(&moved.join("lumen.db")).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyImported(_)), "{err:?}");
        assert_eq!(counts(&i.n_db()), after);
    }

    #[test]
    fn a_restored_copy_written_to_after_the_restore_adds_only_its_new_rows() {
        let i = install();
        let before = counts(&i.n_db());
        let copy = restored(&i);
        q(&copy.join("lumen.db"), &rows_for("after"));
        fs::write(
            copy.join("faults.jsonl"),
            format!("{}\n", line("late-spool")),
        )
        .unwrap();

        let r = i.import(&copy).unwrap();

        let after = counts(&i.n_db());
        for t in TABLES {
            assert_eq!(after[t], before[t] + 1, "{t}: {r:?}");
        }
        assert_eq!(r.rows["read_events"], 1);
        assert_eq!(r.rows["faults"], 1);
        assert!(kinds(&i.n_db()).contains("row-after"));
        assert_eq!(
            r.spool,
            Spool::Kept,
            "the spool is the old one's: not taken twice"
        );
        assert!(!kinds(&i.n_db()).contains("late-spool"));
    }

    #[test]
    fn the_prefix_is_rows_equal_in_order_until_the_first_difference() {
        let t = TempDir::new().unwrap();
        let put = |name: &str, paths: &[&str]| -> Connection {
            let p = t.path().join(name);
            let c = Connection::open(&p).unwrap();
            c.execute_batch(V190).unwrap();
            for (n, path) in paths.iter().enumerate() {
                c.execute(
                    "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                       saved_tokens, routed_via) VALUES (?1, 'Read', ?2, 1, 2, 1, 'builtin_read')",
                    params![format!("t{n}"), path],
                )
                .unwrap();
            }
            c
        };
        let src = put("src", &["a", "b", "c", "d", "e"]);
        let prefix = |paths: &[&str]| {
            let o = put(&format!("o{}", paths.join("")), paths);
            common_prefix(&src, &o, "read_events").unwrap()
        };
        assert_eq!(prefix(&["a", "b", "c", "d", "e"]), 5, "the same ledger");
        assert_eq!(
            prefix(&["a", "b", "x", "d", "e"]),
            2,
            "diverged at the third row"
        );
        assert_eq!(prefix(&["a", "b"]), 2, "an older snapshot of the source");
        assert_eq!(
            prefix(&["a", "b", "c", "d", "e", "f", "g"]),
            5,
            "the source is the older one"
        );
        assert_eq!(prefix(&["z"]), 0, "nothing in common");
        assert_eq!(prefix(&[]), 0, "an empty original");
    }

    // ---- killed runs ----

    #[test]
    fn an_import_killed_after_the_move_finishes_on_rerun() {
        let i = install();
        let before = counts(&i.n_db());
        let d = recreated(&i);
        let mut o = opts();
        o.stop = Stop::AfterMove;

        let r = import(&i.n, &d, &o, &Native).unwrap();
        assert!(r.stopped);
        assert!(!d.exists());
        assert_eq!(counts(&i.n_db()), before, "nothing came in yet");
        assert!(!is_done(&i.n_db(), &done_source("2026-10-20-1")));

        // The old path is gone; the rerun is on the folder where it landed.
        let moved = i.first_import();
        let r = i.import(&moved).unwrap();
        assert_eq!(r.placed, Placed::Already);
        assert_eq!(r.rows["read_events"], 1);
        assert_eq!(count(&i.n_db(), "read_events"), before["read_events"] + 1);
        assert_eq!(r.spool, Spool::Taken(1));
        assert!(matches!(
            i.import(&moved).unwrap_err(),
            ImportError::AlreadyImported(_)
        ));
    }

    #[test]
    fn an_import_killed_after_its_commit_adds_nothing() {
        let i = install();
        let d = recreated(&i);
        let before = counts(&i.n_db());
        let mut o = opts();
        o.stop = Stop::AfterCommit;

        let r = import(&i.n, &d, &o, &Native).unwrap();
        assert!(r.stopped);
        let after = counts(&i.n_db());
        assert_eq!(
            after["read_events"],
            before["read_events"] + 1,
            "it committed"
        );

        let err = i.import(&i.first_import()).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyImported(_)), "{err:?}");
        assert_eq!(counts(&i.n_db()), after, "the rerun added nothing");
    }

    #[test]
    fn an_import_killed_before_its_commit_rolls_back_and_the_rerun_imports_once() {
        let i = install();
        let d = recreated(&i);
        let before = counts(&i.n_db());
        let mut o = opts();
        o.stop = Stop::BeforeCommit;

        let r = import(&i.n, &d, &o, &Native).unwrap();
        assert!(r.stopped);
        assert_eq!(counts(&i.n_db()), before, "the transaction rolled back");
        assert!(!kinds(&i.n_db()).contains("fresh-spool"));

        i.import(&i.first_import()).unwrap();
        let once = counts(&i.n_db());
        assert_eq!(once["read_events"], before["read_events"] + 1);
        assert_eq!(once["faults"], before["faults"] + 2);
    }

    // ---- refusals ----

    #[test]
    fn the_backup_and_the_live_ledger_are_refused_and_say_why() {
        let i = install();
        let before = (snap(&i.n), snap(&i.b));
        for (arg, backup) in [
            (i.b_data(), true),
            (i.b_data().join("lumen.db"), true),
            (i.n.clone(), false),
            (i.n_db(), false),
        ] {
            let err = i.import(&arg).unwrap_err();
            let text = err.to_string();
            if backup {
                assert!(matches!(err, ImportError::InBackup(_)), "{arg:?}: {err:?}");
                assert!(text.contains("backup") && text.contains("merge"), "{text}");
            } else {
                assert_eq!(err, ImportError::Live, "{arg:?}");
                assert!(text.contains("live ledger"), "{text}");
            }
        }
        assert_eq!((snap(&i.n), snap(&i.b)), before, "nothing changed");
        assert!(!i.imports().exists());
    }

    #[test]
    fn a_start_that_has_not_finished_refuses_and_so_does_a_foreign_file() {
        let i = install();
        let bare = i.root.join("never-started");
        fs::create_dir_all(&bare).unwrap();
        let d = recreated(&i);
        let err = import(&bare, &d, &opts(), &Native).unwrap_err();
        assert!(matches!(err, ImportError::NotStarted(_)), "{err:?}");
        assert!(err.to_string().contains("start Lumen first"));
        assert!(d.join("lumen.db").is_file(), "nothing moved");

        // Not a Lumen ledger: a SQLite file without the columns, and a file that is not SQLite.
        let other = i.root.join("elsewhere");
        fs::create_dir_all(&other).unwrap();
        q(&other.join("lumen.db"), "CREATE TABLE notes (a TEXT);");
        let err = i.import(&other).unwrap_err();
        assert!(matches!(err, ImportError::NotLumen(_)), "{err:?}");
        let junk = i.root.join("junk");
        fs::create_dir_all(&junk).unwrap();
        fs::write(junk.join("lumen.db"), b"this is not a database at all").unwrap();
        assert!(matches!(
            i.import(&junk).unwrap_err(),
            ImportError::NotLumen(_)
        ));
        assert!(matches!(
            i.import(&i.root.join("nothing-here")).unwrap_err(),
            ImportError::Missing(_)
        ));
        let empty = i.root.join("empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            i.import(&empty).unwrap_err(),
            ImportError::NotLumen(_)
        ));
        assert!(!i.imports().exists(), "no refusal made a folder");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_never_followed() {
        let i = install();
        let d = recreated(&i);
        let link = i.root.join("a-link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        let err = i.import(&link).unwrap_err();
        assert!(matches!(err, ImportError::Link(_)), "{err:?}");
        assert!(d.join("lumen.db").is_file());
    }

    #[test]
    fn a_held_ledger_is_refused_and_nothing_moves() {
        let i = install();
        let d = recreated(&i);
        // An older Lumen with the ledger open in WAL mode, as its daemon holds it.
        let old = Connection::open(d.join("lumen.db")).unwrap();
        old.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        old.execute_batch("INSERT INTO faults (ts, kind) VALUES ('t', 'held')")
            .unwrap();
        let before = (snap(&i.n), snap(&d));

        let err = i.import(&d).unwrap_err();
        assert!(matches!(err, ImportError::InUse(_)), "{err:?}");
        assert!(err.to_string().contains("quit the older Lumen"));

        assert_eq!((snap(&i.n), snap(&d)), before, "nothing changed");
        assert!(!i.imports().exists());
        drop(old);
    }

    #[cfg(windows)]
    #[test]
    fn a_held_handle_in_the_folder_is_refused_and_nothing_moves() {
        use std::os::windows::fs::OpenOptionsExt;
        let i = install();
        let d = recreated(&i);
        // A handle that does not share delete, inside the folder: what an old process's open log
        // or spool looks like to the rename.
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(d.join("faults.jsonl"))
            .unwrap();
        let err = i.import(&d).unwrap_err();
        assert!(matches!(err, ImportError::InUse(_)), "{err:?}");
        assert!(d.join("lumen.db").is_file(), "the folder stayed");
        assert!(!i.imports().exists(), "and nothing was left behind");
        drop(held);
        i.import(&d).unwrap();
        assert!(i.first_import().join("lumen.db").is_file());
    }

    /// A ledger file imported in place (another volume) is an original too: a later copy of it
    /// adds no row.
    #[test]
    fn a_copy_of_an_in_place_imported_ledger_file_adds_no_row() {
        let i = install();
        let f = i.root.join("usb").join("old-backup.db");
        ledger(&f, &["usb1", "usb2"]);
        let r = import(&i.n, &f, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
        assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
        let after_first = counts(&i.n_db());
        let copy = i.root.join("home").join("old-backup-copy.db");
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::copy(&f, &copy).unwrap();
        let r2 = i.import(&copy).unwrap();
        assert_eq!(
            counts(&i.n_db()),
            after_first,
            "a copy came in twice: {r2:?}"
        );
    }

    /// An in-place import (another volume) is known by its rows, not by the path it had.
    mod in_place_by_content {
        use super::*;

        fn in_place(i: &Install, d: &Path) -> ImportReport {
            let r = import(&i.n, d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            r
        }

        /// G: the same folder, given again under another path (remounted elsewhere).
        #[test]
        fn the_same_folder_under_another_path_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("usb-spool"))).unwrap();
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            // Remounted elsewhere: the first path is gone, the rows are the same.
            let again = i.root.join("mnt").join("elsewhere");
            fs::create_dir_all(again.parent().unwrap()).unwrap();
            fs::rename(&d, &again).unwrap();
            let r = i.import(&again).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
            assert!(matches!(r.spool, Spool::Kept), "{r:?}");
        }

        /// H: the original is gone, then a copy of it is imported.
        #[test]
        fn a_copy_after_the_original_was_removed_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            let kept = i.root.join("kept");
            copy_dir(&d, &kept);
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(&d).unwrap();
            let r = i.import(&kept).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// I: the volume is absent (the first path cannot be read at all), a ledger file copy
        /// of it comes from somewhere else.
        #[test]
        fn a_copy_while_the_volume_is_absent_adds_no_row() {
            let i = install();
            let f = i.root.join("vol1").join("backup.db");
            ledger(&f, &["u1", "u2"]);
            let copy = i.root.join("home").join("c.db");
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::copy(&f, &copy).unwrap();
            in_place(&i, &f);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("vol1")).unwrap();
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// A copy that grew keeps its history out and takes only what is past it.
        #[test]
        fn a_copy_that_grew_adds_only_the_new_rows() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            let grown = i.root.join("grown");
            copy_dir(&d, &grown);
            in_place(&i, &d);
            fs::remove_dir_all(&d).unwrap();
            q(&grown.join("lumen.db"), &rows_for("u3"));
            let before = counts(&i.n_db());
            i.import(&grown).unwrap();
            let after = counts(&i.n_db());
            assert_eq!(after["read_events"], before["read_events"] + 1);
            assert_eq!(after["faults"], before["faults"] + 1);
        }

        /// A different ledger still comes in whole.
        #[test]
        fn a_different_ledger_still_imports_all_its_rows() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            in_place(&i, &d);
            let before = counts(&i.n_db());
            let other = i.root.join("usb").join("other");
            ledger(&other.join("lumen.db"), &["x1", "x2", "x3"]);
            i.import(&other).unwrap();
            let after = counts(&i.n_db());
            assert_eq!(after["read_events"], before["read_events"] + 3);
            assert_eq!(after["faults"], before["faults"] + 3);
        }
    }

    /// A rename the OS refuses, or one across volumes.
    struct Refuse(io::ErrorKind);

    impl Ops for Refuse {
        fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
            Err(io::Error::new(self.0, "refused"))
        }
        fn link_dir(&self, _link: &Path, _target: &Path) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no links here"))
        }
        fn crosses_volumes(&self, _from: &Path, _to: &Path) -> bool {
            self.0 == io::ErrorKind::CrossesDevices
        }
    }

    #[test]
    fn a_move_the_os_refuses_changes_nothing() {
        let i = install();
        let d = recreated(&i);
        let before = (snap(&i.n), snap(&d));
        let err = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::PermissionDenied)).unwrap_err();
        assert!(matches!(err, ImportError::InUse(_)), "{err:?}");
        assert_eq!((snap(&i.n), snap(&d)), before);
        assert!(!i.imports().exists());
    }

    #[test]
    fn a_folder_on_another_volume_is_copied_into_legacy_imports() {
        let i = install();
        let before = counts(&i.n_db());
        let d = recreated(&i);
        let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
        assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
        assert!(r.lines().join("\n").contains("on another volume"));
        assert!(d.join("lumen.db").is_file(), "the original stayed");
        assert_eq!(count(&i.n_db(), "read_events"), before["read_events"] + 1);
        assert!(
            real(&r.dir).starts_with(real(&i.imports())),
            "its copy is the original: {r:?}"
        );
        // A second run on the same place is known by its rows, never by the place: it adds none.
        let again = i.import(&d).unwrap();
        assert_eq!(again.total_rows(), 0, "{again:?}");
        assert_eq!(count(&i.n_db(), "read_events"), before["read_events"] + 1);
    }

    // ---- a set-aside ledger, the dry run, a settled install ----

    #[test]
    fn a_set_aside_ledger_is_imported_with_its_wal() {
        let i = install();
        let before = counts(&i.n_db());
        // A ledger in WAL mode whose last rows are only in its -wal, copied as the migration
        // sets one aside.
        let mk = i.root.join("mk");
        fs::create_dir_all(&mk).unwrap();
        let live = Connection::open(mk.join("x.db")).unwrap();
        live.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        live.execute_batch(V190).unwrap();
        live.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        live.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
        live.execute_batch(&rows_for("inwal")).unwrap();
        let aside = i.n.join("lumen.db.pre-legacy");
        fs::copy(mk.join("x.db"), &aside).unwrap();
        fs::copy(mk.join("x.db-wal"), i.n.join("lumen.db.pre-legacy-wal")).unwrap();
        drop(live);
        assert!(
            fs::metadata(i.n.join("lumen.db.pre-legacy-wal"))
                .unwrap()
                .len()
                > 0,
            "the rows are in the wal"
        );

        let r = i.import(&aside).unwrap();

        assert_eq!(r.spool, Spool::NoneHere);
        assert_eq!(r.rows["read_events"], 1, "{r:?}");
        assert_eq!(count(&i.n_db(), "read_events"), before["read_events"] + 1);
        assert!(kinds(&i.n_db()).contains("row-inwal"));
        assert!(!aside.exists(), "it left N's top level");
        for sfx in ["-wal", "-shm", "-journal"] {
            assert!(!sidecar(&aside, sfx).exists(), "{sfx} came along");
        }
        assert_eq!(count(&i.first_import().join("lumen.db"), "read_events"), 1);
        assert!(matches!(
            i.import(&i.first_import().join("lumen.db")).unwrap_err(),
            ImportError::AlreadyImported(_)
        ));
    }

    #[test]
    fn dry_run_changes_nothing() {
        let i = install();
        let d = recreated(&i);
        // The state a running app leaves: the ledger already opened, in WAL mode.
        drop(crate::meter::connect_db(&i.n_db()).unwrap());
        let before = (snap(&i.n), snap(&d), counts(&i.n_db()));

        let mut o = opts();
        o.dry_run = true;
        let r = import(&i.n, &d, &o, &Native).unwrap();

        assert!(r.dry_run && !r.stopped);
        assert_eq!(r.rows["read_events"], 1, "it says what it would add: {r:?}");
        assert_eq!(r.spool, Spool::Taken(1));
        assert!(r.lines().join("\n").contains("Dry run"));
        assert_eq!((snap(&i.n), snap(&d), counts(&i.n_db())), before);
        assert!(!i.imports().exists());

        // And a real run afterwards is untouched by it.
        let real = i.import(&d).unwrap();
        assert_eq!(real.rows, r.rows);
    }

    #[test]
    fn a_settled_install_imports_into_its_empty_ledger() {
        let t = TempDir::new().unwrap();
        let n = t.path().join("example.current.app");
        fs::create_dir_all(&n).unwrap();
        marker::write_settled(&n).unwrap();
        let d = t.path().join("example.previous.app");
        ledger(&d.join("lumen.db"), &["one", "two"]);

        let r = import(&n, &d, &opts(), &Native).unwrap();

        assert_eq!(r.rows["read_events"], 2, "{r:?}");
        assert_eq!(count(&n.join("lumen.db"), "read_events"), 2);
        assert!(n.join(IMPORTS_DIR).join("2026-10-20-1").is_dir());
    }

    #[test]
    fn a_second_import_the_same_day_gets_the_next_name() {
        let i = install();
        let d = recreated(&i);
        i.import(&d).unwrap();
        let again = i.root.join("another-copy");
        ledger(&again.join("lumen.db"), &["third"]);
        let r = i.import(&again).unwrap();
        assert_eq!(r.name, "2026-10-20-2");
        assert!(i.imports().join("2026-10-20-2").join("lumen.db").is_file());
    }

    #[test]
    fn the_reports_name_the_command_with_the_path() {
        let p = Path::new("/data/example.previous.app");
        assert_eq!(
            command_for(p),
            "lumen import-ledger \"/data/example.previous.app\""
        );
        for text in [
            recreated_folder_text(p),
            other_ledger_text(p),
            set_aside_text(p),
        ] {
            assert!(text.contains(&command_for(p)), "{text}");
        }
        assert!(recreated_folder_text(p).contains("Nothing from it was merged"));
    }

    // ---- review fixes ----

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

    /// A migration whose move the OS refused: the marker is written, the previous folder is still
    /// at its place and the backup has no data yet.
    fn install_move_deferred() -> Install {
        let t = TempDir::new().unwrap();
        let root = t.path().join("data");
        let l = root.join("example.previous.app");
        let n = root.join("example.current.app");
        ledger(&l.join("lumen.db"), &["snap", "snap2"]);
        fs::write(
            l.join("faults.jsonl"),
            format!("{}\n", line("backup-spool")),
        )
        .unwrap();
        let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
        o.probe_tries = 1;
        o.probe_delay = Duration::from_millis(10);
        let ops = Refuse(io::ErrorKind::PermissionDenied);
        let m: MarkerV2 = match commit::migrate(&l, &n, &o, &ops) {
            Outcome::Migrated { marker, moved } => {
                assert!(matches!(moved, Moved::Deferred(_)), "{moved:?}");
                marker
            }
            other => panic!("expected Migrated, got {other:?}"),
        };
        let b = backup_dir(&n, &m).unwrap();
        assert!(!b.join(DATA_DIR).exists());
        assert!(l.join("lumen.db").is_file());
        Install { _t: t, root, n, b }
    }

    #[test]
    fn while_the_move_is_pending_the_previous_folder_and_its_copies_are_refused() {
        let i = install_move_deferred();
        let before = counts(&i.n_db());
        let l = i.old_place();
        let dup = i.root.join("example.previous.app copy");
        copy_dir(&l, &dup);
        for arg in [l.clone(), l.join("lumen.db"), dup.clone()] {
            for dry in [false, true] {
                let mut o = opts();
                o.dry_run = dry;
                let err = import(&i.n, &arg, &o, &Native).unwrap_err();
                assert!(
                    matches!(err, ImportError::NotStarted(_)),
                    "{arg:?}: {err:?}"
                );
                assert!(err.to_string().contains("start Lumen again"), "{err}");
            }
        }
        assert_eq!(counts(&i.n_db()), before, "no row came in");
        assert!(l.join("lumen.db").is_file() && dup.join("lumen.db").is_file());
        assert!(!i.imports().exists(), "nothing moved");
    }

    #[test]
    fn a_copy_of_the_previous_folder_adds_no_row_after_its_backup_is_deleted() {
        let i = install();
        let copy = i.root.join("example.previous.app copy");
        copy_dir(&i.b_data(), &copy);
        make_writable(&i.b);
        fs::remove_dir_all(i.b_data()).unwrap();
        let before = counts(&i.n_db());
        let r = i.import(&copy).unwrap();
        assert!(
            r.prefix["read_events"] > 0 && r.prefix["faults"] > 0,
            "{:?}",
            r.prefix
        );
        assert_eq!(counts(&i.n_db()), before, "its rows came in twice");
    }

    /// Everything in a folder that is not a Lumen name.
    fn user_folder(i: &Install) -> PathBuf {
        let docs = i.root.join("home").join("my-documents");
        ledger(&docs.join("lumen.db"), &["mine"]);
        fs::write(docs.join("taxes-2025.pdf"), b"precious").unwrap();
        fs::create_dir_all(docs.join("photos")).unwrap();
        fs::write(docs.join("photos").join("a.jpg"), b"jpg").unwrap();
        docs
    }

    #[test]
    fn a_ledger_in_a_user_folder_moves_alone_and_the_rest_is_not_touched() {
        for as_file in [true, false] {
            let i = install();
            let docs = user_folder(&i);
            fs::write(
                docs.join("faults.jsonl"),
                format!("{}\n", line("docs-spool")),
            )
            .unwrap();
            let arg = if as_file {
                docs.join("lumen.db")
            } else {
                docs.clone()
            };
            let r = i.import(&arg).unwrap();
            assert_eq!(r.spool, Spool::Taken(1));
            assert_eq!(fs::read(docs.join("taxes-2025.pdf")).unwrap(), b"precious");
            assert!(docs.join("photos").join("a.jpg").is_file());
            assert!(!docs.join("lumen.db").exists() && !docs.join("faults.jsonl").exists());
            let landed = i.first_import();
            assert!(landed.join("lumen.db").is_file());
            assert!(!landed.join("taxes-2025.pdf").exists() && !landed.join("photos").exists());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_ne!(
                    mode(docs.join("taxes-2025.pdf")),
                    0o444,
                    "the user's file stayed writable"
                );
                assert_eq!(mode(landed.join("lumen.db")), 0o444);
            }
            assert!(kinds(&i.n_db()).contains("docs-spool"));
        }
    }

    #[test]
    fn a_folder_of_only_lumen_files_still_moves_whole() {
        let i = install();
        let d = i.root.join("some-lumen-copy");
        ledger(&d.join("lumen.db"), &["x"]);
        fs::write(d.join("lumen_settings.json"), b"{}").unwrap();
        fs::create_dir_all(d.join("logs")).unwrap();
        i.import(&d).unwrap();
        assert!(!d.exists());
        assert!(i.first_import().join("lumen_settings.json").is_file());
    }

    #[test]
    fn an_import_and_a_dry_run_work_while_the_live_ledger_is_open() {
        let i = install();
        let app = crate::meter::connect_db(&i.n_db()).unwrap();
        app.query_row("SELECT count(*) FROM read_events", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
        let d = recreated(&i);
        let mut dry = opts();
        dry.dry_run = true;
        let t = std::time::Instant::now();
        let r = import(&i.n, &d, &dry, &Native).unwrap();
        assert!(r.dry_run && r.total_rows() > 0, "{r:?}");
        assert!(d.join("lumen.db").is_file(), "a dry run moves nothing");
        let r = i.import(&d).unwrap();
        assert!(r.total_rows() > 0 && !d.exists());
        assert!(
            t.elapsed() < Duration::from_secs(4),
            "it waited on the lock: {:?}",
            t.elapsed()
        );
        assert_eq!(count(&i.n_db(), "read_events"), 3);
        drop(app);
    }

    // ---- copies of N itself ----
    mod copies_of_n {
        use super::*;

        /// A Finder duplicate (or a restored copy) of N itself, on a settled install.
        #[test]
        fn a_copy_of_the_live_folder_adds_no_row() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let n = root.join("example.current.app");
            ledger(&n.join("lumen.db"), &["one", "two"]);
            marker::write_settled(&n).unwrap();
            drop(crate::meter::connect_db(&n.join("lumen.db")).unwrap());
            let dup = root.join("example.current.app copy");
            copy_dir(&n, &dup);
            let before = counts(&n.join("lumen.db"));
            import(&n, &dup, &opts(), &Native).unwrap();
            assert_eq!(
                counts(&n.join("lumen.db")),
                before,
                "a copy of the live ledger doubled its rows"
            );
        }

        /// A user's own copy of the live ledger, after the migration, once the app wrote its own rows.
        #[test]
        fn a_copy_of_the_live_ledger_after_migration_adds_no_row() {
            let i = install();
            {
                let c = crate::meter::connect_db(&i.n_db()).unwrap();
                c.execute_batch(
                    "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                       routed_via) VALUES ('t-own', 'Read', '/own', 1, 2, 1, 'builtin_read'); \
                     INSERT INTO faults (ts, kind, session_id) VALUES ('t-own', 'row-own', 's');",
                )
                .unwrap();
            }
            let copy = i.root.join("home").join("lumen-backup");
            fs::create_dir_all(&copy).unwrap();
            fs::copy(i.n_db(), copy.join("lumen.db")).unwrap();
            let before = counts(&i.n_db());
            i.import(&copy).unwrap();
            assert_eq!(
                counts(&i.n_db()),
                before,
                "the live ledger's own rows came in twice"
            );
        }

        /// A copy of a folder already imported (not the backup) adds nothing.
        #[test]
        fn a_copy_of_an_imported_folder_adds_no_row() {
            let i = install();
            let d = recreated(&i);
            let spare = i.root.join("home").join("spare-copy");
            copy_dir(&d, &spare);
            i.import(&d).unwrap();
            let after_first = counts(&i.n_db());
            let r = i.import(&spare).unwrap();
            assert_eq!(counts(&i.n_db()), after_first);
            assert_eq!(r.spool, Spool::Kept);
        }
    }
    mod content_identity {
        use super::*;
        const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");

        fn in_place(i: &Install, d: &Path) -> ImportReport {
            let r = import(&i.n, d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            r
        }

        fn rows180(tag: &str) -> String {
            format!(
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                   routed_via) VALUES ('t-{tag}', 'Read', '/{tag}', 1, 2, 1, 'builtin_read'); \
                 INSERT INTO faults (ts, kind, session_id) VALUES ('t-{tag}', 'row-{tag}', 's');"
            )
        }

        fn ledger180(path: &Path, tags: &[&str]) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let c = Connection::open(path).unwrap();
            c.execute_batch(V180).unwrap();
            for t in tags {
                c.execute_batch(&rows180(t)).unwrap();
            }
        }

        /// P1: an OLDER copy (taken before the original grew), imported once the volume is gone.
        #[test]
        fn an_older_copy_after_the_volume_is_gone_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("usb-spool"))).unwrap();
            let older = i.root.join("timemachine").join("old");
            copy_dir(&d, &older);
            q(&d.join("lumen.db"), &rows_for("u3"));
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r = i.import(&older).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// P1 control: the same, with the volume still there.
        #[test]
        fn control_older_with_the_volume_present_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("usb-spool"))).unwrap();
            let older = i.root.join("timemachine").join("old");
            copy_dir(&d, &older);
            q(&d.join("lumen.db"), &rows_for("u3"));
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            let r = i.import(&older).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// P2: a copy an old (1.9.0) app opened after the copy was taken: faults gained agent_id.
        #[test]
        fn a_copy_whose_schema_an_old_app_upgraded_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger180(&d.join("lumen.db"), &["u1", "u2"]);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN agent_id TEXT",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// P2 control: with the volume present the column intersection makes it all prefix.
        #[test]
        fn control_upgraded_with_the_volume_present_adds_no_row() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger180(&d.join("lumen.db"), &["u1", "u2"]);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN agent_id TEXT",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// P3: two in-place imports of overlapping copies, both volumes gone, then a copy between them.
        #[test]
        fn an_intermediate_copy_after_two_overlapping_imports_adds_no_row() {
            let i = install();
            let c1 = i.root.join("vol1").join("old");
            ledger(&c1.join("lumen.db"), &["u1", "u2"]);
            let c3 = i.root.join("home").join("mid");
            copy_dir(&c1, &c3);
            q(&c3.join("lumen.db"), &rows_for("u3"));
            let c2 = i.root.join("vol2").join("old");
            copy_dir(&c3, &c2);
            q(&c2.join("lumen.db"), &rows_for("u4"));
            in_place(&i, &c1);
            in_place(&i, &c2);
            let after = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("vol1")).unwrap();
            fs::remove_dir_all(i.root.join("vol2")).unwrap();
            let r = i.import(&c3).unwrap();
            assert_eq!(counts(&i.n_db()), after, "came in twice: {r:?}");
        }

        /// P4: two different in-place ledgers, then a copy of the FIRST once its volume is gone
        /// (guards the per-source key of the marks).
        #[test]
        fn a_copy_of_the_first_of_two_in_place_ledgers_adds_no_row() {
            let i = install();
            let a = i.root.join("vola").join("old");
            ledger(&a.join("lumen.db"), &["a1", "a2"]);
            let ac = i.root.join("home").join("ac");
            copy_dir(&a, &ac);
            let b = i.root.join("volb").join("old");
            ledger(&b.join("lumen.db"), &["b1", "b2", "b3"]);
            in_place(&i, &a);
            in_place(&i, &b);
            let after = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("vola")).unwrap();
            let r = i.import(&ac).unwrap();
            assert_eq!(counts(&i.n_db()), after, "came in twice: {r:?}");
        }

        /// P5: late rows the merge took from the backup, then the backup deleted, then a copy of
        /// the previous folder with those late rows (the merge-progress marks).
        #[test]
        fn a_copy_with_the_merged_late_rows_after_the_backup_is_deleted_adds_no_row() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late"));
            // N's own row first, so N's rowids are not the backup's.
            q(
                &i.n_db(),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                 routed_via) VALUES ('t-own', 'Read', '/own', 1, 2, 1, 'builtin_read'); \
                 INSERT INTO faults (ts, kind, session_id) VALUES ('t-own', 'row-own', 's');",
            );
            let Seen::Migrated(m) = marker::read(&i.n) else {
                panic!()
            };
            crate::legacy::freeze::merge_backup(&i.n_db(), &i.b, &m).unwrap();
            let copy = i.root.join("example.previous.app copy");
            copy_dir(&i.b_data(), &copy);
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            let before = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), before, "came in twice: {r:?}");
        }

        /// The same, and the copy was opened by a newer app that added a column to faults.
        #[test]
        fn a_merged_copy_with_a_later_column_after_the_backup_is_deleted_adds_no_row() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late"));
            q(
                &i.n_db(),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                 routed_via) VALUES ('t-own', 'Read', '/own', 1, 2, 1, 'builtin_read'); \
                 INSERT INTO faults (ts, kind, session_id) VALUES ('t-own', 'row-own', 's');",
            );
            let Seen::Migrated(m) = marker::read(&i.n) else {
                panic!()
            };
            crate::legacy::freeze::merge_backup(&i.n_db(), &i.b, &m).unwrap();
            let copy = i.root.join("example.previous.app copy");
            copy_dir(&i.b_data(), &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN extra_col TEXT",
            );
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            let before = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), before, "came in twice: {r:?}");
        }
    }

    mod no_faults_table {
        use super::*;
        const V140: &str = include_str!("../../tests/fixtures/ledger_schema_v1.4.0.sql");

        fn old_ledger(i: &Install) -> PathBuf {
            let d = i.root.join("home").join("old-140");
            fs::create_dir_all(&d).unwrap();
            let c = Connection::open(d.join("lumen.db")).unwrap();
            c.execute_batch(V140).unwrap();
            c.execute_batch(
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                   routed_via) VALUES ('t-140', 'Read', '/140', 1, 2, 1, 'builtin_read');",
            )
            .unwrap();
            d
        }

        /// A ledger from 1.1.4 to 1.4.x has no faults table: it imports, and so does a dry run.
        #[test]
        fn a_ledger_without_a_faults_table_imports() {
            let i = install();
            let d = old_ledger(&i);
            let before = counts(&i.n_db());
            let r = i.import(&d).unwrap();
            assert_eq!(r.rows.get("read_events"), Some(&1), "{r:?}");
            assert_eq!(counts(&i.n_db())["read_events"], before["read_events"] + 1);
        }

        #[test]
        fn a_dry_run_on_a_ledger_without_a_faults_table_works() {
            let i = install();
            let d = old_ledger(&i);
            let mut o = opts();
            o.dry_run = true;
            let r = import(&i.n, &d, &o, &Native).unwrap();
            assert_eq!(r.rows.get("read_events"), Some(&1), "{r:?}");
        }
    }

    mod review_attacks {
        use super::*;
        /// The core sets are in every released schema, and none of the columns that came or went.
        #[test]
        fn core_columns_are_in_every_released_schema() {
            for ddl in [V140, V180, V190] {
                let c = Connection::open_in_memory().unwrap();
                c.execute_batch(ddl).unwrap();
                for t in POSITIONAL_TABLES {
                    let cols = columns(&c, "main", t).unwrap();
                    if cols.is_empty() {
                        continue; // `faults` first shipped in 1.5.0
                    }
                    for core in core_columns(t) {
                        assert!(cols.iter().any(|x| x == core), "{t}.{core}");
                    }
                    assert_eq!(core_in(t, &cols).len(), core_columns(t).len());
                    assert!(!core_columns(t).contains(&"is_subagent"));
                    assert!(!core_columns(t).contains(&"agent_id"));
                }
            }
        }

        /// The copy of a cross-volume folder is kept, whole, under legacy-imports and the user's
        /// folder is never touched or read again.
        #[test]
        fn the_copy_is_the_original_and_the_source_is_not_read_afterwards() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            let before = snap(&d);
            let r = in_place(&i, &d);
            assert!(real(&r.dir).starts_with(real(&i.imports())), "{r:?}");
            assert!(r.dir.join("lumen.db").is_file());
            assert_eq!(snap(&d), before, "the user's folder is untouched");
            assert!(!i.imports().join(".2026-10-20-1.copying").exists());
        }

        const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
        const V140: &str = include_str!("../../tests/fixtures/ledger_schema_v1.4.0.sql");

        fn in_place(i: &Install, d: &Path) -> ImportReport {
            let r = import(&i.n, d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            r
        }

        /// G (a drive letter that changed): another drive's Lumen folder now sits at the path the
        /// first in-place import had. Its rows were never imported.
        #[test]
        fn rev_a_different_ledger_at_the_old_in_place_path_is_not_lost() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            in_place(&i, &d);
            fs::remove_dir_all(&d).unwrap();
            ledger(&d.join("lumen.db"), &["x1", "x2", "x3"]);
            let before = counts(&i.n_db());
            let direct = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices));
            eprintln!("direct import of the new drive at the old path: {direct:?}");
            let copy = i.root.join("home").join("xcopy");
            copy_dir(&d, &copy);
            let r = i.import(&copy).unwrap();
            eprintln!("copy: {:?}", r.lines());
            assert_eq!(
                counts(&i.n_db())["read_events"],
                before["read_events"] + 3,
                "x1..x3 never came in: {r:?}"
            );
        }

        /// An old app wrote into the in-place folder after its import; a copy of it is imported.
        #[test]
        fn rev_rows_written_after_an_in_place_import_are_not_lost() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            in_place(&i, &d);
            q(&d.join("lumen.db"), &rows_for("u3"));
            let before = counts(&i.n_db());
            let direct = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices));
            eprintln!("direct: {direct:?}");
            let copy = i.root.join("home").join("pcopy");
            copy_dir(&d, &copy);
            let copy2 = i.root.join("home").join("pcopy2");
            copy_dir(&d, &copy2);
            let r = i.import(&copy).unwrap();
            eprintln!("copy: {:?}", r.lines());
            let mid = counts(&i.n_db());
            // And once the volume is gone, the lists now say u3 came in.
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r2 = i.import(&copy2).unwrap();
            eprintln!("copy2: {:?}", r2.lines());
            assert_eq!(
                (mid["read_events"], counts(&i.n_db())["read_events"]),
                (before["read_events"] + 1, before["read_events"] + 1),
                "u3 never came in"
            );
        }

        fn ledger180(path: &Path, tags: &[&str]) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let c = Connection::open(path).unwrap();
            c.execute_batch(V180).unwrap();
            for t in tags {
                c.execute_batch(&format!(
                    "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                       saved_tokens, routed_via) VALUES ('t-{t}', 'Read', '/{t}', 1, 2, 1, 'builtin_read'); \
                     INSERT INTO faults (ts, kind, session_id) VALUES ('t-{t}', 'row-{t}', 's');"
                ))
                .unwrap();
            }
        }

        /// H with an upgrade between: a 1.8.0 backup copy, the original later opened by 1.9.0
        /// (faults.agent_id), imported in place, the drive lost, then the 1.8.0 copy.
        #[test]
        fn rev_h_an_older_schema_copy_after_the_volume_is_gone() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger180(&d.join("lumen.db"), &["u1", "u2"]);
            let older = i.root.join("timemachine").join("old");
            copy_dir(&d, &older);
            q(
                &d.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN agent_id TEXT",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r = i.import(&older).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        fn ledger140(path: &Path, tags: &[&str]) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let c = Connection::open(path).unwrap();
            c.execute_batch(V140).unwrap();
            for t in tags {
                c.execute_batch(&format!(
                    "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                       saved_tokens, routed_via) VALUES ('t-{t}', 'Read', '/{t}', 1, 2, 1, 'builtin_read');"
                ))
                .unwrap();
            }
        }

        /// H with an upgrade after: a 1.4.x folder imported in place, its copy later opened by a
        /// 1.6+ app (read_events.is_subagent dropped), the drive lost, then the copy.
        #[test]
        fn rev_h_a_copy_that_lost_a_column_after_the_volume_is_gone() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger140(&d.join("lumen.db"), &["u1", "u2"]);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE read_events DROP COLUMN is_subagent",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// Control for the above with the volume present.
        #[test]
        fn rev_control_a_copy_that_lost_a_column_with_the_volume_present() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger140(&d.join("lumen.db"), &["u1", "u2"]);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE read_events DROP COLUMN is_subagent",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// A folder whose positional tables are empty but whose spool is not: a copy of it.
        #[test]
        fn rev_a_copy_of_a_spool_only_folder_takes_its_spool_once() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &[]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("only-spool"))).unwrap();
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "spool came in twice: {r:?}");
        }

        /// The same with a moved (same-volume) folder.
        #[test]
        fn rev_a_copy_of_a_moved_spool_only_folder_takes_its_spool_once() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger(&d.join("lumen.db"), &[]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("only-spool"))).unwrap();
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            i.import(&d).unwrap();
            let after_first = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "spool came in twice: {r:?}");
        }

        /// The previous folder as a 1.8.0-era copy (faults without agent_id), after the backup was
        /// deleted: the marker fallback and the merge marks only try shorter column lists.
        #[test]
        fn rev_an_older_schema_copy_of_the_previous_folder_after_its_backup_is_deleted() {
            let i = install();
            let copy = i.root.join("example.previous.app copy");
            copy_dir(&i.b_data(), &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE faults DROP COLUMN agent_id",
            );
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            let before = counts(&i.n_db());
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), before, "came in twice: {r:?}");
        }

        /// G remount while the old path is still there with the same content: no row twice, and
        /// the spool is kept.
        #[test]
        fn rev_two_overlapping_copies_imported_in_place_then_the_union() {
            let i = install();
            let a = i.root.join("vola").join("old");
            ledger(&a.join("lumen.db"), &["u1", "u2"]);
            let b = i.root.join("volb").join("old");
            copy_dir(&a, &b);
            q(&a.join("lumen.db"), &rows_for("a3"));
            q(&b.join("lumen.db"), &rows_for("b3"));
            in_place(&i, &a);
            let r = in_place(&i, &b);
            eprintln!("b: {:?}", r.lines());
            let after = counts(&i.n_db());
            fs::remove_dir_all(i.root.join("vola")).unwrap();
            fs::remove_dir_all(i.root.join("volb")).unwrap();
            // u1,u2 once, a3 once, b3 once: the snapshot has snap, snap2.
            assert_eq!(after["read_events"], 2 + 4, "{after:?}");
        }
    }

    mod rev0_attacks {
        use super::*;

        fn in_place(i: &Install, d: &Path) -> ImportReport {
            let r = import(&i.n, d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            r
        }

        fn kind_count(p: &Path, kind: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
        }

        /// What an old app does at its start: the spool's lines become rows of its own ledger,
        /// and the spool goes.
        fn old_app_drains(d: &Path) {
            let spool = d.join("faults.jsonl");
            let recs = faults::parse_spool_all(&fs::read(&spool).unwrap());
            let mut c = Connection::open(d.join("lumen.db")).unwrap();
            let tx = c.transaction().unwrap();
            faults::insert_into(&tx, "main", &recs).unwrap();
            tx.commit().unwrap();
            fs::remove_file(&spool).unwrap();
        }

        /// A1: the user's folder stays after a cross-volume copy; the spool the import took is
        /// still in it, an old app drains it into rows, and the next import of that folder brings
        /// those rows in again.
        #[test]
        fn r0_a_spool_drained_after_a_cross_volume_import_comes_in_once() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("usb-spool"))).unwrap();
            let r = in_place(&i, &d);
            assert_eq!(r.spool, Spool::Taken(1), "{r:?}");
            assert_eq!(kind_count(&i.n_db(), "usb-spool"), 1);
            old_app_drains(&d);
            let r2 = in_place(&i, &d);
            eprintln!("second: {:?}", r2.lines());
            assert_eq!(
                kind_count(&i.n_db(), "usb-spool"),
                1,
                "came in twice: {r2:?}"
            );
        }

        /// A2: a spool-only folder on another volume; its spool grows after the import; the
        /// second import takes the first line again.
        #[test]
        fn r0_a_spool_only_folder_whose_spool_grew_takes_each_line_once() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &[]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("first"))).unwrap();
            in_place(&i, &d);
            let mut s = fs::read_to_string(d.join("faults.jsonl")).unwrap();
            s.push_str(&format!("{}\n", line("second")));
            fs::write(d.join("faults.jsonl"), s).unwrap();
            let r = in_place(&i, &d);
            eprintln!("second: {:?}", r.lines());
            assert_eq!(
                (
                    kind_count(&i.n_db(), "first"),
                    kind_count(&i.n_db(), "second")
                ),
                (1, 1),
                "{r:?}"
            );
        }

        /// A3: an older copy of a spool-only folder (fewer spool lines), same volume.
        #[test]
        fn r0_an_older_copy_of_a_spool_only_folder_takes_nothing_twice() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger(&d.join("lumen.db"), &[]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("first"))).unwrap();
            let older = i.root.join("tm").join("old");
            copy_dir(&d, &older);
            let mut s = fs::read_to_string(d.join("faults.jsonl")).unwrap();
            s.push_str(&format!("{}\n", line("second")));
            fs::write(d.join("faults.jsonl"), s).unwrap();
            i.import(&d).unwrap();
            let r = i.import(&older).unwrap();
            assert_eq!(kind_count(&i.n_db(), "first"), 1, "{r:?}");
        }

        /// A4: a `legacy-imports/<k>` the user deleted; the next import that day gets the same
        /// name, the `$done` row of the deleted one still names it, and a run cut short after the
        /// move cannot be finished: its rows never come in.
        #[test]
        fn r0_a_reused_import_name_does_not_lose_the_new_folder() {
            let i = install();
            let d = recreated(&i);
            i.import(&d).unwrap();
            assert!(i.first_import().is_dir());
            make_writable(&i.first_import());
            fs::remove_dir_all(i.first_import()).unwrap();
            let x = i.root.join("home").join("x");
            ledger(&x.join("lumen.db"), &["x1", "x2"]);
            let xcopy = i.root.join("home").join("xcopy");
            copy_dir(&x, &xcopy);
            let before = counts(&i.n_db());
            let mut o = opts();
            o.stop = Stop::AfterMove;
            let r = import(&i.n, &x, &o, &Native).unwrap();
            assert_ne!(r.name, "2026-10-20-1", "the name is reused: {r:?}");
            let rerun = i.import(&i.first_import());
            eprintln!("rerun on the landed folder: {rerun:?}");
            let viacopy = i.import(&xcopy);
            eprintln!("a copy of it: {viacopy:?}");
            assert_eq!(
                counts(&i.n_db())["read_events"],
                before["read_events"] + 2,
                "x1, x2 never came in"
            );
        }

        /// A5: the same, with no kill: a commit that fails after the move (here, the stop before
        /// the commit stands in for a busy live ledger), then the command the error names.
        #[test]
        fn r0_a_reused_import_name_after_a_failed_commit() {
            let i = install();
            let d = recreated(&i);
            i.import(&d).unwrap();
            make_writable(&i.first_import());
            fs::remove_dir_all(i.first_import()).unwrap();
            let x = i.root.join("home").join("x");
            ledger(&x.join("lumen.db"), &["x1", "x2"]);
            let before = counts(&i.n_db());
            let mut o = opts();
            o.stop = Stop::BeforeCommit;
            let r = import(&i.n, &x, &o, &Native).unwrap();
            assert_ne!(r.name, "2026-10-20-1", "the name is reused: {r:?}");
            let rerun = i.import(&r.dir);
            assert!(rerun.is_ok(), "the rerun the error names: {rerun:?}");
            assert_eq!(counts(&i.n_db())["read_events"], before["read_events"] + 2);
        }

        /// A6: a cross-volume import killed after the copy is placed, then the user reruns on
        /// their own folder; then the leftover copy is imported too.
        #[test]
        fn r0_a_cross_volume_copy_killed_after_placing_then_both_runs() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("usb-spool"))).unwrap();
            let before = counts(&i.n_db());
            let mut o = opts();
            o.stop = Stop::AfterMove;
            let r = import(&i.n, &d, &o, &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(r.stopped);
            let left = r.dir.clone();
            in_place(&i, &d);
            let r3 = i.import(&left).unwrap();
            eprintln!("leftover: {:?}", r3.lines());
            let after = counts(&i.n_db());
            assert_eq!(after["read_events"], before["read_events"] + 2);
            assert_eq!(kind_count(&i.n_db(), "usb-spool"), 1);
        }

        /// A7: cross-volume dry run then a real run.
        #[test]
        fn r0_cross_volume_dry_run_writes_nothing() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1", "u2"]);
            let before = (snap(&i.n), snap(&d));
            let mut o = opts();
            o.dry_run = true;
            let r = import(&i.n, &d, &o, &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            eprintln!("dry: {:?}", r.lines());
            assert_eq!(r.rows["read_events"], 2);
            let after = (snap(&i.n), snap(&d));
            assert_eq!(after.1, before.1);
            assert_eq!(
                after.0.keys().collect::<Vec<_>>(),
                before.0.keys().collect::<Vec<_>>()
            );
        }

        /// A8: the user's folder is not made read-only by a cross-volume copy.
        #[cfg(unix)]
        #[test]
        fn r0_the_users_folder_keeps_its_permissions() {
            use std::os::unix::fs::PermissionsExt;
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            let mode = fs::metadata(d.join("lumen.db"))
                .unwrap()
                .permissions()
                .mode();
            in_place(&i, &d);
            assert_eq!(
                fs::metadata(d.join("lumen.db"))
                    .unwrap()
                    .permissions()
                    .mode(),
                mode
            );
        }
    }

    mod rev0_deleted_originals {
        use super::*;
        const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
        const V140: &str = include_str!("../../tests/fixtures/ledger_schema_v1.4.0.sql");

        fn in_place(i: &Install, d: &Path) -> ImportReport {
            let r = import(&i.n, d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            r
        }

        fn drop_imports(i: &Install) {
            make_writable(&i.imports());
            fs::remove_dir_all(i.imports()).unwrap();
        }

        fn ledger_with(ddl: &str, path: &Path, tags: &[&str], faults: bool) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let c = Connection::open(path).unwrap();
            c.execute_batch(ddl).unwrap();
            for t in tags {
                c.execute_batch(&format!(
                    "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, \
                       saved_tokens, routed_via) VALUES ('t-{t}', 'Read', '/{t}', 1, 2, 1, 'builtin_read');"
                ))
                .unwrap();
                if faults {
                    c.execute_batch(&format!(
                        "INSERT INTO faults (ts, kind, session_id) VALUES ('t-{t}', 'row-{t}', 's');"
                    ))
                    .unwrap();
                }
            }
        }

        /// D1: 1.8.0 copy kept aside; the original later gets faults.agent_id, is imported across
        /// volumes; the copy in legacy-imports is deleted and the volume is gone; the older copy.
        #[test]
        fn d1_older_schema_copy_after_every_original_is_gone() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger_with(V180, &d.join("lumen.db"), &["u1", "u2"], true);
            let older = i.root.join("tm").join("old");
            copy_dir(&d, &older);
            q(
                &d.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN agent_id TEXT",
            );
            in_place(&i, &d);
            let after_first = counts(&i.n_db());
            drop_imports(&i);
            fs::remove_dir_all(i.root.join("usb")).unwrap();
            let r = i.import(&older).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// D2: 1.4.0 folder moved (same volume) and imported; its copy later lost
        /// read_events.is_subagent; legacy-imports is deleted; the copy.
        #[test]
        fn d2_a_copy_that_lost_a_column_after_the_moved_original_is_deleted() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger_with(V140, &d.join("lumen.db"), &["u1", "u2"], false);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE read_events DROP COLUMN is_subagent",
            );
            i.import(&d).unwrap();
            let after_first = counts(&i.n_db());
            drop_imports(&i);
            let r = i.import(&copy).unwrap();
            assert_eq!(counts(&i.n_db()), after_first, "came in twice: {r:?}");
        }

        /// D3: a copy upgraded by 1.9.0 (agent_id at the end) after its 1.8.0 original was imported
        /// and deleted, and the copy grew by one row.
        #[test]
        fn d3_an_upgraded_copy_that_grew_after_the_original_is_deleted() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger_with(V180, &d.join("lumen.db"), &["u1", "u2"], true);
            let copy = i.root.join("home").join("c");
            copy_dir(&d, &copy);
            i.import(&d).unwrap();
            let before = counts(&i.n_db());
            drop_imports(&i);
            q(
                &copy.join("lumen.db"),
                "ALTER TABLE faults ADD COLUMN agent_id TEXT",
            );
            q(
                &copy.join("lumen.db"),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                 routed_via) VALUES ('t-u3', 'Read', '/u3', 1, 2, 1, 'builtin_read'); \
                 INSERT INTO faults (ts, kind, session_id, agent_id) VALUES ('t-u3', 'row-u3', 's', 'a');",
            );
            let r = i.import(&copy).unwrap();
            let after = counts(&i.n_db());
            assert_eq!(
                (after["read_events"], after["faults"]),
                (before["read_events"] + 1, before["faults"] + 1),
                "{r:?}"
            );
        }

        /// D4: the previous folder's 1.4.0-era copy (before 1.5.0 there is no faults table) after the
        /// backup is deleted: the marker and N's own rows.
        #[test]
        fn d4_two_chained_imports_then_both_deleted_then_the_middle_copy() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger_with(V180, &d.join("lumen.db"), &["u1", "u2"], true);
            let mid = i.root.join("home").join("mid");
            copy_dir(&d, &mid);
            i.import(&d).unwrap();
            // The middle copy grows and is imported (adds one), then a later copy of it.
            q(
                &mid.join("lumen.db"),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, routed_via) VALUES ('t-m3', 'Read', '/m3', 1, 2, 1, 'builtin_read')",
            );
            let late = i.root.join("home").join("late");
            copy_dir(&mid, &late);
            q(
                &late.join("lumen.db"),
                "ALTER TABLE read_events DROP COLUMN target_outline",
            );
            i.import(&mid).unwrap();
            let after = counts(&i.n_db());
            drop_imports(&i);
            let r = i.import(&late).unwrap();
            assert_eq!(counts(&i.n_db()), after, "came in twice: {r:?}");
        }
    }

    mod t6nup_rev1 {
        use super::*;
        use crate::legacy::freeze::{Freeze, freeze_if_ready};

        fn kind_count(p: &Path, kind: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
        }

        fn old_app_drains(d: &Path) {
            let spool = d.join("faults.jsonl");
            let recs = faults::parse_spool_all(&fs::read(&spool).unwrap());
            let mut c = Connection::open(d.join("lumen.db")).unwrap();
            let tx = c.transaction().unwrap();
            faults::insert_into(&tx, "main", &recs).unwrap();
            tx.commit().unwrap();
            fs::remove_file(&spool).unwrap();
        }

        fn marker_of(i: &Install) -> MarkerV2 {
            match marker::read(&i.n) {
                Seen::Migrated(m) => (*m).clone(),
                other => panic!("{other:?}"),
            }
        }

        fn freeze(i: &Install) {
            let rec = i.root.join(".setup_done");
            fs::write(&rec, format!("data_dir={}\n", i.n.display())).unwrap();
            match freeze_if_ready(&i.n, &rec, &marker_of(i)) {
                Freeze::Frozen(_) => {}
                other => panic!("{other:?}"),
            }
        }

        fn restored_writable(i: &Install) -> PathBuf {
            let d = i.old_place();
            copy_dir(&i.b_data(), &d);
            make_writable(&d);
            d
        }

        /// X1: the freeze took the backup's spool line; a restored copy of the previous folder,
        /// which an old app wrote to and drained its (same) spool into its own table, is imported.
        #[test]
        fn x1_a_restored_copy_that_drained_the_frozen_spool_adds_it_again() {
            let i = install();
            // N's own app recorded a fault of its own between the migration and the freeze.
            q(
                &i.n_db(),
                "INSERT INTO faults (ts, kind, session_id) VALUES ('t-n', 'n-own', 'sn')",
            );
            freeze(&i);
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1);
            let c = restored_writable(&i);
            old_app_drains(&c);
            q(&c.join("lumen.db"), &rows_for("later"));
            let r = i.import(&c).unwrap();
            eprintln!("x1: {:?}", r.lines());
            assert_eq!(kind_count(&i.n_db(), "row-later"), 1, "the new row came in");
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1, "twice: {r:?}");
        }

        /// X1b: the same, imported before the freeze; then the freeze.
        #[test]
        fn x1b_the_same_imported_before_the_freeze() {
            let i = install();
            let c = restored_writable(&i);
            old_app_drains(&c);
            let r = i.import(&c).unwrap();
            eprintln!("x1b: {:?}", r.lines());
            freeze(&i);
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1, "twice: {r:?}");
        }

        /// X2: a spool-only folder whose spool an old app drained, imported first; then an older
        /// copy of it (the spool not drained yet).
        #[test]
        fn x2_an_older_spool_only_copy_after_its_drained_folder() {
            let i = install();
            let d = i.root.join("home").join("old");
            ledger(&d.join("lumen.db"), &[]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("only-spool"))).unwrap();
            let older = i.root.join("tm").join("old");
            copy_dir(&d, &older);
            old_app_drains(&d);
            i.import(&d).unwrap();
            assert_eq!(kind_count(&i.n_db(), "only-spool"), 1);
            let r = i.import(&older).unwrap();
            eprintln!("x2: {:?}", r.lines());
            assert_eq!(kind_count(&i.n_db(), "only-spool"), 1, "twice: {r:?}");
        }

        /// A migrated install whose previous folder holds no positional rows, only a spool.
        fn spool_only_install() -> Install {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            let n = root.join("example.current.app");
            ledger(&l.join("lumen.db"), &[]);
            fs::write(
                l.join("faults.jsonl"),
                format!("{}\n", line("backup-spool")),
            )
            .unwrap();
            let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
            o.probe_tries = 1;
            o.probe_delay = Duration::from_millis(10);
            let m: MarkerV2 = match commit::migrate(&l, &n, &o, &Native) {
                Outcome::Migrated { marker, .. } => marker,
                other => panic!("{other:?}"),
            };
            let b = backup_dir(&n, &m).unwrap();
            Install { _t: t, root, n, b }
        }

        /// X3: the RTG case with a spool-only previous folder: after the freeze took its spool, a
        /// restored copy of the folder.
        #[test]
        fn x3_a_restored_copy_of_a_spool_only_previous_folder_after_the_freeze() {
            let i = spool_only_install();
            freeze(&i);
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1);
            let c = restored_writable(&i);
            let r = i.import(&c).unwrap();
            eprintln!("x3: {:?}", r.lines());
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1, "twice: {r:?}");
        }

        /// X3b: the same, imported before the freeze; then the freeze.
        #[test]
        fn x3b_the_same_before_the_freeze() {
            let i = spool_only_install();
            let c = restored_writable(&i);
            let r = i.import(&c).unwrap();
            eprintln!("x3b: {:?}", r.lines());
            freeze(&i);
            assert_eq!(kind_count(&i.n_db(), "backup-spool"), 1, "twice: {r:?}");
        }

        /// X4: two equal spool lines (two subagents, same second): an earlier import took one,
        /// the folder later holds two.
        #[test]
        fn x4_equal_lines_are_counted() {
            let i = install();
            let mut rec = FaultRecord::now("same", "v");
            rec.session_id = Some("s".into());
            let a = {
                let mut r = rec.clone();
                r.agent_id = Some("a1".into());
                serde_json::to_string(&r).unwrap()
            };
            let b = {
                let mut r = rec.clone();
                r.agent_id = Some("a2".into());
                serde_json::to_string(&r).unwrap()
            };
            let d1 = i.root.join("home").join("one");
            ledger(&d1.join("lumen.db"), &[]);
            fs::write(d1.join("faults.jsonl"), format!("{a}\n")).unwrap();
            i.import(&d1).unwrap();
            let d2 = i.root.join("home").join("two");
            ledger(&d2.join("lumen.db"), &[]);
            fs::write(d2.join("faults.jsonl"), format!("{a}\n{b}\n")).unwrap();
            let r = i.import(&d2).unwrap();
            eprintln!("x4: {:?}", r.lines());
            assert_eq!(kind_count(&i.n_db(), "same"), 2, "{r:?}");
        }

        /// X5: a cross-volume folder whose old app keeps running: rows and spool lines written
        /// after the first copy come in with the second, each once.
        #[test]
        fn x5_a_live_cross_volume_folder_imported_twice() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("s1"))).unwrap();
            import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            q(&d.join("lumen.db"), &rows_for("u2"));
            let mut s = fs::read_to_string(d.join("faults.jsonl")).unwrap();
            s.push_str(&format!("{}\n", line("s2")));
            fs::write(d.join("faults.jsonl"), s).unwrap();
            old_app_drains(&d);
            let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            eprintln!("x5: {:?}", r.lines());
            assert_eq!(
                (
                    kind_count(&i.n_db(), "s1"),
                    kind_count(&i.n_db(), "s2"),
                    kind_count(&i.n_db(), "row-u2")
                ),
                (1, 1, 1),
                "{r:?}"
            );
        }

        /// A9: a source in WAL mode on another volume is left as it was, byte for byte.
        #[test]
        fn r0_a_wal_source_on_another_volume_is_left_as_it_was() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            let c = Connection::open(d.join("lumen.db")).unwrap();
            let m: String = c
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                .unwrap();
            assert_eq!(m, "wal");
            c.execute_batch(&rows_for("u2")).unwrap();
            drop(c);
            let before = fs::read(d.join("lumen.db")).unwrap();
            let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            let after = fs::read(d.join("lumen.db")).unwrap();
            assert_eq!(before, after, "the user's ledger was rewritten");
            assert_eq!(kind_count(&i.n_db(), "row-u2"), 1, "{r:?}");
        }
    }
}
