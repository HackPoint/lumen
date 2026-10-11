//! The intercept's retry escape valve after a working smart_read, end to end: the real
//! `lumen-mcp` server writes the ledger rows, the real `lumen-mcp hook intercept` reads them.
//!
//! The scenario is the one in issue #36. The model is redirected once, calls smart_read, which
//! works, and then reads the file with the built-in Read anyway because it wants the code. That
//! Read used to be released whole. Claude Code's own unchanged-file dedup cannot help, because
//! the first Read was blocked and never entered its cache.
//!
//! Everything is isolated to a tempdir: the ledger, the fault spool and the marker directory.

use lumen_core::tokenizer::count_tokens;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;
use tempfile::TempDir;

const SESSION: &str = "5b8c7e0a-1d2f-4a36-9c5e-0f7a2b3c4d5e";

struct Rig {
    dir: TempDir,
    db: PathBuf,
    tmp: PathBuf,
    spool: PathBuf,
}

struct Ran {
    code: i32,
    stderr: String,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("ledger.db");
    let tmp = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    // The schema exists before anything runs, so the hook finds a ledger from the start.
    lumen_core::meter::connect_db(&db).unwrap();
    assert!(
        db.starts_with(std::env::temp_dir()),
        "test ledger must be in the temp dir"
    );
    Rig {
        spool: dir.path().join("faults.jsonl"),
        dir,
        db,
        tmp,
    }
}

/// A Rust file of `defs` functions with `body` statements each.
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
    fn file(&self, name: &str, body: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn intercept(&self, session: &str, file: &Path) -> Ran {
        let payload = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": file.to_string_lossy()},
            "cwd": self.dir.path().to_string_lossy(),
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
            .args(["hook", "intercept"])
            .env("LUMEN_DB", &self.db)
            .env("LUMEN_FAULT_SPOOL", &self.spool)
            .env("TMPDIR", &self.tmp)
            .env("TMP", &self.tmp)
            .env("TEMP", &self.tmp)
            .env_remove("LUMEN_HOOK_ENABLED")
            .env_remove("LUMEN_LINE_THRESHOLD")
            .env_remove("LUMEN_CAPTURE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.stdout.is_empty(), "the intercept printed to stdout");
        Ran {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn faults(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.spool)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn start(&self, session: &str) -> Live {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
            .env("LUMEN_DB", &self.db)
            .env("LUMEN_PROJECTS_DIR", self.dir.path().join("projects"))
            .env_remove("LUMEN_RANKED_OUTLINE")
            .env_remove("LUMEN_REPEAT_READS")
            .env("CLAUDE_CODE_SESSION_ID", session)
            .env("LUMEN_RANKED_TIME_BUDGET_MS", "60000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
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
}

struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    id: i64,
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

    /// Tokens the tool returned. The server replies first and writes its ledger row after;
    /// it is sequential, so once the second answer arrives the row is there.
    fn call(&mut self, tool: &str, args: serde_json::Value) -> i64 {
        let v = self.rpc(
            "tools/call",
            serde_json::json!({"name": tool, "arguments": args}),
        );
        self.rpc("tools/list", serde_json::json!({}));
        v["result"]["_meta"]["returned_tokens"]
            .as_i64()
            .unwrap_or_else(|| panic!("no returned_tokens in {v}"))
    }

    fn end(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

/// smart_read worked, then the model reads the file anyway. Before the change that Read was
/// released whole; now it is sent to recall_file, and the model can still get the whole file
/// by asking once more.
#[test]
fn a_built_in_read_after_a_working_smart_read_costs_a_redirect_not_the_whole_file() {
    let rig = rig();
    let big = rig.file("big.rs", &source(120, 8));
    let lines = std::fs::read_to_string(&big).unwrap().lines().count();
    assert!(lines > 1000, "fixture is {lines} lines");
    let full = count_tokens(&std::fs::read_to_string(&big).unwrap()) as i64;

    // 1. The first Read is redirected, as always.
    let first = rig.intercept(SESSION, &big);
    assert_eq!(first.code, 2, "{}", first.stderr);

    // 2. The model follows it, and smart_read works.
    let mut s = rig.start(SESSION);
    let outline = s.call("smart_read", serde_json::json!({ "path": big }));

    // 3. It reads the file anyway. Before: released, the file comes back whole.
    //    After: redirected to recall_file for the symbols it names.
    let second = rig.intercept(SESSION, &big);
    assert_eq!(
        second.code, 2,
        "a full read of {lines} lines ({full} tokens) was released after smart_read worked: {}",
        second.stderr
    );
    assert!(
        second.stderr.contains("lumen:recall_file(path="),
        "{}",
        second.stderr
    );
    let redirect = count_tokens(&second.stderr) as i64;

    // 4. It asks for the one symbol it wanted.
    let recall = s.call(
        "recall_file",
        serde_json::json!({ "path": big, "names": ["operation_7"] }),
    );
    s.end();

    let before = full; // the released Read
    let after = redirect + recall;
    println!("scenario: smart_read, then a built-in Read of a {lines}-line file");
    println!("  smart_read outline           {outline:>7} tokens (both arms)");
    println!("  before: released built-in Read {before:>7} tokens");
    println!("  after:  redirect {redirect} + recall_file {recall} = {after:>7} tokens");
    println!(
        "  saved {:>7} tokens ({:.1}% of the full read)",
        before - after,
        100.0 * (before - after) as f64 / before as f64
    );
    assert!(
        after <= before,
        "the change must never cost more than what it replaced: {after} vs {before}"
    );
    assert!(
        after * 10 < before,
        "expected an order of magnitude: {after} vs {before}"
    );

    // 5. Never stranded: a model that does need the whole file gets it on the next try, and the
    //    release is recorded as the valve firing after a working smart_read.
    let third = rig.intercept(SESSION, &big);
    assert_eq!((third.code, third.stderr.as_str()), (0, ""));
    let faults = rig.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(faults[0]["variant"], "retry_escape_valve");
    assert_eq!(faults[0]["detail"], "after_lumen_read");
}

/// The valve exists for "the lumen tools are unavailable". With no smart_read or recall_file
/// recorded for the path, the retry goes through exactly as before: tokens are unchanged.
#[test]
fn with_no_working_lumen_read_the_retry_is_released_as_before() {
    let rig = rig();
    let big = rig.file("big.rs", &source(120, 8));
    let other = rig.file("other.rs", &source(5, 3));

    // smart_read worked on another file, and in another session on this one.
    let mut s = rig.start(SESSION);
    s.call("smart_read", serde_json::json!({ "path": other }));
    s.end();
    let mut s = rig.start("another-session");
    s.call("smart_read", serde_json::json!({ "path": big }));
    s.end();

    let first = rig.intercept(SESSION, &big);
    assert_eq!(first.code, 2);
    let retry = rig.intercept(SESSION, &big);
    assert_eq!((retry.code, retry.stderr.as_str()), (0, ""));
    let faults = rig.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert!(faults[0]["detail"].is_null(), "{faults:?}");
}

/// The lookup runs before every large-file Read, so it must stay cheap on a real ledger: a
/// ledger of the size in the field (tens of thousands of rows, all in other sessions) must not
/// change what the hook costs.
#[test]
fn the_ledger_lookup_costs_next_to_nothing_on_a_large_ledger() {
    let rig = rig();
    let big = rig.file("big.rs", &source(120, 8));
    {
        let conn = lumen_core::meter::connect_db(&rig.db).unwrap();
        conn.execute_batch("BEGIN").unwrap();
        let mut st = conn
            .prepare(
                "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,\
                 saved_tokens,routed_via,channel,session_id) \
                 VALUES ('2026-01-01T00:00:00Z','Read',?1,400,100,100,0,'builtin_read','cli',?2)",
            )
            .unwrap();
        for i in 0..60_000 {
            st.execute(rusqlite::params![
                format!("/x/f{}.rs", i % 900),
                format!("session-{}", i % 40)
            ])
            .unwrap();
        }
        drop(st);
        conn.execute_batch("COMMIT").unwrap();
    }

    let time = |sessions: &[&str]| {
        let mut ms: Vec<f64> = Vec::new();
        for s in sessions {
            // Block first, so the timed call is the retry that does the lookup.
            rig.intercept(s, &big);
            let t = Instant::now();
            let r = rig.intercept(s, &big);
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(r.code, 0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ms[ms.len() / 2]
    };
    let names: Vec<String> = (0..15).map(|i| format!("timed-{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let with_ledger = time(&refs);
    println!("retry on a 60k-row ledger: median {with_ledger:.1} ms");
    assert!(
        with_ledger < 500.0,
        "the retry took {with_ledger:.1} ms on a large ledger"
    );
}
