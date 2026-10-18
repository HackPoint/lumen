#![cfg(debug_assertions)] // the migration's test hook exists only in debug builds
//! Steps 1-5 of the legacy migration, against temp directories and fake ids.
//! Two tests re-run this binary as a child process: one to hold the old ledger
//! open in WAL while writing, one to be killed halfway through a copy.

use lumen_core::legacy::migrate::{LOCK_NAME, Options, Outcome, Phase, STAGE_DIR, migrate};
use lumen_core::legacy::{MARKER_NAME, Marker, Take, effective_with, read_marker};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const OLD: &str = "fake.old.app";
const NEW: &str = "fake.new.app";

fn opts() -> Options {
    let mut o = Options::new(OLD, "fake.old.app.2026-01-01.migrated", Take::Dir);
    o.probe_tries = 2;
    o.probe_delay = Duration::from_millis(20);
    o
}

/// The marker of a committed migration; any other outcome fails with its full text.
fn migrated(out: Outcome) -> Marker {
    match out {
        Outcome::Migrated(m) => m,
        other => panic!("expected Migrated, got {other:?}"),
    }
}

fn rows(db: &Path) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

fn seed_db(path: &Path, wal: bool, n: i64) -> Connection {
    let c = Connection::open(path).unwrap();
    let mode = if wal { "WAL" } else { "DELETE" };
    c.query_row(&format!("PRAGMA journal_mode={mode}"), [], |_| Ok(()))
        .unwrap();
    c.execute_batch("CREATE TABLE IF NOT EXISTS t(id INTEGER PRIMARY KEY, v TEXT); CREATE TABLE u(a TEXT PRIMARY KEY, b) WITHOUT ROWID;").ok();
    for i in 0..n {
        c.execute("INSERT INTO t(v) VALUES (?1)", [format!("row{i}")])
            .unwrap();
    }
    c
}

/// An old directory with every kind of file the app keeps.
fn seed_old(old: &Path, wal: bool) {
    std::fs::create_dir_all(old.join("bin")).unwrap();
    drop(seed_db(&old.join("lumen.db"), wal, 20));
    std::fs::write(old.join("faults.jsonl"), "{\"k\":1}\n{\"k\":2}\n").unwrap();
    std::fs::write(old.join("lumen_settings.json"), "{\"a\":true}").unwrap();
    std::fs::write(old.join("lumen_call_toolu_1.json"), "{}").unwrap();
    std::fs::write(old.join("bin").join("lumen-mcp"), b"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            old.join("bin").join("lumen-mcp"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
    }
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn go(base: &Path, rel: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(base.join(rel)).unwrap() {
            let e = e.unwrap();
            let r = rel.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                go(base, &r, out);
            } else {
                out.insert(
                    r.to_string_lossy().replace('\\', "/"),
                    std::fs::read(base.join(&r)).unwrap(),
                );
            }
        }
    }
    let mut m = BTreeMap::new();
    go(dir, Path::new(""), &mut m);
    m
}

fn eff(root: &Path) -> PathBuf {
    effective_with(&root.join(NEW).join("lumen.db"), NEW, OLD, &[root])
}

// --- child processes -------------------------------------------------------

/// A child that is killed and reaped when the guard drops, so a failed assertion
/// cannot leave the writer looping.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_child(role: &str, root: &Path) -> ChildGuard {
    ChildGuard(spawn_raw(role, root))
}

fn spawn_raw(role: &str, root: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
        .env("LUMEN_T2_ROLE", role)
        .env("LUMEN_T2_ROOT", root)
        .spawn()
        .unwrap()
}

fn wait_for(p: &Path) {
    let t = Instant::now();
    while !p.exists() {
        assert!(t.elapsed() < Duration::from_secs(60), "timed out on {p:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pause_hook(p: &Phase<'_>) {
    if let Phase::Copied { rel } = p
        && rel.ends_with("lumen.db")
    {
        let root = PathBuf::from(std::env::var("LUMEN_T2_ROOT").unwrap());
        std::fs::write(root.join("paused"), "").unwrap();
        std::thread::sleep(Duration::from_secs(600));
    }
}

#[test]
fn child_entry() {
    let Ok(role) = std::env::var("LUMEN_T2_ROLE") else {
        return; // an ordinary run of the suite: nothing to do
    };
    let root = PathBuf::from(std::env::var("LUMEN_T2_ROOT").unwrap());
    match role.as_str() {
        // The old app: WAL, one connection held open, rows landing until told to stop.
        "writer" => {
            let c = Connection::open(root.join(OLD).join("lumen.db")).unwrap();
            c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
                .unwrap();
            std::fs::write(root.join("ready"), "").unwrap();
            while !root.join("stop").exists() {
                c.execute("INSERT INTO t(v) VALUES ('late')", []).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        // The new app, killed by the parent while this hook sleeps.
        "migrator" => {
            let mut o = opts();
            o.hook = Some(pause_hook);
            let out = migrate(&root.join(OLD), &root.join(NEW), &o);
            panic!("the hook should have blocked: {out:?}");
        }
        other => panic!("unknown role {other}"),
    }
}

// --- the tests -------------------------------------------------------------

#[test]
fn the_copy_is_complete() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    // An orphaned -wal: a database whose last writer died, rows only in the log.
    {
        let live = root.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let c = seed_db(&live.join("lumen.db"), true, 5);
        c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        c.execute("INSERT INTO t(v) VALUES ('only in wal')", [])
            .unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::copy(live.join("lumen.db"), old.join("lumen.db")).unwrap();
        std::fs::copy(live.join("lumen.db-wal"), old.join("lumen.db-wal")).unwrap();
        drop(c);
        for (n, c) in [
            ("faults.jsonl", "{\"k\":1}\n"),
            ("lumen_settings.json", "{}"),
        ] {
            std::fs::write(old.join(n), c).unwrap();
        }
        std::fs::write(old.join("lumen_call_toolu_1.json"), "{}").unwrap();
        std::fs::create_dir_all(old.join("bin")).unwrap();
        std::fs::write(old.join("bin").join("lumen-mcp"), b"bin").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                old.join("bin").join("lumen-mcp"),
                std::fs::Permissions::from_mode(0o750),
            )
            .unwrap();
        }
    }
    assert!(old.join("lumen.db-wal").exists());

    let m = migrated(migrate(&old, &new, &opts()));
    assert_eq!(rows(&new.join("lumen.db")), 6, "the wal's row came along");
    assert!(!new.join("lumen.db-wal").exists());
    for f in [
        "faults.jsonl",
        "lumen_settings.json",
        "lumen_call_toolu_1.json",
        "bin/lumen-mcp",
    ] {
        assert!(new.join(f).is_file(), "{f}");
    }
    assert_eq!(m.spool_offset, 8);
    assert_eq!(m.hwm["t"], 6);
    assert!(!m.hwm.contains_key("u"), "WITHOUT ROWID has no rowid");
    assert_eq!(read_marker(&new).unwrap().unwrap(), m);
    assert!(!new.join(STAGE_DIR).exists());
    assert!(!new.join(format!("{MARKER_NAME}.tmp")).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(new.join("bin/lumen-mcp")), 0o750);
    }
    assert_eq!(eff(root.path()), new.join("lumen.db"));
}

#[test]
fn a_second_run_does_nothing() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, true);
    migrated(migrate(&old, &new, &opts()));
    let before = (tree(&new), tree(&old));
    let marker = std::fs::read(new.join(MARKER_NAME)).unwrap();
    assert_eq!(migrate(&old, &new, &opts()), Outcome::NothingToDo);
    assert_eq!((tree(&new), tree(&old)), before);
    assert_eq!(std::fs::read(new.join(MARKER_NAME)).unwrap(), marker);
}

#[test]
fn no_old_directory_is_nothing_to_do() {
    let root = TempDir::new().unwrap();
    let new = root.path().join(NEW);
    assert_eq!(
        migrate(&root.path().join(OLD), &new, &opts()),
        Outcome::NothingToDo
    );
    assert!(!new.join(MARKER_NAME).exists());
}

#[test]
fn a_file_already_in_new_is_moved_aside_never_overwritten() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    std::fs::create_dir_all(&new).unwrap();
    std::fs::write(new.join("lumen_settings.json"), "mine").unwrap();
    migrated(migrate(&old, &new, &opts()));
    assert_eq!(
        std::fs::read(new.join("lumen_settings.json.pre-legacy")).unwrap(),
        b"mine"
    );
    assert_eq!(
        std::fs::read(new.join("lumen_settings.json")).unwrap(),
        b"{\"a\":true}"
    );
}

fn corrupt(p: &Phase<'_>) {
    if let Phase::Staged { dir } = p {
        std::fs::write(dir.join("faults.jsonl"), "{\"k\":9}\n").unwrap();
    }
}

/// A change that keeps the file valid and every count equal: only the content
/// digest can see it.
fn corrupt_ledger(p: &Phase<'_>) {
    if let Phase::Staged { dir } = p {
        Connection::open(dir.join("lumen.db"))
            .unwrap()
            .execute("UPDATE t SET v='tampered' WHERE id=1", [])
            .unwrap();
    }
}

#[test]
fn verify_catches_a_staged_file_that_changed() {
    for hook in [corrupt as fn(&Phase<'_>), corrupt_ledger] {
        let root = TempDir::new().unwrap();
        let (old, new) = (root.path().join(OLD), root.path().join(NEW));
        seed_old(&old, false);
        let before = tree(&old);
        let mut o = opts();
        o.hook = Some(hook);
        match migrate(&old, &new, &o) {
            Outcome::Failed { step, .. } => assert_eq!(step, "verify"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!new.join(MARKER_NAME).exists(), "no marker");
        assert!(!new.join("lumen.db").exists(), "nothing moved into N");
        assert!(!new.join(STAGE_DIR).exists(), "stage cleaned");
        assert_eq!(tree(&old), before);
        assert_eq!(eff(root.path()), old.join("lumen.db"));
        // And the next run, without the fault, succeeds.
        migrated(migrate(&old, &new, &opts()));
    }
}

/// What the code did before: copy the files with no probe.
fn naive_copy(old: &Path, new: &Path) {
    std::fs::create_dir_all(new).unwrap();
    for n in ["lumen.db", "lumen.db-wal"] {
        let _ = std::fs::copy(old.join(n), new.join(n));
    }
}

#[test]
fn a_live_old_ledger_defers_instead_of_splitting() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, true);
    let mut child = spawn_child("writer", root.path());
    wait_for(&root.path().join("ready"));

    // Red: a copy with no probe leaves the child writing to L only.
    let naive = root.path().join("naive");
    naive_copy(&old, &naive);
    let at_copy = rows(&naive.join("lumen.db"));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        rows(&old.join("lumen.db")) > at_copy,
        "the ledger split: rows land only in L"
    );

    // Green: the probe sees the held ledger and nothing moves.
    let before = std::fs::read(old.join("faults.jsonl")).unwrap();
    match migrate(&old, &new, &opts()) {
        Outcome::Deferred { reason } => assert!(reason.contains("in use"), "{reason}"),
        other => panic!("expected Deferred, got {other:?}"),
    }
    assert!(!new.join(MARKER_NAME).exists());
    assert!(!new.join("lumen.db").exists());
    assert!(!new.join(STAGE_DIR).exists());
    assert_eq!(eff(root.path()), old.join("lumen.db"), "still on L");
    assert_eq!(std::fs::read(old.join("faults.jsonl")).unwrap(), before);

    // The child exits; the next try completes with every row in N.
    std::fs::write(root.path().join("stop"), "").unwrap();
    assert!(child.0.wait().unwrap().success());
    let l_rows = rows(&old.join("lumen.db"));
    assert!(l_rows > 20);
    migrated(migrate(&old, &new, &opts()));
    assert_eq!(rows(&new.join("lumen.db")), l_rows);
    assert_eq!(eff(root.path()), new.join("lumen.db"));
}

#[test]
fn a_run_killed_mid_copy_leaves_the_old_directory_whole_and_reruns() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    let before = tree(&old);
    let mut child = spawn_child("migrator", root.path());
    wait_for(&root.path().join("paused"));
    assert!(new.join(STAGE_DIR).join("lumen.db").exists(), "mid-copy");
    child.0.kill().unwrap();
    child.0.wait().unwrap();

    // Rollback-journal mode, so the probe changes no byte. A ledger still in WAL mode
    // is switched to rollback-journal mode by the probe (a header change, every row
    // intact); the split-ledger test covers that case by row count.
    assert_eq!(tree(&old), before, "L is byte-identical");
    assert!(!new.join(MARKER_NAME).exists());
    assert_eq!(eff(root.path()), old.join("lumen.db"), "resolution is L");
    assert!(root.path().join(LOCK_NAME).exists());

    // The OS dropped the lock and the ledger's lock with the process.
    migrated(migrate(&old, &new, &opts()));
    assert_eq!(rows(&new.join("lumen.db")), 20);
    assert!(!new.join(STAGE_DIR).exists());
    assert_eq!(eff(root.path()), new.join("lumen.db"));
}

#[test]
fn a_second_migrator_defers_while_the_first_holds_the_mutex() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    let f = std::fs::File::create(root.path().join(LOCK_NAME)).unwrap();
    f.lock().unwrap();
    match migrate(&old, &new, &opts()) {
        Outcome::Deferred { reason } => assert!(reason.contains("another migration")),
        other => panic!("{other:?}"),
    }
    drop(f);
    migrated(migrate(&old, &new, &opts()));
}

#[test]
fn a_spool_with_a_partial_last_line_is_staged_up_to_the_newline() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    std::fs::write(old.join("faults.jsonl"), "{\"k\":1}\n{\"k\":2}\n{\"k\":3").unwrap();
    let m = migrated(migrate(&old, &new, &opts()));
    assert_eq!(m.spool_offset, 16);
    assert_eq!(
        std::fs::read(new.join("faults.jsonl")).unwrap(),
        b"{\"k\":1}\n{\"k\":2}\n"
    );
}

#[test]
fn the_ledger_is_a_snapshot_taken_through_the_held_connection() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    // Free pages make the source file bigger than a snapshot of the same rows.
    {
        let c = Connection::open(old.join("lumen.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE junk(b); INSERT INTO junk SELECT zeroblob(4000) FROM t, t; DROP TABLE junk;",
        )
        .unwrap();
    }
    let src_len = std::fs::metadata(old.join("lumen.db")).unwrap().len();
    migrated(migrate(&old, &new, &opts()));
    let dst_len = std::fs::metadata(new.join("lumen.db")).unwrap().len();
    assert!(dst_len < src_len, "{dst_len} vs {src_len}: not a byte copy");
    assert_eq!(rows(&new.join("lumen.db")), 20);
}

static HELD_LEDGER: OnceLock<PathBuf> = OnceLock::new();
static SAW_BUSY: AtomicBool = AtomicBool::new(false);

fn probe_the_lock(p: &Phase<'_>) {
    if let Phase::Staged { .. } = p {
        let c = Connection::open(HELD_LEDGER.get().unwrap()).unwrap();
        c.busy_timeout(Duration::ZERO).unwrap();
        let r = c.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0));
        SAW_BUSY.store(r.is_err(), Ordering::SeqCst);
    }
}

#[test]
fn the_old_ledger_stays_exclusively_locked_until_the_commit() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    HELD_LEDGER.set(old.join("lumen.db")).unwrap();
    let mut o = opts();
    o.hook = Some(probe_the_lock);
    migrated(migrate(&old, &new, &o));
    assert!(SAW_BUSY.load(Ordering::SeqCst), "a reader got in mid-copy");
    assert_eq!(rows(&old.join("lumen.db")), 20, "and it is free afterwards");
}

/// On Windows, SQLite's locks are mandatory byte-range locks at 0x40000000 (1 GiB):
/// reading a held ledger larger than that through another handle fails with
/// ERROR_LOCK_VIOLATION. This one has a ledger over 1 GiB. It writes about 4 GiB
/// of scratch files, so it is ignored by default; run it on windows-latest (and
/// anywhere else) with `cargo test -p lumen-core --test legacy_migrate -- --ignored`.
#[test]
#[ignore = "writes ~4 GiB; run on windows-latest"]
fn a_ledger_over_one_gibibyte_migrates() {
    let root = TempDir::new().unwrap();
    let (old, new) = (root.path().join(OLD), root.path().join(NEW));
    seed_old(&old, false);
    {
        let c = Connection::open(old.join("lumen.db")).unwrap();
        for _ in 0..2 {
            c.execute("INSERT INTO t(v) SELECT zeroblob(550000000)", [])
                .unwrap();
        }
    }
    assert!(std::fs::metadata(old.join("lumen.db")).unwrap().len() > 1 << 30);
    let mut o = opts();
    o.probe_tries = 1;
    migrated(migrate(&old, &new, &o));
    assert_eq!(rows(&new.join("lumen.db")), 22);
}
