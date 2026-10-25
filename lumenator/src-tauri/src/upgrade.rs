//! What the app does about a previous install, at start and after Setup.
//!
//! The previous install is found by what it holds (`lumen_core::legacy::discover`), never by its
//! folder's name. Only the app runs the migration (it knows the real data roots, honours XDG
//! through `dirs`, and runs before anything opens a ledger or spawns the daemon). Hooks, the CLI
//! and the MCP server only resolve paths through `lumen_core::legacy::redirect`. Nothing here is
//! fatal: every outcome that is not "done" becomes a degradation with its reason, and a ledger
//! that could not move keeps being used where it is.
//!
//! [`at_start`] works the state out from the disk, under the migration lock, in the order of the
//! design (F53 v2 §5):
//!
//! 1. no marker: discover; nothing found settles the install (`none` marker, never discovered
//!    again), a folder found is migrated and moved;
//! 2. a committed marker: tidy, retry a deferred move, re-point Lumen's entries in Claude's
//!    files and the CLI link, move the satellites and login entries, report a re-created
//!    folder, then merge the backup's rows until Setup has re-registered and freeze it once.
//!
//! Setup re-registers through `setup::lumen_run_setup`, which calls [`freeze_after_setup`].

use crate::setup::{self, repoint, AutoStart};
use lumen_core::legacy::commit::{self, CommitOptions, LockError, Moved, DATA_DIR};
use lumen_core::legacy::discover::{self, Verdict};
use lumen_core::legacy::freeze::{self, Freeze};
use lumen_core::legacy::marker::{self, MarkerV2, Seen};
use lumen_core::legacy::redirect::{redirect_in, MarkerCache};
use lumen_core::legacy::retire::{self, Native, Ops};
use lumen_core::legacy::satellites::{self, Platform, Roots};
use lumen_core::legacy::{self, Retire, Source, SOURCES};
use std::path::{Path, PathBuf};

pub mod autostart;

use autostart::{Env as AutostartEnv, Exe, RunKey};

/// Everything the upgrade needs to know about the machine. Real in the app, temp folders and
/// neutral names in tests.
pub struct Ctx<'a> {
    /// The new data folder, N. It sits directly in one of `roots`.
    pub n: PathBuf,
    pub new_id: String,
    /// The data roots, scanned one level deep.
    pub roots: Vec<PathBuf>,
    pub home: PathBuf,
    pub platform: Platform,
    pub satellite_roots: Roots,
    /// Ledger-only sources (the generic `com.tauri.dev` bundle), already filtered to this system.
    pub ledger_only: Vec<Source>,
    pub autostart: &'a dyn AutoStart,
    /// `~/Library/LaunchAgents` or the XDG autostart folder. `None` on Windows.
    pub autostart_dir: Option<PathBuf>,
    /// The Run key. Windows only.
    pub run_key: Option<&'a dyn RunKey>,
    /// The running executable, and `$APPIMAGE` standing in for it. `None` when the system cannot
    /// say which it is: login entries are found by the binary they run, so none is touched.
    pub exe: Option<String>,
    pub appimage: Option<String>,
    /// The product name, which the autostart plugin gives its entry.
    pub plugin_name: String,
    pub ops: &'a dyn Ops,
    /// This build's `lumen-mcp`, looked up only when something has to be re-pointed.
    pub mcp_bin: &'a dyn Fn() -> Option<String>,
    /// Copies the previous app's menu-bar position into this app's preferences, given the previous
    /// folder's name (its bundle id); a note per key carried. Real only in the app, on macOS.
    pub status_item_carry: &'a dyn Fn(&str) -> Vec<String>,
    /// The `lumen` link and the `lumen-cli` it should name.
    pub cli_link: Option<PathBuf>,
    pub cli_bin: Option<PathBuf>,
    pub commit: CommitOptions,
}

impl<'a> Ctx<'a> {
    /// The real machine. `None` when there is no home.
    pub fn real(
        autostart: &'a dyn AutoStart,
        run_key: Option<&'a dyn RunKey>,
        mcp_bin: &'a dyn Fn() -> Option<String>,
    ) -> Option<Ctx<'a>> {
        dirs::home_dir().and_then(|home| Self::real_in(home, autostart, run_key, mcp_bin))
    }

    /// [`Ctx::real`] for a given home. N is `setup::app_support_dir_in(home)`, the one function
    /// the Setup prompt, the health check, `lumen_run_setup` and the metering writers all use:
    /// were any of them to read `dirs::data_dir()` (which follows `XDG_DATA_HOME`, or a
    /// redirected Roaming AppData), the prompt would look for the marker in another folder than
    /// the start wrote it in.
    pub fn real_in(
        home: PathBuf,
        autostart: &'a dyn AutoStart,
        run_key: Option<&'a dyn RunKey>,
        mcp_bin: &'a dyn Fn() -> Option<String>,
    ) -> Option<Ctx<'a>> {
        let platform = Platform::current();
        let now = now_utc();
        Some(Ctx {
            n: setup::app_support_dir_in(&home),
            new_id: lumen_core::meter::APP_ID.into(),
            roots: legacy::data_roots(Some(&home)),
            home,
            commit: CommitOptions::new(&now),
            platform,
            satellite_roots: Roots::current(),
            ledger_only: ledger_only_sources(),
            autostart,
            autostart_dir: autostart::real_dir(platform),
            run_key,
            exe: match std::env::current_exe() {
                Ok(p) => Some(p.to_string_lossy().into_owned()),
                Err(e) => {
                    log::warn!("cannot tell which executable this is: {e}");
                    None
                }
            },
            appimage: std::env::var("APPIMAGE").ok(),
            plugin_name: "Lumen".into(),
            ops: &Native,
            mcp_bin,
            status_item_carry: &crate::health::carry_status_item_position,
            cli_link: Some(setup::cli_symlink_path()),
            cli_bin: setup::find_binary("lumen-cli"),
        })
    }

    pub fn new_db(&self) -> PathBuf {
        self.n.join("lumen.db")
    }

    /// The ledger to use now: N's, or the previous folder's while the migration is pending or
    /// failed (the pending file says where). The same answer every other component gets.
    pub fn db_path(&self) -> PathBuf {
        let roots: Vec<&Path> = self.roots.iter().map(PathBuf::as_path).collect();
        redirect_in(&self.new_db(), &self.new_id, &roots, &MarkerCache::new())
    }

    fn setup_done(&self) -> PathBuf {
        setup::marker_path_in(&self.home)
    }
}

fn ledger_only_sources() -> Vec<Source> {
    SOURCES
        .iter()
        .filter(|s| retire::applies(s) && matches!(s.retire, Retire::LeaveInPlace))
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

    fn problem(&mut self, why: String) {
        self.problems.push(("migration", why));
    }
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// The sentence that tells a person how to bring a Lumen folder in by hand.
fn import_hint(dir: &Path) -> String {
    format!(
        "Nothing from it was merged. Quit the app that uses it, then run: lumen import-ledger \"{}\"",
        dir.display()
    )
}

/// Run at start, after the log plugin and before anything opens a ledger.
pub fn at_start(ctx: &Ctx) -> Report {
    let mut r = Report::default();
    // A settled install has nothing left to do and takes no lock.
    if matches!(marker::read(&ctx.n), Seen::Settled) {
        return r;
    }
    let lock = match commit::take_lock(&ctx.n) {
        Ok(l) => l,
        Err(LockError::Held) => {
            r.problem(
                "another Lumen is migrating; this start uses the state as it finds it".into(),
            );
            return r;
        }
        Err(LockError::Io(e)) => {
            r.problem(format!(
                "cannot take the migration lock in {}: {e}",
                ctx.n.display()
            ));
            return r;
        }
    };
    // Read again under the lock: another start may have just finished.
    match marker::read(&ctx.n) {
        Seen::Settled => {}
        Seen::Bad => r.problem(format!(
            "{} does not parse; nothing was migrated or merged",
            legacy::MARKER_NAME
        )),
        Seen::V1 { source } => r.problem(format!(
            "a pre-release migration marker names the folder {source}; its leftovers (a link at \
             the old path, a .migrated folder) were not touched"
        )),
        Seen::Migrated(m) => committed(ctx, &lock, &m, None, &mut r),
        Seen::Absent => first_start(ctx, &lock, &mut r),
    }
    r
}

/// No marker yet: discover, then migrate and move, or settle.
fn first_start(ctx: &Ctx, lock: &commit::Lock, r: &mut Report) {
    let hints = discover::hints_in(&ctx.home);
    let roots: Vec<&Path> = ctx.roots.iter().map(PathBuf::as_path).collect();
    let found = discover::discover(&roots, &ctx.new_id, &hints);
    for dir in &found.others {
        r.problem(format!(
            "Another Lumen folder was found at {} and not migrated. {}",
            dir.display(),
            import_hint(dir)
        ));
    }
    for dir in &found.unknown {
        r.problem(format!(
            "could not tell whether {} holds a Lumen ledger; this install is not settled, and the \
             next start looks again",
            dir.display()
        ));
    }
    if let Some(chosen) = &found.chosen {
        // Before anything can fail: from here until the commit, every component must resolve
        // the ledger to the previous folder, or a new empty one would be started beside it.
        let pending = marker::Pending {
            source: chosen.name.clone(),
            dir: std::path::absolute(&chosen.dir).unwrap_or_else(|_| chosen.dir.clone()),
        };
        if let Err(e) = marker::write_pending(&ctx.n, &pending) {
            r.problem(format!(
                "cannot record where the previous data folder is ({e}); nothing was migrated"
            ));
            return;
        }
        let mut opts = ctx.commit.clone();
        opts.others = found
            .others
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        match commit::migrate_under(lock, &chosen.dir, &ctx.n, &opts, ctx.ops) {
            commit::Outcome::Migrated { marker, moved } => {
                r.notes.push(format!(
                    "migrated {} into {}",
                    chosen.dir.display(),
                    ctx.n.display()
                ));
                committed(ctx, lock, &marker, Some(moved), r);
            }
            commit::Outcome::Deferred { reason } => r.problem(format!(
                "deferred, the previous data folder stays in use until the next start: {reason}"
            )),
            commit::Outcome::Failed { step, error } => r.problem(format!(
                "failed at {step}, the previous data folder stays in use and is untouched: {error}"
            )),
            commit::Outcome::NothingToDo => {}
        }
        return;
    }
    if !found.unknown.is_empty() {
        return;
    }
    // Nothing found: the generic bundle's ledger, then settle so no folder that appears later is
    // ever migrated over the live ledger.
    import_ledger_only(ctx, r);
    if let Err(e) = marker::write_settled(&ctx.n) {
        r.problem(format!(
            "could not record a fresh install in {}: {e}",
            ctx.n.display()
        ));
    }
}

/// The generic bundle's ledger, only while there is no ledger of ours.
fn import_ledger_only(ctx: &Ctx, r: &mut Report) {
    if ctx.new_db().exists() {
        return;
    }
    for s in &ctx.ledger_only {
        let Some(root) = ctx.roots.first() else {
            return;
        };
        match retire::import_ledger_only(s, root, &ctx.new_db()) {
            Ok(retire::LedgerCopy::Copied { rows }) => {
                r.notes.push(format!("imported {rows} rows from {}", s.id));
            }
            Ok(retire::LedgerCopy::Busy) => r.problem(format!(
                "the ledger in {} is in use; its history comes in at a later start",
                s.id
            )),
            Ok(_) => {}
            Err(e) => r.problem(format!(
                "could not copy the ledger from {}: {e}. History from before the rename stays \
                 in the old file; this starts an empty ledger.",
                s.id
            )),
        }
        if ctx.new_db().exists() {
            return;
        }
    }
}

/// A committed migration: S2, S3 or S4 of the design, every start. `moved` is what the move
/// did when the commit was made in this very start.
fn committed(ctx: &Ctx, lock: &commit::Lock, m: &MarkerV2, moved: Option<Moved>, r: &mut Report) {
    let Some(b) = commit::backup_dir(&ctx.n, m) else {
        r.problem("the marker names no backup folder".into());
        return;
    };
    tidy(ctx, &b, r);

    // S4 with the backup deleted by hand: a folder at the previous path is a re-created one,
    // never a move still to be made. Nothing moves after the freeze.
    let frozen = freeze::is_frozen(&ctx.new_db(), &b);
    // S2: the move, or its retry.
    let mv = match moved {
        Some(mv) => mv,
        None if frozen => Moved::Already,
        None => commit::retry_move_under(lock, &ctx.n, m, &ctx.commit, ctx.ops),
    };
    let mut moved_now = false;
    match mv {
        Moved::Done => {
            moved_now = true;
            r.notes.push(format!(
                "moved {} into {}",
                m.source_dir,
                b.join(DATA_DIR).display()
            ));
        }
        Moved::Deferred(why) => r.problem(format!(
            "the previous folder could not be moved into the backup yet, tried again at the next \
             start: {why}"
        )),
        // S2 with nothing to move: the folder was deleted between the commit and the move, and
        // whatever it held is lost. That is said as a problem, not a note.
        Moved::Gone => r.problem(format!(
            "the previous folder {} is gone before it was moved: nothing to merge",
            m.source_dir
        )),
        Moved::Already | Moved::Recreated => {}
    }
    let data = b.join(DATA_DIR);
    let mcp = (ctx.mcp_bin)();

    // The window in which the shims on disk still bake the previous folder is the rename itself.
    if moved_now {
        if let Some(mcp) = &mcp {
            let db = ctx.new_db().to_string_lossy().into_owned();
            if let Some(what) = setup::ensure_scripts_fresh_in(&ctx.home, &db, mcp) {
                r.notes.push(format!("hook scripts: {what}"));
            }
        }
    }

    repoint_entries(ctx, m, mcp.as_deref(), r);

    if data.is_dir() || frozen {
        // First: the previous app's preferences file is one of the satellites, and the menu-bar
        // position is read out of its domain before it moves into the backup.
        r.notes.extend((ctx.status_item_carry)(&m.source));
        move_satellites(ctx, m, &b, r);
        settle_login_entries(ctx, &b, r);
        recreated_folder(ctx, m, &b, r);
    }

    merge_and_freeze(ctx, lock, m, &b, r);
    report_standing(ctx, m, r);
}

/// Remove what a finished or killed commit leaves: the pending file, the stage and its intent, and
/// empty backup folders the marker does not name (`remove_dir` only).
fn tidy(ctx: &Ctx, b: &Path, r: &mut Report) {
    // A leftover never fails the start; it is said, and removed at the next one.
    if let Err(e) = marker::remove_pending(&ctx.n) {
        r.notes
            .push(format!("could not remove the pending file: {e}"));
    }
    let stage = ctx.n.join(legacy::migrate::STAGE_DIR);
    if stage.exists() {
        if let Err(e) = std::fs::remove_dir_all(&stage) {
            r.notes
                .push(format!("could not remove {}: {e}", stage.display()));
        }
    }
    let Some(holder) = b.parent() else {
        return;
    };
    let entries = match std::fs::read_dir(holder) {
        Ok(entries) => entries,
        Err(e) => {
            r.notes
                .push(format!("could not look in {}: {e}", holder.display()));
            return;
        }
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // `remove_dir` only: a folder that holds anything is somebody's backup and stays.
        if name.starts_with("legacy-backup-")
            && e.path() != b
            && std::fs::remove_dir(e.path()).is_ok()
        {
            r.notes.push(format!(
                "removed the empty backup folder {}",
                e.path().display()
            ));
        }
    }
}

/// Re-point Lumen's entries in Claude's files and the CLI link.
fn repoint_entries(ctx: &Ctx, m: &MarkerV2, mcp: Option<&str>, r: &mut Report) {
    match mcp {
        Some(mcp) => {
            let rp = repoint::repoint_lumen_entries_in(&ctx.home, m, mcp, &ctx.new_db());
            for f in rp.rewritten {
                r.notes.push(format!("re-pointed Lumen's entries in {f}"));
            }
            for p in rp.problems {
                r.problem(format!("Claude's files were not re-pointed: {p}"));
            }
        }
        None => r.problem(
            "lumen-mcp was not found, so Lumen's entries in Claude's files were not re-pointed"
                .into(),
        ),
    }
    if let Some(link) = &ctx.cli_link {
        match repoint::repoint_cli_link(link, m, &ctx.n, ctx.cli_bin.as_deref()) {
            Ok(true) => r
                .notes
                .push(format!("re-pointed the CLI link {}", link.display())),
            Ok(false) => {}
            Err(why) => r.problem(why),
        }
    }
}

fn move_satellites(ctx: &Ctx, m: &MarkerV2, b: &Path, r: &mut Report) {
    let rep = satellites::move_satellites(
        ctx.platform,
        &ctx.satellite_roots,
        &m.source,
        &ctx.new_id,
        b,
        ctx.ops,
    );
    for mv in rep.moved {
        r.notes.push(format!(
            "moved {} to {}{}",
            mv.from.display(),
            mv.to.display(),
            if mv.fallback {
                " (another volume: beside it, in the new folder)"
            } else {
                ""
            }
        ));
    }
    for left in rep.left {
        r.problem(format!(
            "{} was left where it is ({}); the next start tries again",
            left.from.display(),
            left.why
        ));
    }
}

fn settle_login_entries(ctx: &Ctx, b: &Path, r: &mut Report) {
    let Some(exe) = ctx.exe.as_deref() else {
        r.problem("login entries were not looked at: this executable's path is unknown".into());
        return;
    };
    let marker = setup::autostart_marker_in(&ctx.home);
    let env = AutostartEnv {
        platform: ctx.platform,
        exe: Exe {
            path: exe,
            appimage: ctx.appimage.as_deref(),
        },
        plugin_name: &ctx.plugin_name,
        dir: ctx.autostart_dir.as_deref(),
        run_key: ctx.run_key,
        plugin: ctx.autostart,
        marker: &marker,
        backup: b,
        new_id: &ctx.new_id,
        ops: ctx.ops,
    };
    let out = autostart::adopt_other_entries(&env);
    for name in out.moved {
        r.notes
            .push(format!("moved the login entry {name} into the backup"));
    }
    if out.enabled_plugin {
        r.notes
            .push("turned Lumen's own login item on, as the entry it replaces was on".into());
    }
    for p in out.problems {
        r.problem(p);
    }
}

/// The previous folder is a real directory again while its backup exists: an old binary wrote
/// there after the move.
fn recreated_folder(ctx: &Ctx, m: &MarkerV2, b: &Path, r: &mut Report) {
    let l = PathBuf::from(&m.source_dir);
    if !is_real_dir(&l) {
        return;
    }
    let empty = std::fs::read_dir(&l)
        .map(|mut d| d.next().is_none())
        .unwrap_or(false);
    if empty {
        match std::fs::remove_dir(&l) {
            Ok(()) => r
                .notes
                .push(format!("removed the empty folder {}", l.display())),
            Err(e) => r.problem(format!("could not remove the empty {}: {e}", l.display())),
        }
        return;
    }
    match discover::is_lumen_ledger(&l.join("lumen.db")) {
        Verdict::Lumen => r.problem(format!(
            "An older Lumen wrote to {} after the move. {}",
            l.display(),
            import_hint(&l)
        )),
        Verdict::Unknown(why) => r.problem(format!(
            "{} came back after the move and cannot be read yet ({why}); it is looked at again at \
             the next start",
            l.display()
        )),
        Verdict::NotLumen => {
            let label = (1u32..)
                .map(|k| format!("recreated-{k}"))
                .find(|n| !b.join(n).exists())
                .unwrap_or_else(|| "recreated".into());
            match satellites::move_into_backup(
                &l,
                Path::new(&label),
                b,
                &ctx.new_id,
                false,
                ctx.ops,
            ) {
                Ok(Some(mv)) => r.notes.push(format!(
                    "{} came back without a ledger; moved to {}",
                    l.display(),
                    mv.to.display()
                )),
                Ok(None) => {}
                Err(why) => r.problem(format!(
                    "{} came back without a ledger and is in use ({why}); the next start tries again",
                    l.display()
                )),
            }
        }
    }
}

/// S3: merge the backup's rows at every start until the freeze; the freeze itself once Setup has
/// recorded N. S4: nothing is merged, the stamp and the read-only bits are made sure of.
fn merge_and_freeze(ctx: &Ctx, lock: &commit::Lock, m: &MarkerV2, b: &Path, r: &mut Report) {
    match freeze::freeze_if_ready_under(lock, &ctx.n, &ctx.setup_done(), m) {
        Freeze::NotReady(_) => {
            if b.join(DATA_DIR).join("lumen.db").is_file() {
                merge_rows(ctx, m, b, r);
            }
        }
        Freeze::Frozen(rep) => r.notes.push(format!(
            "froze the backup after Setup: {} rows and {} spool records taken",
            rep.total_rows(),
            rep.spool_records
        )),
        Freeze::Already { changed } => {
            if changed {
                r.problem(format!(
                    "the backup's ledger changed after it was frozen; nothing from it was merged ({})",
                    b.display()
                ));
            }
        }
        Freeze::Rewound(tables) => r.problem(format!(
            "the backup was replaced (rewound tables: {}); nothing more was merged from it",
            tables.join(", ")
        )),
        Freeze::Failed(why) => r.problem(format!("the backup was not frozen: {why}")),
    }
}

fn merge_rows(ctx: &Ctx, m: &MarkerV2, b: &Path, r: &mut Report) {
    match freeze::merge_backup(&ctx.new_db(), b, m) {
        Ok(rep) => {
            if rep.total_rows() > 0 {
                r.notes.push(format!(
                    "merged {} rows written to the previous folder after the move",
                    rep.total_rows()
                ));
            }
            if !rep.rewound.is_empty() {
                r.problem(format!(
                    "the backup was replaced (rewound tables: {}); nothing more was merged from it",
                    rep.rewound.join(", ")
                ));
            }
        }
        Err(e) => r.problem(format!("late rows were not merged: {e}")),
    }
}

/// What stays true at every start and is worth showing until someone acts on it.
fn report_standing(ctx: &Ctx, m: &MarkerV2, r: &mut Report) {
    for other in &m.others {
        let dir = PathBuf::from(other);
        if is_real_dir(&dir) {
            r.problem(format!(
                "Another Lumen folder at {other} was not migrated. {}",
                import_hint(&dir)
            ));
        }
    }
    if let Ok(entries) = std::fs::read_dir(&ctx.n) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("lumen.db.pre-legacy")
                && !name.ends_with("-wal")
                && !name.ends_with("-shm")
                && !name.ends_with("-journal")
            {
                r.problem(format!(
                    "a ledger that was in the new folder was set aside as {}. {}",
                    e.path().display(),
                    import_hint(&e.path())
                ));
            }
        }
    }
}

/// After Setup: freeze the backup if the gate allows it. The gate is "Setup has recorded N",
/// which `lumen_run_setup` has just made true when every step went through.
pub fn freeze_after_setup(home: &Path, n: &Path) -> Report {
    let mut r = Report::default();
    let m = match marker::read(n) {
        Seen::Migrated(m) => m,
        _ => return r,
    };
    let Some(b) = commit::backup_dir(n, &m) else {
        return r;
    };
    match freeze::freeze_if_ready(n, &setup::marker_path_in(home), &m) {
        Freeze::Frozen(rep) => r.notes.push(format!(
            "froze the backup after Setup: {} rows and {} spool records taken",
            rep.total_rows(),
            rep.spool_records
        )),
        Freeze::Rewound(tables) => r.problem(format!(
            "the backup was replaced (rewound tables: {}); nothing was merged or frozen",
            tables.join(", ")
        )),
        Freeze::Failed(why) => r.problem(format!("the backup was not frozen: {why}")),
        Freeze::Already { changed: true } => r.problem(format!(
            "the backup's ledger changed after it was frozen ({})",
            b.display()
        )),
        Freeze::NotReady(_) | Freeze::Already { changed: false } => {}
    }
    r
}

/// Now, UTC, as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now_utc() -> String {
    // A clock before 1970 is a negative count, not a missing one.
    let secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let day = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        ymd(day),
        tod / 3600,
        tod % 3600 / 60,
        tod % 60
    )
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

#[cfg(test)]
mod tests;
