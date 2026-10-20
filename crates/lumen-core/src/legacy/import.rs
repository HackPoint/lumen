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
//!    ledger is probed again at its new place. A rename across volumes leaves the folder where it is
//!    and says so. This takes the old name out of the data root, and nothing can name it again.
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
//! 5. **One transaction** from the held connection, with N attached: the keyed tables with
//!    `INSERT OR IGNORE`, the new rows of the positional tables, the folder's spool and `.draining`
//!    lines (only when every prefix is 0: a folder that shares history keeps its spool), and the
//!    `('import:<k>', '$done', ..)` row that makes a second run say "already imported".
//! 6. **Finish:** the moved folder's files are made read-only.
//!
//! A run killed before step 3 changed nothing. Killed after it, `legacy-imports/<k>` has no `$done`
//! row, so a rerun on that path imports. Killed after step 5, the rerun says "already imported".
//! `--dry-run` runs the refusals, the probe and the prefix search, takes the same transaction and
//! rolls it back: it moves nothing and writes nothing.

use super::commit::{LockError, backup_dir, take_lock};
use super::discover::{Verdict, is_lumen_ledger};
use super::freeze::make_read_only;
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
    /// A rename across volumes: imported where it is. The reason is the OS's.
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
                "The folder is on another volume and stays where it is, {}: imported in place ({why})",
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
    let in_place_name = import_name(&imports, &src_real, &src.kind);
    let done_key = match &in_place_name {
        Some(k) => done_source(k),
        None => done_source(&src_real.to_string_lossy()),
    };
    if is_done(&n_db, &done_key) {
        return Err(ImportError::AlreadyImported(
            in_place_name.unwrap_or_else(|| src_real.display().to_string()),
        ));
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
    let out = import_under(n, &n_real, &src, in_place_name, &done_key, opts, ops);
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

#[allow(clippy::too_many_arguments)]
fn import_under(
    n: &Path,
    n_real: &Path,
    src: &Source,
    in_place_name: Option<String>,
    done_key: &str,
    opts: &ImportOptions,
    ops: &dyn Ops,
) -> Result<ImportReport, ImportError> {
    let n_db = n.join(LEDGER);
    let mut held = Some(take(&src.ledger, opts)?);

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
            let k = free_name(&imports, opts.date());
            dir = imports.join(&k);
            (k, Placed::Moved)
        }
        None => {
            let k = free_name(&imports, opts.date());
            let target = imports.join(&k);
            let made_imports = !imports.exists();
            fs::create_dir_all(&imports).map_err(|e| failed("legacy-imports", e))?;
            if cfg!(windows) {
                held = None; // an open handle in the folder refuses the rename
            }
            let moved = match src.kind {
                Kind::Folder => ops.rename(&src.path, &target),
                Kind::Shared => move_ledger_set(ops, &src.ledger, &target, true),
                Kind::Ledger => move_ledger_set(ops, &src.path, &target, false),
            };
            let placed = match moved {
                Ok(()) => {
                    sync_dir(&imports);
                    ledger = target.join(LEDGER);
                    dir = target;
                    actual = dir.clone();
                    Placed::Moved
                }
                // Imported where it is. For a set-aside ledger the new folder was not kept.
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    Placed::InPlace(e.to_string())
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
        return Ok(ImportReport {
            stopped: true,
            ..empty_report(name, dir, placed, opts)
        });
    }
    // After the move, so a refusal above leaves N as it was: the current schema, `legacy_merge`
    // included, whatever the install last ran.
    drop(crate::meter::connect_db(&n_db).map_err(|e| failed("cannot open the live ledger", e))?);
    let done_key = match placed {
        Placed::InPlace(_) => done_key.to_string(),
        Placed::Moved | Placed::Already => done_source(&name),
    };
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
    run_import(&place, src, held, opts).map_err(|e| match e {
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

/// `<date>-1`, `<date>-2`, ... the first name not yet in `imports`.
fn free_name(imports: &Path, date: &str) -> String {
    let mut k = 1u32;
    loop {
        let name = format!("{date}-{k}");
        if fs::symlink_metadata(imports.join(&name)).is_err() {
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

/// The originals already merged: `B/data/lumen.db` and every `legacy-imports/<k>` (or in-place
/// folder) with a `$done` row. Read-only connections, one per ledger, opened on demand.
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
        let k = s.strip_prefix("import:").unwrap_or(&s);
        let dir = if Path::new(k).is_absolute() {
            PathBuf::from(k)
        } else {
            n.join(IMPORTS_DIR).join(k)
        };
        // An in-place import of a ledger FILE records the file's own path.
        let db = if dir.is_file() { dir } else { dir.join(LEDGER) };
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
    (1..=cols.len()).rev().any(|k| {
        row_sig_in(src, "main", table, &cols[..k], mark)
            .ok()
            .flatten()
            .as_deref()
            == Some(sig)
    })
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
fn record_rows(tx: &rusqlite::Transaction<'_>, table: &str) -> Result<(), ImportError> {
    let cols = columns(tx, "main", table).map_err(|e| failed(table, e))?;
    if cols.is_empty() {
        return Ok(());
    }
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
        let added = tx
            .execute(
                &format!(
                    "{verb} INTO n.{t} ({list}) SELECT {list} FROM main.{t} \
                     WHERE rowid > ?1 ORDER BY rowid"
                ),
                params![from],
            )
            .map_err(|e| failed(table, e))?;
        if added > 0 {
            rows.insert(table.to_string(), added);
        }
    }
    let (spool, spool_sig) = match src.kind {
        Kind::Ledger => (Spool::NoneHere, None),
        Kind::Folder | Kind::Shared if shares_history => (Spool::Kept, None),
        Kind::Folder | Kind::Shared => {
            let spool = p.actual.join("faults.jsonl");
            let mut bytes = Vec::new();
            let mut recs = Vec::new();
            for f in [faults::draining_path(&spool), spool] {
                match fs::read(&f) {
                    Ok(b) => {
                        recs.extend(faults::parse_spool_all(&b));
                        bytes.extend_from_slice(&b);
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(failed(&format!("cannot read {}", f.display()), e)),
                }
            }
            faults::insert_into(&tx, "n", &recs).map_err(|e| failed("faults from the spool", e))?;
            (Spool::Taken(recs.len()), Some(hex(&bytes)))
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

    // 6. Finish: what moved is read-only. A folder imported in place is the user's, and is not touched.
    if matches!(rep.placed, Placed::Moved | Placed::Already)
        && let Err(e) = make_read_only(&p.actual)
    {
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
    fn a_folder_on_another_volume_is_imported_where_it_is() {
        let i = install();
        let before = counts(&i.n_db());
        let d = recreated(&i);
        let r = import(&i.n, &d, &opts(), &Refuse(io::ErrorKind::CrossesDevices)).unwrap();
        assert!(matches!(r.placed, Placed::InPlace(_)), "{r:?}");
        assert!(r.lines().join("\n").contains("on another volume"));
        assert!(d.join("lumen.db").is_file(), "it stayed");
        assert_eq!(count(&i.n_db(), "read_events"), before["read_events"] + 1);
        // A second run on the same place is recognised by where it is.
        let err = i.import(&d).unwrap_err();
        assert!(matches!(err, ImportError::AlreadyImported(_)), "{err:?}");
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
}
