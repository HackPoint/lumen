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

use lumen_core::savings::{BillingView, Calibrated, Facts};

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
    // Blocks the transcripts record: before sess-03's call and again after a Lumen answer, in
    // sess-05's subagent, on sess-24's faulted file, and in a session with no Lumen call at all;
    // and the transcripts the daemon read for them.
    let start = |i: i64| MONDAY_MS - 3 * 86_400_000 + i * 4 * 3_600_000;
    l.turn(
        "sess-blk",
        at(20) + 3_000,
        "claude-sonnet-4-6",
        Some("standard"),
        30_000,
        Some((0, 1_200)),
        None,
    );
    let blocks = [
        (
            "toolu_b1",
            "sess-03",
            None,
            "/repo/src/module_3.rs",
            "redirect",
            start(3) + 1_000,
        ),
        (
            "toolu_b2",
            "sess-03",
            None,
            "/repo/src/module_3.rs",
            "served",
            start(3) + 10_000,
        ),
        (
            "toolu_b3",
            "sess-05",
            Some("a1b2"),
            "/repo/src/sub.rs",
            "redirect",
            at(4) + 5_000,
        ),
        (
            "toolu_b4",
            "sess-24",
            None,
            "/repo/src/big.rs",
            "redirect",
            start(24) + 1_000,
        ),
        (
            "toolu_b5",
            "sess-blk",
            None,
            "/repo/src/x.rs",
            "log",
            at(20) + 1_000,
        ),
    ];
    for (id, session, agent, path, kind, ms) in blocks {
        conn.execute(
            "INSERT INTO intercept_blocks(tool_use_id,session_id,agent_id,path,lines,kind,ts_ms,
                                          uuid,source_file)
             VALUES(?1,?2,?3,?4,400,?5,?6,?7,?8)",
            rusqlite::params![
                id,
                session,
                agent,
                path,
                kind,
                ms,
                format!("r-{id}"),
                "/h/t.jsonl"
            ],
        )
        .unwrap();
    }
    for (session, agent, through) in [
        ("sess-03", None, AS_OF_MS),
        ("sess-05", Some("a1b2"), at(4) + 9_000),
        ("sess-24", None, AS_OF_MS),
        ("sess-blk", None, at(20) + 3_000),
    ] {
        conn.execute(
            "INSERT INTO transcript_scans(source_file,session_id,agent_id,matcher,through_ms)
             VALUES(?1,?2,?3,1,?4)",
            rusqlite::params![
                match agent {
                    Some(a) =>
                        format!("/h/.claude/projects/-p/{session}/subagents/agent-{a}.jsonl"),
                    None => format!("/h/.claude/projects/-p/{session}.jsonl"),
                },
                session,
                agent,
                through
            ],
        )
        .unwrap();
    }
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
    let app = lumen_stats::savings_summary(&pool, AS_OF_MS, Some(&spool), &facts, &Calibrated)
        .await
        .unwrap();
    let app = serde_json::to_string_pretty(&app).unwrap();

    assert_eq!(cli.trim_end(), app, "the CLI and the app disagree");

    // And the fixture exercised what it was built to: every part of the dollars, both periods,
    // blocked Reads from the table and the spool.
    let v: serde_json::Value = serde_json::from_str(&app).unwrap();
    let all = &v["allTime"]["counts"];
    assert!(all["derivedPairs"].as_i64().unwrap() >= 20, "{all}");
    // Five blocks the transcripts record, sess-24's fault among their later Reads, and the
    // spooled fault on a file no transcript records a block of.
    assert_eq!(
        (
            &all["blockedReads"],
            &all["blockedReadsFromTranscripts"],
            &all["blockedReadsFromFaults"],
            &all["blockedReadsBounded"],
            &all["valveFaults"]
        ),
        (&6.into(), &5.into(), &1.into(), &0.into(), &2.into()),
        "{all}"
    );
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
    // sess-03's call followed its recorded block: the block carries the round, not the call.
    let sess03: Vec<_> = lines
        .iter()
        .filter(|l| l["path"] == "/repo/src/module_3.rs")
        .map(|l| l["extraRoundBasis"].as_str().unwrap())
        .collect();
    assert!(sess03.contains(&"blockRecorded"), "{sess03:?}");
    assert!(sess03.contains(&"blockedReadRecorded"), "{sess03:?}");
}

// ── what `lumen` and `lumen report` say, from fixture ledgers ───────────────────────────────
//
// The app's headline is tested on the text it renders for each state, from summaries in the case
// list it shares with lumen_core::savings::text (lumenator/src/app/savings-text.cases.json).
// These hold the terminal to the same states from real ledgers, through the built binary: each
// figure is read back from `lumen report --json` and must be what the line says, in the words the
// window uses.

use lumen_core::savings::text;

/// `lumen <args>` in the sandbox, as `cli_json` runs it: (stdout, stderr, exit code).
fn run_lumen(home: &Path, db: &Path, spool: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_lumen"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LUMEN_DB", db)
        .env("LUMEN_FAULT_SPOOL", spool)
        .env("TZ", "UTC")
        .output()
        .expect("the lumen binary runs");
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
        out.status.code().unwrap_or(-1),
    )
}

/// A sandbox: its home, ledger and spool paths, nothing written yet.
fn sandbox() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let db = dir.path().join("lumen.db");
    let spool = dir.path().join("faults.jsonl");
    (dir, home, db, spool)
}

/// `lumen --as-of`, `lumen report --as-of` and the summary they come from.
fn surfaces(home: &Path, db: &Path, spool: &Path) -> (String, String, serde_json::Value) {
    let (line, err, code) = run_lumen(home, db, spool, &["--as-of", AS_OF]);
    assert_eq!(code, 0, "lumen: {err}");
    let (detail, err, code) = run_lumen(home, db, spool, &["report", "--as-of", AS_OF]);
    assert_eq!(code, 0, "lumen report: {err}");
    let json = serde_json::from_str(&cli_json(home, db, spool)).unwrap();
    (line.trim_end().to_string(), detail, json)
}

fn billing(db: &Path, mode: Option<&str>) {
    let path = lumen_core::settings::settings_path(db);
    match mode {
        Some(m) => std::fs::write(&path, format!("{{\"billing_mode\": \"{m}\"}}")).unwrap(),
        None => {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The phrase a period's figures read as in the line, from the summary's own values: its tokens
/// and its measured net, in the words of the app's box for it.
fn clause(p: &serde_json::Value, basis: &str) -> String {
    format!(
        "{}; {}{basis}",
        text::tokens_phrase(p["tokensSaved"]["value"].as_i64().unwrap()),
        text::usd_phrase(p["measured"]["net"]["value"].as_f64().unwrap())
    )
}

#[test]
fn lumen_says_each_billing_mode_as_the_app_does_and_its_dollars_are_the_measured_net() {
    // SAFETY: as in the parity test; nothing else in this binary reads TZ.
    unsafe { std::env::set_var("TZ", "UTC") };
    let (_dir, home, db, spool) = sandbox();
    realistic(&db, &spool);

    for (mode, view, basis) in [
        (Some("api"), "API", ""),
        (Some("subscription"), "subscription", " at API prices"),
        (None, "not set", " at API prices"),
    ] {
        billing(&db, mode);
        let (line, detail, v) = surfaces(&home, &db, &spool);
        let (week, all) = (&v["week"], &v["allTime"]);
        assert!(line.contains(&clause(week, basis)), "{mode:?}: {line}");
        assert!(line.contains(&clause(all, basis)), "{mode:?}: {line}");
        // The bold figure is the measured net: not the gross, and the modelled said beside it.
        let net = text::usd_signed(all["measured"]["net"]["value"].as_f64().unwrap());
        let gross = text::usd_signed(all["measured"]["gross"]["value"].as_f64().unwrap());
        assert_ne!(net, gross, "premise: the extra rounds cost something");
        assert!(!line.contains(&gross), "{line}");
        assert!(
            line.contains(&format!("at least {net} saved, net")),
            "{line}"
        );
        let modelled = text::usd_plus(all["modelled"]["value"]["value"].as_f64().unwrap());
        assert!(
            line.contains(&format!("({modelled} modelled; ")),
            "{mode:?}: {line}"
        );
        let unpriced = all["unpriced"]["calls"].as_i64().unwrap();
        assert!(
            line.contains(&format!(
                "{} calls unpriced)",
                text::tokens_signed(unpriced)
            )),
            "{mode:?}: {line}"
        );
        // Where the dollars are at API prices, the share of tokens comes first, in the app's
        // words.
        let share = format!(
            "Lumen this week: \u{2248}\u{a0}{} tokens from file and tool reads; ",
            text::share_text(week["sharePct"].as_f64().unwrap())
        );
        assert_eq!(
            line.contains(&share),
            mode != Some("api"),
            "{mode:?}: {line}"
        );
        assert_eq!(
            line.contains("at API prices"),
            mode != Some("api"),
            "{line}"
        );

        assert!(detail.contains(&format!("Billing: {view}")), "{detail}");
        assert!(
            detail.contains(&format!(
                "on record              {}{basis}, from ",
                text::usd_phrase(week["measured"]["net"]["value"].as_f64().unwrap())
            )),
            "{mode:?}: {detail}"
        );
        // Rows with no session: partial data, said why.
        assert!(
            detail.contains("partial data: 3 rows with no session recorded"),
            "{detail}"
        );
    }
}

/// One session that read a call's result: the turn that made it, the call, and `after` turns.
fn read_session(l: &mut Ledger<'_>, s: &str, start: i64, after: i64) {
    l.turn(
        s,
        start,
        "claude-opus-4-8",
        Some("standard"),
        30_000,
        Some((0, 2_000)),
        None,
    );
    for k in 0..after {
        l.turn(
            s,
            start + 5_000 + k * 30_000,
            "claude-opus-4-8",
            Some("standard"),
            32_000 + k * 1_000,
            Some((0, 2_000)),
            None,
        );
    }
}

#[test]
fn a_losing_week_reads_as_a_loss_in_lumen_and_lumen_report() {
    // SAFETY: as above.
    unsafe { std::env::set_var("TZ", "UTC") };
    let (_dir, home, db, spool) = sandbox();
    {
        let conn = lumen_core::meter::connect_db(&db).unwrap();
        let mut l = Ledger {
            conn: &conn,
            turn_n: 0,
        };
        // The daemon recorded events from before the conversation, so its end is vouched for
        // and its R cannot be too long.
        conn.execute(
            "INSERT INTO session_events(session_id,kind,uuid,ts_ms,source_file)
             VALUES('earlier','resync','r0',?1,'/h/.claude/projects/-p/earlier.jsonl')",
            [MONDAY_MS - 86_400_000],
        )
        .unwrap();
        // compress_logs on inline text: saves nothing and pays for a round.
        read_session(&mut l, "loss", MONDAY_MS + 3_600_000, 4);
        l.call(
            Some("loss"),
            None,
            MONDAY_MS + 3_600_000 + 2_000,
            "compress_logs",
            "compress_logs",
            "(inline)",
            9_000,
            1_200,
            Some("measured"),
            None,
        );
    }
    billing(&db, Some("api"));
    let (line, detail, v) = surfaces(&home, &db, &spool);
    let net = v["week"]["measured"]["net"]["value"].as_f64().unwrap();
    assert!(net < 0.0, "premise: {net}");
    assert!(line.contains(&clause(&v["week"], "")), "{line}");
    let phrase = text::usd_phrase(net);
    assert!(phrase.starts_with("net loss of at most $"), "{phrase}");
    assert!(
        line.contains(&format!("; {phrase} \u{b7} all time: ")),
        "{line}"
    );
    assert!(line.contains(" \u{b7} not working ("), "{line}");
    // Inline text pays a round on evidence and saves nothing: a loss at every end of its band,
    // and the band's high end is a bound.
    assert!(v["week"]["measured"]["net"]["high"].as_f64().unwrap() <= 0.0);
    assert_eq!(v["week"]["measured"]["highOpen"], false);
    assert!(
        detail.contains(&format!(
            "Status: not working: Lumen cost more than it saved: {phrase} this week"
        )),
        "{detail}"
    );
    assert!(detail.contains("Recommendation: "), "{detail}");
}

#[test]
fn a_loss_under_half_a_cent_reads_minus_zero_in_lumen_and_lumen_report() {
    // SAFETY: as above.
    unsafe { std::env::set_var("TZ", "UTC") };
    let (_dir, home, db, spool) = sandbox();
    {
        let conn = lumen_core::meter::connect_db(&db).unwrap();
        let mut l = Ledger {
            conn: &conn,
            turn_n: 0,
        };
        // Last week, a first answer of 30 tokens; this week, in the same conversation, a stub
        // of 62 for the same request: 32 tokens more than the answer it stood in for, and no
        // extra round. A loss of a few hundredths of a cent.
        let first = MONDAY_MS - 2 * 86_400_000;
        read_session(&mut l, "tiny", first, 2);
        l.call(
            Some("tiny"),
            None,
            first + 2_000,
            "smart_read",
            "smart_read",
            "/repo/src/tiny.rs",
            6_000,
            30,
            Some("measured"),
            Some(("treatment", "first")),
        );
        let again = MONDAY_MS + 3_600_000;
        read_session(&mut l, "tiny", again, 3);
        l.call(
            Some("tiny"),
            None,
            again + 2_000,
            "smart_read",
            "smart_read_unchanged",
            "/repo/src/tiny.rs",
            6_000,
            62,
            Some("measured"),
            Some(("treatment", "unchanged")),
        );
    }
    billing(&db, Some("api"));
    let (line, detail, v) = surfaces(&home, &db, &spool);
    let net = v["week"]["measured"]["net"]["value"].as_f64().unwrap();
    assert!(net < 0.0 && net > -0.005, "premise: {net}");
    // 32 more in cl100k_base's count; in Claude's, each side converted by the file's type and
    // the model that read it.
    let model = v["ledger"]["recent"][0]["pair"]["model"].as_str().unwrap();
    let claude = |n: u64| {
        lumen_core::calibration::calibrate(
            n,
            lumen_core::calibration::ContentType::Rust,
            lumen_core::calibration::Model::from_model(model),
        )
        .value as i64
    };
    let more = claude(62) - claude(30);
    assert!(
        more > 32,
        "premise: Claude counts more than the proxy, {more}"
    );
    assert_eq!(v["week"]["tokensSaved"]["value"], -more);
    assert!(
        line.contains(&format!(
            "Lumen this week: \u{2248}\u{a0}{more} more tokens, net; net loss under half a \
             cent \u{b7} "
        )),
        "{line}"
    );
    assert!(
        detail.contains("on record              net loss under half a cent, from 1 row"),
        "{detail}"
    );
}

#[test]
fn a_week_of_blocked_reads_alone_says_how_many_beside_its_figure() {
    // SAFETY: as above.
    unsafe { std::env::set_var("TZ", "UTC") };
    let (_dir, home, db, spool) = sandbox();
    {
        let conn = lumen_core::meter::connect_db(&db).unwrap();
        let mut l = Ledger {
            conn: &conn,
            turn_n: 0,
        };
        // A call nine days ago, outside the last seven; this week, blocked Reads and no call.
        let old = MONDAY_MS - 7 * 86_400_000;
        read_session(&mut l, "old", old, 3);
        l.call(
            Some("old"),
            None,
            old + 2_000,
            "smart_read",
            "smart_read",
            "/repo/src/a.rs",
            6_000,
            500,
            Some("measured"),
            Some(("treatment", "first")),
        );
        let t = MONDAY_MS + 3_600_000;
        read_session(&mut l, "blk", t, 4);
        for (i, path) in ["/repo/src/big.rs", "/repo/src/big2.rs"].iter().enumerate() {
            conn.execute(
                "INSERT INTO faults(ts,kind,variant,path,session_id)
                 VALUES(?1,'hook_fail_open','retry_escape_valve',?2,'blk')",
                rusqlite::params![iso(t + 6_000 + i as i64 * 30_000), path],
            )
            .unwrap();
        }
    }
    billing(&db, Some("api"));
    let (line, detail, v) = surfaces(&home, &db, &spool);
    assert_eq!(v["week"]["counts"]["calls"], 0);
    assert_eq!(v["week"]["counts"]["blockedReads"], 2);
    let net = text::usd_phrase(v["week"]["measured"]["net"]["value"].as_f64().unwrap());
    assert!(
        line.contains(&format!(
            "Lumen this week: \u{2248}\u{a0}0 tokens saved, net; {net} (2 blocked Reads) \u{b7} "
        )),
        "{line}"
    );
    assert!(detail.contains("Status: not working: in the last 7 days the Read intercept blocked"));
    assert!(detail.contains("`lumen doctor`"), "{detail}");
}

#[test]
fn an_empty_ledger_and_a_ledger_that_cannot_be_read_say_so_with_no_figure() {
    let (_dir, home, db, spool) = sandbox();
    let (line, _, code) = run_lumen(&home, &db, &spool, &["--as-of", AS_OF]);
    assert_eq!(code, 0);
    assert_eq!(
        line.trim_end(),
        "Lumen: no Lumen activity recorded yet \u{b7} no data (lumen report for detail, lumen \
         dash for the live dashboard)"
    );
    let (detail, _, _) = run_lumen(&home, &db, &spool, &["report", "--as-of", AS_OF]);
    assert!(detail.contains("no Lumen activity recorded"), "{detail}");
    assert!(!detail.contains('$'), "{detail}");

    // A ledger that is a directory cannot be opened: said, with no figure, and a failing exit.
    std::fs::create_dir(&db).unwrap();
    let (line, _, code) = run_lumen(&home, &db, &spool, &["--as-of", AS_OF]);
    assert_eq!(code, 1, "{line}");
    assert!(
        line.starts_with("Lumen: no savings figure (cannot open "),
        "{line}"
    );
    assert!(!line.contains('$'), "{line}");
    let (_, err, code) = run_lumen(&home, &db, &spool, &["report", "--as-of", AS_OF]);
    assert_eq!(code, 1);
    assert!(err.starts_with("lumen report: cannot open "), "{err}");
}
