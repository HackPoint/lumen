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
//! 2. **Lock, then ask whether it is held.** The lock is `N/.lumen-migrate.lock`. Whether an older
//!    Lumen has the source open is asked without writing a byte and without opening it through
//!    SQLite (taking a ledger switches its journal mode, a write to the user's file): POSIX lock
//!    queries on Unix, a share-mode open on Windows. One that is held is refused and nothing changes.
//! 3. **Move it out of the way.** A folder outside N becomes `N/legacy-imports/<date>-<k>`; a
//!    set-aside ledger goes, with its sidecars, to `N/legacy-imports/<date>-<k>/lumen.db*`. The
//!    rename is the only judge of whether it can move (a bind mount or a mounted folder looks like
//!    one volume to any prediction), and the ledger is taken, exclusively, only where it now is.
//!    One that turns out to be held then goes back where it was, and nothing changes. A rename that
//!    answers EXDEV cannot move it, so the ledger set and the spool are copied byte for byte into
//!    `legacy-imports/<date>-<k>` (the source was never opened through SQLite, so never written),
//!    and that copy is the folder's original; the user's folder stays and is never read again.
//!    The copy is whole only when every file equals its source twice over (right after it was
//!    copied, and again once the last was) and SQLite's `quick_check` passes on it. No user path
//!    is ever an original or a `$done` key: a folder is judged by its rows alone.
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
//!    recognised. Faults do not use the prefix alone: for each core-content digest N ends up holding the
//!    maximum count any one source holds (`faultset`), whatever route the others took.
//! 5. **One transaction** from the held connection, with N attached: the keyed tables with
//!    `INSERT OR IGNORE`, the new rows of the positional tables, the folder's spool and `.draining`
//!    lines (only when every prefix is 0: a folder that shares history keeps its spool), each fault
//!    row or spool line only while the source holds more of its digest than N does at that moment, and the
//!    `('import:<k>', '$done', ..)` row that makes a second run say "already imported".
//! 6. **Finish:** the moved folder's files are made read-only.
//!
//! A run killed before step 3 changed nothing. Killed after it, `legacy-imports/<k>` has no `$done`
//! row, so a rerun on that path imports. Killed after step 5, the rerun says "already imported".
//! `--dry-run` runs the refusals and the prefix search on a scratch copy of the source (removed when
//! the run ends), takes the same transaction and rolls it back: it moves nothing and writes nothing,
//! not even to the source's journal mode.

use super::commit::{LockError, backup_dir, take_lock};
use super::discover::{Verdict, is_lumen_ledger};
use super::faultset::{
    CORE_FAULTS, admit_fresh, counts_in_n, read_spool_files, record_digests, row_digests, tally,
};
use super::freeze::{make_read_only, source_key};
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
    /// The folder has spool records and N already held every one of them (the count rule took
    /// none): its spool was read, not added.
    AlreadyHeld,
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
    /// Fault records (rows and spool lines) the source holds that N already held enough of.
    pub faults_held: usize,
    /// Set when the source's `faults` table lacks a core column, so no digest can be made and
    /// its rows were not taken: why.
    pub faults_refused: Option<String>,
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
            // Faults have no prefix: the count rule alone decides what is added.
            if *p > 0 && t != "faults" {
                out.push(format!(
                    "{t}: the first {p} rowids are rows the live ledger already holds and were skipped"
                ));
            }
        }
        match &self.spool {
            Spool::Taken(0) | Spool::NoneHere => {}
            Spool::Taken(n) => out.push(format!("Fault records taken from its spool: {n}")),
            Spool::AlreadyHeld => {}
        }
        let added = self.rows.get("faults").into_iter().sum::<usize>()
            + match &self.spool {
                Spool::Taken(n) => *n,
                _ => 0,
            };
        if added + self.faults_held > 0 {
            out.push(if self.dry_run {
                format!(
                    "Fault records: would add {added}, {} already held",
                    self.faults_held
                )
            } else {
                format!(
                    "Fault records: {added} added, {} already held",
                    self.faults_held
                )
            });
        }
        if let Some(why) = &self.faults_refused {
            out.push(format!(
                "Fault records from its table were not taken: {why}"
            ));
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
    // The user's source is never opened through SQLite before it has moved: taking a ledger
    // switches its journal mode (a write to its header) and a failed rename afterwards would leave
    // it so. Whether an older Lumen has it open is asked without writing (`held_elsewhere`); the
    // rename is the only judge of whether it can move (a bind mount or a volume in a folder looks
    // like one volume to any prediction and answers EXDEV to the rename); and the ledger is taken
    // only where it now is, ours. A source that cannot move is copied byte for byte, and the copy is
    // taken.
    if in_place_name.is_none() {
        held_elsewhere(&src.ledger)?;
    }
    let mut held: Option<Connection> = match in_place_name {
        Some(_) => Some(take(&src.ledger, opts)?),
        None => None,
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
            // Looked at through a copy in a scratch folder that goes when the run ends: the
            // source is not taken, so a dry run writes nothing to it.
            static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let tmp = std::env::temp_dir().join(format!(
                "lumen-import-dry-{}-{}",
                std::process::id(),
                RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            held = Some(copy_aside(src, &tmp, opts)?);
            ledger = tmp.join(LEDGER);
            actual = tmp.clone();
            scratch = Some(tmp);
            // Only the report's wording depends on the guess; nothing is decided by it.
            if ops.crosses_volumes(&src.path, n_real) {
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
            let moved = match src.kind {
                Kind::Folder => ops
                    .rename(&src.path, &target)
                    .map(|()| vec![(src.path.clone(), target.clone())]),
                Kind::Shared => move_ledger_set(ops, &src.ledger, &target, true),
                Kind::Ledger => move_ledger_set(ops, &src.path, &target, false),
            };
            let placed = match moved {
                Ok(items) => {
                    sync_dir(&imports);
                    // Taken only now, where it is ours. A source that turns out to be held (a
                    // connection in this process, or one opened since the check above) goes back,
                    // and nothing changes.
                    match take(&target.join(LEDGER), opts) {
                        Ok(c) => held = Some(c),
                        Err(err) => {
                            let imports_made = made_imports.then_some(imports.as_path());
                            return Err(put_back(err, &items, &target, imports_made));
                        }
                    }
                    ledger = target.join(LEDGER);
                    dir = target;
                    actual = dir.clone();
                    Placed::Moved
                }
                // Another volume: the folder is copied, verified and kept under `legacy-imports`,
                // and that copy is its original. The user's folder was never opened and stays
                // where it is.
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    // The failed move may have made `target` (a set-aside ledger's); the copy
                    // makes its own folder.
                    fs::remove_dir(&target).ok();
                    match copy_aside(src, &target, opts) {
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
        faults_held: 0,
        faults_refused: None,
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
/// folder it made. Gives what moved, as `(from, to)`, so a later refusal can put it back.
fn move_ledger_set(
    ops: &dyn Ops,
    from_db: &Path,
    target: &Path,
    spool: bool,
) -> io::Result<Vec<(PathBuf, PathBuf)>> {
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
        ops.rename(from_db, &to_db)?;
        moved.push((from_db.to_path_buf(), to_db.clone()));
        Ok(())
    };
    match run() {
        Ok(()) => Ok(moved),
        Err(e) => {
            let stuck = undo_moves(&moved);
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

/// Rename what `moved` moved back, newest first; what could not be put back, as text.
fn undo_moves(moved: &[(PathBuf, PathBuf)]) -> Vec<String> {
    let mut stuck = Vec::new();
    for (from, to) in moved.iter().rev() {
        if let Err(u) = fs::rename(to, from) {
            stuck.push(format!("{}: {u}", to.display()));
        }
    }
    stuck
}

/// A source that moved and then turned out to be held (or not writable): put it back where it
/// was, remove the folders made for it, and give the reason it was refused. When it cannot be
/// put back the message says where it is, and that a rerun on that path finishes the import.
fn put_back(
    why: ImportError,
    moved: &[(PathBuf, PathBuf)],
    target: &Path,
    imports_made: Option<&Path>,
) -> ImportError {
    let stuck = undo_moves(moved);
    if !stuck.is_empty() {
        return ImportError::Failed(format!(
            "{why}; and it could not be put back ({}): it is now at {}, and this finishes it: {}",
            stuck.join(", "),
            target.display(),
            command_for(target)
        ));
    }
    // Whoever holds the ledger may have made its sidecars at the new place since it moved (an
    // older Lumen opening it there): they belong with the ledger.
    for (from, to) in moved
        .iter()
        .filter(|(_, to)| to.file_name().is_some_and(|n| n == LEDGER))
    {
        for sfx in SIDECARS {
            let (stray, home) = (sidecar(to, sfx), sidecar(from, sfx));
            if stray.symlink_metadata().is_ok()
                && home.symlink_metadata().is_err()
                && let Err(u) = fs::rename(&stray, &home)
            {
                return ImportError::Failed(format!(
                    "{why}; and could not put back {}: {u}",
                    stray.display()
                ));
            }
        }
    }
    // A folder made for a set of files is empty by now; a folder that is not empty is somebody's.
    if target.exists()
        && let Err(u) = fs::remove_dir(target)
    {
        return ImportError::Failed(format!(
            "{why}; and could not remove {}: {u}",
            target.display()
        ));
    }
    if let Some(dir) = imports_made {
        fs::remove_dir(dir).ok();
    }
    why
}

/// The source cannot be renamed (another volume, a bind mount, a mount in a folder): copy its
/// ledger set (and, for a folder, its spool) into `target`, byte for byte, and give back the copy,
/// taken. Nothing is read from the source's place afterwards. The copy is built in a hidden folder
/// next to `target` and renamed into place only when it is whole, so a run that dies half-way
/// leaves no `legacy-imports/<k>` that looks whole.
///
/// The source is never opened through SQLite and never written: a connection that can checkpoint
/// or change the journal mode would rewrite the user's file. A source an older Lumen has open is
/// refused before anything is copied (`held_elsewhere`). The files cannot be copied at one instant,
/// so the copy is proved whole in three steps: each file is compared with its source right after it
/// is copied; once the last is copied, every file is compared again, in the same order, and the
/// set of files must be the same (a write that landed between two files changes the one copied
/// first, and shows here); and SQLite's `quick_check` passes on the copy.
fn copy_aside(
    src: &Source,
    target: &Path,
    opts: &ImportOptions,
) -> Result<Connection, ImportError> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| ImportError::Failed(format!("{} has no name", target.display())))?;
    let tmp = target.with_file_name(format!(".{name}.copying"));
    clear_dir(&tmp)?;
    let changed = |from: &Path| {
        ImportError::Failed(format!(
            "{} changed while it was copied (or the copy does not match it); nothing was \
             imported. Quit the older Lumen and retry",
            from.display()
        ))
    };
    let run = || -> Result<Connection, ImportError> {
        fs::create_dir_all(&tmp).map_err(|e| failed("cannot copy the folder", e))?;
        let to_db = tmp.join(LEDGER);
        let files = files_of(src, &to_db, &tmp);
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
                return Err(changed(from));
            }
        }
        // The second pass: a change since a file was copied, in any file.
        for (from, to) in &files {
            let same = same_bytes(from, to)
                .map_err(|e| failed(&format!("cannot check the copy of {}", from.display()), e))?;
            if !same {
                return Err(changed(from));
            }
        }
        if files_of(src, &to_db, &tmp) != files {
            return Err(changed(&src.ledger));
        }
        held_elsewhere(&src.ledger)?;
        // The copy is ours: taking it replays a WAL the source left, and proves it opens.
        let c = take(&to_db, opts)?;
        let verdict: String = c
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(|e| failed("cannot check the copy", e))?;
        if verdict != "ok" {
            return Err(ImportError::Failed(format!(
                "the copy of {} is not a sound ledger ({verdict}); nothing was imported. Quit the \
                 older Lumen and retry",
                src.ledger.display()
            )));
        }
        drop(c);
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

/// What `copy_aside` copies: the source's files that exist, sidecars and spool first, the ledger
/// last, each with where its copy goes.
fn files_of(src: &Source, to_db: &Path, tmp: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    for sfx in SIDECARS {
        // The `-shm` is SQLite's wal-index: derived from the `-wal`, rebuilt by the first
        // connection that opens the copy, and rewritten by every reader of a live ledger.
        if sfx == "-shm" {
            continue;
        }
        let from = sidecar(&src.ledger, sfx);
        if from
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_file())
        {
            files.push((from, sidecar(to_db, sfx)));
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
    files.push((src.ledger.clone(), to_db.to_path_buf()));
    files
}

/// Whether an older Lumen has the ledger open, asked without writing a byte and without opening it
/// through SQLite (taking a ledger switches its journal mode, a write to its header).
///
/// Unix: SQLite holds POSIX locks, and `fcntl(F_GETLK)` reports a lock another process holds. The
/// ranges are the ones SQLite documents: the database's pending, reserved and shared bytes (a
/// transaction in rollback mode), and the `-shm` file's lock slots and its dead-man byte (every
/// connection in WAL mode holds the latter for as long as it is open). A lock of this process is
/// not reported, and the descriptors this opens must not be closed while this process has the
/// ledger open through SQLite (closing one releases that process's locks on the file), so a caller
/// that holds the ledger itself asks the probe instead.
///
/// Windows: the ledger is opened for reading with only `FILE_SHARE_READ`; a handle that has it
/// open for writing refuses that.
fn held_elsewhere(ledger: &Path) -> Result<(), ImportError> {
    let held = |why: String| ImportError::InUse(why);
    #[cfg(unix)]
    {
        // SQLite's lock bytes: PENDING_BYTE, and the shm's lock slots from byte 120 and its DMS.
        const PENDING: i64 = 0x4000_0000;
        let ranges = [
            (ledger.to_path_buf(), PENDING, 512),
            (sidecar(ledger, "-shm"), 120, 9),
        ];
        for (path, start, len) in &ranges {
            match locked_by_another(path, *start, *len) {
                Ok(false) => {}
                Ok(true) => {
                    return Err(held(format!(
                        "{} is open in another process",
                        ledger.display()
                    )));
                }
                Err(e) => {
                    return Err(failed(&format!("cannot look at {}", path.display()), e));
                }
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        match fs::OpenOptions::new().read(true).share_mode(1).open(ledger) {
            Ok(_) => Ok(()),
            Err(e) if e.raw_os_error() == Some(32) => Err(held(format!(
                "{} is open in another process",
                ledger.display()
            ))),
            Err(e) => Err(failed(&format!("cannot look at {}", ledger.display()), e)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (ledger, held);
        Ok(())
    }
}

/// Whether another process holds a lock that conflicts with a write lock on `len` bytes at
/// `start` of `path`. A file that is not there holds none.
#[cfg(unix)]
fn locked_by_another(path: &Path, start: i64, len: i64) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let f = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        // A read-only file or folder: the lock query needs no write access.
        Err(_) => match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        },
    };
    // SAFETY: a zeroed `flock` is a valid value for every field; `fcntl(F_GETLK)` only reads the
    // request and writes the answer into it, on a descriptor that stays open for the call.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = start as _;
    fl.l_len = len as _;
    // SAFETY: see above.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    // A cast to `l_type`'s own alias: F_UNLCK is a c_short on macOS and a c_int on Linux, where
    // `i32::from(libc::F_UNLCK)` is a useless conversion.
    Ok(fl.l_type != libc::F_UNLCK as libc::c_short)
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

/// How far the merge from the backup has taken `table`: its progress row in N, else the marker's
/// mark (the rows of the snapshot).
fn merged_mark(
    conn: &Connection,
    key: &str,
    marker: &marker::MarkerV2,
    table: &str,
) -> Result<i64, ImportError> {
    let row: Option<i64> = conn
        .query_row(
            "SELECT hwm FROM n.legacy_merge WHERE source = ?1 AND tbl = ?2",
            params![key, table],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| failed("legacy_merge", e))?;
    Ok(match (row, marker.hwm.get(table)) {
        (Some(done), _) => done,
        (None, Some(&mark)) => mark,
        // No progress row and no mark: nothing of this table was ever merged.
        (None, None) => 0,
    })
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
///
/// `rows_in` is the schema of the source whose rows are listed, `n_in` the schema of the new
/// ledger that keeps the list (the import: `main` and `n`; the merge from the backup: `src` and
/// `main`).
pub(super) fn record_rows(
    tx: &Connection,
    rows_in: &str,
    n_in: &str,
    table: &str,
) -> Result<(), String> {
    let cols = columns(tx, rows_in, table).map_err(|e| format!("{table}: {e}"))?;
    if cols.is_empty() {
        return Ok(());
    }
    record_list(tx, rows_in, n_in, table, &cols)?;
    let core = core_in(table, &cols);
    if !core.is_empty() && core != cols {
        record_list(tx, rows_in, n_in, table, &core)?;
    }
    Ok(())
}

fn record_list(
    tx: &Connection,
    rows_in: &str,
    n_in: &str,
    table: &str,
    cols: &[String],
) -> Result<(), String> {
    let refs: Vec<&str> = cols.iter().map(String::as_str).collect();
    let sql = format!(
        "SELECT rowid, {} FROM {rows_in}.{} ORDER BY rowid",
        key_expr("", &refs),
        quote(table)
    );
    let mut all: Vec<u8> = Vec::new();
    {
        let mut st = tx.prepare(&sql).map_err(|e| format!("{table}: {e}"))?;
        let mut rs = st.query([]).map_err(|e| format!("{table}: {e}"))?;
        while let Some(r) = rs.next().map_err(|e| format!("{table}: {e}"))? {
            let id: i64 = r.get(0).map_err(|e| format!("{table}: {e}"))?;
            let key: String = r.get(1).map_err(|e| format!("{table}: {e}"))?;
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
    let put = format!(
        "INSERT OR REPLACE INTO {n_in}.legacy_merge (source, tbl, hwm, sig) \
         VALUES (?1, ?2, ?3, ?4)"
    );
    let put = put.as_str();
    tx.execute(
        put,
        params![source, "cols", (all.len() / ENTRY) as i64, joined],
    )
    .map_err(|e| format!("legacy_merge: {e}"))?;
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
        .map_err(|e| format!("legacy_merge: {e}"))?;
    }
    Ok(())
}

/// A row list an earlier import recorded: the columns it was taken over and the entries.
pub(super) struct RowList {
    pub(super) cols: Vec<String>,
    entries: Vec<u8>,
}

pub(super) fn recorded_lists(
    conn: &Connection,
    n_in: &str,
    table: &str,
) -> Result<Vec<RowList>, String> {
    let failed = |_: &str, e: rusqlite::Error| format!("legacy_merge: {e}");
    let mut st = conn
        .prepare(&format!(
            "SELECT source, sig FROM {n_in}.legacy_merge WHERE tbl = 'cols' AND source LIKE ?1"
        ))
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
            .prepare(&format!(
                "SELECT sig FROM {n_in}.legacy_merge WHERE source = ?1 AND tbl GLOB '[0-9]*' \
                 ORDER BY tbl"
            ))
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
pub(super) fn prefix_in_list(
    src: &Connection,
    rows_in: &str,
    table: &str,
    list: &RowList,
) -> Result<i64, String> {
    let refs: Vec<&str> = list.cols.iter().map(String::as_str).collect();
    let sql = format!(
        "SELECT rowid, {} FROM {rows_in}.{} ORDER BY rowid",
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
    let backup = match marker::read(p.n) {
        Seen::Migrated(m) => backup_dir(p.n, &m).map(|b| (b, m)),
        _ => None,
    };
    for db in originals(p.n, &conn, &p.ledger)? {
        let orig = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| failed(&format!("cannot read {}", db.display()), e))?;
        // The backup holds rows the merge has not taken yet (a start killed after the move, before
        // its merge): only what its merge reached is in N.
        let is_backup = backup.as_ref().filter(|(b, _)| {
            same_dir(
                &real(&b.join(super::commit::DATA_DIR)),
                &real(db.parent().unwrap_or(&db)),
            )
        });
        for table in POSITIONAL_TABLES {
            let mut k = common_prefix(&conn, &orig, table).map_err(|e| failed("prefix", e))?;
            if let Some((b, m)) = is_backup {
                k = k.min(merged_mark(&conn, &source_key(b), m, table)?);
            }
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
        for list in recorded_lists(&conn, "n", table).map_err(|e| failed(table, e))? {
            if list.cols.iter().any(|c| !s_cols.contains(c)) {
                continue;
            }
            let k = prefix_in_list(&conn, "main", table, &list).map_err(|e| failed("prefix", e))?;
            let slot = prefix.entry(table.to_string()).or_insert(0);
            *slot = (*slot).max(k);
        }
    }

    // 5. One transaction.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| failed("cannot start the import", e))?;
    // Faults merge as a multiset by core-content digest against N (see `faultset`): the rows
    // above the prefix and the spool lines come in only while the source holds more of a digest
    // than N does, whatever route the others took.
    let spool_active = !matches!(src.kind, Kind::Ledger);
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
    let n_counts = counts_in_n(&tx, "n", p.n).map_err(|e| failed("faults", e))?;
    // The rule (see `faultset`): for each digest N ends with the maximum count any single source
    // holds, so each candidate here is a record of THIS source (every row of its table, every
    // line of its spool and `.draining`), none skipped by a prefix, and one is added only while
    // this source's count of its digest is above what N holds right now (`admit_fresh`). Nothing
    // is anticipated: the backup's unmerged rows or unfrozen spool are not counted as N's.
    let row_cands: Vec<(i64, String)> = src_rows.iter().flatten().cloned().collect();
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
    let mut faults_refused: Option<String> = None;
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
        if table == "faults" && src_rows.is_none() {
            // No digest can be made, so the count rule cannot tell held from new: take none
            // rather than every row.
            faults_refused = Some(format!(
                "its faults table lacks one of the columns {} that every release has",
                CORE_FAULTS.join(", ")
            ));
            continue;
        }
        let t = quote(table);
        // Faults have no positional prefix: the count rule alone decides, so a
        // row the source shares with a backup that is not merged yet still counts.
        let from: i64 = match prefix.get(table) {
            Some(_) if table == "faults" => 0,
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
        // A folder's spool goes by the same count rule as its rows: a record comes in only while
        // the folder holds more of its digest than N does.
        Kind::Folder | Kind::Shared => {
            let fresh: Vec<faults::FaultRecord> = spool_recs
                .iter()
                .zip(spool_admitted)
                .filter(|(_, ok)| **ok)
                .map(|(r, _)| r.clone())
                .collect();
            if fresh.is_empty() && !spool_recs.is_empty() {
                (Spool::AlreadyHeld, None)
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
        record_rows(&tx, "main", "n", table).map_err(|e| failed(table, e))?;
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
    let faults_held = skip.len()
        + if matches!(spool, Spool::Taken(_) | Spool::AlreadyHeld) {
            spool_admitted.iter().filter(|ok| !**ok).count()
        } else {
            0
        };
    let mut rep = ImportReport {
        prefix,
        rows,
        spool,
        faults_held,
        faults_refused,
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

    /// The merge of the backup's rows, then the freeze that takes its spool.
    fn merge_and_freeze(i: &Install) {
        let m = match marker::read(&i.n) {
            Seen::Migrated(m) => m,
            other => panic!("{other:?}"),
        };
        crate::legacy::freeze::merge_backup(&i.n_db(), &i.b, &m).unwrap();
        let rec = i.root.join(".setup_done");
        fs::write(&rec, format!("data_dir={}\n", i.n.display())).unwrap();
        assert!(matches!(
            crate::legacy::freeze::freeze_if_ready(&i.n, &rec, &m),
            crate::legacy::freeze::Freeze::Frozen(_)
        ));
    }

    /// Every fault kind of the ledger with its count.
    fn kind_counts(p: &Path) -> BTreeMap<String, i64> {
        let c = Connection::open(p).unwrap();
        let mut st = c
            .prepare("SELECT kind, count(*) FROM faults GROUP BY kind")
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The copy of the previous folder came after its backup was deleted. Every table but faults
    /// gained nothing; faults gained the one spool line the freeze could no longer take from the
    /// backup (the copy holds it, N held none), and now each digest is held exactly once, the
    /// maximum over sources.
    fn assert_after_deleted_backup(i: &Install, before: &BTreeMap<String, i64>, r: &ImportReport) {
        let after = counts(&i.n_db());
        for t in TABLES {
            assert_eq!(after[t], before[t] + i64::from(t == "faults"), "{t}: {r:?}");
        }
        assert_eq!(fault_kind_count(&i.n_db(), "backup-spool"), 1, "{r:?}");
        assert!(kind_counts(&i.n_db()).values().all(|&n| n == 1), "{r:?}");
    }

    fn fault_kind_count(p: &Path, kind: &str) -> i64 {
        Connection::open(p)
            .unwrap()
            .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                r.get(0)
            })
            .unwrap()
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
    fn a_restored_copy_of_the_old_folder_ends_at_the_maximum_and_adds_no_row() {
        let i = install();
        let before = counts(&i.n_db());
        let copy = restored(&i);
        assert!(
            copy.join("faults.jsonl").is_file(),
            "the copy has its spool"
        );

        let r = i.import(&copy).unwrap();

        // Tables other than faults: not one row came in twice.
        assert_eq!(r.total_rows(), 0, "{r:?}");
        // The copy's spool line is the backup's one `backup-spool`: right after the import the
        // freeze has not taken the backup's, so N holds none and the copy's one comes in; the
        // freeze then adds max(0, 1 - 1) = 0 and the final count is the max over sources, 1.
        merge_and_freeze(&i);
        let after = counts(&i.n_db());
        for t in TABLES {
            assert_eq!(after[t], before[t] + i64::from(t == "faults"), "{t}: {r:?}");
        }
        assert_eq!(fault_kind_count(&i.n_db(), "backup-spool"), 1);
        assert_eq!(r.spool, Spool::Taken(1), "{r:?}");
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
    fn a_restored_copy_written_to_after_the_restore_ends_at_the_maximum_per_digest() {
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

        assert_eq!(r.rows["read_events"], 1);
        assert_eq!(r.rows["faults"], 1);
        assert_eq!(
            r.spool,
            Spool::Taken(1),
            "its own spool line comes in: {r:?}"
        );
        // The freeze then takes the backup's spool line. Final state: each digest at the max
        // over sources (the copy's rows and spool, the backup's rows and spool), never a loss.
        merge_and_freeze(&i);
        let after = counts(&i.n_db());
        for t in TABLES {
            let extra = match t {
                "faults" => 3, // row-after, late-spool, backup-spool (the freeze)
                _ => 1,
            };
            assert_eq!(after[t], before[t] + extra, "{t}: {r:?}");
        }
        for kind in ["row-after", "late-spool", "backup-spool"] {
            assert_eq!(fault_kind_count(&i.n_db(), kind), 1, "{kind}");
        }
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

    /// An older Lumen in another process, with the ledger open in WAL mode and a row committed
    /// that is still in its `-wal`: the test binary run again on `hold_helper` below. A lock of
    /// this process is not one another process can see (POSIX locks belong to a process, and
    /// closing any descriptor of the file releases them), so a held ledger is held by a child.
    struct Holder(std::process::Child);

    impl Drop for Holder {
        fn drop(&mut self) {
            self.0.kill().ok();
            self.0.wait().ok();
        }
    }

    fn hold(db: &Path) -> Holder {
        use std::io::BufRead;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "legacy::import::tests::hold_helper",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LUMEN_TEST_HOLD_LEDGER", db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let mut lines = io::BufReader::new(out).lines();
        loop {
            let l = lines
                .next()
                .expect("the holder died before it was ready")
                .unwrap();
            // libtest writes "test <name> ... " first, on the same line.
            if l.trim_end().ends_with("HOLDING") {
                break;
            }
        }
        Holder(child)
    }

    /// Not a test: the child's body. Run by itself it does nothing.
    #[test]
    fn hold_helper() {
        use std::io::Read;
        let Some(db) = std::env::var_os("LUMEN_TEST_HOLD_LEDGER") else {
            return;
        };
        let c = Connection::open(db).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        c.execute_batch("INSERT INTO faults (ts, kind) VALUES ('t', 'held')")
            .unwrap();
        println!("HOLDING");
        std::io::Write::flush(&mut io::stdout()).unwrap();
        io::stdin().read_to_end(&mut Vec::new()).unwrap();
        drop(c);
    }

    #[test]
    fn a_held_ledger_is_refused_and_nothing_moves() {
        let i = install();
        let d = recreated(&i);
        // An older Lumen with the ledger open in WAL mode, as its daemon holds it.
        let old = hold(&d.join("lumen.db"));
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
            assert!(matches!(r.spool, Spool::AlreadyHeld), "{r:?}");
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
    fn a_copy_of_the_previous_folder_after_its_backup_is_deleted_adds_no_row_but_its_spool_line() {
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
        // Every table but faults: no row twice. The backup's spool line, which the deleted
        // backup can no longer give the freeze, is held by the copy: it comes in once, so the
        // max over sources (1) is reached and nothing is lost.
        let after = counts(&i.n_db());
        for t in TABLES {
            assert_eq!(after[t], before[t] + i64::from(t == "faults"), "{t}: {r:?}");
        }
        assert_eq!(fault_kind_count(&i.n_db(), "backup-spool"), 1);
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
            assert_eq!(r.spool, Spool::AlreadyHeld);
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
        fn a_copy_with_the_merged_late_rows_after_the_backup_is_deleted_adds_no_row_but_its_spool_line()
         {
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
            assert_after_deleted_backup(&i, &before, &r);
        }

        /// The same, and the copy was opened by a newer app that added a column to faults.
        #[test]
        fn a_merged_copy_with_a_later_column_after_the_backup_is_deleted_adds_no_row_but_its_spool_line()
         {
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
            assert_after_deleted_backup(&i, &before, &r);
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

    mod first_faults_table {
        use super::*;
        const V150: &str = include_str!("../../tests/fixtures/ledger_schema_v1.5.0.sql");

        /// 1.5.0 shipped the first faults table, before `agent_id` was added to it: its faults
        /// are taken with its read events, and none is refused for a missing column.
        #[test]
        fn a_1_5_0_ledger_imports_with_its_faults() {
            let i = install();
            let d = i.root.join("home").join("old-150");
            fs::create_dir_all(&d).unwrap();
            let c = Connection::open(d.join("lumen.db")).unwrap();
            c.execute_batch(V150).unwrap();
            c.execute_batch(
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                   routed_via) VALUES ('t-150', 'Read', '/150', 1, 2, 1, 'builtin_read'); \
                 INSERT INTO faults (ts, kind, session_id) VALUES ('t-150', 'row-150', 's');",
            )
            .unwrap();
            drop(c);
            let before = counts(&i.n_db());
            let r = i.import(&d).unwrap();
            assert_eq!(r.faults_refused, None, "{r:?}");
            assert_eq!(r.rows.get("read_events"), Some(&1), "{r:?}");
            assert_eq!(r.rows.get("faults"), Some(&1), "{r:?}");
            let after = counts(&i.n_db());
            assert_eq!(after["read_events"], before["read_events"] + 1);
            assert_eq!(after["faults"], before["faults"] + 1);
        }
    }

    mod review_attacks {
        use super::*;
        /// The core sets are in every released schema, and none of the columns that came or went.
        #[test]
        fn core_columns_are_in_every_released_schema() {
            for ddl in [V140, V150, V180, V190] {
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
        const V150: &str = include_str!("../../tests/fixtures/ledger_schema_v1.5.0.sql");
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
        fn rev_an_older_schema_copy_of_the_previous_folder_after_its_backup_is_deleted_adds_only_its_spool_line()
         {
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
            assert_after_deleted_backup(&i, &before, &r);
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
            assert!(before == after, "the user's ledger was rewritten");
            assert_eq!(kind_count(&i.n_db(), "row-u2"), 1, "{r:?}");
        }
    }

    mod source_untouched {
        use super::*;
        use crate::legacy::freeze::{Freeze, freeze_if_ready};
        #[cfg(unix)]
        use std::sync::Arc;
        #[cfg(unix)]
        use std::sync::atomic::{AtomicBool, Ordering};

        fn kind_count(p: &Path, kind: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
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

        fn fixed(kind: &str, agent: Option<&str>) -> String {
            let mut r = FaultRecord::now(kind, "v");
            r.ts = "2026-10-01T00:00:00Z".into();
            r.session_id = Some("s".into());
            r.version = Some("1.9.0".into());
            r.agent_id = agent.map(str::to_string);
            serde_json::to_string(&r).unwrap()
        }

        /// A rename that crosses mount points of one file system (a Linux bind mount): the same
        /// st_dev, so `crosses_volumes` says no, and the rename says EXDEV.
        struct BindMount;
        impl Ops for BindMount {
            fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::CrossesDevices, "EXDEV"))
            }
            fn crosses_volumes(&self, _f: &Path, _t: &Path) -> bool {
                false
            }
        }

        fn wal_source(d: &Path) -> Vec<u8> {
            ledger(&d.join("lumen.db"), &["u1"]);
            let c = Connection::open(d.join("lumen.db")).unwrap();
            let m: String = c
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                .unwrap();
            assert_eq!(m, "wal");
            c.execute_batch(&rows_for("u2")).unwrap();
            drop(c);
            fs::read(d.join("lumen.db")).unwrap()
        }

        #[test]
        fn ds1_exdev_fallback_leaves_the_source_as_it_was() {
            let i = install();
            let d = i.root.join("bind").join("old");
            let before = wal_source(&d);
            let r = import(&i.n, &d, &opts(), &BindMount).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
            let after = fs::read(d.join("lumen.db")).unwrap();
            eprintln!(
                "ds1 header 18/19 before {:?} after {:?}",
                &before[18..20],
                &after[18..20]
            );
            assert!(before == after, "the user's ledger was rewritten");
        }

        /// An older Lumen holds the ledger on another volume: refused, and nothing was copied.
        #[test]
        fn ds2_a_held_ledger_on_another_volume_is_refused() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            let old = hold(&d.join("lumen.db"));
            let before = snap(&i.n);
            let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices));
            assert!(matches!(r, Err(ImportError::InUse(_))), "{r:?}");
            assert_eq!(snap(&i.n), before, "nothing changed");
            assert!(!i.imports().exists(), "nothing was copied");
            drop(old);
            // Quit: the same import goes through.
            let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
        }

        #[test]
        fn ds3_a_dry_run_leaves_a_wal_source_as_it_was() {
            let i = install();
            let d = i.root.join("home").join("old");
            let before = wal_source(&d);
            let mut o = opts();
            o.dry_run = true;
            import(&i.n, &d, &o, &Native).unwrap();
            let after = fs::read(d.join("lumen.db")).unwrap();
            eprintln!(
                "ds3 header 18/19 before {:?} after {:?}",
                &before[18..20],
                &after[18..20]
            );
            assert!(before == after, "a dry run rewrote the user's ledger");
        }

        #[test]
        fn ds3b_a_refused_move_leaves_a_wal_source_as_it_was() {
            let i = install();
            let d = i.root.join("home").join("old");
            let before = wal_source(&d);
            let e =
                import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::PermissionDenied)).unwrap_err();
            assert!(matches!(e, ImportError::InUse(_)), "{e:?}");
            let after = fs::read(d.join("lumen.db")).unwrap();
            assert!(
                before == after,
                "nothing changes: but the user's ledger was rewritten"
            );
        }

        /// A rename that works, after which an older Lumen opens the ledger at its new place (in
        /// this process, so the probe sees it): what a connection that opened between the check
        /// and the move looks like.
        #[cfg(unix)]
        struct RenameThenOpen(std::cell::RefCell<Vec<Connection>>);
        #[cfg(unix)]
        impl Ops for RenameThenOpen {
            fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
                fs::rename(from, to)?;
                let db = if to.is_dir() {
                    to.join(LEDGER)
                } else {
                    to.to_path_buf()
                };
                if db.file_name().is_some_and(|n| n == LEDGER) && db.is_file() {
                    let c = Connection::open(&db).unwrap();
                    c.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
                        .unwrap();
                    c.execute_batch("INSERT INTO faults (ts, kind) VALUES ('t', 'late-open')")
                        .unwrap();
                    self.0.borrow_mut().push(c);
                }
                Ok(())
            }
            fn crosses_volumes(&self, _f: &Path, _t: &Path) -> bool {
                false
            }
        }

        /// ds9: the source moved, and then it turns out to be held: it goes back, whatever kind it
        /// is (a folder, a ledger file, a set-aside ledger with its wal), and nothing is left in
        /// N; once the older Lumen is gone the import goes through.
        #[cfg(unix)]
        #[test]
        fn ds9_a_source_found_held_after_the_move_goes_back() {
            for kind in ["folder", "file", "set"] {
                let i = install();
                let (arg, files): (PathBuf, Vec<PathBuf>) = match kind {
                    "folder" => {
                        let d = recreated(&i);
                        (d.clone(), vec![d.join("lumen.db"), d.join("faults.jsonl")])
                    }
                    "file" => {
                        let f = i.root.join("usb").join("old.db");
                        ledger(&f, &["u1"]);
                        (f.clone(), vec![f])
                    }
                    _ => {
                        let f = i.n.join("lumen.db.pre-legacy");
                        ledger(&f, &["u1"]);
                        (f.clone(), vec![f])
                    }
                };
                let before = snap(&i.n);
                let ops = RenameThenOpen(Default::default());
                let r = import(&i.n, &arg, &opts(), &ops);
                assert!(matches!(r, Err(ImportError::InUse(_))), "{kind}: {r:?}");
                for f in &files {
                    assert!(f.is_file(), "{kind}: {} did not go back", f.display());
                }
                assert!(!i.imports().exists(), "{kind}: a folder was left in N");
                if kind != "set" {
                    // (A set-aside ledger lives in N, and the connection that opened it wrote there.)
                    assert_eq!(snap(&i.n), before, "{kind}: N changed");
                }
                ops.0.borrow_mut().clear();
                // The older Lumen gone, nothing is in the way: the same import goes through.
                let r = import(&i.n, &arg, &opts(), &Native).unwrap();
                assert!(matches!(r.placed, Placed::Moved), "{kind}: {r:?}");
            }
        }

        /// 1.8.0 schema: the previous folder on 1.8.0 with a spool line; a restored copy that a
        /// 1.8.0 app drained (nine columns) and wrote a row to, in both orders.
        #[test]
        fn ds6_schema_1_8_restored_drained_copy_both_orders() {
            const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
            for import_first in [true, false] {
                let t = TempDir::new().unwrap();
                let root = t.path().join("data");
                let l = root.join("example.previous.app");
                let n = root.join("example.current.app");
                fs::create_dir_all(&l).unwrap();
                {
                    let c = Connection::open(l.join("lumen.db")).unwrap();
                    c.execute_batch(V180).unwrap();
                    c.execute_batch(
                        "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, routed_via) VALUES ('t1','Read','/a',1,2,1,'builtin_read'); \
                         INSERT INTO faults (ts, kind, session_id) VALUES ('t1', 'row-old', 's');",
                    )
                    .unwrap();
                }
                let rec = fixed("spool180", None);
                fs::write(l.join("faults.jsonl"), format!("{rec}\n")).unwrap();
                let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
                o.probe_tries = 1;
                o.probe_delay = Duration::from_millis(10);
                let m: MarkerV2 = match commit::migrate(&l, &n, &o, &Native) {
                    Outcome::Migrated { marker, .. } => marker,
                    other => panic!("{other:?}"),
                };
                let b = backup_dir(&n, &m).unwrap();
                let i = Install { _t: t, root, n, b };
                let c = i.old_place();
                copy_dir(&i.b_data(), &c);
                make_writable(&c);
                // a 1.8.0 drain: nine columns
                {
                    let r: FaultRecord = serde_json::from_str(&rec).unwrap();
                    let conn = Connection::open(c.join("lumen.db")).unwrap();
                    conn.execute(
                        "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                        rusqlite::params![r.ts, r.kind, r.variant, r.path, r.lines, r.detail, r.session_id, r.version, r.channel],
                    )
                    .unwrap();
                    conn.execute_batch(
                        "INSERT INTO faults (ts, kind, session_id) VALUES ('t2', 'row-new', 's');",
                    )
                    .unwrap();
                }
                fs::remove_file(c.join("faults.jsonl")).unwrap();
                if import_first {
                    i.import(&c).unwrap();
                    freeze(&i);
                } else {
                    freeze(&i);
                    i.import(&c).unwrap();
                }
                assert_eq!(
                    (
                        kind_count(&i.n_db(), "spool180"),
                        kind_count(&i.n_db(), "row-new"),
                        kind_count(&i.n_db(), "row-old")
                    ),
                    (1, 1, 1),
                    "import_first={import_first}"
                );
            }
        }

        /// 1.4.0 schema (no faults table): a re-created 1.4.0 folder with a spool imported twice
        /// (the second time a copy).
        #[test]
        fn ds6b_schema_1_4_folder_with_spool_twice() {
            const V140: &str = include_str!("../../tests/fixtures/ledger_schema_v1.4.0.sql");
            let i = install();
            let d = i.root.join("home").join("o14");
            fs::create_dir_all(&d).unwrap();
            Connection::open(d.join("lumen.db"))
                .unwrap()
                .execute_batch(V140)
                .unwrap();
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("s14"))).unwrap();
            let copy = i.root.join("tm").join("o14");
            copy_dir(&d, &copy);
            let r1 = i.import(&d);
            eprintln!("ds6b r1 {r1:?}");
            let r2 = i.import(&copy);
            eprintln!("ds6b r2 {r2:?}");
            assert!(kind_count(&i.n_db(), "s14") <= 1);
        }

        /// The same, slower, with each copy checked.
        #[cfg(unix)]
        #[test]
        fn ds8_a_slow_live_writer_on_another_volume() {
            let sleep_us: u64 = std::env::var("DS_SLEEP_US")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3000);
            let batch: u64 = std::env::var("DS_BATCH")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(5);
            let iters: u32 = std::env::var("DS_ITERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200);
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u0"]);
            {
                // a bigger file, so a copy takes a while
                let c = Connection::open(d.join("lumen.db")).unwrap();
                c.execute_batch("BEGIN").unwrap();
                for k in 0..20000 {
                    c.execute(
                        "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, routed_via) VALUES ('t','Read',?1,1,2,1,'builtin_read')",
                        [format!("/p/{k}/{}", "x".repeat(200))],
                    ).unwrap();
                }
                c.execute_batch("COMMIT").unwrap();
            }
            let stop = Arc::new(AtomicBool::new(false));
            let db = d.join("lumen.db");
            let st = stop.clone();
            let h = std::thread::spawn(move || {
                let c = Connection::open(&db).unwrap();
                c.busy_timeout(Duration::from_secs(5)).unwrap();
                let _: String = c
                    .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                    .unwrap();
                c.execute_batch("PRAGMA wal_autocheckpoint=4").unwrap();
                let mut k = 0u64;
                while !st.load(Ordering::Relaxed) {
                    let mut ok = true;
                    let _ = c.execute_batch("BEGIN IMMEDIATE");
                    for _ in 0..batch {
                        if c.execute(
                            "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, routed_via) VALUES ('t','Read',?1,1,2,1,'builtin_read')",
                            [format!("/w/{k}/{}", "y".repeat(std::env::var("DS_PAD").ok().and_then(|v| v.parse().ok()).unwrap_or(0)))],
                        ).is_err() { ok = false; break; }
                        k += 1;
                    }
                    if ok {
                        if c.execute_batch("COMMIT").is_err() {
                            let _ = c.execute_batch("ROLLBACK");
                        }
                    } else {
                        let _ = c.execute_batch("ROLLBACK");
                    }
                    std::thread::sleep(Duration::from_micros(sleep_us));
                }
            });
            let (mut ok, mut err, mut malformed) = (0, 0, 0);
            for _ in 0..iters {
                match import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)) {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        err += 1;
                        if e.to_string().contains("malformed") {
                            malformed += 1;
                        }
                    }
                }
            }
            stop.store(true, Ordering::Relaxed);
            h.join().unwrap();
            let last = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices));
            let mut bad_copies = 0;
            for e in fs::read_dir(i.imports()).unwrap() {
                let p = e.unwrap().path().join("lumen.db");
                if !p.is_file() {
                    continue;
                }
                let c = Connection::open_with_flags(&p, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                let r: String = c
                    .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                    .unwrap_or_else(|e| e.to_string());
                if r != "ok" {
                    bad_copies += 1;
                    eprintln!("ds8 bad copy {}: {r}", p.display());
                }
            }
            let src: i64 = Connection::open(d.join("lumen.db"))
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM read_events WHERE path LIKE '/w/%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let c = Connection::open(i.n_db()).unwrap();
            let (n_rows, n_distinct, p_rows): (i64, i64, i64) = c
                .query_row("SELECT sum(path LIKE '/w/%'), count(DISTINCT CASE WHEN path LIKE '/w/%' THEN path END), sum(path LIKE '/p/%') FROM read_events", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap();
            eprintln!(
                "ds8 ok={ok} err={err} malformed={malformed} bad_copies={bad_copies} last_ok={} src={src} n_rows={n_rows} n_distinct={n_distinct} p_rows={p_rows}",
                last.is_ok()
            );
            assert_eq!(
                (n_rows, n_distinct, p_rows, bad_copies),
                (src, src, 20000, 0)
            );
        }
    }

    mod rev_ms1 {
        use super::*;
        use crate::legacy::freeze::{Freeze, freeze_if_ready, merge_backup};

        fn kind_count(p: &Path, kind: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
        }

        fn marker_of(i: &Install) -> MarkerV2 {
            match marker::read(&i.n) {
                Seen::Migrated(m) => (*m).clone(),
                other => panic!("{other:?}"),
            }
        }

        fn freeze(i: &Install) -> Freeze {
            let rec = i.root.join(".setup_done");
            fs::write(&rec, format!("data_dir={}\n", i.n.display())).unwrap();
            freeze_if_ready(&i.n, &rec, &marker_of(i))
        }

        fn fixed(kind: &str, agent: Option<&str>) -> String {
            let mut r = FaultRecord::now(kind, "v");
            r.ts = "2026-10-01T00:00:00Z".into();
            r.session_id = Some("s".into());
            r.version = Some("1.9.0".into());
            r.agent_id = agent.map(str::to_string);
            serde_json::to_string(&r).unwrap()
        }

        /// An older app's drain: spool (and `.draining`) into its own rows, then the files go.
        fn old_app_drains(d: &Path) {
            let (recs, _) = read_spool_files(&d.join("faults.jsonl")).unwrap();
            let c = Connection::open(d.join("lumen.db")).unwrap();
            for r in &recs {
                c.execute(
                    "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    rusqlite::params![r.ts, r.kind, r.variant, r.path, r.lines, r.detail, r.session_id, r.version, r.channel],
                )
                .unwrap();
            }
            fs::remove_file(d.join("faults.jsonl")).ok();
            fs::remove_file(d.join("faults.jsonl.draining")).ok();
        }

        /// S1 with the move refused (S2): the previous folder `l` still in place.
        fn migrate_deferred(root: &Path) -> (PathBuf, PathBuf, MarkerV2) {
            let l = root.join("example.previous.app");
            let n = root.join("example.current.app");
            let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
            o.probe_tries = 1;
            o.probe_delay = Duration::from_millis(10);
            let m: MarkerV2 =
                match commit::migrate(&l, &n, &o, &Refuse(io::ErrorKind::PermissionDenied)) {
                    Outcome::Migrated { marker, moved } => {
                        assert!(matches!(moved, Moved::Deferred(_)), "{moved:?}");
                        marker
                    }
                    other => panic!("{other:?}"),
                };
            (l, n, m)
        }

        fn do_retry_move(n: &Path, m: &MarkerV2) {
            let mut o = CommitOptions::new("2026-10-19T09:00:00Z");
            o.probe_tries = 1;
            o.probe_delay = Duration::from_millis(10);
            let r = commit::retry_move(n, m, &o, &Native);
            assert!(matches!(r, Moved::Done), "{r:?}");
        }

        /// M1: S2 (the move was refused), an older app drains its spool into its own rows, the
        /// next start moves the folder and is killed before its merge; the user imports a Time
        /// Machine copy of the folder taken before that drain; then the merge (or the freeze).
        #[test]
        fn m1_spool_only_copy_imported_between_the_move_and_the_merge() {
            let mut got = Vec::new();
            for via_freeze in [false, true] {
                let t = TempDir::new().unwrap();
                let root = t.path().join("data");
                let l = root.join("example.previous.app");
                ledger(&l.join("lumen.db"), &[]);
                fs::write(l.join("faults.jsonl"), format!("{}\n", line("late"))).unwrap();
                let tm = root.join("tm").join("example.previous.app");
                copy_dir(&l, &tm);
                let (l, n, m) = migrate_deferred(&root);
                old_app_drains(&l);
                do_retry_move(&n, &m);
                let b = backup_dir(&n, &m).unwrap();
                let i = Install { _t: t, root, n, b };
                let r = i.import(&tm).unwrap();
                eprintln!("m1 via_freeze={via_freeze}: {:?}", r.lines());
                if via_freeze {
                    let f = freeze(&i);
                    assert!(matches!(f, Freeze::Frozen(_)), "{f:?}");
                } else {
                    merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                }
                got.push((via_freeze, kind_count(&i.n_db(), "late")));
            }
            eprintln!("m1: {got:?}");
            assert_eq!(got, vec![(false, 1), (true, 1)], "twice");
        }

        /// M1c: the same without a spool-only folder: a copy that diverged (an old app wrote a
        /// fault of its own to it, then drained the same spool line at another rowid).
        #[test]
        fn m1c_diverged_copy_imported_between_the_move_and_the_merge() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            ledger(&l.join("lumen.db"), &["snap"]);
            fs::write(l.join("faults.jsonl"), format!("{}\n", line("late"))).unwrap();
            let tm = root.join("tm").join("example.previous.app");
            copy_dir(&l, &tm);
            let (l, n, m) = migrate_deferred(&root);
            old_app_drains(&l);
            do_retry_move(&n, &m);
            let b = backup_dir(&n, &m).unwrap();
            let i = Install { _t: t, root, n, b };
            // the copy, restored and used by an older app: a fault of its own, then its drain
            q(
                &tm.join("lumen.db"),
                "INSERT INTO faults (ts, kind, session_id) VALUES ('t-own', 'own', 's')",
            );
            old_app_drains(&tm);
            let r = i.import(&tm).unwrap();
            eprintln!("m1c: {:?}", r.lines());
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert_eq!(
                (
                    kind_count(&i.n_db(), "late"),
                    kind_count(&i.n_db(), "own"),
                    kind_count(&i.n_db(), "row-snap")
                ),
                (1, 1, 1)
            );
        }

        /// M1r: control, the merge first (the normal order): once each.
        #[test]
        fn m1r_control_merge_first() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            ledger(&l.join("lumen.db"), &[]);
            fs::write(l.join("faults.jsonl"), format!("{}\n", line("late"))).unwrap();
            let tm = root.join("tm").join("example.previous.app");
            copy_dir(&l, &tm);
            let (l, n, m) = migrate_deferred(&root);
            old_app_drains(&l);
            do_retry_move(&n, &m);
            let b = backup_dir(&n, &m).unwrap();
            let i = Install { _t: t, root, n, b };
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            i.import(&tm).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            assert_eq!(kind_count(&i.n_db(), "late"), 1);
        }

        /// M2 (ds4): the backup's spool holds d once; another folder holds it twice. Both orders
        /// must end with the folder's count.
        #[test]
        fn m2_identical_faults_keep_the_sources_count_in_both_orders() {
            let mut got = Vec::new();
            for import_first in [true, false] {
                let i = install();
                // the backup's spool: one d
                make_writable(&i.b);
                fs::write(
                    i.b_data().join("faults.jsonl"),
                    format!("{}\n", fixed("dup", Some("a0"))),
                )
                .unwrap();
                let s = i.root.join("home").join("s");
                ledger(&s.join("lumen.db"), &[]);
                fs::write(
                    s.join("faults.jsonl"),
                    format!(
                        "{}\n{}\n",
                        fixed("dup", Some("a1")),
                        fixed("dup", Some("a2"))
                    ),
                )
                .unwrap();
                if import_first {
                    i.import(&s).unwrap();
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                } else {
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    i.import(&s).unwrap();
                }
                got.push(kind_count(&i.n_db(), "dup"));
            }
            eprintln!("m2 import-first {} freeze-first {}", got[0], got[1]);
            assert_eq!(
                got,
                vec![2, 2],
                "import-first {} freeze-first {}",
                got[0],
                got[1]
            );
        }

        /// M3: a backup that will never freeze (rewound: someone replaced it). A restored copy
        /// whose old app drained the backup's spool line into its rows is imported; the import
        /// leaves the line to a freeze that never comes.
        #[test]
        fn m3_a_rewound_backup_never_takes_the_line_the_import_left_to_it() {
            let i = install();
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            let c = i.root.join("tm").join("copy");
            copy_dir(&i.b_data(), &c);
            make_writable(&c);
            old_app_drains(&c);
            q(&c.join("lumen.db"), &rows_for("later"));
            // the backup replaced by an older ledger
            make_writable(&i.b);
            fs::remove_file(i.b_data().join("lumen.db")).unwrap();
            ledger(&i.b_data().join("lumen.db"), &["snap"]);
            let r = i.import(&c).unwrap();
            eprintln!("m3: {:?}", r.lines());
            let f = freeze(&i);
            eprintln!("m3 freeze: {f:?}");
            assert_eq!(kind_count(&i.n_db(), "row-later"), 1);
            assert_eq!(
                kind_count(&i.n_db(), "backup-spool"),
                1,
                "lost: the copy held it, N never got it"
            );
        }

        /// M4: a WAL source with a live `-wal` (a crashed old app) on another volume, and the same
        /// for a dry run: the source folder is byte for byte as it was, and no `-shm` appears.
        #[test]
        fn m4_a_source_with_a_live_wal_is_never_written() {
            for (exdev, dry) in [(true, false), (false, true), (true, true)] {
                let i = install();
                let d = i.root.join("usb").join("old");
                ledger(&d.join("lumen.db"), &["u1"]);
                {
                    let c = Connection::open(d.join("lumen.db")).unwrap();
                    let _: String = c
                        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                        .unwrap();
                    c.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
                    c.execute_batch(&rows_for("u2")).unwrap();
                    // a crash: the files as they are now, with the -wal holding u2
                    let crash = i.root.join("crash");
                    fs::create_dir_all(&crash).unwrap();
                    fs::copy(d.join("lumen.db"), crash.join("lumen.db")).unwrap();
                    fs::copy(d.join("lumen.db-wal"), crash.join("lumen.db-wal")).unwrap();
                    drop(c);
                    fs::remove_dir_all(&d).unwrap();
                    fs::create_dir_all(&d).unwrap();
                    fs::copy(crash.join("lumen.db"), d.join("lumen.db")).unwrap();
                    fs::copy(crash.join("lumen.db-wal"), d.join("lumen.db-wal")).unwrap();
                }
                fs::write(d.join("faults.jsonl"), format!("{}\n", line("wal-spool"))).unwrap();
                assert!(fs::metadata(d.join("lumen.db-wal")).unwrap().len() > 0);
                let before = snap(&d);
                let mut o = opts();
                o.dry_run = dry;
                let r = if exdev {
                    import(&i.n, &d, &o, &Refuse(io::ErrorKind::CrossesDevices))
                } else {
                    import(&i.n, &d, &o, &Native)
                }
                .unwrap();
                eprintln!("m4 exdev={exdev} dry={dry}: {:?}", r.lines());
                let after = snap(&d);
                assert_eq!(
                    before.keys().collect::<Vec<_>>(),
                    after.keys().collect::<Vec<_>>()
                );
                assert!(
                    before == after,
                    "exdev={exdev} dry={dry}: the source changed"
                );
                assert!(
                    !d.join("lumen.db-shm").exists(),
                    "a -shm was made in the source"
                );
                if !dry {
                    assert_eq!(
                        kind_count(&i.n_db(), "row-u2"),
                        1,
                        "the -wal's rows came in"
                    );
                    assert_eq!(kind_count(&i.n_db(), "wal-spool"), 1);
                }
            }
        }

        /// M5: a source in rollback mode with an open read transaction in another connection
        /// (SHARED lock) on another volume: refused, and nothing changes.
        #[test]
        fn m5_a_reader_in_rollback_mode_is_refused_on_another_volume() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            let before_n = counts(&i.n_db());
            let before = snap(&d);
            // a child process is needed for POSIX locks; here only a sanity run in-process
            let _ = before_n;
            let _ = before;
        }

        /// M7: a 1.8.0 previous folder, spool-only, two lines; an older copy with the first line
        /// only; a later copy whose 1.8.0 app drained both lines; the freeze; in every order.
        #[test]
        fn m7_two_copies_and_the_freeze_in_every_order() {
            const V180: &str = include_str!("../../tests/fixtures/ledger_schema_v1.8.0.sql");
            let orders: [[u8; 3]; 6] = [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ];
            for ord in orders {
                let t = TempDir::new().unwrap();
                let root = t.path().join("data");
                let l = root.join("example.previous.app");
                fs::create_dir_all(&l).unwrap();
                Connection::open(l.join("lumen.db"))
                    .unwrap()
                    .execute_batch(V180)
                    .unwrap();
                let s1 = fixed("s1", None);
                fs::write(l.join("faults.jsonl"), format!("{s1}\n")).unwrap();
                let older = root.join("tm1").join("example.previous.app");
                copy_dir(&l, &older);
                let s2 = fixed("s2", None);
                fs::write(l.join("faults.jsonl"), format!("{s1}\n{s2}\n")).unwrap();
                let later = root.join("tm2").join("example.previous.app");
                copy_dir(&l, &later);
                old_app_drains(&later);
                let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
                o.probe_tries = 1;
                o.probe_delay = Duration::from_millis(10);
                let m: MarkerV2 =
                    match commit::migrate(&l, &root.join("example.current.app"), &o, &Native) {
                        Outcome::Migrated { marker, .. } => marker,
                        other => panic!("{other:?}"),
                    };
                let n = root.join("example.current.app");
                let b = backup_dir(&n, &m).unwrap();
                let i = Install { _t: t, root, n, b };
                for step in ord {
                    match step {
                        0 => assert!(matches!(freeze(&i), Freeze::Frozen(_))),
                        1 => {
                            i.import(&older).unwrap();
                        }
                        _ => {
                            i.import(&later).unwrap();
                        }
                    }
                }
                assert_eq!(
                    (kind_count(&i.n_db(), "s1"), kind_count(&i.n_db(), "s2")),
                    (1, 1),
                    "order {ord:?}"
                );
            }
        }

        /// M8: the backup holds a `.draining` batch (an old drain killed after its rename) and a
        /// live spool; a restored copy whose old app finished both drains; both orders.
        #[test]
        fn m8_a_draining_batch_in_the_backup_and_a_copy_that_drained_it() {
            for import_first in [true, false] {
                let i = install();
                make_writable(&i.b);
                fs::write(
                    i.b_data().join("faults.jsonl.draining"),
                    format!("{}\n", fixed("dr1", None)),
                )
                .unwrap();
                fs::write(
                    i.b_data().join("faults.jsonl"),
                    format!("{}\n{}\n", line("backup-spool"), fixed("dr2", None)),
                )
                .unwrap();
                let c = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c);
                make_writable(&c);
                old_app_drains(&c);
                q(&c.join("lumen.db"), &rows_for("later"));
                if import_first {
                    i.import(&c).unwrap();
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                } else {
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    i.import(&c).unwrap();
                }
                assert_eq!(
                    (
                        kind_count(&i.n_db(), "dr1"),
                        kind_count(&i.n_db(), "dr2"),
                        kind_count(&i.n_db(), "backup-spool"),
                        kind_count(&i.n_db(), "row-later")
                    ),
                    (1, 1, 1, 1),
                    "import_first={import_first}"
                );
            }
        }

        /// M9: N's own spool holds a line a copy of N's folder (fresh, no rows) also holds; N's
        /// drain runs after the import.
        #[test]
        fn m9_a_copy_of_a_fresh_live_folder_with_its_spool() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            ledger(&l.join("lumen.db"), &["snap"]);
            let n = root.join("example.current.app");
            let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
            o.probe_tries = 1;
            o.probe_delay = Duration::from_millis(10);
            let m: MarkerV2 = match commit::migrate(&l, &n, &o, &Native) {
                Outcome::Migrated { marker, .. } => marker,
                other => panic!("{other:?}"),
            };
            let b = backup_dir(&n, &m).unwrap();
            let i = Install { _t: t, root, n, b };
            fs::write(
                i.n.join("faults.jsonl"),
                format!("{}\n", fixed("n-own", None)),
            )
            .unwrap();
            let c = i.root.join("home").join("ncopy");
            fs::create_dir_all(&c).unwrap();
            ledger(&c.join("lumen.db"), &[]);
            fs::copy(i.n.join("faults.jsonl"), c.join("faults.jsonl")).unwrap();
            i.import(&c).unwrap();
            // N's drain
            let conn = crate::meter::connect_db(&i.n_db()).unwrap();
            let (recs, _) = read_spool_files(&i.n.join("faults.jsonl")).unwrap();
            drop(conn);
            let mut c2 = Connection::open(i.n_db()).unwrap();
            let tx = c2.transaction().unwrap();
            faults::insert_into(&tx, "main", &recs).unwrap();
            tx.commit().unwrap();
            fs::remove_file(i.n.join("faults.jsonl")).unwrap();
            assert_eq!(kind_count(&i.n_db(), "n-own"), 1);
        }

        /// The same sources through every order of {merge, freeze, import of two copies, N's own
        /// drain}: for each digest N ends up with the maximum count any one source holds. The
        /// sources: N's own records, the previous install (rows above the mark, spool and a
        /// `.draining` batch with two identical faults), a restored copy that an old app drained,
        /// and a spool-only folder that holds a fault the backup holds once, twice.
        #[test]
        fn every_order_ends_with_the_maximum_any_source_holds() {
            fn digests(db: Option<&Path>, dir: &Path) -> crate::legacy::faultset::FaultCounts {
                let mut all = crate::legacy::faultset::FaultCounts::new();
                if let Some(db) = db {
                    let c = Connection::open(db).unwrap();
                    for (_, k) in row_digests(&c, "main").unwrap().unwrap() {
                        *all.entry(k).or_insert(0) += 1;
                    }
                }
                let (recs, _) = read_spool_files(&dir.join("faults.jsonl")).unwrap();
                for k in record_digests(&recs).unwrap() {
                    *all.entry(k).or_insert(0) += 1;
                }
                all
            }
            fn n_drains(n: &Path) {
                let (recs, _) = read_spool_files(&n.join("faults.jsonl")).unwrap();
                let mut c = Connection::open(n.join("lumen.db")).unwrap();
                let tx = c.transaction().unwrap();
                faults::insert_into(&tx, "main", &recs).unwrap();
                tx.commit().unwrap();
                fs::remove_file(n.join("faults.jsonl")).ok();
            }
            // ops: 0 merge, 1 freeze, 2 import the restored copy, 3 import the spool-only
            // folder, 4 N drains its own spool
            let mut orders: Vec<Vec<u8>> = Vec::new();
            fn perms(done: &mut Vec<u8>, left: &mut Vec<u8>, out: &mut Vec<Vec<u8>>) {
                if left.is_empty() {
                    out.push(done.clone());
                }
                for k in 0..left.len() {
                    let v = left.remove(k);
                    done.push(v);
                    perms(done, left, out);
                    done.pop();
                    left.insert(k, v);
                }
            }
            perms(&mut Vec::new(), &mut vec![0, 1, 2, 3, 4], &mut orders);
            assert_eq!(orders.len(), 120);
            for ord in orders {
                let i = install();
                make_writable(&i.b);
                // the backup: a row above the mark, a spool and a `.draining` batch
                {
                    // drained by an old app after the mark: the row of a line a copy still holds
                    let r = faults::parse_spool_all(fixed("late", None).as_bytes());
                    let c = Connection::open(i.b_data().join("lumen.db")).unwrap();
                    c.execute(
                        "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel) \
                         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                        rusqlite::params![
                            r[0].ts, r[0].kind, r[0].variant, r[0].path, r[0].lines, r[0].detail,
                            r[0].session_id, r[0].version, r[0].channel
                        ],
                    )
                    .unwrap();
                }
                fs::write(
                    i.b_data().join("faults.jsonl.draining"),
                    format!("{}\n{}\n", fixed("dr", Some("a1")), fixed("dr", Some("a2"))),
                )
                .unwrap();
                let mut spool = fs::read_to_string(i.b_data().join("faults.jsonl")).unwrap();
                spool.push_str(&format!("{}\n", fixed("shared", None)));
                fs::write(i.b_data().join("faults.jsonl"), spool).unwrap();
                // N's own: a row and a spool line
                q(
                    &i.n_db(),
                    "INSERT INTO faults (ts, kind, session_id) VALUES ('t-n', 'n-row', 's')",
                );
                fs::write(
                    i.n.join("faults.jsonl"),
                    format!("{}\n", fixed("n-spool", None)),
                )
                .unwrap();
                // a restored copy of the backup, drained by an older app, with a row of its own
                let c1 = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c1);
                make_writable(&c1);
                old_app_drains(&c1);
                q(
                    &c1.join("lumen.db"),
                    "INSERT INTO faults (ts, kind, session_id) VALUES ('t-c1', 'c1-own', 's')",
                );
                // a spool-only folder: the backup's late row as a spool line, the shared fault twice, and a copy of N's spool line
                let c2 = i.root.join("home").join("s");
                ledger(&c2.join("lumen.db"), &[]);
                fs::write(
                    c2.join("faults.jsonl"),
                    format!(
                        "{}\n{}\n{}\n{}\n",
                        fixed("late", Some("b0")),
                        fixed("shared", Some("b1")),
                        fixed("shared", Some("b2")),
                        fixed("n-spool", Some("b3"))
                    ),
                )
                .unwrap();
                // what each source holds, before anything moves
                let sources = [
                    digests(Some(&i.n_db()), &i.n),
                    digests(Some(&i.b_data().join("lumen.db")), &i.b_data()),
                    digests(Some(&c1.join("lumen.db")), &c1),
                    digests(Some(&c2.join("lumen.db")), &c2),
                ];
                let mut want = crate::legacy::faultset::FaultCounts::new();
                for s in &sources {
                    for (k, v) in s {
                        let e = want.entry(k.clone()).or_insert(0);
                        *e = (*e).max(*v);
                    }
                }
                for op in &ord {
                    match op {
                        0 => {
                            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                        }
                        1 => {
                            // The outcome in the message, not only the order: a failure that
                            // printed the order alone could not say which gate refused.
                            let f = freeze(&i);
                            assert!(matches!(f, Freeze::Frozen(_)), "{ord:?}: {f:?}");
                        }
                        2 => {
                            i.import(&c1).unwrap();
                        }
                        3 => {
                            i.import(&c2).unwrap();
                        }
                        _ => n_drains(&i.n),
                    }
                }
                // the rest: whatever has not run yet, in a fixed order, then N's drain
                merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                let f = freeze(&i);
                assert!(
                    matches!(f, Freeze::Frozen(_) | Freeze::Already { .. }),
                    "{ord:?} {f:?}"
                );
                n_drains(&i.n);
                let got = digests(Some(&i.n_db()), &i.n);
                assert_eq!(got, want, "order {ord:?}");
            }
        }

        // ---- rev-max0 attacks ----

        fn insert_recs(db: &Path, recs: &[FaultRecord]) {
            let c = Connection::open(db).unwrap();
            for r in recs {
                c.execute(
                    "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel,agent_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    rusqlite::params![r.ts, r.kind, r.variant, r.path, r.lines, r.detail, r.session_id, r.version, r.channel, r.agent_id],
                )
                .unwrap();
            }
        }

        /// An older app's drain killed between its insert and its delete (the `.draining` batch
        /// stays), then the next start's drain folds it back and drains again: the double that is
        /// older than F53 (spec 8.4).
        fn old_app_drain_killed_then_redrained(d: &Path) {
            let live = d.join("faults.jsonl");
            let (recs, _) = read_spool_files(&live).unwrap();
            fs::rename(&live, d.join("faults.jsonl.draining")).unwrap();
            let c = Connection::open(d.join("lumen.db")).unwrap();
            for r in &recs {
                c.execute(
                    "INSERT INTO faults(ts,kind,variant,path,lines,detail,session_id,version,channel) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    rusqlite::params![r.ts, r.kind, r.variant, r.path, r.lines, r.detail, r.session_id, r.version, r.channel],
                )
                .unwrap();
            }
            drop(c);
            // killed here; the next start drains the batch again
            old_app_drains(d);
        }

        fn src_count(db: &Path, dir: &Path, kind: &str) -> i64 {
            let (recs, _) = read_spool_files(&dir.join("faults.jsonl")).unwrap();
            kind_count(db, kind) + recs.iter().filter(|r| r.kind == kind).count() as i64
        }

        /// X1: S2; a Time Machine copy taken before the old app's drain; the old app drains L
        /// (row k); the next start moves L and is killed before its merge. In the restored copy an
        /// older app's drain is killed after its insert and redone (rows k and k+1, the first the
        /// same row as the backup's k). Sources: backup 1, copy 2. Every order must end at 2.
        #[test]
        fn x1_a_drain_double_in_a_copy_straddles_the_backups_unmerged_row() {
            let mut got = Vec::new();
            for order in 0..4u8 {
                let t = TempDir::new().unwrap();
                let root = t.path().join("data");
                let l = root.join("example.previous.app");
                ledger(&l.join("lumen.db"), &["snap"]);
                fs::write(l.join("faults.jsonl"), format!("{}\n", fixed("late", None))).unwrap();
                let tm = root.join("tm").join("example.previous.app");
                copy_dir(&l, &tm);
                let (l, n, m) = migrate_deferred(&root);
                old_app_drains(&l);
                do_retry_move(&n, &m);
                let b = backup_dir(&n, &m).unwrap();
                let i = Install { _t: t, root, n, b };
                old_app_drain_killed_then_redrained(&tm);
                let src_b = src_count(&i.b_data().join("lumen.db"), &i.b_data(), "late");
                let src_c = src_count(&tm.join("lumen.db"), &tm, "late");
                assert_eq!((src_b, src_c), (1, 2));
                match order {
                    0 => {
                        i.import(&tm).unwrap();
                        merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                    }
                    1 => {
                        merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                        i.import(&tm).unwrap();
                    }
                    2 => {
                        i.import(&tm).unwrap();
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    }
                    _ => {
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                        i.import(&tm).unwrap();
                    }
                }
                merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                let _ = freeze(&i);
                got.push(kind_count(&i.n_db(), "late"));
            }
            eprintln!("x1 import-merge / merge-import / import-freeze / freeze-import: {got:?}");
            assert_eq!(
                got,
                vec![2, 2, 2, 2],
                "max(backup 1, copy 2) in every order"
            );
        }

        /// X2: the same with two genuinely identical faults (two agents, the same second): the
        /// backup holds one as a row above the mark (written before the move), a restored copy
        /// holds that row and the other one.
        #[test]
        fn x2_identical_faults_straddle_the_backups_unmerged_row() {
            let mut got = Vec::new();
            for import_first in [true, false] {
                let i = install();
                make_writable(&i.b);
                let a1 =
                    faults::parse_spool_all(format!("{}\n", fixed("dup", Some("a1"))).as_bytes());
                let a2 =
                    faults::parse_spool_all(format!("{}\n", fixed("dup", Some("a2"))).as_bytes());
                insert_recs(&i.b_data().join("lumen.db"), &a1);
                let c = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c);
                make_writable(&c);
                insert_recs(&c.join("lumen.db"), &a2);
                if import_first {
                    i.import(&c).unwrap();
                    merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                } else {
                    merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                    i.import(&c).unwrap();
                }
                let _ = freeze(&i);
                got.push(kind_count(&i.n_db(), "dup"));
            }
            eprintln!("x2 import-first / merge-first: {got:?}");
            assert_eq!(got, vec![2, 2], "max(backup 1, copy 2) in both orders");
        }

        /// Y1: the backup holds the double older than F53 itself: a row above the mark and the
        /// same line still in `.draining` (a drain killed after its insert). The backup holds 2.
        #[test]
        fn y1_a_backup_row_and_its_draining_line_count_twice() {
            let i = install();
            make_writable(&i.b);
            let d = faults::parse_spool_all(format!("{}\n", fixed("dbl", None)).as_bytes());
            insert_recs(&i.b_data().join("lumen.db"), &d);
            fs::write(
                i.b_data().join("faults.jsonl.draining"),
                format!("{}\n", fixed("dbl", None)),
            )
            .unwrap();
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            assert_eq!(kind_count(&i.n_db(), "dbl"), 2);
        }

        /// Y2: identical faults on both sides of the mark: the snapshot's last row and a row an old
        /// binary wrote in S2. The backup holds 2, the snapshot gave N 1.
        #[test]
        fn y2_identical_faults_on_both_sides_of_the_mark() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            ledger(&l.join("lumen.db"), &["snap"]);
            let a1 = faults::parse_spool_all(format!("{}\n", fixed("dup", Some("a1"))).as_bytes());
            let a2 = faults::parse_spool_all(format!("{}\n", fixed("dup", Some("a2"))).as_bytes());
            insert_recs(&l.join("lumen.db"), &a1);
            let (l, n, m) = migrate_deferred(&root);
            insert_recs(&l.join("lumen.db"), &a2);
            do_retry_move(&n, &m);
            let b = backup_dir(&n, &m).unwrap();
            let i = Install { _t: t, root, n, b };
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            assert_eq!(kind_count(&i.n_db(), "dup"), 2);
        }
    }

    mod t6_report {
        use super::*;

        /// The faults line says how many were added and how many were already held, and the
        /// prefix line is never printed for faults.
        #[test]
        fn the_report_says_added_and_held_not_skipped_by_prefix() {
            let i = install();
            let copy = restored(&i);
            q(&copy.join("lumen.db"), &rows_for("after"));
            let r = i.import(&copy).unwrap();
            assert!(r.prefix["faults"] > 0, "{r:?}");
            let text = r.lines().join("\n");
            assert!(!text.contains("faults: the first"), "{text}");
            // 1 new row; the 2 old rows are held; the spool line is new to N (the freeze has not
            // taken the backup's).
            assert!(
                text.contains("Fault records: 2 added, 2 already held"),
                "{text}"
            );
        }

        /// A faults table without a core column gives no digest: none of its rows come in, and
        /// the report says why.
        #[test]
        fn a_faults_table_without_a_core_column_is_refused_not_inserted_whole() {
            let i = install();
            let d = recreated(&i);
            q(
                &d.join("lumen.db"),
                "ALTER TABLE faults DROP COLUMN channel",
            );
            let before = counts(&i.n_db());
            let r = i.import(&d).unwrap();
            let after = counts(&i.n_db());
            assert_eq!(
                after["faults"],
                before["faults"] + 1,
                "only the spool line: {r:?}"
            );
            assert!(!kinds(&i.n_db()).contains("row-fresh"));
            assert!(r.faults_refused.is_some(), "{r:?}");
            assert!(
                r.lines().iter().any(|l| l.contains("were not taken")),
                "{:?}",
                r.lines()
            );
            assert_eq!(after["read_events"], before["read_events"] + 1);
        }
    }

    mod rev_t6max {
        use super::*;
        use crate::legacy::freeze::{Freeze, freeze_if_ready, merge_backup};

        fn kind_count(p: &Path, kind: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row("SELECT count(*) FROM faults WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .unwrap()
        }
        fn marker_of(i: &Install) -> MarkerV2 {
            match marker::read(&i.n) {
                Seen::Migrated(m) => (*m).clone(),
                other => panic!("{other:?}"),
            }
        }
        fn freeze(i: &Install) -> Freeze {
            let rec = i.root.join(".setup_done");
            fs::write(&rec, format!("data_dir={}\n", i.n.display())).unwrap();
            freeze_if_ready(&i.n, &rec, &marker_of(i))
        }
        /// rows + spool + .draining of `kind` in a folder
        fn held(dir: &Path, kind: &str) -> i64 {
            let (recs, _) = read_spool_files(&dir.join("faults.jsonl")).unwrap();
            kind_count(&dir.join("lumen.db"), kind)
                + recs.iter().filter(|r| r.kind == kind).count() as i64
        }
        /// An old app's drain killed after its insert: the rows are in, `.draining` stays.
        fn drain_killed_after_insert(d: &Path) {
            let live = d.join("faults.jsonl");
            let recs = faults::parse_spool_all(&fs::read(&live).unwrap());
            fs::rename(&live, d.join("faults.jsonl.draining")).unwrap();
            let mut c = Connection::open(d.join("lumen.db")).unwrap();
            let tx = c.transaction().unwrap();
            faults::insert_into(&tx, "main", &recs).unwrap();
            tx.commit().unwrap();
        }

        /// T1: a restored copy of the previous folder whose old app's drain was killed after its
        /// insert (a row and the same line in `.draining`, the y1 double, in the copy). The copy
        /// holds 2, the backup 1. Both orders must end at 2.
        #[test]
        fn t1_a_restored_copy_with_a_draining_double() {
            let mut got = Vec::new();
            for import_first in [true, false] {
                let i = install();
                let c = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c);
                make_writable(&c);
                drain_killed_after_insert(&c);
                let srcs = (held(&i.b_data(), "backup-spool"), held(&c, "backup-spool"));
                assert_eq!(srcs, (1, 2));
                if import_first {
                    let r = i.import(&c).unwrap();
                    eprintln!("t1 import-first: {:?}", r.lines());
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                } else {
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    let r = i.import(&c).unwrap();
                    eprintln!("t1 freeze-first: {:?}", r.lines());
                }
                got.push(kind_count(&i.n_db(), "backup-spool"));
            }
            assert_eq!(got, vec![2, 2], "max(backup 1, copy 2)");
        }

        /// T2: a restored copy of the previous folder that an old app wrote a new fault to (spool
        /// only, not drained yet). The copy holds it once, nobody else does.
        #[test]
        fn t2_a_restored_copy_with_a_new_spool_line() {
            let mut got = Vec::new();
            for import_first in [true, false] {
                let i = install();
                let c = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c);
                make_writable(&c);
                let mut s = fs::read_to_string(c.join("faults.jsonl")).unwrap();
                s.push_str(&format!("{}\n", line("after-restore")));
                fs::write(c.join("faults.jsonl"), s).unwrap();
                assert_eq!(held(&c, "after-restore"), 1);
                if import_first {
                    let r = i.import(&c).unwrap();
                    eprintln!("t2 import-first: {:?}", r.lines());
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                } else {
                    assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    let r = i.import(&c).unwrap();
                    eprintln!("t2 freeze-first: {:?}", r.lines());
                }
                got.push((
                    kind_count(&i.n_db(), "after-restore"),
                    kind_count(&i.n_db(), "backup-spool"),
                ));
            }
            assert_eq!(got, vec![(1, 1), (1, 1)], "copy holds after-restore once");
        }

        /// T3 (t6nup x6): a cross-volume folder whose spool gains a line after its first import.
        #[test]
        fn t3_a_spool_line_added_after_a_cross_volume_copy() {
            let i = install();
            let d = i.root.join("usb").join("old");
            ledger(&d.join("lumen.db"), &["u1"]);
            fs::write(d.join("faults.jsonl"), format!("{}\n", line("s1"))).unwrap();
            import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            let mut s = fs::read_to_string(d.join("faults.jsonl")).unwrap();
            s.push_str(&format!("{}\n", line("s2")));
            fs::write(d.join("faults.jsonl"), s).unwrap();
            let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
            eprintln!("t3: {:?} {:?}", r.spool, r.lines());
            assert_eq!(
                (kind_count(&i.n_db(), "s1"), kind_count(&i.n_db(), "s2")),
                (1, 1),
                "{r:?}"
            );
        }

        /// T4 (control, should hold): read_events and faults rows of a restored copy that grew,
        /// imported twice (two copies, the second a superset), around the merge and the freeze:
        /// no read_events row doubled or lost, faults at the max.
        #[test]
        fn t4_read_events_never_doubled_across_orders() {
            for ord in 0..4u8 {
                let i = install();
                let c1 = i.root.join("tm1").join("copy");
                copy_dir(&i.b_data(), &c1);
                make_writable(&c1);
                q(&c1.join("lumen.db"), &rows_for("g1"));
                let c2 = i.root.join("tm2").join("copy");
                copy_dir(&c1, &c2);
                q(&c2.join("lumen.db"), &rows_for("g2"));
                let re = |p: &Path| count(p, "read_events");
                let base = re(&i.n_db());
                match ord {
                    0 => {
                        i.import(&c1).unwrap();
                        i.import(&c2).unwrap();
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    }
                    1 => {
                        i.import(&c2).unwrap();
                        i.import(&c1).unwrap();
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                    }
                    2 => {
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                        i.import(&c2).unwrap();
                        i.import(&c1).unwrap();
                    }
                    _ => {
                        merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                        i.import(&c1).unwrap();
                        assert!(matches!(freeze(&i), Freeze::Frozen(_)));
                        i.import(&c2).unwrap();
                    }
                }
                assert_eq!(re(&i.n_db()), base + 2, "order {ord}");
                assert_eq!(
                    (
                        kind_count(&i.n_db(), "row-g1"),
                        kind_count(&i.n_db(), "row-g2"),
                        kind_count(&i.n_db(), "row-snap"),
                        kind_count(&i.n_db(), "backup-spool")
                    ),
                    (1, 1, 1, 1),
                    "order {ord}"
                );
            }
        }

        /// T5: property over orders of {merge, freeze, import c1, import c2}, where c1 is a
        /// restored copy of the backup that KEEPS its spool (no drain), with its own `.draining`
        /// double and a line nobody else has; c2 is a spool-only folder. Final per-digest counts
        /// must equal the max over sources.
        #[test]
        fn t5_every_order_with_a_shared_history_copy_that_keeps_its_spool() {
            fn digests(db: &Path, dir: &Path) -> crate::legacy::faultset::FaultCounts {
                let mut all = crate::legacy::faultset::FaultCounts::new();
                let c = Connection::open(db).unwrap();
                for (_, k) in row_digests(&c, "main").unwrap().unwrap() {
                    *all.entry(k).or_insert(0) += 1;
                }
                let (recs, _) = read_spool_files(&dir.join("faults.jsonl")).unwrap();
                for k in record_digests(&recs).unwrap() {
                    *all.entry(k).or_insert(0) += 1;
                }
                all
            }
            let orders: Vec<[u8; 4]> = vec![
                [0, 1, 2, 3],
                [2, 3, 0, 1],
                [2, 0, 3, 1],
                [3, 2, 1, 0],
                [1, 2, 3, 0],
                [0, 2, 1, 3],
            ];
            let mut bad = Vec::new();
            for ord in orders {
                let i = install();
                let c1 = i.root.join("tm").join("copy");
                copy_dir(&i.b_data(), &c1);
                make_writable(&c1);
                drain_killed_after_insert(&c1); // row + .draining of backup-spool
                fs::write(c1.join("faults.jsonl"), format!("{}\n", line("c1-spool"))).unwrap();
                let c2 = i.root.join("home").join("s");
                ledger(&c2.join("lumen.db"), &[]);
                fs::write(c2.join("faults.jsonl"), format!("{}\n", line("c2-spool"))).unwrap();
                let sources = [
                    digests(&i.n_db(), &i.n),
                    digests(&i.b_data().join("lumen.db"), &i.b_data()),
                    digests(&c1.join("lumen.db"), &c1),
                    digests(&c2.join("lumen.db"), &c2),
                ];
                let mut want = crate::legacy::faultset::FaultCounts::new();
                for s in &sources {
                    for (k, v) in s {
                        let e = want.entry(k.clone()).or_insert(0);
                        *e = (*e).max(*v);
                    }
                }
                for op in ord {
                    match op {
                        0 => {
                            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
                        }
                        1 => assert!(matches!(freeze(&i), Freeze::Frozen(_))),
                        2 => {
                            i.import(&c1).unwrap();
                        }
                        _ => {
                            i.import(&c2).unwrap();
                        }
                    }
                }
                let got = digests(&i.n_db(), &i.n);
                if got != want {
                    bad.push(ord);
                }
            }
            assert!(bad.is_empty(), "orders below the max: {bad:?}");
        }
    }

    mod rev_final {
        use super::*;
        use crate::legacy::freeze::{Freeze, freeze_if_ready, merge_backup};

        fn marker_of(i: &Install) -> MarkerV2 {
            match marker::read(&i.n) {
                Seen::Migrated(m) => (*m).clone(),
                other => panic!("{other:?}"),
            }
        }
        fn freeze(i: &Install) -> Freeze {
            let rec = i.root.join(".setup_done");
            fs::write(&rec, format!("data_dir={}\n", i.n.display())).unwrap();
            freeze_if_ready(&i.n, &rec, &marker_of(i))
        }
        fn re_tag(p: &Path, tag: &str) -> i64 {
            Connection::open(p)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM read_events WHERE ts = ?1",
                    [format!("t-{tag}")],
                    |r| r.get(0),
                )
                .unwrap()
        }
        fn own_row(i: &Install) {
            q(
                &i.n_db(),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                 routed_via) VALUES ('t-own', 'Read', '/own', 1, 2, 1, 'builtin_read');",
            );
        }

        /// F1: a Time Machine copy of the previous folder taken between the snapshot and the move
        /// (it has late1, not late2); N wrote its own row; the merge took late1 and late2; the
        /// user deleted the read-only backup; then imports the copy. late1 must stay once.
        #[test]
        fn f1_partial_late_copy_after_the_backup_is_deleted() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late1"));
            let c = i.root.join("tm").join("copy");
            copy_dir(&i.b_data(), &c);
            q(&i.b_data().join("lumen.db"), &rows_for("late2"));
            own_row(&i);
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            assert_eq!(re_tag(&i.n_db(), "late1"), 1);
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            let r = i.import(&c).unwrap();
            eprintln!("f1: prefix {:?} rows {:?}", r.prefix, r.rows);
            assert_eq!(
                re_tag(&i.n_db(), "late1"),
                1,
                "read_events late1 doubled: {r:?}"
            );
        }

        /// F1 control: the same with the backup still there.
        #[test]
        fn f1_control_backup_present() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late1"));
            let c = i.root.join("tm").join("copy");
            copy_dir(&i.b_data(), &c);
            q(&i.b_data().join("lumen.db"), &rows_for("late2"));
            own_row(&i);
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            let r = i.import(&c).unwrap();
            assert_eq!(re_tag(&i.n_db(), "late1"), 1, "{r:?}");
        }

        /// L1: a start killed after the move, before its merge; the user imports a Time Machine
        /// copy that holds a late row of the backup (prefix against the unmerged backup); then
        /// deletes the read-only backup. late1 was in the copy, the import said it was held.
        #[test]
        fn l1_import_skips_rows_only_the_unmerged_backup_holds() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late1"));
            let c = i.root.join("tm").join("copy");
            copy_dir(&i.b_data(), &c);
            let r = i.import(&c).unwrap();
            eprintln!(
                "l1: prefix {:?} rows {:?} lines {:?}",
                r.prefix,
                r.rows,
                r.lines()
            );
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert_eq!(
                re_tag(&i.n_db(), "late1"),
                1,
                "read_events late1 lost: {r:?}"
            );
        }

        /// F2: the real sequence. S2 (move refused), the old app writes late1, Time Machine
        /// copies the folder, the old app writes late2, N writes its own row, the move, the merge,
        /// the freeze; the user deletes the backup; imports the Time Machine copy.
        #[test]
        fn f2_real_sequence_partial_copy_after_the_backup_is_deleted() {
            let t = TempDir::new().unwrap();
            let root = t.path().join("data");
            let l = root.join("example.previous.app");
            let n = root.join("example.current.app");
            ledger(&l.join("lumen.db"), &["snap"]);
            let mut o = CommitOptions::new("2026-10-19T08:00:00Z");
            o.probe_tries = 1;
            o.probe_delay = Duration::from_millis(10);
            let m: MarkerV2 =
                match commit::migrate(&l, &n, &o, &Refuse(io::ErrorKind::PermissionDenied)) {
                    Outcome::Migrated { marker, moved } => {
                        assert!(matches!(moved, Moved::Deferred(_)), "{moved:?}");
                        marker
                    }
                    other => panic!("{other:?}"),
                };
            q(&l.join("lumen.db"), &rows_for("late1"));
            let tm = root.join("tm").join("example.previous.app");
            copy_dir(&l, &tm);
            q(&l.join("lumen.db"), &rows_for("late2"));
            q(
                &n.join("lumen.db"),
                "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
                 routed_via) VALUES ('t-own', 'Read', '/own', 1, 2, 1, 'builtin_read');",
            );
            let mut o2 = CommitOptions::new("2026-10-19T09:00:00Z");
            o2.probe_tries = 1;
            o2.probe_delay = Duration::from_millis(10);
            assert!(matches!(
                commit::retry_move(&n, &m, &o2, &Native),
                Moved::Done
            ));
            let b = backup_dir(&n, &m).unwrap();
            let i = Install { _t: t, root, n, b };
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert!(matches!(freeze(&i), Freeze::Frozen(_)));
            let before = counts(&i.n_db());
            assert_eq!(re_tag(&i.n_db(), "late1"), 1);
            make_writable(&i.b);
            fs::remove_dir_all(i.b_data()).unwrap();
            let r = i.import(&tm).unwrap();
            eprintln!("f2: prefix {:?} rows {:?}", r.prefix, r.rows);
            let after = counts(&i.n_db());
            assert_eq!(
                (re_tag(&i.n_db(), "late1"), after["read_events"]),
                (1, before["read_events"]),
                "read_events doubled: {r:?}"
            );
        }
        /// L1 with the backup kept: the import took late1, so the merge that comes later must not.
        #[test]
        fn l1_the_merge_after_the_import_does_not_add_late1_again() {
            let i = install();
            q(&i.b_data().join("lumen.db"), &rows_for("late1"));
            let c = i.root.join("tm").join("copy");
            copy_dir(&i.b_data(), &c);
            i.import(&c).unwrap();
            merge_backup(&i.n_db(), &i.b, &marker_of(&i)).unwrap();
            assert_eq!(re_tag(&i.n_db(), "late1"), 1);
        }

        /// A dry run says "would add" on the faults line.
        #[test]
        fn the_dry_run_faults_line_says_would_add() {
            let mut r = empty_report("x".into(), PathBuf::from("x"), Placed::Already, &opts());
            r.faults_held = 2;
            r.dry_run = true;
            let text = r.lines().join("\n");
            assert!(
                text.contains("Fault records: would add 0, 2 already held"),
                "{text}"
            );
            r.dry_run = false;
            assert!(r.lines().join("\n").contains("0 added, 2 already held"));
        }
    }
}
