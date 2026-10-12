//! End-to-end: does a ranked `smart_read` actually record its decision?
//!
//! Driven through the real binary over stdio rather than by calling the function, because
//! the rollout flag and the database path are both environment, and mutating the
//! environment in-process is racy across threads and `unsafe` in edition 2024. A
//! subprocess gets its own environment safely.
//!
//! Every case points `LUMEN_DB` at a tempdir. Nothing here may touch the real ledger.

use lumen_core::tokenizer::count_tokens;
use std::io::Write;
use std::process::{Command, Stdio};

/// Run one `smart_read` against `path` with the given rollout mode, and return the rows
/// written, as `(routed_via, budget, s_min, k, n, coeff, econ_source)`.
type Row = (
    String,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn smart_read(mode: Option<&str>, target: &str) -> (Vec<Row>, String) {
    smart_read_with(mode, target, &[])
}

fn smart_read_with(mode: Option<&str>, target: &str, extra: &[(&str, &str)]) -> (Vec<Row>, String) {
    let run = run_smart_read(mode, target, extra);
    (run.rows, run.stdout)
}

/// Everything one `smart_read` through the real binary leaves behind.
struct Run {
    rows: Vec<Row>,
    /// `(tokens_returned, full_tokens, saved_tokens)` per ledger row, in write order.
    counts: Vec<(i64, i64, i64)>,
    stdout: String,
}

impl Run {
    /// The text of the tool reply, parsed out of the frame rather than searched for in the raw
    /// stdout, which also carries the `initialize` reply.
    fn reply(&self) -> String {
        self.stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["id"] == 2)
            .and_then(|v| {
                v["result"]["content"][0]["text"]
                    .as_str()
                    .map(str::to_owned)
            })
            .expect("a tools/call reply with a text block")
    }
}

fn run_smart_read(mode: Option<&str>, target: &str, extra: &[(&str, &str)]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ledger.db");
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the test ledger must live under the temp dir"
    );

    let reqs = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},
                      "clientInfo":{"name":"t","version":"0"}}}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"smart_read","arguments":{"path": target}}}),
    ];
    let mut input = String::new();
    for r in reqs {
        input.push_str(&serde_json::to_string(&r).unwrap());
        input.push('\n');
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
    cmd.env("LUMEN_DB", &db)
        // A wall-clock deadline is a property of the machine, not of the code. Left at
        // its default these tests declined as `TooSlow` on CI runners and passed
        // locally, which is a test measuring the runner. The TooSlow path has its own
        // test below, where the budget is pinned to zero.
        .env("LUMEN_RANKED_TIME_BUDGET_MS", "60000")
        .env_remove("LUMEN_RANKED_OUTLINE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(m) = mode {
        cmd.env("LUMEN_RANKED_OUTLINE", m);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn lumen-mcp");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    let conn = rusqlite::Connection::open(&db).expect("ledger must exist");
    let mut stmt = conn
        .prepare(
            "SELECT routed_via, budget, s_min, k_selected, n_total, coeff_version, econ_source
             FROM read_events ORDER BY rowid",
        )
        .unwrap();
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let mut stmt = conn
        .prepare(
            "SELECT tokens_returned, full_tokens, saved_tokens FROM read_events ORDER BY rowid",
        )
        .unwrap();
    let counts: Vec<(i64, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    Run {
        rows,
        counts,
        stdout,
    }
}

/// A file large enough to clear `S_min` at the measured defaults (~5.4k tokens).
fn big_rust_file(dir: &std::path::Path) -> String {
    let mut src = String::new();
    for i in 0..400 {
        src.push_str(&format!(
            "/// Item {i}.\npub fn function_number_{i}(a: u32, b: &str) -> Result<Vec<String>, Error> {{\n    let mut v = Vec::new();\n    v.push(format!(\"{{}}{{}}\", a, b));\n    Ok(v)\n}}\n\n"
        ));
    }
    let p = dir.join("big.rs");
    std::fs::write(&p, src).unwrap();
    p.to_string_lossy().to_string()
}

#[test]
fn the_default_is_the_legacy_arm_and_records_no_decision() {
    let dir = tempfile::tempdir().unwrap();
    let target = big_rust_file(dir.path());
    let (rows, _) = smart_read(None, &target);

    assert_eq!(rows.len(), 1, "one call, one row");
    let (route, budget, s_min, k, n, coeff, src) = &rows[0];
    assert_eq!(route, "smart_read", "the default arm must be unchanged");
    assert_eq!(
        (budget, s_min, k, n, coeff, src),
        (&None, &None, &None, &None, &None, &None),
        "the legacy arm makes no budget decision, so every decision column must be NULL \
         rather than zero — a zero would claim a decision was made"
    );
}

#[test]
fn an_unrecognised_flag_value_does_not_enable_the_experiment() {
    let dir = tempfile::tempdir().unwrap();
    let target = big_rust_file(dir.path());
    // "ON " is deliberately NOT here: the parser trims and case-folds, so surrounding
    // whitespace and capitalisation are accepted spellings of a real value. What must not
    // be accepted is a value that merely looks affirmative.
    for bad in ["yes", "enabled", "2", "", "off", "no"] {
        let (rows, _) = smart_read(Some(bad), &target);
        assert_eq!(
            rows[0].0, "smart_read",
            "{bad:?} must not switch arms; a typo enabling an experiment is worse than \
             one disabling it"
        );
    }
}

#[test]
fn the_ranked_arm_records_every_decision_input() {
    let dir = tempfile::tempdir().unwrap();
    let target = big_rust_file(dir.path());
    let run = run_smart_read(Some("on"), &target, &[]);
    let rows = &run.rows;

    assert_eq!(rows.len(), 1);
    let (route, budget, s_min, k, n, coeff, econ_source) = &rows[0];
    assert_eq!(
        count_tokens(&run.reply()) as i64,
        run.counts[0].0,
        "the row must carry the cost of the reply that went out"
    );
    assert_eq!(
        route, "ranked_outline",
        "a successful ranked call gets its own route so it can never be pooled with the \
         legacy arm by a query written before the experiment"
    );
    assert!(
        budget.is_some() && s_min.is_some(),
        "budget and S_min recorded"
    );
    assert!(
        budget.unwrap() > 0,
        "premise: this file clears the bar (budget {})",
        budget.unwrap()
    );
    let (k, n) = (k.unwrap(), n.unwrap());
    assert!(n > 0, "definitions were found");
    assert!(k > 0 && k <= n, "k={k} of n={n}");
    assert_eq!(
        *coeff,
        Some(1),
        "the coefficient set version is pinned on the row"
    );
    // No ledger to average, so it must say so rather than pass off a shipped constant as
    // this installation's own measurement.
    assert_eq!(econ_source.as_deref(), Some("measured_defaults"));
}

/// A file below the ranked arm's break-even whose outline is still far smaller than it.
fn modest_rust_file(dir: &std::path::Path) -> String {
    let mut src = String::new();
    for i in 0..6 {
        src.push_str(&format!(
            "/// Step {i}.\npub fn step_number_{i}(a: u32) -> u32 {{\n"
        ));
        for j in 0..18 {
            src.push_str(&format!(
                "    let v{j} = a.wrapping_mul({j}).wrapping_add({i});\n"
            ));
        }
        src.push_str("    a\n}\n\n");
    }
    let p = dir.join("modest.rs");
    std::fs::write(&p, src).unwrap();
    p.to_string_lossy().to_string()
}

/// A decline still writes a row, on the decline's own route, with the decision that
/// produced it. Without this the ledger would hold only the calls that went ahead — the
/// population that makes any gate look unnecessary.
#[test]
fn a_declined_call_records_the_refusal_and_still_returns_an_outline() {
    let dir = tempfile::tempdir().unwrap();
    // Comfortably below S_min at the measured defaults, and much larger than its outline.
    let target = modest_rust_file(dir.path());

    let run = run_smart_read(Some("on"), &target, &[]);
    let (rows, stdout) = (&run.rows, &run.stdout);

    assert_eq!(rows.len(), 1);
    let (route, budget, s_min, k, n, _, _) = &rows[0];
    assert_eq!(
        route, "ranked_not_worth_it",
        "the refusal must be visible under its own route"
    );
    assert!(
        budget.unwrap() < 0,
        "a file this small cannot pay for the round it forced (budget {})",
        budget.unwrap()
    );
    assert!(s_min.unwrap() > 0);
    assert_eq!((k, n), (&Some(0), &Some(0)), "nothing was selected");

    // And the model still got a usable answer: the caller asked for an outline.
    assert!(
        stdout.contains("outline"),
        "a decline must fall back to the legacy outline, not return nothing"
    );
    let (returned, full, saved) = run.counts[0];
    assert!(returned < full, "the outline is the cheaper answer here");
    assert_eq!(saved, full - returned);
}

/// The fallback behind a decline is the legacy outline, and a file smaller than its own outline
/// is exactly the file a decline is most likely to be about. Returning that outline would cost
/// more than reading the file, on a path that exists to be a cheap fallback.
#[test]
fn a_declined_call_never_returns_more_than_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("small.rs");
    let body = "pub fn a() -> u8 { 1 }\npub fn b() -> u8 { 2 }\n";
    std::fs::write(&p, body).unwrap();
    let target = p.to_string_lossy().to_string();

    let run = run_smart_read(Some("on"), &target, &[]);

    assert_eq!(run.rows.len(), 1);
    let (route, budget, _, k, n, _, _) = &run.rows[0];
    assert_eq!(
        route, "ranked_not_worth_it",
        "the refusal stays visible under its own route"
    );
    assert!(budget.unwrap() < 0, "premise: the arm declined for size");
    assert_eq!((k, n), (&Some(0), &Some(0)));
    assert_eq!(run.reply(), body, "the file, not a larger rendering of it");
    let (returned, full, saved) = run.counts[0];
    assert_eq!(
        (returned, saved),
        (full, 0),
        "the row must say what was returned: the whole file, nothing saved"
    );
}

/// An empty text block is a reply a client may refuse, and the file is the cheaper answer only
/// when there is something in it to return.
#[test]
fn a_declined_call_on_a_blank_file_still_returns_something_visible() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [("empty.rs", ""), ("blank.rs", "\n\n  \n")] {
        let p = dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        let run = run_smart_read(Some("on"), &p.to_string_lossy(), &[]);
        assert!(
            !run.reply().trim().is_empty(),
            "{name}: a blank file must not be answered with a blank text block"
        );
        let (returned, full, saved) = run.counts[0];
        assert_eq!(
            returned,
            count_tokens(&run.reply()) as i64,
            "{name}: the row counts the reply as sent"
        );
        assert_eq!(saved, full - returned, "{name}");
    }
}

#[test]
fn an_unsupported_language_declines_on_its_own_route() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("thing.go");
    // Large enough to clear S_min, so the decline is attributable to the language.
    let body = "// padding padding padding padding padding padding\n".repeat(2_000);
    std::fs::write(&p, format!("package main\n{body}")).unwrap();
    let target = p.to_string_lossy().to_string();

    let (rows, _) = smart_read(Some("on"), &target);
    assert_eq!(rows[0].0, "ranked_no_query");
    assert!(
        rows[0].1.unwrap() > 0,
        "premise: the budget was positive, so the language is the reason"
    );
}

/// The A/B arm is a pure function of the path, so a given file is always compared
/// against itself across sessions.
#[test]
fn the_ab_mode_assigns_a_stable_arm_to_a_given_path() {
    let dir = tempfile::tempdir().unwrap();
    let target = big_rust_file(dir.path());
    let first = smart_read(Some("ab"), &target).0[0].0.clone();
    for _ in 0..3 {
        assert_eq!(
            smart_read(Some("ab"), &target).0[0].0,
            first,
            "the same path must not change arms between runs"
        );
    }
    assert!(
        first == "smart_read" || first.starts_with("ranked"),
        "unexpected route {first}"
    );
}

/// The timeout path, made deterministic by pinning the budget to zero rather than by
/// hoping a machine is slow enough.
#[test]
fn exceeding_the_time_budget_declines_on_its_own_route() {
    let dir = tempfile::tempdir().unwrap();
    let target = big_rust_file(dir.path());
    let (rows, stdout) =
        smart_read_with(Some("on"), &target, &[("LUMEN_RANKED_TIME_BUDGET_MS", "0")]);
    assert_eq!(
        rows[0].0, "ranked_too_slow",
        "a zero budget must always be exceeded, on any machine"
    );
    assert!(
        rows[0].1.unwrap() > 0,
        "premise: the budget was affordable, so time is the reason for the decline"
    );
    assert!(
        stdout.contains("outline"),
        "and the caller still gets an outline"
    );
}
