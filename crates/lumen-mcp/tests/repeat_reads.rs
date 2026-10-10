//! Repeat reads (1.7.0), end to end, through the real `lumen-mcp` binary.
//!
//! Every call is a separate process, which is how Claude Code restarts an MCP server and is
//! the point: what a repeat read depends on has to survive in the ledger, not in memory.
//! Everything is isolated to a tempdir (`LUMEN_DB`, `LUMEN_PROJECTS_DIR`).

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;
use tempfile::TempDir;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const BOUNDARY: &str =
    r#"{"type":"system","subtype":"compact_boundary","content":"Conversation compacted"}"#;
const USER: &str = r#"{"type":"user","message":{"content":"hi"}}"#;

struct Rig {
    dir: TempDir,
    db: std::path::PathBuf,
    projects: std::path::PathBuf,
}

struct Reply {
    text: String,
    returned: i64,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    let projects = dir.path().join("projects");
    std::fs::create_dir_all(projects.join("-proj")).unwrap();
    let db = dir.path().join("ledger.db");
    assert!(db.starts_with(std::env::temp_dir()));
    Rig { dir, db, projects }
}

/// A Rust file of `defs` functions with a body of `body` statements each.
fn source(defs: usize, body: usize) -> String {
    let mut s = String::from("use std::collections::HashMap;\n\n");
    for i in 0..defs {
        s.push_str(&format!("pub fn operation_{i}(input: &str) -> String {{\n"));
        for j in 0..body {
            s.push_str(&format!("    let v{j} = input.len() + {j};\n"));
        }
        s.push_str("    input.to_string()\n}\n\n");
    }
    s
}

impl Rig {
    fn file(&self, name: &str, body: &str) -> String {
        let p = self.dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }

    fn transcript(&self, session: &str) -> std::path::PathBuf {
        self.projects.join("-proj").join(format!("{session}.jsonl"))
    }

    fn write_transcript(&self, session: &str, lines: &[&str]) {
        let mut s = lines.join("\n");
        s.push('\n');
        std::fs::write(self.transcript(session), s).unwrap();
    }

    fn append_transcript(&self, session: &str, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.transcript(session))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    /// `n` smart_read calls on one process, so a timing loop is not dominated by spawns.
    fn calls(&self, args: &[serde_json::Value], env: &[(&str, &str)]) -> Vec<Reply> {
        let mut input = String::new();
        let init = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},
                      "clientInfo":{"name":"e2e","version":"0"}}});
        input.push_str(&format!("{init}\n"));
        input.push_str("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
        for (i, a) in args.iter().enumerate() {
            let r = serde_json::json!({"jsonrpc":"2.0","id":i + 2,"method":"tools/call",
                "params":{"name":"smart_read","arguments":a}});
            input.push_str(&format!("{r}\n"));
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.env("LUMEN_DB", &self.db)
            .env("LUMEN_PROJECTS_DIR", &self.projects)
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
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let mut replies: Vec<(i64, Reply)> = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["id"].as_i64().unwrap_or(0) >= 2)
            .map(|v| {
                (
                    v["id"].as_i64().unwrap(),
                    Reply {
                        text: v["result"]["content"][0]["text"]
                            .as_str()
                            .unwrap_or_else(|| panic!("no text in {v}"))
                            .to_string(),
                        returned: v["result"]["_meta"]["returned_tokens"].as_i64().unwrap(),
                    },
                )
            })
            .collect();
        replies.sort_by_key(|(id, _)| *id);
        replies.into_iter().map(|(_, r)| r).collect()
    }

    fn read(&self, path: &str, env: &[(&str, &str)]) -> Reply {
        self.calls(&[serde_json::json!({ "path": path })], env)
            .remove(0)
    }

    /// `(routed_via, repeat_arm, repeat_plan)` of every smart_read row, oldest first.
    fn rows(&self) -> Vec<(String, Option<String>, Option<String>)> {
        let conn = lumen_core::meter::open_read_only(&self.db).unwrap();
        let mut st = conn
            .prepare(
                "SELECT routed_via, repeat_arm, repeat_plan FROM read_events \
                 WHERE tool LIKE '%smart_read' ORDER BY rowid",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn state_rows(&self) -> i64 {
        match lumen_core::meter::open_read_only(&self.db) {
            Ok(c) => c
                .query_row("SELECT count(*) FROM read_state", [], |r| r.get(0))
                .unwrap_or(0),
            Err(_) => 0,
        }
    }
}

const ON: &[(&str, &str)] = &[("LUMEN_REPEAT_READS", "on")];

fn plans(rig: &Rig) -> Vec<String> {
    rig.rows()
        .into_iter()
        .map(|(_, _, p)| p.unwrap_or_else(|| "-".into()))
        .collect()
}

#[test]
fn flag_off_changes_nothing_and_remembers_nothing() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));
    let a = rig.read(&f, &[]);
    let b = rig.read(&f, &[]);
    assert_eq!(a.text, b.text, "a repeat is answered exactly as the first");
    assert_eq!(a.returned, b.returned);
    assert!(a.text.contains("— outline"));
    assert_eq!(rig.state_rows(), 0, "off writes no state");
    assert!(
        rig.rows()
            .iter()
            .all(|(_, arm, plan)| arm.is_none() && plan.is_none()),
        "off records no repeat provenance"
    );
}

#[test]
fn an_unchanged_repeat_is_a_stub_and_survives_a_restart() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));

    let first = rig.read(&f, ON);
    assert!(first.text.contains("— outline"), "{}", first.text);

    // A new process: the baseline is in the ledger, not in memory.
    let second = rig.read(&f, ON);
    assert!(second.text.contains("unchanged since your last smart_read"));
    assert!(
        second.text.contains("refresh=true"),
        "the way out must be stated"
    );
    assert!(
        second.returned * 3 < first.returned,
        "stub {} vs outline {}",
        second.returned,
        first.returned
    );

    let third = rig.read(&f, ON);
    assert!(
        third.text.contains("unchanged since"),
        "stable across repeats"
    );

    let rows = rig.rows();
    assert_eq!(rows[1].0, "smart_read_unchanged");
    assert_eq!(rows[1].1.as_deref(), Some("treatment"));
    assert_eq!(plans(&rig), ["first", "unchanged", "unchanged"]);
    eprintln!(
        "unchanged path: {} tokens vs {} for the full outline",
        second.returned, first.returned
    );
}

#[test]
fn an_edit_gets_a_delta_naming_the_item_and_not_the_whole_outline() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let before = source(40, 6);
    let f = rig.file("a.rs", &before);
    let first = rig.read(&f, ON);

    // Edit one body without changing its length in lines.
    let after = before.replacen("let v3 = input.len() + 3;", "let v3 = input.len() * 3;", 1);
    assert_ne!(before, after);
    rig.file("a.rs", &after);
    let delta = rig.read(&f, ON);
    assert!(
        delta.text.contains("changed since your last smart_read"),
        "{}",
        delta.text
    );
    assert!(
        delta.text.contains("~ function operation_0 "),
        "{}",
        delta.text
    );
    assert!(
        !delta.text.contains("operation_30"),
        "unchanged items are not repeated"
    );
    assert!(
        delta.returned * 5 < first.returned,
        "{} vs {}",
        delta.returned,
        first.returned
    );

    // The delta is the new baseline: an immediate repeat is unchanged, not another delta.
    let again = rig.read(&f, ON);
    assert!(again.text.contains("unchanged since"), "{}", again.text);
    assert_eq!(plans(&rig), ["first", "delta", "unchanged"]);
    assert_eq!(rig.rows()[1].0, "smart_read_delta");
}

#[test]
fn an_inserted_function_is_one_shift_line_not_a_relisted_tail() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let before = source(40, 6);
    let f = rig.file("a.rs", &before);
    rig.read(&f, ON);

    let after = before.replacen(
        "pub fn operation_5(",
        "pub fn inserted() {}\n\npub fn operation_5(",
        1,
    );
    rig.file("a.rs", &after);
    let d = rig.read(&f, ON);
    assert!(d.text.contains("+ function inserted"), "{}", d.text);
    assert!(d.text.contains("moved +2 lines"), "{}", d.text);
    assert!(!d.text.contains("operation_39"), "{}", d.text);
}

#[test]
fn a_rewrite_that_would_cost_as_much_as_the_outline_gets_the_outline() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));
    rig.read(&f, ON);
    // Every function renamed: the delta must list 40 removals and 40 additions.
    rig.file("a.rs", &source(40, 6).replace("operation_", "renamed_"));
    let r = rig.read(&f, ON);
    assert!(r.text.contains("— outline"), "{}", r.text);
    assert_eq!(plans(&rig), ["first", "delta_too_big"]);
}

#[test]
fn compaction_forces_a_full_outline_once_then_repeats_resume() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));
    rig.read(&f, ON);
    assert!(rig.read(&f, ON).text.contains("unchanged since"));

    rig.append_transcript(SESSION, USER);
    rig.append_transcript(SESSION, BOUNDARY);
    let after = rig.read(&f, ON);
    assert!(
        after.text.contains("— outline"),
        "the outline was compacted away; a stub would be a lie: {}",
        after.text
    );
    assert!(
        rig.read(&f, ON).text.contains("unchanged since"),
        "new baseline after compaction"
    );
    assert_eq!(
        plans(&rig),
        ["first", "unchanged", "compacted", "unchanged"]
    );
}

#[test]
fn the_phrase_in_a_tool_result_is_not_a_compaction() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));
    rig.read(&f, ON);
    rig.append_transcript(
        SESSION,
        r#"{"type":"user","message":{"content":"grep -rn compact_boundary src"}}"#,
    );
    assert!(rig.read(&f, ON).text.contains("unchanged since"));
}

#[test]
fn no_transcript_means_no_stub() {
    let rig = rig();
    // Nothing at LUMEN_PROJECTS_DIR for this session.
    let f = rig.file("a.rs", &source(40, 6));
    rig.read(&f, ON);
    let second = rig.read(&f, ON);
    assert!(second.text.contains("— outline"), "{}", second.text);
    assert_eq!(plans(&rig), ["first", "unverifiable"]);
}

#[test]
fn refresh_and_other_sessions_get_the_full_outline() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("a.rs", &source(40, 6));
    rig.read(&f, ON);

    let forced = rig
        .calls(&[serde_json::json!({ "path": f, "refresh": true })], ON)
        .remove(0);
    assert!(forced.text.contains("— outline"));

    let other = "99999999-2222-3333-4444-555555555555";
    rig.write_transcript(other, &[USER]);
    let mut env = ON.to_vec();
    env.push(("CLAUDE_CODE_SESSION_ID", other));
    assert!(
        rig.read(&f, &env).text.contains("— outline"),
        "state is per session"
    );
    assert_eq!(plans(&rig), ["first", "refresh", "first"]);
}

#[test]
fn ab_withholds_the_stub_from_control_files_and_records_what_it_would_have_done() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let env = [("LUMEN_REPEAT_READS", "ab")];
    let (mut treated, mut control) = (0, 0);
    for i in 0..16 {
        let f = rig.file(&format!("f{i}.rs"), &source(30 + i, 5));
        rig.read(&f, &env);
        let second = rig.read(&f, &env);
        if second.text.contains("unchanged since") {
            treated += 1;
        } else {
            assert!(second.text.contains("— outline"));
            control += 1;
        }
    }
    assert!(treated >= 3 && control >= 3, "split {treated}/{control}");
    let rows = rig.rows();
    let ctl: Vec<_> = rows
        .iter()
        .filter(|r| r.1.as_deref() == Some("control"))
        .collect();
    assert_eq!(
        ctl.len(),
        2 * control,
        "both reads of a control file are recorded as control"
    );
    assert!(
        ctl.iter()
            .any(|r| r.2.as_deref() == Some("unchanged") && r.0 == "smart_read"),
        "a control row records the stub it would have been served: {ctl:?}"
    );
}

#[test]
fn a_tiny_file_is_never_answered_with_a_stub_larger_than_it() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let f = rig.file("t.rs", "fn a() {}\nfn b() {}\n");
    rig.read(&f, ON);
    let second = rig.read(&f, ON);
    assert!(!second.text.contains("unchanged since"), "{}", second.text);
    assert_eq!(plans(&rig), ["first", "unchanged_not_smaller"]);
}

#[test]
fn a_repeat_costs_no_more_time_than_a_first_read() {
    let rig = rig();
    rig.write_transcript(SESSION, &[USER]);
    let n = 40;
    let big = rig.file("big.rs", &source(400, 8));
    let a = serde_json::json!({ "path": big });
    let args: Vec<_> = (0..n).map(|_| a.clone()).collect();

    // Off: every call is a full outline.
    let t = Instant::now();
    rig.calls(&args, &[]);
    let off = t.elapsed();

    // On: one fresh outline, then stubs.
    let t = Instant::now();
    let r = rig.calls(&args, ON);
    let on = t.elapsed();
    assert!(r[n - 1].text.contains("unchanged since"));

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

/// Real edits from this repository's history, through the real binary: for each, the full
/// outline of the edited file, what the delta cost instead, and what a further unchanged read
/// cost. A delta that fell back to the outline counts as 100%, so the figures are not flattered.
#[test]
fn real_edits_from_history_cost_less_than_the_outline() {
    let Some(log) = git(&["log", "--no-merges", "-n", "600", "--format=%H"]) else {
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
    rig.write_transcript(SESSION, &[USER]);
    let (mut delta_pct, mut stub_pct, mut fell_back) = (vec![], vec![], 0);
    let (mut sum_stub, mut sum_outline) = (0i64, 0i64);
    for (i, (ext, old, new)) in pairs.iter().enumerate() {
        let name = format!("p{i}.{ext}");
        let f = rig.file(&name, old);
        rig.read(&f, ON);
        rig.file(&name, new);
        let d = rig.read(&f, ON);
        let full = rig
            .calls(&[serde_json::json!({ "path": f, "refresh": true })], ON)
            .remove(0);
        let stub = rig.read(&f, ON);
        assert!(
            stub.text.contains("unchanged since"),
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
    let pct = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    let (dm, d90) = (pct(&mut delta_pct, 0.5), pct(&mut delta_pct, 0.9));
    let sm = pct(&mut stub_pct, 0.5);
    eprintln!(
        "{} real edits ({} fell back to the outline): delta median {dm:.1}% p90 {d90:.1}% of the \
         outline; unchanged stub median {sm:.1}% ({} vs {} tokens summed)",
        pairs.len(),
        fell_back,
        sum_stub,
        sum_outline
    );
    assert!(
        dm < 60.0,
        "delta median {dm:.1}% of the outline is not worth the complexity"
    );
    assert!(
        sum_stub * 4 < sum_outline,
        "the stub saves too little: {sum_stub} vs {sum_outline}"
    );
}
