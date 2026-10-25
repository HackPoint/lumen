#![cfg(debug_assertions)] // the migration's test hook exists only in debug builds
//! The migration, against temp directories and neutral folder names: migrate-and-move
//! (`legacy::commit`) on the 1.9.0 ledger schema. Tests re-run this binary as a child process: to
//! hold the old ledger open in WAL while writing, or to be killed at a chosen phase.

use lumen_core::legacy::MARKER_NAME;
use lumen_core::legacy::commit::{self, CommitOptions, Moved};
use lumen_core::legacy::marker::{self, MarkerV2, Seen};
use lumen_core::legacy::migrate::{LOCK_NAME, Phase, STAGE_DIR};
use lumen_core::legacy::retire::Ops;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::OnceLock;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

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

#[test]
fn child_entry() {
    let Ok(role) = std::env::var("LUMEN_T2_ROLE") else {
        return; // an ordinary run of the suite: nothing to do
    };
    let root = PathBuf::from(std::env::var("LUMEN_T2_ROOT").unwrap());
    match role.as_str() {
        "writer190" => v2::child_writer(&root),
        "migrator190" => v2::child_migrator(&root),
        other => panic!("unknown role {other}"),
    }
}

// --- v2: migrate and move (legacy::commit), on the 1.9.0 ledger -------------

mod v2 {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::Cell;
    use std::io;

    const PREV: &str = "example.previous.app";
    const CUR: &str = "example.current.app";
    const NOW: &str = "2026-10-19T08:00:00Z";
    const BACKUP: &str = "legacy-backup-2026-10-19";
    const DDL_190: &str = include_str!("fixtures/ledger_schema_v1.9.0.sql");

    fn copts() -> CommitOptions {
        let mut o = CommitOptions::new(NOW);
        o.probe_tries = 2;
        o.probe_delay = Duration::from_millis(20);
        o.move_tries = 2;
        o.move_delay = Duration::from_millis(10);
        o
    }

    /// Real renames; the link half of the seam is never used by the new code.
    struct Real;
    impl Ops for Real {}

    /// Refuses the first `n` renames, as an antivirus scanner or a sharing violation would.
    struct Refuse(Cell<u32>);
    impl Ops for Refuse {
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            if self.0.get() > 0 {
                self.0.set(self.0.get() - 1);
                return Err(io::Error::other("refused"));
            }
            std::fs::rename(from, to)
        }
    }

    fn migrated(out: commit::Outcome) -> (MarkerV2, Moved) {
        match out {
            commit::Outcome::Migrated { marker, moved } => (marker, moved),
            other => panic!("expected Migrated, got {other:?}"),
        }
    }

    fn events(db: &Path) -> i64 {
        Connection::open(db)
            .unwrap()
            .query_row("SELECT count(*) FROM read_events", [], |r| r.get(0))
            .unwrap()
    }

    fn add_events(c: &Connection, n: i64) {
        for i in 0..n {
            c.execute(
                "INSERT INTO read_events(ts,tool,path,tokens_returned,full_tokens,saved_tokens,routed_via) \
                 VALUES ('2026-10-01T00:00:00Z','smart_read',?1,10,100,90,'smart_read')",
                [format!("/p/{i}")],
            )
            .unwrap();
        }
    }

    /// A previous folder holding a 1.9.0 ledger and every kind of file the app keeps.
    fn seed_prev(l: &Path, wal: bool, n: i64) {
        std::fs::create_dir_all(l.join("bin")).unwrap();
        std::fs::create_dir_all(l.join("logs")).unwrap();
        let c = Connection::open(l.join("lumen.db")).unwrap();
        let mode = if wal { "WAL" } else { "DELETE" };
        c.query_row(&format!("PRAGMA journal_mode={mode}"), [], |_| Ok(()))
            .unwrap();
        c.execute_batch(DDL_190).unwrap();
        add_events(&c, n);
        c.execute(
            "INSERT INTO faults(ts,kind) VALUES ('2026-10-01T00:00:00Z','x')",
            [],
        )
        .unwrap();
        drop(c);
        std::fs::write(l.join("faults.jsonl"), "{\"k\":1}\n{\"k\":2}\n").unwrap();
        std::fs::write(l.join("faults.jsonl.draining"), "{\"k\":0}\n").unwrap();
        std::fs::write(l.join("lumen_settings.json"), "{\"a\":true}").unwrap();
        std::fs::write(l.join("update_check.json"), "{\"u\":1}").unwrap();
        std::fs::write(l.join("lumen_call_toolu_1.json"), "{}").unwrap();
        std::fs::write(l.join("bin").join("lumen-mcp"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(l.join("logs").join("lumen.log"), "line\n").unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn dirs(root: &Path) -> (PathBuf, PathBuf) {
        (root.join(PREV), root.join(CUR))
    }

    // --- child processes ---------------------------------------------------

    /// The old app: WAL, one connection held open, rows landing until told to stop.
    pub fn child_writer(root: &Path) {
        let c = Connection::open(root.join(PREV).join("lumen.db")).unwrap();
        c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .unwrap();
        std::fs::write(root.join("ready"), "").unwrap();
        while !root.join("stop").exists() {
            add_events(&c, 1);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pause_after_ledger_rename(p: &Phase<'_>) {
        if let Phase::Commit { rel } = p
            && rel.ends_with("lumen.db")
        {
            pause("paused");
        }
    }

    fn pause_at_commit_point(p: &Phase<'_>) {
        if let Phase::Committed = p {
            pause("paused");
        }
    }

    fn pause_mid_copy(p: &Phase<'_>) {
        if let Phase::Copied { rel } = p
            && rel.ends_with("lumen.db")
        {
            pause("paused");
        }
    }

    fn pause(flag: &str) {
        let root = PathBuf::from(std::env::var("LUMEN_T2_ROOT").unwrap());
        std::fs::write(root.join(flag), "").unwrap();
        std::thread::sleep(Duration::from_secs(600));
    }

    /// The new app, killed by the parent while the hook of `LUMEN_T2_AT` sleeps.
    pub fn child_migrator(root: &Path) {
        let mut o = copts();
        o.hook = Some(match std::env::var("LUMEN_T2_AT").unwrap().as_str() {
            "ledger-renamed" => pause_after_ledger_rename,
            "committed" => pause_at_commit_point,
            "mid-copy" => pause_mid_copy,
            other => panic!("unknown pause {other}"),
        });
        let (l, n) = dirs(root);
        let out = commit::migrate(&l, &n, &o, &Real);
        panic!("the hook should have blocked: {out:?}");
    }

    fn kill_migrator_at(root: &Path, at: &str) {
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
                .env("LUMEN_T2_ROLE", "migrator190")
                .env("LUMEN_T2_ROOT", root)
                .env("LUMEN_T2_AT", at)
                .spawn()
                .unwrap(),
        );
        wait_for(&root.join("paused"));
        child.0.kill().unwrap();
        child.0.wait().unwrap();
    }

    // --- what is taken, and what stays -------------------------------------

    #[test]
    fn the_spool_and_everything_but_lumens_own_files_stay_in_the_old_folder() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, true, 20);
        let before = tree(&l);
        let (_, moved) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(moved, Moved::Done);

        // N holds Lumen's own files and nothing else of L's.
        let mut want = vec![
            BACKUP,
            ".legacy-migration.json",
            LOCK_NAME,
            "lumen.db",
            "lumen_call_toolu_1.json",
            "lumen_settings.json",
            "update_check.json",
        ];
        want.sort();
        assert_eq!(names(&n), want);

        // The spool, the draining file, bin/ and logs/ are in L, which is now the backup.
        let data = n.join(BACKUP).join("data");
        for f in [
            "faults.jsonl",
            "faults.jsonl.draining",
            "bin/lumen-mcp",
            "logs/lumen.log",
        ] {
            assert_eq!(std::fs::read(data.join(f)).unwrap(), before[f], "{f}");
        }
    }

    #[test]
    fn a_committed_migration_leaves_nothing_at_the_old_path() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, true, 20);
        let before = tree(&l);
        migrated(commit::migrate(&l, &n, &copts(), &Real));

        assert!(
            l.symlink_metadata().is_err(),
            "nothing, not even a link, at the old path"
        );
        let data = n.join(BACKUP).join("data");
        assert!(n.join(BACKUP).join(".moved_at").is_file());
        // B/data holds L byte for byte, except the ledger's header (the probe left WAL mode).
        let mut after = tree(&data);
        let (a, b) = (
            after.remove("lumen.db").unwrap(),
            before["lumen.db"].clone(),
        );
        assert!(!a.is_empty() && !b.is_empty());
        let mut expect = before.clone();
        expect.remove("lumen.db");
        expect.remove("lumen.db-wal");
        expect.remove("lumen.db-shm");
        assert_eq!(after, expect);
        assert_eq!(
            events(&data.join("lumen.db")),
            20,
            "every row is in the backup"
        );
        assert_eq!(events(&n.join("lumen.db")), 20, "and in N");
        assert!(matches!(marker::read(&n), Seen::Migrated(_)));
        assert!(marker::read_pending(&n).is_none());
        assert!(!n.join(STAGE_DIR).exists());
    }

    #[test]
    fn the_marker_names_the_folder_and_the_backup_and_is_written_once() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 7);
        let (m, _) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(m.format, 2);
        assert_eq!(m.state, "migrated");
        assert_eq!(m.source, PREV);
        assert_eq!(Path::new(&m.source_dir), l);
        assert_eq!(m.backup, BACKUP, "relative, a name and no path");
        assert_eq!(m.committed_at, NOW);
        assert_eq!(m.hwm["read_events"], 7);
        assert_eq!(m.hwm["faults"], 1);

        // hwm_sig is the SHA-256 of the quoted columns of the row at the mark, computed here
        // independently of the migration.
        let c = Connection::open(n.join("lumen.db")).unwrap();
        for (table, mark) in [("read_events", 7), ("faults", 1)] {
            let cols: Vec<String> = c
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            let key = cols
                .iter()
                .map(|c| format!("quote(\"{c}\")"))
                .collect::<Vec<_>>()
                .join(" || ',' || ");
            let row: String = c
                .query_row(
                    &format!("SELECT {key} FROM {table} WHERE rowid = {mark}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let sig: String = Sha256::digest(row.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(m.hwm_sig[table], sig, "{table}");
        }
        assert!(
            !m.hwm_sig.contains_key("turns"),
            "keyed tables carry no signature"
        );

        // Written once: a second run changes nothing, and the writer refuses to replace it.
        let bytes = std::fs::read(n.join(MARKER_NAME)).unwrap();
        assert_eq!(
            commit::migrate(&l, &n, &copts(), &Real),
            commit::Outcome::NothingToDo
        );
        assert_eq!(std::fs::read(n.join(MARKER_NAME)).unwrap(), bytes);
        assert_eq!(
            marker::write_marker(&n, &m).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(marker::read(&n), Seen::Migrated(m.into()));
    }

    #[test]
    fn a_taken_backup_name_gets_a_suffix() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 3);
        std::fs::create_dir_all(n.join(BACKUP).join("data")).unwrap();
        let (m, _) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(m.backup, format!("{BACKUP}-2"));
        assert!(n.join(&m.backup).join("data").join("lumen.db").is_file());
    }

    // --- what is in the way in N -------------------------------------------

    #[test]
    fn an_existing_ledger_is_moved_aside_with_its_wal_and_shm() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        std::fs::create_dir_all(&n).unwrap();
        // A ledger in N with rows that live only in its WAL: a live connection holds the
        // checkpoint off while the three files are copied, as a killed process leaves them.
        let live = root.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let c = seed_db(&live.join("lumen.db"), true, 0);
        c.query_row("PRAGMA wal_autocheckpoint=0", [], |_| Ok(()))
            .ok();
        for i in 0..5 {
            c.execute("INSERT INTO t(v) VALUES (?1)", [format!("foreign{i}")])
                .unwrap();
        }
        for sfx in ["", "-wal", "-shm"] {
            std::fs::copy(
                live.join(format!("lumen.db{sfx}")),
                n.join(format!("lumen.db{sfx}")),
            )
            .unwrap();
        }
        drop(c);
        assert!(n.join("lumen.db-wal").metadata().unwrap().len() > 0);

        migrated(commit::migrate(&l, &n, &copts(), &Real));

        // The migrated ledger is the previous folder's, with none of N's rows.
        assert_eq!(events(&n.join("lumen.db")), 20);
        // The set that was in the way is whole, under one name, and still has its rows.
        assert!(n.join("lumen.db.pre-legacy-wal").exists());
        assert_eq!(rows(&n.join("lumen.db.pre-legacy")), 5);
        assert!(
            !n.join("lumen.db-wal").exists(),
            "no WAL of N's is left beside the migrated ledger"
        );
    }

    #[test]
    fn a_second_aside_never_overwrites_the_first_set() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        std::fs::create_dir_all(&n).unwrap();
        drop(seed_db(&n.join("lumen.db"), false, 3));
        // A WAL left by an earlier aside is in the way of the first free name.
        std::fs::write(n.join("lumen.db.pre-legacy-wal"), b"earlier").unwrap();
        std::fs::write(n.join("lumen.db-wal"), b"stale").unwrap();
        migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(
            std::fs::read(n.join("lumen.db.pre-legacy-wal")).unwrap(),
            b"earlier"
        );
        assert_eq!(
            std::fs::read(n.join("lumen.db.pre-legacy-1-wal")).unwrap(),
            b"stale"
        );
        assert_eq!(rows(&n.join("lumen.db.pre-legacy-1")), 3);
        assert!(!n.join("lumen.db-wal").exists());
    }

    #[test]
    fn a_wal_with_no_ledger_beside_it_is_set_aside_too() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        std::fs::create_dir_all(&n).unwrap();
        std::fs::write(n.join("lumen.db-wal"), b"orphan").unwrap();
        migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert!(!n.join("lumen.db-wal").exists(), "it would be replayed");
        assert_eq!(
            std::fs::read(n.join("lumen.db.pre-legacy-wal")).unwrap(),
            b"orphan"
        );
        assert_eq!(events(&n.join("lumen.db")), 20);
    }

    // --- crash points ------------------------------------------------------

    fn has_pre_legacy(n: &Path) -> Vec<String> {
        names(n)
            .into_iter()
            .filter(|f| f.contains(".pre-legacy"))
            .collect()
    }

    #[test]
    fn a_commit_killed_halfway_leaves_no_second_copy() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        kill_migrator_at(root.path(), "ledger-renamed");

        // Halfway: the ledger is in N, the others are still staged, no marker.
        assert_eq!(events(&n.join("lumen.db")), 20);
        assert!(n.join(STAGE_DIR).join(commit::INTENT_NAME).is_file());
        assert!(!n.join(MARKER_NAME).exists());
        assert!(!n.join("lumen_settings.json").exists());
        assert!(l.join("lumen.db").is_file(), "L is whole");

        // The rerun recognises the ledger as its own, and completes.
        let (m, moved) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(moved, Moved::Done);
        assert_eq!(
            has_pre_legacy(&n),
            Vec::<String>::new(),
            "no .pre-legacy holds L's rows"
        );
        assert_eq!(events(&n.join("lumen.db")), 20);
        assert_eq!(m.hwm["read_events"], 20);
        assert!(n.join("lumen_settings.json").is_file());
        assert!(!n.join(STAGE_DIR).exists());
    }

    static N_MARKER: OnceLock<PathBuf> = OnceLock::new();

    /// After the ledger is in N, the marker's temp name is made unusable, so the marker write
    /// fails, as ENOSPC or a sharing violation on its temp file would.
    fn block_marker_write(p: &Phase<'_>) {
        if let Phase::Commit { rel } = p
            && rel.ends_with("update_check.json")
        {
            std::fs::create_dir_all(N_MARKER.get().unwrap().join(".legacy-migration.tmp")).unwrap();
        }
    }

    /// A commit that fails by returning (not by a kill) takes back what it renamed into N, so
    /// the rerun does not set the staged ledger aside as a ledger someone else left there.
    #[test]
    fn a_commit_that_fails_at_the_marker_leaves_no_second_copy() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        N_MARKER.set(n.clone()).unwrap();
        let mut o = copts();
        o.hook = Some(block_marker_write);
        let out = commit::migrate(&l, &n, &o, &Real);
        assert!(matches!(out, commit::Outcome::Failed { .. }), "{out:?}");
        assert!(!n.join("lumen.db").exists(), "the ledger was taken back");
        assert!(l.join("lumen.db").is_file(), "L is whole");

        std::fs::remove_dir(n.join(".legacy-migration.tmp")).unwrap();
        let (_, moved) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(moved, Moved::Done);
        assert_eq!(has_pre_legacy(&n), Vec::<String>::new());
        assert_eq!(events(&n.join("lumen.db")), 20);
    }

    #[test]
    fn a_commit_that_keeps_failing_adds_no_copy_per_start() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        std::fs::create_dir_all(n.join(".legacy-migration.tmp")).unwrap();
        for _ in 0..3 {
            let out = commit::migrate(&l, &n, &copts(), &Real);
            assert!(matches!(out, commit::Outcome::Failed { .. }), "{out:?}");
        }
        assert_eq!(has_pre_legacy(&n), Vec::<String>::new());
        std::fs::remove_dir(n.join(".legacy-migration.tmp")).unwrap();
        migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(has_pre_legacy(&n), Vec::<String>::new());
    }

    static N_RENAME: OnceLock<PathBuf> = OnceLock::new();

    /// The next staged file vanishes, so its rename into N fails after the ledger's succeeded.
    fn drop_next_staged(p: &Phase<'_>) {
        if let Phase::Commit { rel } = p
            && rel.ends_with("lumen.db")
        {
            let stage = N_RENAME.get().unwrap().join(STAGE_DIR);
            std::fs::remove_file(stage.join("lumen_call_toolu_1.json")).unwrap();
        }
    }

    #[test]
    fn a_commit_that_fails_at_a_rename_leaves_no_second_copy() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        N_RENAME.set(n.clone()).unwrap();
        let mut o = copts();
        o.hook = Some(drop_next_staged);
        let out = commit::migrate(&l, &n, &o, &Real);
        assert!(matches!(out, commit::Outcome::Failed { .. }), "{out:?}");
        let (_, moved) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(moved, Moved::Done);
        assert_eq!(has_pre_legacy(&n), Vec::<String>::new());
        assert_eq!(events(&n.join("lumen.db")), 20);
    }

    #[test]
    fn a_run_killed_after_the_commit_moves_the_folder_at_the_next_start() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        kill_migrator_at(root.path(), "committed");

        // Committed, not moved: the marker is there and L is where it was.
        let Seen::Migrated(m) = marker::read(&n) else {
            panic!("the marker is the commit point");
        };
        assert!(l.join("lumen.db").is_file());
        assert!(!n.join(BACKUP).join("data").exists());
        assert_eq!(
            commit::migrate(&l, &n, &copts(), &Real),
            commit::Outcome::NothingToDo,
            "a committed migration never migrates again"
        );

        // The next start retries the move.
        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Done);
        assert!(l.symlink_metadata().is_err());
        assert_eq!(events(&n.join(BACKUP).join("data").join("lumen.db")), 20);
        assert!(n.join(BACKUP).join(".moved_at").is_file());
        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Already);
    }

    #[test]
    fn a_move_killed_before_its_stamp_gets_the_stamp_at_the_next_start() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 5);
        let (m, _) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        std::fs::remove_file(n.join(BACKUP).join(".moved_at")).unwrap();
        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Already);
        assert!(n.join(BACKUP).join(".moved_at").is_file());
    }

    #[test]
    fn a_move_refused_by_the_os_keeps_the_commit_and_retries() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        // Refuse enough renames for every try of the first call (Windows tries more than once).
        let ops = Refuse(Cell::new(copts().move_tries));
        let (m, moved) = migrated(commit::migrate(&l, &n, &copts(), &ops));
        assert!(
            matches!(moved, Moved::Deferred(ref why) if why.contains("refused")),
            "{moved:?}"
        );

        // The commit stands: marker, ledger in N, L still a real directory, nothing in B.
        assert!(matches!(marker::read(&n), Seen::Migrated(_)));
        assert_eq!(events(&n.join("lumen.db")), 20);
        assert!(l.join("lumen.db").is_file());
        assert!(!n.join(BACKUP).join("data").exists());

        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Done);
        assert!(l.symlink_metadata().is_err());
        assert_eq!(events(&n.join(BACKUP).join("data").join("lumen.db")), 20);
    }

    #[cfg(unix)]
    #[test]
    fn a_retried_move_waits_while_an_old_app_holds_the_ledger() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        let ops = Refuse(Cell::new(u32::MAX));
        let (m, moved) = migrated(commit::migrate(&l, &n, &copts(), &ops));
        assert!(matches!(moved, Moved::Deferred(_)));

        // An old app opens the ledger in WAL and keeps writing.
        let mut child = spawn_child("writer190", root.path());
        wait_for(&root.path().join("ready"));
        match commit::retry_move(&n, &m, &copts(), &Real) {
            Moved::Deferred(why) => assert!(why.contains("in use"), "{why}"),
            other => panic!("expected Deferred while the ledger is held, got {other:?}"),
        }
        assert!(l.join("lumen.db").is_file(), "L stays where it is");
        assert!(!n.join(BACKUP).join("data").exists());

        // The old app exits; the next try moves it, with every row it wrote.
        std::fs::write(root.path().join("stop"), "").unwrap();
        assert!(child.0.wait().unwrap().success());
        let l_rows = events(&l.join("lumen.db"));
        assert!(l_rows > 20);
        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Done);
        assert_eq!(
            events(&n.join(BACKUP).join("data").join("lumen.db")),
            l_rows
        );
    }

    // --- the move and the lock ---------------------------------------------

    // Used only by the unix test below: Windows cannot move a folder whose ledger is held open.
    #[cfg(unix)]
    static BUSY_THROUGH_MOVE: AtomicBool = AtomicBool::new(false);
    #[cfg(unix)]
    static MOVED_AT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    #[cfg(unix)]
    fn probe_the_moved_ledger(p: &Phase<'_>) {
        if let Phase::Moved { dir } = p {
            MOVED_AT.set(dir.to_path_buf()).ok();
            let c = Connection::open(dir.join("lumen.db")).unwrap();
            c.busy_timeout(Duration::ZERO).unwrap();
            let r = c.query_row("SELECT count(*) FROM read_events", [], |r| {
                r.get::<_, i64>(0)
            });
            BUSY_THROUGH_MOVE.store(r.is_err(), Ordering::SeqCst);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_old_ledger_stays_exclusively_locked_through_the_move() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        let mut o = copts();
        o.hook = Some(probe_the_moved_ledger);
        migrated(commit::migrate(&l, &n, &o, &Real));
        assert_eq!(MOVED_AT.get().unwrap(), &n.join(BACKUP).join("data"));
        assert!(
            BUSY_THROUGH_MOVE.load(Ordering::SeqCst),
            "a reader got into the ledger after it was renamed"
        );
        assert_eq!(
            events(&n.join(BACKUP).join("data").join("lumen.db")),
            20,
            "and it is free afterwards"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_connection_opened_before_the_move_cannot_write_into_the_backup() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        // An old process with the ledger open and idle: no lock, so the probe takes it.
        let c = Connection::open(l.join("lumen.db")).unwrap();
        c.query_row("SELECT count(*) FROM read_events", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
        migrated(commit::migrate(&l, &n, &copts(), &Real));

        let wrote = c.execute(
            "INSERT INTO read_events(ts,tool,path,tokens_returned,full_tokens,saved_tokens,routed_via) \
             VALUES ('2026-10-02T00:00:00Z','smart_read','/late',1,1,0,'smart_read')",
            [],
        );
        assert!(wrote.is_err(), "the write went through to {:?}", l);
        assert_eq!(
            wrote.unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
        drop(c);
        assert!(
            l.symlink_metadata().is_err(),
            "no file was made at the old path"
        );
        assert_eq!(
            events(&n.join(BACKUP).join("data").join("lumen.db")),
            20,
            "the backup did not change"
        );
    }

    #[cfg(windows)]
    #[test]
    fn an_open_handle_in_the_old_folder_defers_the_move_until_it_closes() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        // A handle from another process (an indexer, a scanner) with no sharing at all.
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(l.join("logs").join("lumen.log"))
            .unwrap();
        let (m, moved) = migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert!(matches!(moved, Moved::Deferred(_)), "{moved:?}");
        assert!(l.join("lumen.db").is_file());
        assert!(matches!(marker::read(&n), Seen::Migrated(_)));
        drop(held);
        assert_eq!(commit::retry_move(&n, &m, &copts(), &Real), Moved::Done);
        assert_eq!(events(&n.join(BACKUP).join("data").join("lumen.db")), 20);
    }

    // --- still green on the new path ---------------------------------------

    #[test]
    fn a_live_old_ledger_defers_instead_of_splitting() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, true, 20);
        let mut child = spawn_child("writer190", root.path());
        wait_for(&root.path().join("ready"));

        match commit::migrate(&l, &n, &copts(), &Real) {
            commit::Outcome::Deferred { reason } => assert!(reason.contains("in use"), "{reason}"),
            other => panic!("expected Deferred, got {other:?}"),
        }
        assert!(!n.join(MARKER_NAME).exists());
        assert!(!n.join("lumen.db").exists());
        assert!(!n.join(STAGE_DIR).exists());
        // Resolution stays on L through the pending file: nothing splits.
        let pending = marker::read_pending(&n).expect("pending is written while deferred");
        assert_eq!(pending.source, PREV);
        assert_eq!(pending.dir, l);

        std::fs::write(root.path().join("stop"), "").unwrap();
        assert!(child.0.wait().unwrap().success());
        let l_rows = events(&l.join("lumen.db"));
        assert!(l_rows > 20);
        migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(events(&n.join("lumen.db")), l_rows);
        assert!(marker::read_pending(&n).is_none());
    }

    #[test]
    fn a_run_killed_mid_copy_leaves_the_old_directory_whole_and_reruns() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        let before = tree(&l);
        kill_migrator_at(root.path(), "mid-copy");

        assert!(n.join(STAGE_DIR).join("lumen.db").exists(), "mid-copy");
        assert_eq!(tree(&l), before, "L is byte-identical");
        assert!(!n.join(MARKER_NAME).exists());
        assert!(!n.join("lumen.db").exists());
        assert!(n.join(LOCK_NAME).exists());

        migrated(commit::migrate(&l, &n, &copts(), &Real));
        assert_eq!(events(&n.join("lumen.db")), 20);
        assert!(!n.join(STAGE_DIR).exists());
    }

    #[test]
    fn a_second_migrator_defers_while_the_first_holds_the_mutex() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 5);
        std::fs::create_dir_all(&n).unwrap();
        let f = std::fs::File::create(n.join(LOCK_NAME)).unwrap();
        f.lock().unwrap();
        match commit::migrate(&l, &n, &copts(), &Real) {
            commit::Outcome::Deferred { reason } => assert!(reason.contains("another migration")),
            other => panic!("{other:?}"),
        }
        assert!(l.join("lumen.db").is_file());
        drop(f);
        migrated(commit::migrate(&l, &n, &copts(), &Real));
    }

    /// A holder that lets go within the wait, as a spawned child's copy of the descriptor does
    /// once its exec closes it, is waited for: it is no migration.
    #[test]
    fn a_lock_held_for_a_moment_is_waited_for() {
        let root = TempDir::new().unwrap();
        let (_, n) = dirs(root.path());
        std::fs::create_dir_all(&n).unwrap();
        let f = std::fs::File::create(n.join(LOCK_NAME)).unwrap();
        f.lock().unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(f);
        });
        assert!(
            commit::take_lock(&n).is_ok(),
            "a lock released in 100 ms was reported held"
        );
        holder.join().unwrap();
    }

    #[test]
    fn the_ledger_is_a_snapshot_taken_through_the_held_connection() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        // Free pages make the source file bigger than a snapshot of the same rows.
        {
            let c = Connection::open(l.join("lumen.db")).unwrap();
            c.execute_batch(
                "CREATE TABLE junk(b); INSERT INTO junk SELECT zeroblob(4000) FROM read_events, read_events; DROP TABLE junk;",
            )
            .unwrap();
        }
        let src_len = std::fs::metadata(l.join("lumen.db")).unwrap().len();
        migrated(commit::migrate(&l, &n, &copts(), &Real));
        let dst_len = std::fs::metadata(n.join("lumen.db")).unwrap().len();
        assert!(dst_len < src_len, "{dst_len} vs {src_len}: not a byte copy");
        assert_eq!(events(&n.join("lumen.db")), 20);
    }

    #[test]
    fn a_link_at_the_old_path_is_not_a_previous_folder() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        let real = root.path().join("real");
        seed_prev(&real, false, 3);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &l).unwrap();
        #[cfg(unix)]
        assert_eq!(
            commit::migrate(&l, &n, &copts(), &Real),
            commit::Outcome::NothingToDo
        );
        #[cfg(not(unix))]
        let _ = (&l, &n);
    }

    /// On Windows, SQLite's locks are mandatory byte-range locks at 0x40000000 (1 GiB):
    /// reading a held ledger larger than that through another handle fails with
    /// ERROR_LOCK_VIOLATION. This one has a ledger over 1 GiB. It writes about 4 GiB of scratch
    /// files, so it is ignored by default; run it on windows-latest (and anywhere else) with
    /// `cargo test -p lumen-core --test legacy_migrate -- --ignored`.
    #[test]
    #[ignore = "writes ~4 GiB; run on windows-latest"]
    fn a_ledger_over_one_gibibyte_migrates() {
        let root = TempDir::new().unwrap();
        let (l, n) = dirs(root.path());
        seed_prev(&l, false, 20);
        {
            let c = Connection::open(l.join("lumen.db")).unwrap();
            add_events(&c, 2);
            c.execute_batch("CREATE TABLE big(b BLOB)").unwrap();
            for _ in 0..2 {
                c.execute("INSERT INTO big SELECT zeroblob(550000000)", [])
                    .unwrap();
            }
        }
        assert!(std::fs::metadata(l.join("lumen.db")).unwrap().len() > 1 << 30);
        let mut o = copts();
        o.probe_tries = 1;
        migrated(commit::migrate(&l, &n, &o, &Real));
        assert_eq!(events(&n.join("lumen.db")), 22);
    }
}
