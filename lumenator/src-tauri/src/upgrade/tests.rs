//! The wiring end to end, with fake identifiers. (The real pair differs since the flip;
//! the same-identifier state, where nothing moves, is made here with a fake pair.)

use super::*;
use crate::health::StartupHealth;
use lumen_core::legacy::{Os, MARKER_NAME};
use rusqlite::Connection;
use tempfile::TempDir;

const NEW: &str = "fake.new.app";
const OLD: &str = "fake.old.app";
const GENERIC: &str = "fake.generic.app";
const DATE: &str = "2026-10-18";

fn generic_source() -> Source {
    Source {
        id: GENERIC,
        os: Os::All,
        take: Take::LedgerOnly,
        retire: Retire::LeaveInPlace,
    }
}

fn ctx(root: &Path) -> Ctx {
    Ctx {
        root: root.to_path_buf(),
        new_id: NEW.into(),
        old_id: OLD.into(),
        date: DATE.into(),
        logs: None,
        ledger_only: vec![generic_source()],
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

/// An old data directory with a ledger of `n` rows and a file of each other kind.
fn seed_old(root: &Path, n: usize) -> PathBuf {
    let old = root.join(OLD);
    std::fs::create_dir_all(old.join("bin")).unwrap();
    let db = old.join("lumen.db");
    for i in 0..n {
        read(&db, &format!("/old/{i}"));
    }
    std::fs::write(old.join("lumen_settings.json"), "{}").unwrap();
    db
}

fn backup_of_marker(root: &Path) -> PathBuf {
    let m = legacy::read_marker(&root.join(NEW)).unwrap().unwrap();
    PathBuf::from(m.backup)
}

#[test]
fn a_start_migrates_retires_links_and_the_next_start_merges_what_an_old_hook_wrote() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    let old_db = seed_old(t.path(), 3);

    // Before the first start the ledger in use is the old one.
    assert_eq!(c.db_path(), old_db);

    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert!(c.new_dir().join(MARKER_NAME).is_file());
    assert_eq!(paths(&c.new_db()).len(), 3, "every row moved");
    assert_eq!(c.db_path(), c.new_db());
    assert!(
        std::fs::symlink_metadata(c.old_dir())
            .unwrap()
            .file_type()
            .is_symlink(),
        "the old path is linked to the backup"
    );
    let backup = backup_of_marker(t.path());
    assert!(backup.is_dir());
    assert!(backup.file_name().unwrap().to_string_lossy().contains(DATE));

    // An old shim still writes through the old path, which lands in the backup.
    read(&old_db, "/late/1");
    read(&old_db, "/late/2");
    assert_eq!(paths(&backup.join("lumen.db")).len(), 5);
    assert_eq!(paths(&c.new_db()).len(), 3, "not in the new ledger yet");

    // The next start merges them; and again, nothing doubles.
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()).len(), 5);
    assert!(paths(&c.new_db()).contains(&"/late/2".to_string()));
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()).len(), 5);
    assert_eq!(
        paths(&backup.join("lumen.db")).len(),
        5,
        "the backup is only read"
    );
}

#[test]
fn merging_after_setup_takes_what_was_written_since_the_start() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    let old_db = seed_old(t.path(), 1);
    assert!(at_start(&c, &AppOps).problems.is_empty());
    read(&old_db, "/between");
    let r = merge_again(&c);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()).len(), 2);
}

#[test]
fn a_deferral_shows_in_the_startup_health_and_the_old_ledger_stays_in_use() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    let old_db = seed_old(t.path(), 2);
    // An old daemon holding the ledger in WAL mode: the probe cannot leave it.
    let held = Connection::open(&old_db).unwrap();
    held.pragma_update(None, "journal_mode", "WAL").unwrap();
    held.execute(
        "INSERT INTO faults(ts, kind, session_id) VALUES ('t','k','s')",
        [],
    )
    .unwrap();

    let r = at_start(&c, &AppOps);
    let health = StartupHealth::default();
    r.apply(&health, "TEST");
    let lines = health.degradations();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("migration: deferred"), "{lines:?}");
    assert!(health.is_degraded());
    assert!(!c.new_dir().join(MARKER_NAME).exists());
    assert_eq!(
        c.db_path(),
        old_db,
        "nothing splits: the old ledger stays in use"
    );
    assert!(std::fs::symlink_metadata(c.old_dir()).unwrap().is_dir());

    // Once the holder is gone, the next start completes.
    drop(held);
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(c.db_path(), c.new_db());
}

#[test]
fn a_failed_migration_says_why_and_leaves_the_old_directory_in_use() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    let old = c.old_dir();
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("lumen.db"), b"this is not a database").unwrap();

    let r = at_start(&c, &AppOps);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert_eq!(r.problems[0].0, "migration");
    assert!(!c.new_dir().join(MARKER_NAME).exists());
    assert_eq!(c.db_path(), old.join("lumen.db"));
    assert_eq!(
        std::fs::read(old.join("lumen.db")).unwrap(),
        b"this is not a database"
    );
}

struct NoLink;
impl retire::Ops for NoLink {
    fn link_dir(&self, _: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("no links here"))
    }
}

#[test]
fn a_link_that_could_not_be_made_shows_and_is_made_at_the_next_start() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    seed_old(t.path(), 1);
    let r = at_start(&c, &NoLink);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert!(r.problems[0].1.contains("not linked yet"), "{r:?}");
    assert!(c.new_dir().join(MARKER_NAME).is_file(), "the copy stands");
    assert_eq!(c.db_path(), c.new_db());

    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert!(std::fs::symlink_metadata(c.old_dir())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn logs_follow_the_data_and_a_failure_to_move_them_is_not_fatal() {
    let t = TempDir::new().unwrap();
    let mut c = ctx(t.path());
    seed_old(t.path(), 1);
    let (ol, nl) = (t.path().join("logs-old"), t.path().join("logs-new"));
    std::fs::create_dir_all(&ol).unwrap();
    std::fs::write(ol.join("Lumen.log"), "line").unwrap();
    c.logs = Some((ol.clone(), nl.clone()));
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(
        std::fs::read_to_string(nl.join("Lumen.log")).unwrap(),
        "line"
    );
    assert!(!ol.exists(), "renamed to a dated .migrated directory");
}

// ── the com.tauri.dev replacement ──────────────────────────────────────────

fn generic_ledger_with_wal_rows(root: &Path) -> (Connection, PathBuf) {
    let dir = root.join(GENERIC);
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
    let c = ctx(t.path());
    let (_held, generic_db) = generic_ledger_with_wal_rows(t.path());

    // Red: the block this replaced copied the main file alone.
    let bare = t.path().join("bare.db");
    std::fs::copy(&generic_db, &bare).unwrap();
    assert!(paths(&bare).len() < 5, "the bare copy misses WAL rows");

    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()).len(), 5, "green: every row came");
    assert!(generic_db.exists(), "the source stays where it is");
}

#[test]
fn the_generic_bundle_is_only_read_while_there_is_no_ledger_of_ours() {
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    generic_ledger_with_wal_rows(t.path());
    std::fs::create_dir_all(c.new_dir()).unwrap();
    read(&c.new_db(), "/mine");
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()), vec!["/mine".to_string()]);
}

#[test]
fn the_generic_bundle_waits_while_an_older_directory_is_still_to_move() {
    // Importing now would start a ledger of ours next to a pending one: a split.
    let t = TempDir::new().unwrap();
    let c = ctx(t.path());
    let old_db = seed_old(t.path(), 1);
    generic_ledger_with_wal_rows(t.path());
    let held = Connection::open(&old_db).unwrap();
    held.pragma_update(None, "journal_mode", "WAL").unwrap();
    held.execute(
        "INSERT INTO faults(ts, kind, session_id) VALUES ('t','k','s')",
        [],
    )
    .unwrap();
    let r = at_start(&c, &AppOps);
    assert_eq!(r.problems.len(), 1, "{r:?}");
    assert!(!c.new_db().exists(), "no ledger of ours was started");
}

#[test]
fn the_real_generic_source_applies_on_macos_only() {
    let real = ledger_only_sources();
    assert_eq!(real.len(), usize::from(cfg!(target_os = "macos")));
    assert!(real.iter().all(|s| s.id == "com.tauri.dev"));
}

#[test]
fn with_the_same_identifier_twice_nothing_moves() {
    // The state of every real install before the flip, and of a build whose two ids agree.
    let t = TempDir::new().unwrap();
    let mut c = ctx(t.path());
    c.new_id = OLD.into();
    c.ledger_only.clear();
    let old_db = seed_old(t.path(), 2);
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty() && r.notes.is_empty(), "{r:?}");
    assert!(std::fs::symlink_metadata(c.old_dir()).unwrap().is_dir());
    assert!(!c.old_dir().join(MARKER_NAME).exists());
    assert_eq!(c.db_path(), old_db);
}

#[test]
fn with_the_same_identifier_twice_the_generic_bundle_is_still_imported_when_there_is_no_ledger() {
    // `moving()` is false, so the whole directory step is skipped; the ledger-only source
    // must not be, or a build whose ids agree would start empty beside a history.
    let t = TempDir::new().unwrap();
    let mut c = ctx(t.path());
    c.new_id = OLD.into();
    let (_held, generic_db) = generic_ledger_with_wal_rows(t.path());
    assert!(!c.new_db().exists());
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()).len(), 5, "every row came, WAL rows too");
    assert!(generic_db.exists(), "the source stays where it is");
    assert!(
        !c.new_dir().join(MARKER_NAME).exists(),
        "no directory move happened"
    );
}

#[test]
fn with_the_same_identifier_twice_the_generic_bundle_never_replaces_a_ledger() {
    let t = TempDir::new().unwrap();
    let mut c = ctx(t.path());
    c.new_id = OLD.into();
    let old_db = seed_old(t.path(), 2);
    let before = paths(&old_db);
    generic_ledger_with_wal_rows(t.path());
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(paths(&c.new_db()), before);
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
fn setup_is_needed_when_the_claude_config_still_points_into_the_legacy_directory() {
    let t = TempDir::new().unwrap();
    let home = t.path();
    let root = lumen_core::legacy::data_roots(Some(home))[0].clone();
    done_marker(home);
    // A command that exists on every system, so only the legacy path is in question.
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
    assert!(!crate::setup::setup_needed_with(home, NEW, OLD));

    // The LUMEN_DB value was exempt from validation; now it is what shows the prompt.
    write_config(
        home,
        &exe,
        &root.join(OLD).join("lumen.db").to_string_lossy(),
    );
    assert!(
        crate::setup::setup_needed_with(home, NEW, OLD),
        "red before: false"
    );
    let v = crate::setup::validate_mcp_with(home, NEW, OLD);
    assert!(
        !v.healthy && v.detail.contains("legacy data directory"),
        "{v:?}"
    );

    // The command inside the legacy directory (the DMG copy under bin/) shows too.
    let bin = root.join(OLD).join("bin").join("lumen-mcp");
    write_config(home, &bin.to_string_lossy(), "/elsewhere/lumen.db");
    assert!(crate::setup::setup_needed_with(home, NEW, OLD));

    // Not Setup's business: another directory that merely has the same name, and
    // the unflipped state where the two identifiers are one.
    write_config(home, &exe, &format!("/elsewhere/{OLD}/lumen.db"));
    assert!(!crate::setup::setup_needed_with(home, NEW, OLD));
    write_config(
        home,
        &exe,
        &root.join(OLD).join("lumen.db").to_string_lossy(),
    );
    assert!(!crate::setup::setup_needed_with(home, OLD, OLD));

    // And without the completion marker it is needed whatever the config says.
    std::fs::remove_file(home.join(".claude/lumen/.setup_done")).unwrap();
    assert!(crate::setup::setup_needed_with(home, OLD, OLD));
}

#[test]
fn startup_never_writes_the_claude_config() {
    let t = TempDir::new().unwrap();
    let home = t.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = home.join("data");
    let c = ctx(&root);
    seed_old(&root, 1);
    write_config(
        &home,
        "/x",
        &root.join(OLD).join("lumen.db").to_string_lossy(),
    );
    let before = std::fs::read(home.join(".claude.json")).unwrap();
    let r = at_start(&c, &AppOps);
    assert!(r.problems.is_empty(), "{r:?}");
    assert_eq!(std::fs::read(home.join(".claude.json")).unwrap(), before);
}

#[test]
fn dates_are_iso() {
    assert_eq!(ymd(0), "1970-01-01");
    assert_eq!(ymd(11_016), "2000-02-29");
    assert_eq!(ymd(20_744), "2026-10-18");
    assert_eq!(today().len(), 10);
}
