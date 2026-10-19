use super::*;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::time::Duration;
use tempfile::TempDir;

const NEW: &str = "example.current.app";
const PREV: &str = "example.previous.app";

/// The columns discovery reads, plus a couple more, as every release has them.
const LUMEN_SCHEMA: &str = "
    CREATE TABLE read_events (id INTEGER PRIMARY KEY, ts TEXT NOT NULL, tool TEXT NOT NULL,
        path TEXT, tokens_returned INTEGER NOT NULL, full_tokens INTEGER NOT NULL,
        saved_tokens INTEGER NOT NULL, routed_via TEXT, extra TEXT);
    CREATE TABLE turns (message_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, ts TEXT NOT NULL,
        input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
        cache_read_input_tokens INTEGER NOT NULL, cache_creation_input_tokens INTEGER NOT NULL);
    INSERT INTO read_events (ts, tool, tokens_returned, full_tokens, saved_tokens)
        VALUES ('2026-01-01T00:00:00Z', 'smart_read', 1, 2, 1);
";

fn ledger(dir: &Path, wal: bool) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let db = dir.join("lumen.db");
    let c = Connection::open(&db).unwrap();
    if wal {
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
    }
    c.execute_batch(LUMEN_SCHEMA).unwrap();
    drop(c);
    db
}

fn stamp(db: &Path, secs_after_epoch: u64) {
    let f = fs::File::options().write(true).open(db).unwrap();
    f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs_after_epoch))
        .unwrap();
}

fn found(root: &Path, hints: &Hints) -> Discovery {
    discover(&[root], NEW, hints)
}

#[test]
fn only_a_lumen_ledger_counts() {
    let root = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    // The one that counts.
    ledger(&root.path().join(PREV), true);
    // Each of these holds a real Lumen ledger, or looks like one, and must be ignored.
    ledger(&root.path().join(NEW), true);
    ledger(&root.path().join(".hidden.app"), false);
    ledger(&root.path().join(GENERIC_DEV_ID), false);
    // A symlinked folder (a junction on Windows) whose target is a Lumen ledger.
    let target = elsewhere.path().join("target");
    ledger(&target, false);
    let link = root.path().join("linked.app");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[cfg(windows)]
    {
        let out = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(out.status.success(), "mklink /J: {out:?}");
    }
    // A lumen.db that is not SQLite.
    fs::create_dir_all(root.path().join("text.app")).unwrap();
    fs::write(
        root.path().join("text.app/lumen.db"),
        b"not a database at all",
    )
    .unwrap();
    // A file that is too short to be one.
    fs::create_dir_all(root.path().join("short.app")).unwrap();
    fs::write(root.path().join("short.app/lumen.db"), b"SQLite").unwrap();
    // A SQLite file without Lumen's columns, and one with the tables but a column short.
    let other = root.path().join("other.app");
    fs::create_dir_all(&other).unwrap();
    Connection::open(other.join("lumen.db"))
        .unwrap()
        .execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    let short = root.path().join("short-cols.app");
    fs::create_dir_all(&short).unwrap();
    Connection::open(short.join("lumen.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE read_events (ts TEXT, tool TEXT);
             CREATE TABLE turns (message_id TEXT, session_id TEXT, ts TEXT);",
        )
        .unwrap();
    // A folder without a ledger, and a plain file in the root.
    fs::create_dir_all(root.path().join("empty.app")).unwrap();
    fs::write(root.path().join("a-file.app"), b"x").unwrap();
    // A directory named lumen.db is not a ledger file.
    fs::create_dir_all(root.path().join("dir-db.app/lumen.db")).unwrap();

    let d = found(root.path(), &Hints::default());
    assert_eq!(d.chosen.as_ref().map(|c| c.name.as_str()), Some(PREV));
    assert_eq!(d.chosen.unwrap().dir, root.path().join(PREV));
    assert!(d.others.is_empty(), "others: {:?}", d.others);
    assert!(d.unknown.is_empty(), "unknown: {:?}", d.unknown);
}

#[test]
fn n_is_ignored_even_when_it_is_the_only_ledger_and_in_any_case() {
    let root = TempDir::new().unwrap();
    ledger(&root.path().join(NEW.to_ascii_uppercase()), false);
    let d = found(root.path(), &Hints::default());
    // N folds case where the file system does; elsewhere that name is a different folder.
    if cfg!(any(windows, target_os = "macos")) {
        assert!(d.chosen.is_none() && d.may_settle());
    } else {
        assert!(d.chosen.is_some());
    }
}

#[test]
fn a_root_that_is_missing_or_listed_twice_is_harmless() {
    let root = TempDir::new().unwrap();
    ledger(&root.path().join(PREV), false);
    let gone = root.path().join("no-such-root");
    let d = discover(&[&gone, root.path(), root.path()], NEW, &Hints::default());
    assert_eq!(d.chosen.unwrap().name, PREV);
    assert!(d.others.is_empty(), "a root listed twice is scanned once");
}

/// Names, bytes and mtimes of everything under `dir`.
fn snapshot(dir: &Path) -> BTreeMap<String, (Option<Vec<u8>>, Option<SystemTime>)> {
    fn walk(
        base: &Path,
        dir: &Path,
        out: &mut BTreeMap<String, (Option<Vec<u8>>, Option<SystemTime>)>,
    ) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let md = fs::symlink_metadata(&p).unwrap();
            if md.is_dir() {
                out.insert(format!("{rel}/"), (None, md.modified().ok()));
                walk(base, &p, out);
            } else {
                out.insert(rel, (Some(fs::read(&p).unwrap()), md.modified().ok()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

#[test]
fn discovery_creates_and_changes_no_file() {
    // An idle WAL ledger (the header says WAL, no -wal or -shm beside it) and a rollback one.
    // A `mode=ro` open of the first makes a -shm; `immutable=1` makes nothing.
    let root = TempDir::new().unwrap();
    ledger(&root.path().join("wal.app"), true);
    ledger(&root.path().join("rollback.app"), false);
    for (name, extra) in [
        ("wal.app", "lumen.db-shm"),
        ("rollback.app", "lumen.db-journal"),
    ] {
        let dir = root.path().join(name);
        assert!(
            !dir.join(extra).exists(),
            "{name}: the fixture must start idle"
        );
    }
    let before = snapshot(root.path());
    let d = found(root.path(), &Hints::default());
    assert!(d.chosen.is_some() && d.others.len() == 1);
    assert_eq!(
        snapshot(root.path()),
        before,
        "discovery created or changed a file"
    );
    // The same through the verdict alone, and again after the first look.
    for name in ["wal.app", "rollback.app"] {
        assert_eq!(
            is_lumen_ledger(&root.path().join(name).join("lumen.db")),
            Verdict::Lumen
        );
    }
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn a_ledger_whose_schema_is_only_in_its_wal_is_unknown_not_absent() {
    let root = TempDir::new().unwrap();
    // A new ledger in WAL mode whose tables have not been checkpointed: the main file holds
    // the header and nothing else, and the -wal holds the rest.
    let dir = root.path().join(PREV);
    fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lumen.db");
    let c = Connection::open(&db).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(c);
    assert_eq!(
        is_lumen_ledger(&db),
        Verdict::NotLumen,
        "no -wal: a database with no tables"
    );
    fs::write(
        dir.join("lumen.db-wal"),
        b"frames the main file does not have yet",
    )
    .unwrap();
    assert!(matches!(is_lumen_ledger(&db), Verdict::Unknown(_)));

    let d = found(root.path(), &Hints::default());
    assert!(d.chosen.is_none() && d.others.is_empty());
    assert_eq!(d.unknown, vec![dir.clone()]);

    // An empty -wal says nothing.
    fs::write(dir.join("lumen.db-wal"), b"").unwrap();
    assert_eq!(is_lumen_ledger(&db), Verdict::NotLumen);

    // A zero-length main file with a -wal is the same case; with none it is not a ledger.
    fs::write(&db, b"").unwrap();
    assert_eq!(is_lumen_ledger(&db), Verdict::NotLumen);
    fs::write(dir.join("lumen.db-wal"), b"x").unwrap();
    assert!(matches!(is_lumen_ledger(&db), Verdict::Unknown(_)));
}

#[cfg(unix)]
#[test]
fn a_live_writers_uncheckpointed_ledger_is_unknown() {
    // The real thing: copy a ledger while a writer holds it open and has not checkpointed.
    let root = TempDir::new().unwrap();
    let live = root.path().join("live");
    fs::create_dir_all(&live).unwrap();
    let c = Connection::open(live.join("lumen.db")).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    c.execute_batch(LUMEN_SCHEMA).unwrap();
    let dir = root.path().join(PREV);
    fs::create_dir_all(&dir).unwrap();
    fs::copy(live.join("lumen.db"), dir.join("lumen.db")).unwrap();
    fs::copy(live.join("lumen.db-wal"), dir.join("lumen.db-wal")).unwrap();
    drop(c);
    assert!(matches!(
        is_lumen_ledger(&dir.join("lumen.db")),
        Verdict::Unknown(_)
    ));
}

#[cfg(unix)]
#[test]
fn a_ledger_that_cannot_be_read_is_unknown() {
    use std::os::unix::fs::PermissionsExt;
    let root = TempDir::new().unwrap();
    let db = ledger(&root.path().join(PREV), false);
    fs::set_permissions(&db, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::File::open(&db).is_err();
    let d = found(root.path(), &Hints::default());
    fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
    if unreadable {
        assert_eq!(d.unknown, vec![root.path().join(PREV)]);
        assert!(!d.may_settle());
    }
}

#[test]
fn unknown_blocks_settling() {
    assert!(Discovery::default().may_settle());
    let unknown = Discovery {
        unknown: vec![PathBuf::from("/r/x")],
        ..Default::default()
    };
    assert!(!unknown.may_settle());
    let others = Discovery {
        others: vec![PathBuf::from("/r/x")],
        ..Default::default()
    };
    assert!(!others.may_settle());
    let chosen = Discovery {
        chosen: Some(Candidate {
            name: "x".into(),
            dir: PathBuf::from("/r/x"),
            written: None,
        }),
        ..Default::default()
    };
    assert!(!chosen.may_settle());

    // Through the scan: an empty root settles, a root with a not-yet-readable ledger does not.
    let root = TempDir::new().unwrap();
    assert!(found(root.path(), &Hints::default()).may_settle());
    let dir = root.path().join(PREV);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("lumen.db"), b"").unwrap();
    fs::write(dir.join("lumen.db-wal"), b"x").unwrap();
    assert!(!found(root.path(), &Hints::default()).may_settle());
}

fn three_ledgers(root: &Path) -> [PathBuf; 3] {
    let dirs = [root.join("a.app"), root.join("b.app"), root.join("c.app")];
    for (i, d) in dirs.iter().enumerate() {
        stamp(&ledger(d, false), 1_000 + 100 * i as u64);
    }
    dirs
}

fn write_home(home: &Path, pointer: Option<&Path>, mcp: Option<&Path>, shim: Option<&Path>) {
    let _ = fs::remove_file(home.join(".lumen_db_path"));
    let _ = fs::remove_file(home.join(".claude.json"));
    fs::create_dir_all(home.join(".claude/lumen")).unwrap();
    let _ = fs::remove_file(home.join(".claude/lumen/lumen_meter.sh"));
    if let Some(p) = pointer {
        fs::write(home.join(".lumen_db_path"), format!("{}\n", p.display())).unwrap();
    }
    if let Some(p) = mcp {
        // Lumen's entry is found by what it runs; a foreign entry never counts, whatever it sets.
        let json = serde_json::json!({
            "numStartups": 3,
            "mcpServers": {"other": {"command": "/bin/other", "env": {"LUMEN_DB": "/foreign/lumen.db"}}},
            "projects": {"/p": {"mcpServers": {
                "renamed": {"command": "\"/Applications/Lumen.app/Contents/MacOS/Lumen-MCP.exe\"",
                            "env": {"LUMEN_DB": p.to_string_lossy()}}}}}
        });
        fs::write(
            home.join(".claude.json"),
            serde_json::to_vec(&json).unwrap(),
        )
        .unwrap();
    }
    if let Some(p) = shim {
        fs::write(
            home.join(".claude/lumen/lumen_meter.sh"),
            format!(
                "#!/bin/sh\ndefault_db='{}'\nexport LUMEN_DB=\"${{LUMEN_DB:-$default_db}}\"\n",
                p.display()
            ),
        )
        .unwrap();
    }
}

#[test]
fn the_folder_lumen_was_pointed_at_wins() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let [a, b, c] = three_ledgers(root.path());
    let (a_db, b_db, c_db) = (a.join("lumen.db"), b.join("lumen.db"), c.join("lumen.db"));
    let pick = |home: &Path| {
        found(root.path(), &hints_in(home))
            .chosen
            .expect("a candidate")
            .dir
    };

    // c is the newest. All three hints disagree: the pointer wins, then the MCP entry, then the shim.
    write_home(home.path(), Some(&a_db), Some(&b_db), Some(&c_db));
    assert_eq!(pick(home.path()), a);
    write_home(home.path(), None, Some(&b_db), Some(&c_db));
    assert_eq!(pick(home.path()), b);
    write_home(home.path(), None, None, Some(&a.join("bin").join("x")));
    assert_eq!(
        pick(home.path()),
        a,
        "a path inside the folder points at it"
    );
    // No hint at all: the newest.
    write_home(home.path(), None, None, None);
    assert_eq!(pick(home.path()), c);

    // A hint at a folder that is not a candidate changes nothing.
    write_home(
        home.path(),
        Some(&root.path().join("gone.app/lumen.db")),
        None,
        None,
    );
    assert_eq!(pick(home.path()), c);

    // The rest are listed, never moved.
    write_home(home.path(), Some(&a_db), None, None);
    let d = found(root.path(), &hints_in(home.path()));
    assert_eq!(d.chosen.unwrap().dir, a);
    assert_eq!(d.others, vec![c, b]);
}

#[test]
fn a_foreign_mcp_entry_is_never_a_hint() {
    let json = serde_json::json!({
        "mcpServers": {
            "lumen": {"command": "/x/anything", "env": {"LUMEN_DB": "/by/key/lumen.db"}},
            "x": {"command": "/x/lumen-mcp", "env": {"LUMEN_DB": "/by/command/lumen.db"}},
            "foreign": {"command": "/x/other", "env": {"LUMEN_DB": "/foreign/lumen.db"}},
            "nocmd": {"env": {"LUMEN_DB": "/nocmd/lumen.db"}},
            "empty": {"command": "/x/lumen-mcp", "env": {"LUMEN_DB": ""}}
        },
        "projects": {"/p": "not an object", "/q": {"mcpServers": {"y": {"command": "C:\\Lumen\\lumen-mcp.EXE", "env": {"LUMEN_DB": "C:\\d\\lumen.db"}}}}}
    });
    let mut got = mcp_entry_dbs(&json);
    got.sort();
    assert_eq!(
        got,
        [
            "/by/command/lumen.db",
            "/by/key/lumen.db",
            "C:\\d\\lumen.db"
        ]
    );
    assert!(mcp_entry_dbs(&serde_json::json!([])).is_empty());
}

#[test]
fn the_shim_line_is_read_with_or_without_quotes() {
    assert_eq!(
        shim_default_db("a\ndefault_db='/x y/lumen.db'\nb"),
        Some("/x y/lumen.db".into())
    );
    assert_eq!(
        shim_default_db("default_db=\"/x/lumen.db\""),
        Some("/x/lumen.db".into())
    );
    assert_eq!(
        shim_default_db("default_db=/x/lumen.db"),
        Some("/x/lumen.db".into())
    );
    assert_eq!(shim_default_db("default_db=''"), None);
    assert_eq!(shim_default_db("export LUMEN_DB=x"), None);
}

#[test]
fn without_a_hint_the_newest_ledger_wins_and_the_rest_are_listed() {
    let root = TempDir::new().unwrap();
    let [a, b, c] = three_ledgers(root.path());
    let d = found(root.path(), &Hints::default());
    assert_eq!(d.chosen.unwrap().dir, c);
    assert_eq!(d.others, vec![b.clone(), a.clone()]);
    assert!(d.unknown.is_empty());

    // A -wal written later than the main file counts as the ledger having been written.
    fs::write(a.join("lumen.db-wal"), b"").unwrap();
    stamp(&a.join("lumen.db-wal"), 9_000);
    let d = found(root.path(), &Hints::default());
    assert_eq!(d.chosen.unwrap().dir, a);
    assert_eq!(d.others, vec![c, b]);
}

#[test]
fn one_candidate_is_chosen_with_nothing_else_listed() {
    let root = TempDir::new().unwrap();
    ledger(&root.path().join(PREV), false);
    let d = found(root.path(), &Hints::default());
    let c = d.chosen.as_ref().unwrap();
    assert_eq!(
        (c.name.as_str(), c.dir.as_path()),
        (PREV, root.path().join(PREV).as_path())
    );
    assert!(d.others.is_empty() && d.unknown.is_empty());
}
