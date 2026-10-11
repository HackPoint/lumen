//! Repeat reads (1.7.0), end to end, through the real `lumen-mcp` binary.
//!
//! The base lives in the server's memory, so a test holds one process open and makes several
//! calls on it, the way a session does; a new process is a restart. What invalidates a base
//! (compaction, rewind) is a row in the ledger's `session_events`, which the daemon writes from
//! the transcript; the tests write those rows directly. Everything is isolated to a tempdir.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;
use tempfile::TempDir;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const ON: &[(&str, &str)] = &[("LUMEN_REPEAT_READS", "on")];

struct Rig {
    dir: TempDir,
    db: std::path::PathBuf,
}

struct Reply {
    text: String,
    returned: i64,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("ledger.db");
    // The schema exists before the server runs, so a test can add events without racing its
    // first write.
    lumen_core::meter::connect_db(&db).unwrap();
    Rig { dir, db }
}

/// A Rust file of `defs` functions with a body of `body` statements each.
fn source(defs: usize, body: usize) -> String {
    let mut s = String::from("use std::collections::HashMap;\n\n");
    for i in 0..defs {
        s.push_str(&func(&format!("operation_{i}"), "input: &str", body, ""));
    }
    s
}

fn func(name: &str, params: &str, body: usize, tweak: &str) -> String {
    let mut s = format!("pub fn {name}({params}) -> String {{\n");
    for j in 0..body {
        s.push_str(&format!("    let v{j} = input.len() + {j};{tweak}\n"));
    }
    s.push_str("    input.to_string()\n}\n\n");
    s
}

struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    id: i64,
}

impl Rig {
    fn file(&self, name: &str, body: &str) -> String {
        let p = self.dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }

    fn start(&self, env: &[(&str, &str)]) -> Live {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.env("LUMEN_DB", &self.db)
            .env("LUMEN_PROJECTS_DIR", self.dir.path().join("projects"))
            .env_remove("LUMEN_RANKED_OUTLINE")
            .env_remove("LUMEN_REPEAT_READS")
            .env("CLAUDE_CODE_SESSION_ID", SESSION)
            .env("LUMEN_RANKED_TIME_BUDGET_MS", "60000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut live = Live {
            child,
            stdin,
            out,
            id: 1,
        };
        live.rpc(
            "initialize",
            serde_json::json!({"protocolVersion":"2024-11-05","capabilities":{},
                "clientInfo":{"name":"e2e","version":"0"}}),
        );
        live
    }

    /// Insert a compaction or rewind the daemon would have recorded, stamped now.
    fn event(&self, kind: &str, uuid: &str) {
        let c = lumen_core::meter::connect_db(&self.db).unwrap();
        c.execute(
            &format!(
                "INSERT INTO session_events (session_id, kind, uuid, ts_ms, source_file) \
                 VALUES ('{SESSION}', '{kind}', '{uuid}', {}, 't.jsonl')",
                lumen_core::repeat::now_ms()
            ),
            [],
        )
        .unwrap();
    }

    /// `(routed_via, repeat_arm, repeat_plan)` of every smart_read / recall_file row.
    fn rows(&self) -> Vec<(String, Option<String>, Option<String>)> {
        let conn = lumen_core::meter::open_read_only(&self.db).unwrap();
        let mut st = conn
            .prepare(
                "SELECT routed_via, repeat_arm, repeat_plan FROM read_events \
                 WHERE tool LIKE '%smart_read' OR tool LIKE '%recall_file' ORDER BY rowid",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }
}

impl Live {
    fn rpc(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let req = serde_json::json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        writeln!(self.stdin.as_mut().unwrap(), "{req}").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    fn call(&mut self, tool: &str, args: serde_json::Value) -> Reply {
        let v = self.rpc(
            "tools/call",
            serde_json::json!({"name": tool, "arguments": args}),
        );
        // The server replies first and writes its ledger row after. The server is sequential,
        // so once this answer arrives that write is done and the test may touch the ledger.
        self.rpc("tools/list", serde_json::json!({}));
        Reply {
            text: v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no text in {v}"))
                .to_string(),
            returned: v["result"]["_meta"]["returned_tokens"].as_i64().unwrap(),
        }
    }

    fn read(&mut self, path: &str) -> Reply {
        self.call("smart_read", serde_json::json!({ "path": path }))
    }

    fn fresh(&mut self, path: &str) -> Reply {
        self.call(
            "smart_read",
            serde_json::json!({ "path": path, "fresh": true }),
        )
    }

    fn recall(&mut self, path: &str, names: &[&str]) -> Reply {
        self.call(
            "recall_file",
            serde_json::json!({ "path": path, "names": names }),
        )
    }

    /// Close the server so its last ledger write has happened before the ledger is read.
    fn end(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

fn plans(rig: &Rig) -> Vec<String> {
    rig.rows()
        .into_iter()
        .map(|(_, _, p)| p.unwrap_or_else(|| "-".into()))
        .collect()
}

/// Names on the lines of `text` that start with `marker`, ranges stripped.
fn names(text: &str, marker: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix(marker))
        .flat_map(|rest| rest.split(", ").map(str::to_string).collect::<Vec<_>>())
        .map(|n| n.split(" L").next().unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn flag_off_changes_nothing_and_advertises_nothing() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(&[]);
    let a = s.read(&f);
    let b = s.read(&f);
    assert_eq!(a.text, b.text, "a repeat is answered exactly as the first");
    assert_eq!(a.returned, b.returned);
    let list = s.rpc("tools/list", serde_json::json!({})).to_string();
    assert!(!list.contains("fresh"), "off must not change the tool list");
    s.end();
    assert!(
        rig.rows()
            .iter()
            .all(|(_, arm, plan)| arm.is_none() && plan.is_none()),
        "off records no repeat provenance"
    );

    let on = rig
        .start(ON)
        .rpc("tools/list", serde_json::json!({}))
        .to_string();
    assert!(on.contains("fresh"));
}

#[test]
fn an_unchanged_repeat_is_a_short_answer_that_says_how_to_ask_again() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    let first = s.read(&f);
    assert!(first.text.contains("— outline"), "{}", first.text);

    let second = s.read(&f);
    assert_eq!(
        second.text,
        "Unchanged since an earlier read of this file in this session. If that read is not in \
         your context, call smart_read(path, fresh=true).\n"
    );
    assert!(second.returned * 3 < first.returned);
    assert!(s.read(&f).text.starts_with("Unchanged since"));
    s.end();

    let rows = rig.rows();
    assert_eq!(rows[1].0, "smart_read_unchanged");
    assert_eq!(rows[1].1.as_deref(), Some("treatment"));
    assert_eq!(plans(&rig), ["first", "unchanged", "unchanged"]);
    eprintln!(
        "unchanged response: {} tokens vs {} for the full outline",
        second.returned, first.returned
    );
}

#[test]
fn a_restart_loses_the_base_and_falls_back_to_the_full_outline() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    s.end();
    let mut s = rig.start(ON);
    assert!(s.read(&f).text.contains("— outline"));
    s.end();
    assert_eq!(plans(&rig), ["first", "first"]);
}

/// The fixture: add, remove, signature change, body-only change, and a symbol shifted by an
/// insertion above it.
fn edited_fixture() -> (String, String) {
    let mut before = String::from("use std::collections::HashMap;\n\n");
    for i in 0..30 {
        before.push_str(&func(&format!("op_{i}"), "input: &str", 6, ""));
    }
    let mut after = String::from("use std::collections::HashMap;\n\n");
    for i in 0..30 {
        match i {
            2 => {
                after.push_str(&func("brand_new", "input: &str", 3, ""));
                after.push_str(&func("op_2", "input: &str", 6, ""));
            }
            5 => after.push_str(&func("op_5", "input: &str", 6, " // edited")),
            9 => after.push_str(&func("op_9", "input: &String, extra: u8", 6, "")),
            12 => {}
            _ => after.push_str(&func(&format!("op_{i}"), "input: &str", 6, "")),
        }
    }
    (before, after)
}

#[test]
fn an_edit_gets_a_delta_naming_exactly_the_changed_symbols() {
    let rig = rig();
    let (before, after) = edited_fixture();
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    let full = s.read(&f);
    rig.file("a.rs", &after);
    let d = s.read(&f);
    s.end();

    assert!(
        d.text.starts_with("Changed since an earlier read"),
        "{}",
        d.text
    );
    assert!(d.text.contains("fresh=true"), "the way out must be stated");
    assert_eq!(names(&d.text, "changed [body]: "), ["op_5"], "{}", d.text);
    assert_eq!(
        names(&d.text, "changed [signature]: "),
        ["op_9"],
        "{}",
        d.text
    );
    assert_eq!(names(&d.text, "added: "), ["brand_new"], "{}", d.text);
    assert_eq!(names(&d.text, "removed: "), ["op_12"], "{}", d.text);
    // op_20 is pushed down by the insertion above it: moved, never changed.
    assert!(
        d.text.lines().any(|l| l.starts_with("moved")),
        "a shifted symbol is reported as moved: {}",
        d.text
    );
    assert!(
        !d.text
            .lines()
            .any(|l| l.starts_with("changed") && l.contains("op_20"))
    );
    assert_eq!(plans(&rig)[1], "delta");
    eprintln!(
        "fixture delta: {} tokens vs {} for the full outline ({:.1}%)",
        d.returned,
        full.returned,
        100.0 * d.returned as f64 / full.returned as f64
    );
}

#[test]
fn a_symbol_only_shifted_by_an_insertion_is_never_changed() {
    let rig = rig();
    let before = source(30, 6);
    let after = before.replacen(
        "pub fn operation_3",
        "pub fn inserted() {}\n\npub fn operation_3",
        1,
    );
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    s.read(&f);
    rig.file("a.rs", &after);
    let d = s.read(&f);
    s.end();
    assert!(names(&d.text, "changed [body]: ").is_empty(), "{}", d.text);
    assert!(
        names(&d.text, "changed [signature]: ").is_empty(),
        "{}",
        d.text
    );
    assert_eq!(names(&d.text, "added: "), ["inserted"]);
}

#[test]
fn a_same_size_edit_in_the_same_second_is_detected() {
    let rig = rig();
    let before = source(40, 6);
    let after = before.replacen("input.len() + 3;", "input.len() * 3;", 1);
    assert_eq!(before.len(), after.len());
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    s.read(&f);
    rig.file("a.rs", &after);
    let d = s.read(&f);
    s.end();
    assert!(d.text.starts_with("Changed since"), "{}", d.text);
    assert!(!d.text.contains("Unchanged since"));
}

#[test]
fn crlf_and_lf_copies_are_the_same_file() {
    let rig = rig();
    let lf = source(40, 6);
    let crlf = lf.replace('\n', "\r\n");
    let f = rig.file("a.rs", &lf);
    let mut s = rig.start(ON);
    s.read(&f);
    rig.file("a.rs", &crlf);
    let again = s.read(&f);
    s.end();
    assert!(again.text.starts_with("Unchanged since"), "{}", again.text);
}

#[test]
fn compaction_between_two_reads_yields_the_full_outline() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    rig.event("compaction", "c1");
    let after = s.read(&f);
    assert!(after.text.contains("— outline"), "{}", after.text);
    // The outline was delivered again, so what follows can be a repeat of it.
    assert!(s.read(&f).text.starts_with("Unchanged since"));
    s.end();
    assert_eq!(plans(&rig), ["first", "compaction", "unchanged"]);
}

#[test]
fn compaction_between_two_reads_is_not_answered_with_a_delta_either() {
    let rig = rig();
    let (before, after) = edited_fixture();
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    s.read(&f);
    rig.event("compaction", "c1");
    rig.file("a.rs", &after);
    let d = s.read(&f);
    s.end();
    assert!(d.text.contains("— outline"), "{}", d.text);
}

#[test]
fn a_rewind_between_two_reads_yields_the_full_outline() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    rig.event("rewind", "r1");
    let after = s.read(&f);
    assert!(after.text.contains("— outline"), "{}", after.text);
    s.end();
    assert_eq!(plans(&rig), ["first", "rewind"]);
}

#[test]
fn a_resync_between_two_reads_yields_the_full_outline() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    rig.event("resync", "resync-1-0");
    let after = s.read(&f);
    assert!(after.text.contains("— outline"), "{}", after.text);
    assert!(s.read(&f).text.starts_with("Unchanged since"));
    s.end();
    assert_eq!(plans(&rig), ["first", "resync", "unchanged"]);
}

#[test]
fn a_resync_between_two_reads_is_not_answered_with_a_delta_either() {
    let rig = rig();
    let (before, after) = edited_fixture();
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    s.read(&f);
    rig.event("resync", "resync-1-0");
    rig.file("a.rs", &after);
    let d = s.read(&f);
    s.end();
    assert!(d.text.contains("— outline"), "{}", d.text);
}

#[test]
fn an_event_from_before_the_read_does_not_invalidate_it() {
    let rig = rig();
    rig.event("compaction", "old");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    assert!(s.read(&f).text.starts_with("Unchanged since"));
    s.end();
}

#[test]
fn a_ledger_that_cannot_be_asked_gets_the_full_outline() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.read(&f);
    {
        let c = lumen_core::meter::connect_db(&rig.db).unwrap();
        c.execute_batch("DROP TABLE session_events").unwrap();
    }
    let again = s.read(&f);
    s.end();
    assert!(again.text.contains("— outline"), "{}", again.text);
    assert_eq!(plans(&rig)[1], "unverifiable");
}

#[test]
fn fresh_always_returns_the_full_outline_and_is_recorded() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    let first = s.read(&f);
    let fresh = s.fresh(&f);
    assert_eq!(fresh.text, first.text);
    assert!(
        s.read(&f).text.starts_with("Unchanged since"),
        "fresh re-bases"
    );
    s.end();
    assert_eq!(plans(&rig), ["first", "fresh", "unchanged"]);
}

#[test]
fn fresh_is_honoured_and_counted_with_the_flag_off() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(&[]);
    let a = s.fresh(&f);
    assert!(a.text.contains("— outline"));
    s.end();
    assert_eq!(plans(&rig), ["fresh"]);
}

#[test]
fn ab_withholds_the_answer_from_control_files_and_records_what_it_would_have_done() {
    let rig = rig();
    let mut s = rig.start(&[("LUMEN_REPEAT_READS", "ab")]);
    let (mut treated, mut control) = (0, 0);
    for i in 0..16 {
        let f = rig.file(&format!("f{i}.rs"), &source(30 + i, 5));
        s.read(&f);
        let second = s.read(&f);
        if second.text.starts_with("Unchanged since") {
            treated += 1;
        } else {
            assert!(second.text.contains("— outline"));
            control += 1;
        }
    }
    s.end();
    assert!(treated >= 3 && control >= 3, "split {treated}/{control}");
    let rows = rig.rows();
    let ctl: Vec<_> = rows
        .iter()
        .filter(|r| r.1.as_deref() == Some("control"))
        .collect();
    assert_eq!(ctl.len(), 2 * control);
    assert!(
        ctl.iter()
            .any(|r| r.2.as_deref() == Some("unchanged") && r.0 == "smart_read"),
        "a control row records the answer it would have been served: {ctl:?}"
    );
}

#[test]
fn a_tiny_file_is_never_answered_with_something_larger_than_it() {
    let rig = rig();
    let f = rig.file("t.rs", "fn a() {}\nfn b() {}\n");
    let mut s = rig.start(ON);
    s.read(&f);
    let second = s.read(&f);
    s.end();
    assert!(
        !second.text.starts_with("Unchanged since"),
        "{}",
        second.text
    );
}

#[test]
fn recall_file_repeats_are_unchanged_then_a_scoped_diff() {
    let rig = rig();
    let before = source(30, 14);
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(ON);
    let first = s.recall(&f, &["operation_3"]);
    assert!(first.text.contains("operation_3"));

    let second = s.recall(&f, &["operation_3"]);
    assert!(
        second.text.starts_with("Unchanged since an earlier read"),
        "{}",
        second.text
    );
    assert!(second.text.contains("recall_file(path, fresh=true)"));
    assert!(second.returned < first.returned);

    // A line inside operation_3 changes: the answer is that hunk, under its own header.
    let at = before.find("pub fn operation_3").unwrap();
    let (head, tail) = before.split_at(at);
    rig.file(
        "a.rs",
        &format!(
            "{head}{}",
            tail.replacen("let v4 = input.len() + 4;", "let v4 = input.len() - 4;", 1)
        ),
    );
    let third = s.recall(&f, &["operation_3"]);
    assert!(third.text.starts_with("Changed since"), "{}", third.text);
    assert!(third.text.contains("@@ L"), "{}", third.text);
    assert!(
        third.text.contains("-    let v4 = input.len() + 4;"),
        "{}",
        third.text
    );
    assert!(third.returned < first.returned);

    // A different request has its own base.
    assert!(s.recall(&f, &["operation_4"]).text.contains("operation_4"));
    let fresh = s.call(
        "recall_file",
        serde_json::json!({"path": f, "names": ["operation_3"], "fresh": true}),
    );
    assert!(fresh.text.contains("let v4 = input.len() - 4;"));
    s.end();
    assert_eq!(
        plans(&rig),
        ["first", "unchanged", "delta", "first", "fresh"]
    );
}

#[test]
fn recall_file_after_compaction_gets_the_full_answer() {
    let rig = rig();
    let f = rig.file("a.rs", &source(30, 14));
    let mut s = rig.start(ON);
    s.recall(&f, &["operation_3"]);
    rig.event("compaction", "c1");
    let again = s.recall(&f, &["operation_3"]);
    s.end();
    assert!(
        again.text.contains("let v13 = input.len() + 13;"),
        "{}",
        again.text
    );
    assert!(!again.text.starts_with("Unchanged"));
}

#[test]
fn a_repeat_costs_no_more_time_than_a_first_read() {
    let rig = rig();
    let n = 40;
    let big = rig.file("big.rs", &source(400, 8));

    let time = |env: &[(&str, &str)]| {
        let mut s = rig.start(env);
        let t = Instant::now();
        let mut last = None;
        for _ in 0..n {
            last = Some(s.read(&big));
        }
        let e = t.elapsed();
        s.end();
        (e, last.unwrap())
    };
    let (off, _) = time(&[]);
    let (on, last) = time(ON);
    assert!(last.text.starts_with("Unchanged since"));
    eprintln!("{n} reads of a 400-function file: off {off:?}, on {on:?}");
    assert!(
        on.as_secs_f64() < off.as_secs_f64() * 1.5 + 1.0,
        "repeat path slower than the path it replaces: {on:?} vs {off:?}"
    );
}

fn git(args: &[&str]) -> Option<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let o = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

fn percentile(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// Real edits from this repository's history, through the real binary: for each, the full
/// outline of the edited file, what the delta cost instead, and what a further unchanged read
/// cost. A delta that fell back to the outline counts as 100%, so the figures are not flattered.
/// Test-only use of `git`: the tool and hook paths never run it.
#[test]
fn real_edits_from_history_cost_less_than_the_outline() {
    // Pinned, so the sample does not move with every commit; absent in a shallow clone, where
    // the test skips.
    const CORPUS_TIP: &str = "a791f86";
    let Some(log) = git(&["log", "--no-merges", "-n", "900", "--format=%H", CORPUS_TIP]) else {
        eprintln!("no git history available; corpus measurement skipped");
        return;
    };
    let want: usize = std::env::var("LUMEN_REPEAT_CORPUS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let mut pairs: Vec<(String, String, String)> = vec![];
    'commits: for c in log.lines() {
        let Some(files) = git(&[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--diff-filter=M",
            c,
        ]) else {
            continue;
        };
        for f in files.lines() {
            if !(f.ends_with(".rs") || f.ends_with(".ts") || f.ends_with(".py")) {
                continue;
            }
            let (Some(old), Some(new)) = (
                git(&["show", &format!("{c}^:{f}")]),
                git(&["show", &format!("{c}:{f}")]),
            ) else {
                continue;
            };
            if new.lines().count() < 300 || old == new {
                continue;
            }
            let ext = f.rsplit('.').next().unwrap().to_string();
            pairs.push((ext, old, new));
            if pairs.len() >= want {
                break 'commits;
            }
        }
    }
    if pairs.len() < 10 {
        eprintln!(
            "only {} edit pairs in this checkout (shallow clone?); skipped",
            pairs.len()
        );
        return;
    }

    let rig = rig();
    let mut s = rig.start(ON);
    let (mut delta_pct, mut stub_pct, mut fell_back) = (vec![], vec![], 0);
    let (mut sum_stub, mut sum_outline) = (0i64, 0i64);
    for (i, (ext, old, new)) in pairs.iter().enumerate() {
        let name = format!("p{i}.{ext}");
        let f = rig.file(&name, old);
        s.read(&f);
        rig.file(&name, new);
        let d = s.read(&f);
        let full = s.fresh(&f);
        let stub = s.read(&f);
        assert!(
            stub.text.starts_with("Unchanged since"),
            "{name}: {}",
            stub.text
        );
        assert!(
            d.returned <= full.returned,
            "{name}: a delta of {} tokens against an outline of {}",
            d.returned,
            full.returned
        );
        if d.text.contains("— outline") {
            fell_back += 1;
        }
        delta_pct.push(100.0 * d.returned as f64 / full.returned as f64);
        stub_pct.push(100.0 * stub.returned as f64 / full.returned as f64);
        sum_stub += stub.returned;
        sum_outline += full.returned;
    }
    s.end();
    let (dm, d90) = (
        percentile(&mut delta_pct, 0.5),
        percentile(&mut delta_pct, 0.9),
    );
    let sm = percentile(&mut stub_pct, 0.5);
    eprintln!(
        "{} real edits ({} fell back to the outline): delta median {dm:.1}% p90 {d90:.1}% of the \
         outline; unchanged median {sm:.1}% ({} vs {} tokens summed)",
        pairs.len(),
        fell_back,
        sum_stub,
        sum_outline
    );
    assert!(dm <= 25.0, "delta median {dm:.1}% of the outline");
    // The p90 is the edits that add many symbols to a small outline: the mandatory sentence
    // and the added names and ranges are most of such a delta. Bounded, not forced to 25%.
    assert!(d90 <= 35.0, "delta p90 {d90:.1}% of the outline");
    assert!(sum_stub * 4 < sum_outline);
}
