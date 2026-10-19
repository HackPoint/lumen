//! What the app does about an older data directory, at start and after Setup.
//!
//! Only the app runs the migration (it knows the real data root, honours XDG
//! through `dirs::data_dir()`, and runs before anything opens a ledger or spawns
//! the daemon). Hooks, the CLI and the MCP server only resolve paths through
//! `lumen_core::legacy::effective`. Nothing here is fatal: every outcome that is
//! not "done" becomes a degradation with its reason, and a ledger that could not
//! move keeps being used where it is.
//!
//! Nothing here writes `~/.claude.json`; Setup does, through its own steps.

use lumen_core::legacy::{self, merge, migrate, retire, Marker, Retire, Source, Take, SOURCES};
use std::path::{Path, PathBuf};

/// Everything the upgrade needs to know about the machine. Real in the app, fake
/// identifiers and temp roots in tests.
pub struct Ctx {
    /// The directory that holds every application's data directory.
    pub root: PathBuf,
    pub new_id: String,
    pub old_id: String,
    /// `YYYY-MM-DD`, for the backup's name.
    pub date: String,
    /// The old and new log directories, where they are not inside the data directory.
    pub logs: Option<(PathBuf, PathBuf)>,
    /// Ledger-only sources (the generic `com.tauri.dev` bundle), already filtered to this system.
    pub ledger_only: Vec<Source>,
}

impl Ctx {
    /// The real machine. `None` when the platform has no data directory.
    pub fn current() -> Option<Ctx> {
        dirs::data_dir().map(Ctx::real)
    }

    fn real(root: PathBuf) -> Ctx {
        let new_id = lumen_core::meter::APP_ID;
        let old_id = legacy::LEGACY_APP_ID;
        let logs = match (retire::log_dir(old_id), retire::log_dir(new_id)) {
            (Some(o), Some(n)) => Some((o, n)),
            _ => None,
        };
        Ctx {
            root,
            new_id: new_id.into(),
            old_id: old_id.into(),
            date: today(),
            logs,
            ledger_only: ledger_only_sources(),
        }
    }

    pub fn old_dir(&self) -> PathBuf {
        self.root.join(&self.old_id)
    }

    pub fn new_dir(&self) -> PathBuf {
        self.root.join(&self.new_id)
    }

    pub fn new_db(&self) -> PathBuf {
        self.new_dir().join("lumen.db")
    }

    fn moving(&self) -> bool {
        self.new_id != self.old_id
    }

    /// The ledger to use now: the new one, or the old one while the migration is
    /// pending or failed. The same answer every other component gets.
    pub fn db_path(&self) -> PathBuf {
        legacy::effective_with(
            &self.new_db(),
            &self.new_id,
            &self.old_id,
            &[self.root.as_path()],
        )
    }
}

fn ledger_only_sources() -> Vec<Source> {
    SOURCES
        .iter()
        .filter(|s| matches!(s.take, Take::LedgerOnly) && retire::applies(s))
        .copied()
        .collect()
}

/// What happened, for [`crate::health::StartupHealth`] and the log.
#[derive(Debug, Default)]
pub struct Report {
    /// Each becomes a degradation: (step, reason).
    pub problems: Vec<(&'static str, String)>,
    /// Worth a log line, not a degradation.
    pub notes: Vec<String>,
}

impl Report {
    /// Log the notes and show every problem as a startup degradation.
    pub fn apply(&self, health: &crate::health::StartupHealth, phase: &str) {
        for note in &self.notes {
            log::warn!("{phase}: {note}");
        }
        for (step, why) in &self.problems {
            health.degrade(step, why);
        }
    }
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// The backup path a marker names (a bare name is relative to the data root).
fn backup_of(ctx: &Ctx, m: &Marker) -> Option<PathBuf> {
    if m.backup.is_empty() {
        return None;
    }
    let b = PathBuf::from(&m.backup);
    Some(if b.is_relative() { ctx.root.join(b) } else { b })
}

/// Run at start, after the log plugin and before anything opens a ledger:
/// migrate, retire, move the logs, import the ledger-only sources, merge late rows.
pub fn at_start(ctx: &Ctx, ops: &dyn retire::Ops) -> Report {
    let mut r = Report::default();
    let (old, new) = (ctx.old_dir(), ctx.new_dir());

    if ctx.moving() {
        if legacy::read_marker(&new).is_none() && is_real_dir(&old) {
            let backup = retire::backup_path(&ctx.root, &ctx.old_id, &ctx.date);
            let opts = migrate::Options::new(&ctx.old_id, &backup.to_string_lossy(), Take::Dir);
            match migrate::migrate(&old, &new, &opts) {
                migrate::Outcome::Migrated(m) => r.notes.push(format!(
                    "migrated {} into {} (backup {})",
                    old.display(),
                    new.display(),
                    m.backup
                )),
                migrate::Outcome::Deferred { reason } => r.problems.push((
                    "migration",
                    format!(
                        "deferred, the old data folder stays in use until the next start: {reason}"
                    ),
                )),
                migrate::Outcome::Failed { step, error } => r.problems.push((
                    "migration",
                    format!(
                        "failed at {step}, the old data folder stays in use and is untouched: {error}"
                    ),
                )),
                migrate::Outcome::NothingToDo => {}
            }
        }
        match legacy::read_marker(&new) {
            None => {}
            Some(Err(e)) => r.problems.push((
                "migration",
                format!("{} does not parse: {e}", legacy::MARKER_NAME),
            )),
            Some(Ok(m)) => {
                if let Some(backup) = backup_of(ctx, &m) {
                    match retire::retire(&old, &backup, ops) {
                        retire::Retired::Done => r.notes.push(format!(
                            "retired {} to {}",
                            old.display(),
                            backup.display()
                        )),
                        retire::Retired::AlreadyDone | retire::Retired::Nothing => {}
                        retire::Retired::Deferred(why) => r
                            .problems
                            .push(("migration", format!("retire deferred: {why}"))),
                        retire::Retired::LinkPending(why) => r.problems.push((
                            "migration",
                            format!("the old path is not linked yet: {why}"),
                        )),
                    }
                }
                if let Some((old_logs, new_logs)) = &ctx.logs {
                    let lr = retire::migrate_logs(old_logs, new_logs, &ctx.date, ops);
                    if let Some(e) = lr.error {
                        r.problems
                            .push(("migration", format!("logs not moved: {e}")));
                    }
                    if let Some(why) = lr.not_renamed {
                        r.notes.push(format!("old log folder left in place: {why}"));
                    }
                }
            }
        }
    }

    // The generic bundle's ledger, only while there is no ledger of ours and no
    // older directory still waiting to move (that would split the history).
    let waiting = ctx.moving() && legacy::read_marker(&new).is_none() && is_real_dir(&old);
    if !waiting && !ctx.new_db().exists() {
        for s in &ctx.ledger_only {
            if !retire::applies(s) || !matches!(s.retire, Retire::LeaveInPlace) {
                continue;
            }
            match retire::import_ledger_only(s, &ctx.root, &ctx.new_db()) {
                Ok(retire::LedgerCopy::Copied { rows }) => {
                    r.notes.push(format!("imported {rows} rows from {}", s.id))
                }
                Ok(retire::LedgerCopy::Busy) => r.problems.push((
                    "migration",
                    format!(
                        "the ledger in {} is in use; its history comes in at a later start",
                        s.id
                    ),
                )),
                Ok(_) => {}
                Err(e) => r.problems.push((
                    "migration",
                    format!(
                        "could not copy the ledger from {}: {e}. History from before the rename \
                         stays in the old file; this starts an empty ledger.",
                        s.id
                    ),
                )),
            }
            if ctx.new_db().exists() {
                break;
            }
        }
    }

    let m = merge_again(ctx);
    r.problems.extend(m.problems);
    r.notes.extend(m.notes);
    r
}

/// Carry over rows an old binary wrote after the migration, from the backup and
/// from the old directory if something re-created it. Run at every start and
/// again after Setup (which is what stops old shims writing there).
pub fn merge_again(ctx: &Ctx) -> Report {
    let mut r = Report::default();
    if !ctx.moving() {
        return r;
    }
    let new_db = ctx.new_db();
    let marker = match legacy::read_marker(&ctx.new_dir()) {
        Some(Ok(m)) => m,
        Some(Err(_)) | None => return r, // at_start reports an unreadable marker
    };
    if !new_db.exists() {
        return r;
    }
    let mut owned: Vec<PathBuf> = Vec::new();
    if let Some(b) = backup_of(ctx, &marker) {
        owned.push(b.join("lumen.db"));
    }
    let old = ctx.old_dir();
    if is_real_dir(&old) {
        owned.push(old.join("lumen.db"));
    }
    let refs: Vec<&Path> = owned.iter().map(PathBuf::as_path).collect();
    match merge::merge_late(&new_db, &refs, &marker) {
        Ok(rep) => {
            if rep.rows() > 0 {
                r.notes.push(format!(
                    "merged {} rows written to the old data folder after the move",
                    rep.rows()
                ));
            }
            for s in rep.sources {
                if let Some(why) = s.skipped {
                    if !(why.starts_with("missing") || why.starts_with("same file")) {
                        r.problems
                            .push(("migration", format!("{}: {why}", s.path.display())));
                    }
                }
            }
        }
        Err(e) => r
            .problems
            .push(("migration", format!("late rows not merged: {e}"))),
    }
    r
}

/// Today's date, UTC, as `YYYY-MM-DD` (core has no date crate).
pub fn today() -> String {
    // A clock before 1970 is a negative count, not a missing one.
    let secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    ymd(secs.div_euclid(86_400))
}

/// Days since 1970-01-01 as `YYYY-MM-DD` (Howard Hinnant's civil-from-days).
fn ymd(days: i64) -> String {
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
    format!("{y:04}-{m:02}-{d:02}")
}

/// `retire::Ops` for the app: the default rename, and a junction on Windows,
/// where a symlink needs a privilege most users lack. The `junction` crate is
/// this crate's alone; it never enters lumen-core or the hook path.
pub struct AppOps;

impl retire::Ops for AppOps {
    #[cfg(windows)]
    fn link_dir(&self, link: &Path, target: &Path) -> std::io::Result<()> {
        junction::create(target, link)
    }
    #[cfg(not(windows))]
    fn link_dir(&self, link: &Path, target: &Path) -> std::io::Result<()> {
        retire::Native.link_dir(link, target)
    }
}

// Written beside v1 and not wired yet: the start-up switch-over calls it.
#[allow(dead_code)]
pub mod autostart;

#[cfg(test)]
mod tests;
