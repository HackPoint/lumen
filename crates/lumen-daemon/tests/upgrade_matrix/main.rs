//! Ledgers written by real releases, upgraded by every code path that opens a ledger.

mod openers;
mod releases;
mod snapshot;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

// 1.8.0's schema.rs, copied verbatim into releases, names `crate::busy`; this is that name. Only
// its retry on a busy ledger is used, which shapes no table, column or row.
use lumen_core::busy;
use lumen_core::meter::{HookReadEvent, RankedMeta, insert_hook_event_at, insert_read_event_at};

use openers::Opener;
use releases::{Fixture, Release};
use snapshot::{
    Snapshot, assert_nothing_invented, assert_old_rows_survive, assert_same_schema,
    assert_unchanged, integrity_check, take,
};

const HOOK_WITH: &str = "/probe/Zoë ünï/hook with agent.rs";
const HOOK_WITHOUT: &str = "/probe/Zoë ünï/hook without agent.rs";
const MCP_WITH: &str = "/probe/Zoë ünï/mcp with agent.rs";
const MCP_WITHOUT: &str = "/probe/Zoë ünï/mcp without agent.rs";
const AGENT: &str = "agent-7f3a";

/// Tables a release dropped. A fresh ledger lacks them too, so comparing with one already
/// catches a drop that did not run; naming them also catches the table put back in DDL.
const DROPPED: &[&str] = &["sessions"];

fn assert_dropped_tables_gone(what: &str, snap: &Snapshot) {
    for table in DROPPED {
        assert!(
            !snap.tables.contains_key(*table),
            "{what}: {table} was dropped, and is still there"
        );
    }
}

fn place(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let db = dir.join(name);
    std::fs::write(&db, bytes).expect("write the ledger");
    db
}

/// A ledger `opener` creates from nothing: what an upgraded one has to equal.
async fn fresh(dir: &Path, opener: Opener) -> Snapshot {
    let db = dir.join(format!("fresh-{opener:?}.db"));
    opener.open(&db).await.expect("create a fresh ledger");
    take(&db).await
}

/// One row from each writer, with and without the 1.8.0 `agent_id`.
fn write_probes(db: &Path) {
    for (path, agent_id) in [(HOOK_WITH, Some(AGENT)), (HOOK_WITHOUT, None)] {
        insert_hook_event_at(
            db,
            &HookReadEvent {
                tool: "Read",
                path,
                lines: Some(12),
                tokens_returned: 340,
                full_tokens: 340,
                routed_via: "builtin_read",
                channel: "cli",
                session_id: Some("probe-session"),
                agent_id,
                file_mtime: Some(1_700_000_000),
                req_key: Some("probe-req"),
                writer_hook: "probe",
                token_source: "measured",
            },
        )
        .expect("the hook's row lands");
    }
    for (path, agent_id) in [(MCP_WITH, Some(AGENT)), (MCP_WITHOUT, None)] {
        insert_read_event_at(
            db,
            path,
            Some(12),
            90,
            340,
            250,
            "smart_read",
            "cli",
            "smart_read",
            Some("probe-session"),
            Some(1_700_000_000),
            Some("probe-req"),
            &RankedMeta {
                agent_id: agent_id.map(String::from),
                ..RankedMeta::default()
            },
        )
        .expect("the MCP server's row lands");
    }
}

fn assert_probes_landed(what: &str, snap: &Snapshot) {
    let rows = &snap.tables["read_events"].rows;
    let probes = [
        (HOOK_WITH, Some(AGENT)),
        (HOOK_WITHOUT, None),
        (MCP_WITH, Some(AGENT)),
        (MCP_WITHOUT, None),
    ];
    for (path, agent_id) in probes {
        let quoted = format!("'{path}'");
        let found: Vec<_> = rows.iter().filter(|r| r["path"] == quoted).collect();
        assert_eq!(
            found.len(),
            1,
            "{what}: the row for {path} landed {} times",
            found.len()
        );
        let want = agent_id.map_or_else(|| "NULL".to_string(), |a| format!("'{a}'"));
        assert_eq!(found[0]["agent_id"], want, "{what}: agent_id of {path}");
        assert_eq!(
            found[0]["session_id"], "'probe-session'",
            "{what}: session_id of {path}"
        );
    }
}

async fn cell(fixture: Fixture, opener: Opener) {
    let what = format!("{fixture:?} via {opener:?}");
    let dir = tempfile::tempdir().expect("tempdir");
    let db = place(dir.path(), "ledger.db", &fixture.build().await);
    let old = take(&db).await;
    let fresh = fresh(dir.path(), opener).await;

    opener
        .open(&db)
        .await
        .unwrap_or_else(|e| panic!("{what}: the upgrade failed: {e}"));
    let upgraded = take(&db).await;
    assert_dropped_tables_gone(&what, &upgraded);
    assert_same_schema(&what, &upgraded, &fresh);
    assert_ne!(
        old.facts, upgraded.facts,
        "{what}: the fixture needed no upgrade"
    );
    assert_old_rows_survive(&what, &old, &upgraded, &fresh, true);
    assert_nothing_invented(&what, &old, &upgraded);
    assert_eq!(integrity_check(&db).await, ["ok"], "{what}");

    opener
        .open(&db)
        .await
        .unwrap_or_else(|e| panic!("{what}: the second open failed: {e}"));
    assert_unchanged(&format!("{what}, second open"), &upgraded, &take(&db).await);

    write_probes(&db);
    let written = take(&db).await;
    assert_probes_landed(&what, &written);
    assert_old_rows_survive(
        &format!("{what}, after writes"),
        &old,
        &written,
        &fresh,
        false,
    );
    assert_eq!(integrity_check(&db).await, ["ok"], "{what}, after writes");
}

macro_rules! column {
    ($module:ident, $opener:expr) => {
        mod $module {
            use super::*;

            #[tokio::test]
            async fn v1_1_4() {
                cell(Fixture::V1_1_4, $opener).await;
            }

            #[tokio::test]
            async fn v1_5_0() {
                cell(Fixture::V1_5_0, $opener).await;
            }

            #[tokio::test]
            async fn v1_6_0() {
                cell(Fixture::V1_6_0, $opener).await;
            }

            #[tokio::test]
            async fn v1_7_0() {
                cell(Fixture::V1_7_0, $opener).await;
            }

            #[tokio::test]
            async fn v1_8_0() {
                cell(Fixture::V1_8_0, $opener).await;
            }

            #[tokio::test]
            async fn long_lived() {
                cell(Fixture::LongLived, $opener).await;
            }
        }
    };
}

column!(daemon_path, Opener::Daemon);
column!(gui_path, Opener::Gui);
column!(rusqlite_path, Opener::Rusqlite);

#[tokio::test]
async fn a_fresh_ledger_is_the_same_whichever_path_creates_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = fresh(dir.path(), Opener::Daemon).await;
    assert_dropped_tables_gone("a fresh ledger", &daemon);
    for opener in [Opener::Gui, Opener::Rusqlite] {
        let other = fresh(dir.path(), opener).await;
        assert_same_schema(&format!("{opener:?} against Daemon"), &other, &daemon);
    }
}

async fn old_ledger(fixture: Fixture) -> Snapshot {
    let dir = tempfile::tempdir().expect("tempdir");
    take(&place(dir.path(), "ledger.db", &fixture.build().await)).await
}

#[tokio::test]
async fn each_fixture_has_the_schema_of_the_release_it_stands_for() {
    for fixture in releases::ALL {
        let snap = old_ledger(fixture).await;
        let tables: Vec<&str> = snap.tables.keys().map(String::as_str).collect();
        assert_eq!(tables, fixture.tables(), "{fixture:?}: tables");
        let read_events = &snap.tables["read_events"];
        assert_eq!(
            read_events.columns.len(),
            fixture.newest().read_events_columns(),
            "{fixture:?}: read_events columns"
        );
        assert_eq!(
            read_events.columns.iter().any(|c| c.name == "agent_id"),
            matches!(fixture.newest(), Release::V1_8_0),
            "{fixture:?}: read_events.agent_id, which 1.8.0 added"
        );
    }
}

/// F53: the previous install is found by what is in its ledger. The folder has a name nothing in
/// Lumen knows, and the ledger is the one each release left, from 1.1.4 to 1.9.0, empty or with
/// rows, so the columns discovery relies on are asserted against every released schema.
#[tokio::test]
async fn a_previous_install_is_found_by_its_ledger_under_any_name() {
    use lumen_core::legacy::discover::{Hints, discover};

    fn find(root: &Path, what: &str) {
        let found = discover(&[root], "example.current.app", &Hints::default());
        let chosen = found
            .chosen
            .unwrap_or_else(|| panic!("{what}: the ledger was not recognised"));
        assert_eq!(chosen.name, "example.previous.app", "{what}");
        assert_eq!(chosen.dir, root.join("example.previous.app"), "{what}");
        assert!(
            found.others.is_empty() && found.unknown.is_empty(),
            "{what}"
        );
    }

    for release in [
        Release::V1_1_4,
        Release::V1_5_0,
        Release::V1_6_0,
        Release::V1_7_0,
        Release::V1_8_0,
        Release::V1_9_0,
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("example.previous.app");
        std::fs::create_dir_all(&dir).expect("folder");
        release.open(&dir.join("lumen.db")).await;
        find(root.path(), &format!("{release:?}, empty"));
    }
    for fixture in releases::ALL {
        let root = tempfile::tempdir().expect("tempdir");
        let dir = root.path().join("example.previous.app");
        std::fs::create_dir_all(&dir).expect("folder");
        place(&dir, "lumen.db", &fixture.build().await);
        find(root.path(), &format!("{fixture:?}, with rows"));
    }
}

#[tokio::test]
async fn a_long_lived_ledger_ends_at_the_schema_of_the_last_release_that_opened_it() {
    let long_lived = old_ledger(Fixture::LongLived).await;
    let v1_8_0 = old_ledger(Fixture::V1_8_0).await;
    assert_same_schema("LongLived against V1_8_0", &long_lived, &v1_8_0);
}

/// An install still on 1.8.0, an app not yet updated or a hook from it, can open the ledger after
/// this build has upgraded it. 1.8.0's DDL creates `sessions` again, empty; nothing reads it and no
/// other row changes, and the next open by this build drops it again.
#[tokio::test]
async fn a_ledger_opened_by_1_8_0_after_the_upgrade_has_sessions_back_empty_until_the_next_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = place(dir.path(), "ledger.db", &Fixture::V1_8_0.build().await);
    for opener in [Opener::Daemon, Opener::Gui, Opener::Rusqlite] {
        let what = format!("1.8.0 between two opens via {opener:?}");
        opener
            .open(&db)
            .await
            .unwrap_or_else(|e| panic!("{what}: the upgrade failed: {e}"));
        let upgraded = take(&db).await;
        assert_dropped_tables_gone(&what, &upgraded);

        Release::V1_8_0.open(&db).await;
        let reopened = take(&db).await;
        assert!(
            reopened.tables.contains_key("sessions"),
            "{what}: 1.8.0 did not create sessions again, which is what this test is about"
        );
        assert_old_rows_survive(&what, &upgraded, &reopened, &upgraded, true);
        assert_nothing_invented(&what, &upgraded, &reopened);
        assert_eq!(integrity_check(&db).await, ["ok"], "{what}");

        opener
            .open(&db)
            .await
            .unwrap_or_else(|e| panic!("{what}: the open after 1.8.0 failed: {e}"));
        assert_unchanged(
            &format!("{what}, opened again"),
            &upgraded,
            &take(&db).await,
        );
    }
}

/// The turn 1.8.0's daemon writes, its INSERT as it was at the tag. It names its columns, so the
/// ones this build added take their default.
const TURN_INSERT_1_8_0: &str = "INSERT OR IGNORE INTO turns
             (message_id, session_id, ts, model,
              input_tokens, output_tokens,
              cache_read_input_tokens, cache_creation_input_tokens, source_file,
              is_subagent)
             VALUES (?,?,?,?,?,?,?,?,?,?)";

/// A daemon still on 1.8.0 can ingest into the ledger after this build has upgraded it. Its turn
/// lands, with no speed and no region, NULL: unknown, which is what pricing reads it as. Nothing
/// is filled in when this build opens the ledger again.
#[tokio::test]
async fn a_turn_1_8_0_writes_after_the_upgrade_has_no_speed_or_region() {
    for opener in [Opener::Daemon, Opener::Gui, Opener::Rusqlite] {
        let what = format!("1.8.0 writing after an upgrade via {opener:?}");
        let dir = tempfile::tempdir().expect("tempdir");
        let db = place(dir.path(), "ledger.db", &Fixture::V1_8_0.build().await);
        opener
            .open(&db)
            .await
            .unwrap_or_else(|e| panic!("{what}: the upgrade failed: {e}"));

        Release::V1_8_0.open(&db).await;
        let mut conn = snapshot::connect(&db).await;
        sqlx::query(TURN_INSERT_1_8_0)
            .bind("msg-from-1.8.0")
            .bind("s-1.8.0")
            .bind("2026-10-13T12:00:00Z")
            .bind("claude-opus-4-8")
            .bind(1_i64)
            .bind(2_i64)
            .bind(3_i64)
            .bind(4_i64)
            .bind("f.jsonl")
            .bind(0_i64)
            .execute(&mut conn)
            .await
            .unwrap_or_else(|e| panic!("{what}: 1.8.0's insert failed: {e}"));
        drop(conn);

        opener
            .open(&db)
            .await
            .unwrap_or_else(|e| panic!("{what}: the open after 1.8.0 failed: {e}"));
        let snap = take(&db).await;
        let row = snap.tables["turns"]
            .rows
            .iter()
            .find(|r| r["message_id"] == "'msg-from-1.8.0'")
            .unwrap_or_else(|| panic!("{what}: the turn did not land"));
        assert_eq!(row["speed"], "NULL", "{what}");
        assert_eq!(row["inference_geo"], "NULL", "{what}");
        assert_eq!(integrity_check(&db).await, ["ok"], "{what}");
    }
}

#[tokio::test]
async fn every_fixture_table_holds_each_kind_of_row_an_upgrade_must_keep() {
    for fixture in releases::ALL {
        let snap = old_ledger(fixture).await;
        for (name, table) in &snap.tables {
            let what = format!("{fixture:?} {name}");
            assert!(
                table.rows.len() >= 6,
                "{what}: only {} rows",
                table.rows.len()
            );
            for col in &table.columns {
                let cells: Vec<&String> = table.rows.iter().map(|r| &r[&col.name]).collect();
                let at = format!("{what}.{}", col.name);
                assert!(
                    cells.iter().any(|v| *v != "NULL"),
                    "{at} is never populated"
                );
                if !col.notnull && col.pk == 0 {
                    assert!(cells.iter().any(|v| *v == "NULL"), "{at} is never NULL");
                }
                if col.ty == "TEXT" && col.pk == 0 {
                    assert!(cells.iter().any(|v| *v == "''"), "{at} is never empty");
                    assert!(
                        cells.iter().any(|v| !v.is_ascii()),
                        "{at} is never non-ASCII"
                    );
                }
            }
        }
    }
}

const ROUNDS: usize = 12;
const CONTENDERS: [Opener; 9] = [
    Opener::Daemon,
    Opener::Gui,
    Opener::Rusqlite,
    Opener::Daemon,
    Opener::Gui,
    Opener::Rusqlite,
    Opener::Daemon,
    Opener::Gui,
    Opener::Rusqlite,
];

/// Every path opens the same old ledger at the same instant, on a fresh copy each round.
async fn race(fixture: Fixture) {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = fixture.build().await;
    let old = take(&place(dir.path(), "old.db", &bytes)).await;
    let fresh = fresh(dir.path(), Opener::Daemon).await;
    for round in 0..ROUNDS {
        let what = format!("{fixture:?}, round {round}");
        let db = place(dir.path(), &format!("round-{round}.db"), &bytes);
        let start = Arc::new(Barrier::new(CONTENDERS.len()));
        let threads: Vec<_> = CONTENDERS
            .iter()
            .map(|&opener| {
                let (db, start) = (db.clone(), Arc::clone(&start));
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime");
                    start.wait();
                    (opener, runtime.block_on(opener.open(&db)))
                })
            })
            .collect();
        for thread in threads {
            let (opener, opened) = thread.join().expect("opener thread");
            opened.unwrap_or_else(|e| panic!("{what}: {opener:?} failed beside the others: {e}"));
        }
        let upgraded = take(&db).await;
        assert_dropped_tables_gone(&what, &upgraded);
        assert_same_schema(&what, &upgraded, &fresh);
        assert_old_rows_survive(&what, &old, &upgraded, &fresh, true);
        assert_nothing_invented(&what, &old, &upgraded);
        assert_eq!(integrity_check(&db).await, ["ok"], "{what}");
    }
}

macro_rules! races {
    ($($name:ident: $fixture:expr;)*) => {
        mod concurrent_upgrade {
            use super::*;
            $(
                #[tokio::test]
                async fn $name() {
                    race($fixture).await;
                }
            )*
        }
    };
}

races! {
    v1_1_4: Fixture::V1_1_4;
    v1_5_0: Fixture::V1_5_0;
    v1_6_0: Fixture::V1_6_0;
    v1_7_0: Fixture::V1_7_0;
    v1_8_0: Fixture::V1_8_0;
    long_lived: Fixture::LongLived;
}

/// A running daemon, killed on drop so a failing assertion cannot leak it.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Reserve an unused port by binding and releasing it; the daemon has to bind it itself.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
    listener.local_addr().expect("local addr").port()
}

/// Only the real binary shows that `main` migrates the ledger before it serves.
async fn daemon_binary(fixture: Fixture) {
    let what = format!("{fixture:?} via the lumen-daemon binary");
    let dir = tempfile::tempdir().expect("tempdir");
    let db = place(dir.path(), "ledger.db", &fixture.build().await);
    // A forgotten LUMEN_DB resolves to the developer's own ledger, so this is asserted.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the ledger must live in a temp dir: {db:?}"
    );
    let old = take(&db).await;
    let fresh = fresh(dir.path(), Opener::Daemon).await;

    let projects = dir.path().join("projects");
    std::fs::create_dir_all(&projects).expect("projects dir");
    let addr = format!("127.0.0.1:{}", free_port());
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_lumen-daemon"))
            .env("HOME", dir.path())
            .env("LUMEN_PROJECTS_DIR", &projects)
            .env("LUMEN_DB", &db)
            .env("LUMEN_FAULT_SPOOL", dir.path().join("faults.jsonl"))
            .env("LUMEN_WS_ADDR", &addr)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lumen-daemon"),
    );
    // It binds only after init_schema returned, so an accepting port means the ledger is current.
    let deadline = Instant::now() + Duration::from_secs(30);
    while tokio::net::TcpStream::connect(&addr).await.is_err() {
        if let Some(status) = daemon.0.try_wait().expect("poll the daemon") {
            panic!("{what}: the daemon exited with {status} before it served");
        }
        assert!(
            Instant::now() < deadline,
            "{what}: the daemon never opened ws://{addr}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(daemon);

    let upgraded = take(&db).await;
    assert_dropped_tables_gone(&what, &upgraded);
    assert_same_schema(&what, &upgraded, &fresh);
    assert_old_rows_survive(&what, &old, &upgraded, &fresh, true);
    assert_nothing_invented(&what, &old, &upgraded);
    assert_eq!(integrity_check(&db).await, ["ok"], "{what}");
}

macro_rules! binaries {
    ($($name:ident: $fixture:expr;)*) => {
        mod daemon_binary_path {
            use super::*;
            $(
                #[tokio::test]
                async fn $name() {
                    daemon_binary($fixture).await;
                }
            )*
        }
    };
}

binaries! {
    v1_1_4: Fixture::V1_1_4;
    v1_5_0: Fixture::V1_5_0;
    v1_6_0: Fixture::V1_6_0;
    v1_7_0: Fixture::V1_7_0;
    v1_8_0: Fixture::V1_8_0;
    long_lived: Fixture::LongLived;
}
