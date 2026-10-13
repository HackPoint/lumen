//! One ledger, one summary: `lumen report --json` and the app's loader give the same bytes.
//!
//! The CLI reads the ledger through rusqlite and the app through sqlx (lumen-stats), and both
//! hand the rows to `lumen_core::savings`. If the two loaders ever read different rows, or the
//! same rows in a different order, a figure would differ between the terminal and the window by a
//! rounding step or a whole call. This builds one realistic ledger and compares the serialized
//! summaries byte for byte.
//!
//! The real binary runs, in a sandbox of its own (HOME, ledger and fault spool all temporary):
//! nothing here may read the developer's ledger or settings.

use std::path::Path;
use std::process::Command;

use lumen_core::savings::{BillingView, Facts, Uncalibrated};

/// Wednesday 2026-10-14 12:00 UTC.
const AS_OF: &str = "2026-10-14T12:00:00Z";
const AS_OF_MS: i64 = 1_791_979_200_000;
/// Monday 2026-10-12 00:00 UTC.
const MONDAY_MS: i64 = 1_791_763_200_000;

fn iso(ms: i64) -> String {
    lumen_core::savings::iso_ms(ms)
}

/// ISO with milliseconds, as a transcript writes a turn's time.
fn iso_ms3(ms: i64) -> String {
    let s = iso(ms);
    format!("{}.{:03}Z", &s[..19], ms.rem_euclid(1000))
}

struct Ledger<'a> {
    conn: &'a rusqlite::Connection,
    turn_n: usize,
}

impl Ledger<'_> {
    #[allow(clippy::too_many_arguments)]
    fn turn(
        &mut self,
        session: &str,
        ms: i64,
        model: &str,
        speed: Option<&str>,
        context: i64,
        split: Option<(i64, i64)>,
        subagent: Option<&str>,
    ) {
        self.turn_n += 1;
        let file = match subagent {
            Some(a) => format!("/h/.claude/projects/-p/{session}/subagents/agent-{a}.jsonl"),
            None => format!("/h/.claude/projects/-p/{session}.jsonl"),
        };
        let write = split.map_or(2_500, |(a, b)| a + b);
        self.conn
            .execute(
                "INSERT INTO turns(message_id,session_id,ts,model,input_tokens,output_tokens,
                                   cache_read_input_tokens,cache_creation_input_tokens,cache_5m,
                                   cache_1h,source_file,is_subagent,speed)
                 VALUES(?1,?2,?3,?4,12,?5,?6,?7,?8,?9,?10,?11,?12)",
                rusqlite::params![
                    format!("msg_{:04}", self.turn_n),
                    session,
                    iso_ms3(ms),
                    model,
                    400 + (self.turn_n as i64 * 37) % 900,
                    context,
                    write,
                    split.map(|s| s.0),
                    split.map(|s| s.1),
                    file,
                    subagent.is_some() as i64,
                    speed,
                ],
            )
            .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn call(
        &self,
        session: Option<&str>,
        agent: Option<&str>,
        ms: i64,
        tool: &str,
        route: &str,
        path: &str,
        full: i64,
        returned: i64,
        source: Option<&str>,
        plan: Option<(&str, &str)>,
    ) {
        self.conn
            .execute(
                "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,
                                         saved_tokens,routed_via,channel,session_id,agent_id,
                                         token_source,repeat_arm,repeat_plan)
                 VALUES(?1,?2,?3,400,?4,?5,?6,?7,'cli',?8,?9,?10,?11,?12)",
                rusqlite::params![
                    iso(ms),
                    format!("mcp__lumen__{tool}"),
                    path,
                    returned,
                    full,
                    full - returned,
                    route,
                    session,
                    agent,
                    source,
                    plan.map(|p| p.0),
                    plan.map(|p| p.1),
                ],
            )
            .unwrap();
    }
}

/// A ledger with every kind of row the engine reads: derived pairs on three models, cache
/// durations recorded and not, a compaction, follow-ups, a repeat answer, a fresh re-ask, inline
/// text, calls with no session, a model with no price, a subagent, blocked Reads in the table and
/// in the spool, and calls last week as well as this one.
fn realistic(db: &Path, spool: &Path) {
    let conn = lumen_core::meter::connect_db(db).unwrap();
    let mut l = Ledger {
        conn: &conn,
        turn_n: 0,
    };
    let models = ["claude-opus-4-8", "claude-sonnet-4-6", "claude-opus-5-5"];
    for i in 0..26i64 {
        let s = format!("sess-{i:02}");
        // Sessions spread over four days: the first ones last week.
        let start = MONDAY_MS - 3 * 86_400_000 + i * 4 * 3_600_000;
        let model = models[i as usize % 3];
        let speed = if i % 4 == 0 { None } else { Some("standard") };
        let split = if i % 5 == 0 {
            None
        } else if i % 2 == 0 {
            Some((0, 2_400))
        } else {
            Some((2_400, 0))
        };
        l.turn(&s, start, model, speed, 40_000, split, None);
        let path = format!("/repo/src/module_{i}.rs");
        let full = 6_000 + i * 700;
        let returned = 500 + i * 13;
        l.call(
            Some(&s),
            None,
            start + 2_000,
            "smart_read",
            "smart_read",
            &path,
            full,
            returned,
            Some("measured"),
            Some(("treatment", "first")),
        );
        let ctx = 40_000 + i * 3_000;
        l.turn(&s, start + 6_400, model, speed, ctx, split, None);
        // A follow-up recall on every third session.
        if i % 3 == 0 {
            l.call(
                Some(&s),
                None,
                start + 9_000,
                "recall_file",
                "recall_file",
                &path,
                full,
                900,
                Some("measured"),
                Some(("treatment", "first")),
            );
        }
        for k in 0..(5 + (i * 7) % 40) {
            l.turn(
                &s,
                start + 12_000 + k * 45_000,
                model,
                speed,
                ctx + k * 1_500,
                split,
                None,
            );
        }
        // Early compaction in a few sessions.
        if i % 7 == 1 {
            conn.execute(
                "INSERT INTO session_events(session_id,kind,uuid,ts_ms,source_file)
                 VALUES(?1,'compaction',?2,?3,?4)",
                rusqlite::params![
                    s,
                    format!("u{i}"),
                    start + 12_000 + 2 * 45_000 + 1,
                    format!("/h/.claude/projects/-p/{s}.jsonl")
                ],
            )
            .unwrap();
        }
    }
    let at = |h: i64| MONDAY_MS + h * 3_600_000;
    // A repeat answer, and a fresh re-ask after it.
    l.call(
        Some("sess-20"),
        None,
        at(8) + 60_000,
        "smart_read",
        "smart_read_unchanged",
        "/repo/src/module_20.rs",
        20_000,
        40,
        Some("measured"),
        Some(("treatment", "unchanged")),
    );
    l.call(
        Some("sess-20"),
        None,
        at(8) + 70_000,
        "smart_read",
        "smart_read",
        "/repo/src/module_20.rs",
        20_000,
        600,
        Some("measured"),
        Some(("treatment", "fresh")),
    );
    // Inline text, an outline that cost more than its file, and smart_read(mode=full).
    l.call(
        Some("sess-21"),
        None,
        at(12) + 30_000,
        "compress_logs",
        "compress_logs",
        "(inline)",
        9_000,
        1_200,
        Some("measured"),
        None,
    );
    l.call(
        Some("sess-22"),
        None,
        at(16) + 30_000,
        "smart_read",
        "would_inflate",
        "/repo/src/tiny.rs",
        180,
        212,
        Some("measured"),
        Some(("treatment", "first")),
    );
    // Rows from before 1.1.5: no session, no provenance.
    for k in 0..3 {
        l.call(
            None,
            None,
            MONDAY_MS - 20 * 86_400_000 + k * 60_000,
            "smart_read",
            "smart_read",
            &format!("/old/{k}.py"),
            4_000 + k * 1_000,
            700,
            None,
            None,
        );
    }
    // A model with no price reads one call.
    l.turn(
        "sess-haiku",
        at(30),
        "claude-haiku-5-5",
        None,
        20_000,
        None,
        None,
    );
    l.call(
        Some("sess-haiku"),
        None,
        at(30) + 1_000,
        "smart_read",
        "smart_read",
        "/repo/src/h.rs",
        7_000,
        600,
        Some("measured"),
        None,
    );
    l.turn(
        "sess-haiku",
        at(30) + 5_000,
        "claude-haiku-5-5",
        None,
        21_000,
        None,
        None,
    );
    // A subagent's call, read by its own transcript.
    l.turn(
        "sess-05",
        at(4),
        "claude-opus-4-8",
        Some("standard"),
        9_000,
        Some((800, 0)),
        Some("a1b2"),
    );
    l.call(
        Some("sess-05"),
        Some("a1b2"),
        at(4) + 1_000,
        "smart_read",
        "smart_read",
        "/repo/src/sub.rs",
        11_000,
        700,
        Some("measured"),
        Some(("treatment", "first")),
    );
    l.turn(
        "sess-05",
        at(4) + 4_000,
        "claude-opus-4-8",
        Some("standard"),
        12_000,
        Some((800, 0)),
        Some("a1b2"),
    );
    l.turn(
        "sess-05",
        at(4) + 9_000,
        "claude-opus-4-8",
        Some("standard"),
        13_000,
        Some((800, 0)),
        Some("a1b2"),
    );
    // Blocked Reads the escape valve let through: one drained into the table, one still spooled.
    conn.execute(
        "INSERT INTO faults(ts,kind,variant,path,session_id)
         VALUES(?1,'hook_fail_open','retry_escape_valve','/repo/src/big.rs','sess-24')",
        [iso(MONDAY_MS - 3 * 86_400_000
            + 24 * 4 * 3_600_000
            + 20_000)],
    )
    .unwrap();
    std::fs::write(
        spool,
        format!(
            "{{\"ts\":\"{}\",\"kind\":\"hook_fail_open\",\"variant\":\"retry_escape_valve\",\
             \"path\":\"/repo/src/big2.rs\",\"session_id\":\"sess-25\"}}\n",
            iso(MONDAY_MS - 3 * 86_400_000 + 25 * 4 * 3_600_000 + 30_000)
        ),
    )
    .unwrap();
}

/// `lumen report --json --as-of` in a sandbox holding `db` and `spool`.
fn cli_json(home: &Path, db: &Path, spool: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(["report", "--json", "--as-of", AS_OF])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LUMEN_DB", db)
        .env("LUMEN_FAULT_SPOOL", spool)
        // The zone the week is measured in, the same for both loaders' SQLite.
        .env("TZ", "UTC")
        .output()
        .expect("the lumen binary runs");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn the_cli_and_the_app_return_the_same_bytes_from_one_ledger() {
    // This process's SQLite reads TZ as the child's does: the week starts at the same instant.
    // SAFETY: set before any thread of this test reads the environment; the test binary runs
    // nothing else that reads TZ.
    unsafe { std::env::set_var("TZ", "UTC") };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = dir.path().join("lumen.db");
    let spool = dir.path().join("faults.jsonl");
    realistic(&db, &spool);

    let cli = cli_json(&home, &db, &spool);

    let pool = lumen_stats::connect(&format!("sqlite:{}?mode=rwc", db.display()))
        .await
        .unwrap();
    // What the app's command passes in the same sandbox: no billing choice made, and no
    // settings.json, which registers no hook.
    let facts = Facts {
        billing_mode: BillingView::of(&lumen_core::settings::billing_mode_for(&db)),
        identity_hook_registered: Some(false),
    };
    let app = lumen_stats::savings_summary(&pool, AS_OF_MS, Some(&spool), &facts, &Uncalibrated)
        .await
        .unwrap();
    let app = serde_json::to_string_pretty(&app).unwrap();

    assert_eq!(cli.trim_end(), app, "the CLI and the app disagree");

    // And the fixture exercised what it was built to: every part of the dollars, both periods,
    // blocked Reads from the table and the spool.
    let v: serde_json::Value = serde_json::from_str(&app).unwrap();
    let all = &v["allTime"]["counts"];
    assert!(all["derivedPairs"].as_i64().unwrap() >= 20, "{all}");
    assert_eq!(all["blockedReads"], 2, "{all}");
    // All three parts: measured, modelled (sess-00 began before the first recorded event and
    // shows no collapse), and tokens only (three rows with no session, one model with no price).
    let at = &v["allTime"];
    assert!(at["measured"]["rows"].as_i64().unwrap() >= 20, "{at}");
    assert!(at["measured"]["net"]["value"].is_number());
    assert!(at["modelled"]["rows"].as_i64().unwrap() >= 1, "{at}");
    assert_eq!(at["unpriced"]["noSession"], 3, "{at}");
    assert!(at["unpriced"]["noPrice"].as_i64().unwrap() >= 1, "{at}");
    assert!(
        v["week"]["counts"]["calls"].as_i64() < v["allTime"]["counts"]["calls"].as_i64(),
        "some calls were before this week"
    );
    assert_eq!(v["asOf"], AS_OF);
    // sess-20's fresh re-ask after its stub is a loss of what it returned, not 19,400 saved
    // against the 20,000-token file a second time.
    let lines = v["ledger"]["recent"].as_array().unwrap();
    let again: Vec<_> = lines
        .iter()
        .filter(|l| l["savedAgainst"] == "askedAgain")
        .collect();
    assert_eq!(again.len(), 1, "{lines:?}");
    assert_eq!(again[0]["savedCl100k"], -600);
    assert_eq!(again[0]["extraRoundBasis"], "freshAfterStub");
}
