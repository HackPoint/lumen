//! The start-up wiring end to end, on temp folders and a neutral name for the previous install.
//! The previous folder is never found by its name: every test here builds one the way an old
//! release left it, and the start finds it by what it holds.

use super::*;
use crate::health::StartupHealth;
use lumen_core::legacy::commit::backup_dir;
use lumen_core::legacy::{Os, Take, MARKER_NAME};
use rusqlite::Connection;
use std::cell::Cell;
use std::time::Duration;
use tempfile::TempDir;

const NEW: &str = "fake.new.app";
const PREV: &str = "example.previous.app";
const NOW: &str = "2026-10-18T08:00:00Z";
const DATE: &str = "2026-10-18";

/// The plugin's login item, as a seam.
struct Login(Cell<bool>);

impl AutoStart for Login {
    fn is_enabled(&self) -> Result<bool, String> {
        Ok(self.0.get())
    }
    fn enable(&self) -> Result<(), String> {
        self.0.set(true);
        Ok(())
    }
    fn disable(&self) -> Result<(), String> {
        self.0.set(false);
        Ok(())
    }
}

/// Refuses every rename while `refuse` is set, as a scanner or a sharing violation would.
struct Gate {
    refuse: Cell<bool>,
}

impl Ops for Gate {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        if self.refuse.get() {
            return Err(std::io::Error::other("refused"));
        }
        std::fs::rename(from, to)
    }
}

fn generic_source() -> Source {
    // The real generic id: discovery skips it by name, and the tests supply it as already
    // filtered to this system.
    SOURCES[0]
}

fn mcp() -> Option<String> {
    Some("/opt/lumen/lumen-mcp".into())
}

fn ctx<'a>(root: &Path, login: &'a Login, ops: &'a dyn Ops) -> Ctx<'a> {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut commit = CommitOptions::new(NOW);
    commit.probe_tries = 2;
    commit.probe_delay = Duration::from_millis(20);
    commit.move_tries = 2;
    commit.move_delay = Duration::from_millis(10);
    Ctx {
        n: root.join(NEW),
        new_id: NEW.into(),
        roots: vec![root.to_path_buf()],
        home,
        platform: Platform::current(),
        satellite_roots: Roots::default(),
        ledger_only: vec![generic_source()],
        autostart: login,
        autostart_dir: None,
        run_key: None,
        exe: Some("/Applications/Lumen.app/Contents/MacOS/Lumen".into()),
        appimage: None,
        plugin_name: "Lumen".into(),
        ops,
        mcp_bin: &mcp,
        cli_link: None,
        cli_bin: None,
        commit,
    }
}

fn read(db: &Path, tag: &str) {
    lumen_core::meter::insert_read_event_at(
        db,
        tag,
        Some(1),
        10,
        20,
        10,
        "builtin_read",
        "cli",
        "Read",
        Some("s1"),
        Some(1),
        None,
        &Default::default(),
    )
    .unwrap();
}

fn paths(db: &Path) -> Vec<String> {
    let c = Connection::open(db).unwrap();
    let mut st = c
        .prepare("SELECT path FROM read_events ORDER BY path")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A previous folder with a ledger of `n` rows and a file of each other kind.
fn seed_prev(root: &Path, n: usize) -> PathBuf {
    let dir = root.join(PREV);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let db = dir.join("lumen.db");
    for i in 0..n {
        read(&db, &format!("/old/{i}"));
    }
    std::fs::write(dir.join("lumen_settings.json"), "{}").unwrap();
    db
}

/// An old binary holding the previous ledger, in WAL mode: the probe cannot take it.
fn hold(db: &Path) -> Connection {
    let held = Connection::open(db).unwrap();
    held.pragma_update(None, "journal_mode", "WAL").unwrap();
    held.execute(
        "INSERT INTO faults(ts, kind, session_id) VALUES ('t','k','s')",
        [],
    )
    .unwrap();
    held
}

fn marker_of(c: &Ctx) -> MarkerV2 {
    match marker::read(&c.n) {
        Seen::Migrated(m) => (*m).clone(),
        other => panic!("expected a committed marker, got {other:?}"),
    }
}

fn backup_of(c: &Ctx) -> PathBuf {
    backup_dir(&c.n, &marker_of(c)).unwrap()
}

/// Setup, as `run_setup_with` leaves it: `.setup_done` names the data folder it baked.
fn record_setup(c: &Ctx) {
    let done = c.setup_done();
    std::fs::create_dir_all(done.parent().unwrap()).unwrap();
    std::fs::write(done, format!("data_dir={}\n", c.n.display())).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn clean(r: &Report) {
    assert!(r.problems.is_empty(), "{r:?}");
}

/// Open the backup's files for writing again, as an old binary had before the freeze.
fn make_writable(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            make_writable(&p);
        } else {
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(&p, perms).unwrap();
        }
    }
}

// ── discovery and the move ─────────────────────────────────────────────────

#[test]
fn a_start_finds_the_previous_install_by_content_and_leaves_no_folder_named_like_it() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let old_db = seed_prev(t.path(), 3);

    // Before the first start the ledger in use is the previous one only once a start chose it.
    assert_eq!(c.db_path(), c.new_db());
    let r = at_start(&c);
    clean(&r);

    assert!(c.n.join(MARKER_NAME).is_file());
    assert_eq!(paths(&c.new_db()).len(), 3, "every row came");
    assert_eq!(c.db_path(), c.new_db());
    assert!(!old_db.exists() && !t.path().join(PREV).exists());
    let b = backup_of(&c);
    assert!(b.starts_with(&c.n), "the backup sits inside N: {b:?}");
    assert!(b.file_name().unwrap().to_string_lossy().contains(DATE));
    assert!(b.join("data").join("lumen.db").is_file());
    assert!(
        b.join("data").join("bin").is_dir(),
        "the whole folder travelled"
    );
    // Nothing at the data root carries the previous name any more.
    assert_eq!(names(t.path()), ["fake.new.app", "home"]);
    assert!(!names(&c.n).iter().any(|n| n.contains("previous")));
}

#[test]
fn a_fresh_install_never_migrates_a_folder_that_appears_later() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    clean(&at_start(&c));
    assert_eq!(marker::read(&c.n), Seen::Settled);
    read(&c.new_db(), "/mine");

    // A Finder duplicate, a restored backup or a downgrade brings a Lumen folder back.
    let old_db = seed_prev(t.path(), 4);
    let before = std::fs::read(&old_db).unwrap();
    let r = at_start(&c);
    clean(&r);
    assert_eq!(marker::read(&c.n), Seen::Settled);
    assert_eq!(paths(&c.new_db()), vec!["/mine".to_string()]);
    assert_eq!(std::fs::read(&old_db).unwrap(), before, "it is left alone");
    assert_eq!(c.db_path(), c.new_db());
}

#[test]
fn a_deferral_shows_in_startup_health_and_the_old_ledger_stays_in_use() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let old_db = seed_prev(t.path(), 2);
    let held = hold(&old_db);

    let r = at_start(&c);
    let health = StartupHealth::default();
    r.apply(&health, "TEST");
    let lines = health.degradations();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("migration: deferred"), "{lines:?}");
    assert!(health.is_degraded());
    assert!(!c.n.join(MARKER_NAME).exists(), "there is no marker");
    // Everything that resolves the ledger names the previous folder: the db path the app
    // exports and bakes into the pointer, the daemon and the shims, and the pending file.
    assert_eq!(c.db_path(), old_db, "nothing splits");
    let pending = marker::read_pending(&c.n).unwrap();
    assert_eq!(pending.dir, t.path().join(PREV));
    assert!(t.path().join(PREV).is_dir());

    // Once the holder is gone, the next start completes.
    drop(held);
    clean(&at_start(&c));
    assert_eq!(c.db_path(), c.new_db());
    assert!(
        marker::read_pending(&c.n).is_none(),
        "the pending file is tidied"
    );
}

#[test]
fn a_failed_migration_says_why_and_leaves_the_old_directory_in_use() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let old_db = seed_prev(t.path(), 2);
    let before = std::fs::read(&old_db).unwrap();
    // A file where the stage folder goes: the copy cannot start.
    std::fs::create_dir_all(&c.n).unwrap();
    std::fs::write(c.n.join(legacy::migrate::STAGE_DIR), b"in the way").unwrap();

    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert_eq!(r.problems[0].0, "migration");
    assert!(!c.n.join(MARKER_NAME).exists());
    assert_eq!(c.db_path(), old_db);
    assert_eq!(
        std::fs::read(&old_db).unwrap(),
        before,
        "the previous ledger is untouched"
    );
}

#[test]
fn an_unreadable_ledger_does_not_settle_the_install() {
    // A brand-new ledger whose schema is still only in the WAL: a writer is live. Settling now
    // would let the next start's migration be refused for good.
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let dir = t.path().join(PREV);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lumen.db"), b"").unwrap();
    std::fs::write(dir.join("lumen.db-wal"), b"not empty").unwrap();

    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert!(r.problems[0].1.contains("not settled"), "{r:?}");
    assert_eq!(marker::read(&c.n), Seen::Absent);
}

// ── the merge, the freeze, and what comes back ─────────────────────────────

#[test]
fn merges_run_until_setup_then_once_more_and_never_again() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 3);
    clean(&at_start(&c));
    let b = backup_of(&c);
    let b_db = b.join("data").join("lumen.db");
    assert_eq!(paths(&c.new_db()).len(), 3);

    // An old hook keeps writing into what was the previous ledger. Each start brings it over,
    // and the second start of the same rows adds nothing.
    read(&b_db, "/late/1");
    clean(&at_start(&c));
    assert_eq!(paths(&c.new_db()).len(), 4);
    clean(&at_start(&c));
    assert_eq!(paths(&c.new_db()).len(), 4, "nothing doubles");
    read(&b_db, "/late/2");
    clean(&at_start(&c));
    assert_eq!(
        paths(&c.new_db()).len(),
        5,
        "until Setup, every start merges"
    );

    // Setup re-registers: one more merge, then the freeze.
    read(&b_db, "/between");
    assert!(!freeze::is_frozen(&c.new_db(), &b));
    record_setup(&c);
    let r = freeze_after_setup(&c.home, &c.n);
    clean(&r);
    assert!(freeze::is_frozen(&c.new_db(), &b), "{r:?}");
    assert_eq!(
        paths(&c.new_db()).len(),
        6,
        "what was written since the last start"
    );
    assert!(std::fs::metadata(&b_db).unwrap().permissions().readonly());

    // Never again: a row that lands in the backup after the freeze stays there.
    make_writable(&b.join("data"));
    read(&b_db, "/after-the-freeze");
    let r = at_start(&c);
    assert_eq!(paths(&c.new_db()).len(), 6);
    assert!(
        r.problems
            .iter()
            .any(|(_, why)| why.contains("changed after it was frozen")),
        "{r:?}"
    );
    let r = freeze_after_setup(&c.home, &c.n);
    assert!(
        r.problems
            .iter()
            .any(|(_, why)| why.contains("changed after it was frozen")),
        "{r:?}"
    );
    assert_eq!(paths(&c.new_db()).len(), 6);
}

#[test]
fn a_freeze_waits_for_a_deferred_move() {
    let t = TempDir::new().unwrap();
    let login = Login(Cell::new(true));
    let gate = Gate {
        refuse: Cell::new(true),
    };
    let c = ctx(t.path(), &login, &gate);
    let prev_db = seed_prev(t.path(), 2);

    // The commit stands, the rename is refused: S2. The previous folder is still where it was.
    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert!(r.problems[0].1.contains("could not be moved"), "{r:?}");
    let b = backup_of(&c);
    assert!(prev_db.exists() && !b.join("data").exists());
    assert_eq!(
        c.db_path(),
        c.new_db(),
        "the commit already moved the ledger in use"
    );
    // An old app keeps writing there meanwhile.
    read(&prev_db, "/s2/1");

    // Setup in S2 records N, and the freeze still waits: there is nothing to freeze yet.
    record_setup(&c);
    clean(&freeze_after_setup(&c.home, &c.n));
    assert!(!freeze::is_frozen(&c.new_db(), &b));

    // The start that moves the folder freezes it, taking what was written in S2 once.
    gate.refuse.set(false);
    let r = at_start(&c);
    clean(&r);
    assert!(b.join("data").join("lumen.db").is_file());
    assert!(!prev_db.exists());
    assert!(freeze::is_frozen(&c.new_db(), &b));
    let rows = paths(&c.new_db());
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows.iter().filter(|p| *p == "/s2/1").count(), 1);
}

#[test]
fn a_recreated_old_folder_is_reported_with_the_import_command_and_never_merged() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 2);
    clean(&at_start(&c));
    // An old app is run by hand: it makes its folder again and writes a ledger there. (A copy
    // restored from before the upgrade looks the same.)
    let again = seed_prev(t.path(), 5);

    for _ in 0..2 {
        let r = at_start(&c);
        let msg = r
            .problems
            .iter()
            .map(|(_, why)| why.as_str())
            .find(|why| why.contains("lumen import-ledger"))
            .unwrap_or_else(|| panic!("no import hint in {r:?}"));
        assert!(
            msg.contains(&t.path().join(PREV).to_string_lossy().to_string()),
            "{msg}"
        );
        assert_eq!(paths(&c.new_db()).len(), 2, "never merged");
        assert!(again.exists(), "never moved");
    }
    // Setup and the freeze do not take it either.
    record_setup(&c);
    clean(&freeze_after_setup(&c.home, &c.n));
    assert_eq!(paths(&c.new_db()).len(), 2);
    assert_eq!(paths(&again).len(), 5);
}

#[test]
fn a_recreated_folder_without_a_ledger_moves_into_the_backup() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 1);
    clean(&at_start(&c));
    let b = backup_of(&c);
    let again = t.path().join(PREV);
    std::fs::create_dir_all(again.join("logs")).unwrap();
    std::fs::write(again.join("logs").join("old.log"), "line").unwrap();

    clean(&at_start(&c));
    assert!(!again.exists());
    assert_eq!(
        std::fs::read_to_string(b.join("recreated-1").join("logs").join("old.log")).unwrap(),
        "line"
    );
    // And it is not named like the previous folder inside the backup.
    assert!(!names(&b).iter().any(|n| n.contains("previous")));
}

#[test]
fn an_empty_recreated_folder_is_removed() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 1);
    clean(&at_start(&c));
    std::fs::create_dir_all(t.path().join(PREV)).unwrap();
    clean(&at_start(&c));
    assert!(!t.path().join(PREV).exists());
    assert_eq!(names(t.path()), ["fake.new.app", "home"]);
}

#[test]
fn a_recreated_old_directory_after_the_backup_is_deleted_loses_and_repeats_nothing() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 3);
    clean(&at_start(&c));
    record_setup(&c);
    clean(&freeze_after_setup(&c.home, &c.n));
    let b = backup_of(&c);
    // The user deletes the backup, then an old app makes its folder again.
    make_writable(&b);
    std::fs::remove_dir_all(&b).unwrap();
    let again = seed_prev(t.path(), 7);

    let r = at_start(&c);
    assert!(
        r.problems
            .iter()
            .any(|(_, why)| why.contains("lumen import-ledger")),
        "{r:?}"
    );
    assert_eq!(
        paths(&c.new_db()).len(),
        3,
        "nothing merged, nothing moved in"
    );
    assert_eq!(paths(&again).len(), 7);
    assert!(!b.exists(), "no backup is invented");
}

#[test]
fn a_190_ledger_goes_through_deferred_commit_old_writes_move_merge_setup_freeze() {
    let t = TempDir::new().unwrap();
    let login = Login(Cell::new(true));
    let gate = Gate {
        refuse: Cell::new(false),
    };
    let c = ctx(t.path(), &login, &gate);
    let prev_db = seed_prev(t.path(), 3);

    // Deferred: an old daemon holds the ledger. Nothing is committed and nothing splits.
    let held = hold(&prev_db);
    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert_eq!(c.db_path(), prev_db);
    assert!(!c.n.join(MARKER_NAME).exists());
    drop(held);

    // Commit, but the move is refused: S2. Old writers keep going to the previous folder.
    gate.refuse.set(true);
    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    read(&prev_db, "/s2/a");
    read(&prev_db, "/s2/b");

    // The move happens; the late rows arrive once, however often it starts.
    gate.refuse.set(false);
    clean(&at_start(&c));
    clean(&at_start(&c));
    assert_eq!(paths(&c.new_db()).len(), 5);

    // Setup, then the freeze; the ledger holds every row once and the backup is read-only.
    record_setup(&c);
    clean(&freeze_after_setup(&c.home, &c.n));
    assert_eq!(paths(&c.new_db()).len(), 5);
    assert!(freeze::is_frozen(&c.new_db(), &backup_of(&c)));
}

// ── the com.tauri.dev replacement ──────────────────────────────────────────

fn generic_ledger_with_wal_rows(root: &Path) -> (Connection, PathBuf) {
    let dir = root.join(generic_source().id);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lumen.db");
    let conn = lumen_core::meter::connect_db(&db).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    read(&db, "/checkpointed");
    conn.pragma_update(None, "wal_checkpoint", "TRUNCATE").ok();
    for i in 0..4 {
        read(&db, &format!("/wal/{i}"));
    }
    assert!(dir.join("lumen.db-wal").metadata().unwrap().len() > 0);
    (conn, db)
}

#[test]
fn the_generic_bundle_ledger_comes_with_its_wal_rows_where_the_old_block_lost_them() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let (_held, generic_db) = generic_ledger_with_wal_rows(t.path());

    // Red: the block this replaced copied the main file alone.
    let bare = t.path().join("bare.db");
    std::fs::copy(&generic_db, &bare).unwrap();
    assert!(paths(&bare).len() < 5, "the bare copy misses WAL rows");

    clean(&at_start(&c));
    assert_eq!(paths(&c.new_db()).len(), 5, "green: every row came");
    assert!(generic_db.exists(), "the source stays where it is");
    assert_eq!(
        marker::read(&c.n),
        Seen::Settled,
        "and it is no previous install"
    );
}

#[test]
fn the_generic_bundle_is_only_read_while_there_is_no_ledger_of_ours() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    generic_ledger_with_wal_rows(t.path());
    std::fs::create_dir_all(&c.n).unwrap();
    read(&c.new_db(), "/mine");
    clean(&at_start(&c));
    assert_eq!(paths(&c.new_db()), vec!["/mine".to_string()]);
}

#[test]
fn the_generic_bundle_waits_while_a_previous_folder_is_still_to_move() {
    // Importing now would start a ledger of ours next to a pending one: a split.
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    let prev_db = seed_prev(t.path(), 1);
    generic_ledger_with_wal_rows(t.path());
    let _held = hold(&prev_db);
    let r = at_start(&c);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert!(!c.new_db().exists(), "no ledger of ours was started");
}

#[test]
fn the_real_generic_source_applies_on_macos_only() {
    let real = ledger_only_sources();
    assert_eq!(real.len(), usize::from(cfg!(target_os = "macos")));
    assert!(real.iter().all(|s| s.id == "com.tauri.dev"));
    assert!(matches!(generic_source().os, Os::MacOs));
    assert!(matches!(generic_source().take, Take::LedgerOnly));
}

// ── the Setup prompt ───────────────────────────────────────────────────────

fn write_config(home: &Path, command: &str, db: &str) {
    std::fs::write(
        home.join(".claude.json"),
        serde_json::json!({
            "mcpServers": {"lumen": {
                "type": "stdio", "command": command, "args": [], "env": {"LUMEN_DB": db}
            }}
        })
        .to_string(),
    )
    .unwrap();
}

fn done_marker(home: &Path) {
    std::fs::create_dir_all(home.join(".claude/lumen")).unwrap();
    std::fs::write(home.join(".claude/lumen/.setup_done"), "").unwrap();
}

#[test]
fn setup_is_needed_when_the_claude_config_still_points_into_the_previous_directory() {
    let t = TempDir::new().unwrap();
    let home = t.path();
    let root = lumen_core::legacy::data_roots(Some(home))[0].clone();
    let n = root.join(NEW);
    // What a pre-release migration left: a marker that names the folder it came from.
    let seen = Seen::V1 {
        source: PREV.into(),
    };
    done_marker(home);
    // A command that exists on every system, so only the previous path is in question.
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Setup done, config current: not needed.
    write_config(
        home,
        &exe,
        &root.join(NEW).join("lumen.db").to_string_lossy(),
    );
    assert!(!crate::setup::setup_needed_with(home, &n, &seen));

    // The LUMEN_DB value was exempt from validation; now it is what shows the prompt.
    write_config(
        home,
        &exe,
        &root.join(PREV).join("lumen.db").to_string_lossy(),
    );
    assert!(
        crate::setup::setup_needed_with(home, &n, &seen),
        "red before: false"
    );
    let v = crate::setup::validate_mcp_with(home, &n, &seen);
    assert!(
        !v.healthy && v.detail.contains("legacy data directory"),
        "{v:?}"
    );

    // The command inside the previous directory (the DMG copy under bin/) shows too.
    let bin = root.join(PREV).join("bin").join("lumen-mcp");
    write_config(home, &bin.to_string_lossy(), "/elsewhere/lumen.db");
    assert!(crate::setup::setup_needed_with(home, &n, &seen));

    // Not Setup's business: another directory that merely has the same name, and
    // an install where nothing moved.
    write_config(home, &exe, &format!("/elsewhere/{PREV}/lumen.db"));
    assert!(!crate::setup::setup_needed_with(home, &n, &seen));
    write_config(
        home,
        &exe,
        &root.join(PREV).join("lumen.db").to_string_lossy(),
    );
    assert!(!crate::setup::setup_needed_with(home, &n, &Seen::Settled));

    // And without the completion marker it is needed whatever the config says.
    std::fs::remove_file(home.join(".claude/lumen/.setup_done")).unwrap();
    assert!(crate::setup::setup_needed_with(home, &n, &Seen::Settled));
}

#[test]
fn dates_are_iso() {
    assert_eq!(ymd(0), "1970-01-01");
    assert_eq!(ymd(11_016), "2000-02-29");
    assert_eq!(ymd(20_744), "2026-10-18");
    let now = now_utc();
    assert_eq!(now.len(), 20, "{now}");
    assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
}

// ── one data folder, S2 with nothing to move, a kill before tidy ───────────────────────────

#[test]
fn the_prompt_the_start_and_setup_name_the_same_data_folder() {
    // `Ctx::real_in` is the start's context for a given home. Setup's prompt and `lumen_run_setup`
    // use `setup::app_support_dir_in(home)` too: were one of them on `dirs::data_dir()` (which
    // follows XDG_DATA_HOME, or a redirected Roaming AppData), the prompt would look for the
    // marker in a folder the start never wrote to. The home is injected, so this runs on every OS.
    let t = TempDir::new().unwrap();
    let login = Login(Cell::new(true));
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut c = Ctx::real_in(home.clone(), &login, None, &mcp).unwrap();
    let n = setup::app_support_dir_in(&home);
    assert_eq!(c.n, n, "the start and the prompt disagree about N");
    // Keep the start off the real machine: only the temp folders are searched and touched.
    c.roots = vec![t.path().to_path_buf()];
    c.ledger_only = vec![generic_source()];
    c.autostart_dir = None;
    c.cli_link = None;
    c.cli_bin = None;
    c.satellite_roots = Roots::default();
    c.commit.probe_tries = 2;
    c.commit.probe_delay = Duration::from_millis(20);
    c.commit.move_tries = 2;
    c.commit.move_delay = Duration::from_millis(10);
    seed_prev(t.path(), 3);

    // Setup ran once, for a folder that is not N.
    let done = c.setup_done();
    std::fs::create_dir_all(done.parent().unwrap()).unwrap();
    std::fs::write(&done, "data_dir=/somewhere/else\n").unwrap();
    assert!(
        !setup::setup_needed_with(&home, &n, &marker::read(&n)),
        "premise: nothing is committed yet"
    );
    clean(&at_start(&c));

    // The prompt reads the marker the start wrote, and asks for Setup again.
    assert!(
        matches!(marker::read(&n), Seen::Migrated(_)),
        "the marker is in the folder the prompt reads"
    );
    assert!(
        setup::setup_needed_with(&home, &n, &marker::read(&n)),
        "the prompt did not see the marker"
    );

    // Setup records N, and the freeze (which `lumen_run_setup` calls with the same N) happens.
    let b = backup_of(&c);
    assert!(!freeze::is_frozen(&c.new_db(), &b));
    record_setup(&c);
    assert!(!setup::setup_needed_with(&home, &n, &marker::read(&n)));
    clean(&freeze_after_setup(
        &home,
        &setup::app_support_dir_in(&home),
    ));
    assert!(freeze::is_frozen(&c.new_db(), &b), "the backup never froze");
}

#[test]
fn a_previous_folder_deleted_before_its_move_is_a_problem() {
    let t = TempDir::new().unwrap();
    let login = Login(Cell::new(true));
    let gate = Gate {
        refuse: Cell::new(true),
    };
    let c = ctx(t.path(), &login, &gate);
    seed_prev(t.path(), 2);
    // The commit stands and the move is refused: S2.
    let r = at_start(&c);
    assert!(r.problems[0].1.contains("could not be moved"), "{r:?}");

    // The folder goes before the move is retried (S2 with nothing to move).
    std::fs::remove_dir_all(t.path().join(PREV)).unwrap();
    gate.refuse.set(false);
    let r = at_start(&c);
    assert!(
        r.problems.iter().any(|(_, why)| why.contains("is gone")),
        "a lost previous folder was not reported as a problem: {r:?}"
    );
    assert!(
        !r.notes.iter().any(|n| n.contains("is gone")),
        "it is a problem, not a note: {r:?}"
    );
}

#[test]
fn a_start_killed_between_the_commit_and_the_tidy_leaves_a_pending_file_the_marker_overrides() {
    let t = TempDir::new().unwrap();
    let (login, ops) = (Login(Cell::new(true)), Native);
    let c = ctx(t.path(), &login, &ops);
    seed_prev(t.path(), 3);
    clean(&at_start(&c));
    assert!(marker::read_pending(&c.n).is_none());

    // The kill: the marker is committed and the pending file, written before anything could
    // fail, was not yet removed. The previous path is a real folder again (a re-created one),
    // which is what a pending file would be trusted for if there were no marker.
    std::fs::create_dir_all(t.path().join(PREV)).unwrap();
    marker::write_pending(
        &c.n,
        &marker::Pending {
            source: PREV.into(),
            dir: t.path().join(PREV),
        },
    )
    .unwrap();
    assert!(marker::read_pending(&c.n).is_some(), "premise");
    assert_eq!(
        c.db_path(),
        c.new_db(),
        "the marker overrides the pending file before the next start has tidied it"
    );

    // The next start tidies it, and nothing splits.
    clean(&at_start(&c));
    assert!(
        marker::read_pending(&c.n).is_none(),
        "the pending file outlived the tidy"
    );
    assert_eq!(c.db_path(), c.new_db());
    assert_eq!(paths(&c.new_db()).len(), 3);
}
