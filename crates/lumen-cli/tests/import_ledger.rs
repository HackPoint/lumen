//! `lumen import-ledger`, driven through the built binary: a refusal exits non-zero and says why,
//! `--dry-run` changes nothing, and a real run brings the rows in once.
//!
//! Every run gets a home and a data folder of its own: nothing here reads or writes the
//! developer's real ledger.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;

struct Sandbox {
    _t: tempfile::TempDir,
    home: PathBuf,
    /// This install's data folder, with its ledger and its settled marker.
    n: PathBuf,
    /// Where a folder to import is made.
    root: PathBuf,
}

fn sandbox(settled: bool) -> Sandbox {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("home");
    let n = t.path().join("data").join("example.current.app");
    let root = t.path().join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&n).unwrap();
    if settled {
        lumen_core::legacy::marker::write_settled(&n).unwrap();
    }
    drop(lumen_core::meter::connect_db(&n.join("lumen.db")).unwrap());
    Sandbox {
        _t: t,
        home,
        n,
        root,
    }
}

/// A folder holding a Lumen ledger with `n` rows in `read_events`.
fn old_folder(s: &Sandbox, name: &str, rows: usize) -> PathBuf {
    let dir = s.root.join(name);
    fs::create_dir_all(&dir).unwrap();
    let c = lumen_core::meter::connect_db(&dir.join("lumen.db")).unwrap();
    for i in 0..rows {
        c.execute(
            "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
               routed_via) VALUES (?1, 'Read', '/p', 1, 2, 1, 'builtin_read')",
            [format!("t{i}")],
        )
        .unwrap();
    }
    drop(c);
    // A rollback-mode ledger, as an older app that quit leaves it.
    let c = Connection::open(dir.join("lumen.db")).unwrap();
    c.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get::<_, String>(0))
        .unwrap();
    dir
}

fn lumen(s: &Sandbox, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &s.home)
        .env("USERPROFILE", &s.home)
        .env("LUMEN_DB", s.n.join("lumen.db"))
        .output()
        .expect("the lumen binary runs")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn rows(s: &Sandbox) -> i64 {
    Connection::open(s.n.join("lumen.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM read_events", [], |r| r.get(0))
        .unwrap()
}

fn path_arg(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn a_refusal_exits_non_zero_and_says_why() {
    let s = sandbox(true);
    let live = s.n.join("lumen.db");
    let out = lumen(&s, &["import-ledger", path_arg(&live)]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("live ledger"),
        "{}",
        text(&out.stderr)
    );
    assert!(text(&out.stdout).is_empty());

    let out = lumen(&s, &["import-ledger", path_arg(&s.root.join("nowhere"))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("does not exist"));

    // A start that has not finished: no marker in the data folder.
    let fresh = sandbox(false);
    let folder = old_folder(&fresh, "example.previous.app", 2);
    let out = lumen(&fresh, &["import-ledger", path_arg(&folder)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("start Lumen first"),
        "{}",
        text(&out.stderr)
    );
    assert!(folder.join("lumen.db").is_file(), "nothing moved");
}

#[test]
fn dry_run_says_what_it_would_add_and_changes_nothing() {
    let s = sandbox(true);
    let folder = old_folder(&s, "example.previous.app", 3);
    let before = rows(&s);

    let out = lumen(&s, &["import-ledger", path_arg(&folder), "--dry-run"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let said = text(&out.stdout);
    assert!(said.contains("Dry run"), "{said}");
    assert!(said.contains("read_events: 3"), "{said}");
    assert_eq!(rows(&s), before);
    assert!(folder.join("lumen.db").is_file(), "it did not move");
    assert!(!s.n.join("legacy-imports").exists());
}

#[test]
fn an_import_brings_the_rows_in_once() {
    let s = sandbox(true);
    let folder = old_folder(&s, "example.previous.app", 3);

    let out = lumen(&s, &["import-ledger", path_arg(&folder)]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("read_events: 3"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(rows(&s), 3);
    assert!(!folder.exists(), "the old name is gone");
    let moved = fs::read_dir(s.n.join("legacy-imports"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    let again = lumen(&s, &["import-ledger", path_arg(&moved)]);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("already imported"));
    assert_eq!(rows(&s), 3);
}
