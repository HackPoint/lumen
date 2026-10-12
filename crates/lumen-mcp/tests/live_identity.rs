//! Who is asking (#32), end to end: the real `lumen-mcp` server and the real
//! `lumen-mcp hook intercept`, driven the way Claude Code drives them.
//!
//! Claude Code starts the server once and keeps it across `/clear`, so the session id in the
//! server's environment is the one it started under, and the model that is asking is not.
//! The only place the live conversation is named is the PreToolUse hook payload, which carries
//! the same `tool_use_id` the MCP request carries in `_meta["claudecode/toolUseId"]`. A test
//! therefore does what Claude Code does for each Lumen tool call: run the hook with a payload,
//! then send the request with that id.
//!
//! Every test starts the server under `STALE`, a session that no call in it belongs to, so
//! anything that still reads the environment answers for the wrong conversation.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::TempDir;

const STALE: &str = "00000000-0000-0000-0000-00000000dead";
const S1: &str = "11111111-2222-3333-4444-555555555551";
const S2: &str = "11111111-2222-3333-4444-555555555552";
const ON: &[(&str, &str)] = &[("LUMEN_REPEAT_READS", "on")];

struct Rig {
    dir: TempDir,
    /// In a directory whose name has a space in it: the record directory is derived from this
    /// path by both processes, and a drive path or a home with a space is where that breaks.
    db: PathBuf,
    spool: PathBuf,
    tmp: PathBuf,
}

struct Reply {
    text: String,
}

struct Ran {
    code: i32,
    stderr: String,
}

struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    id: i64,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    let ledger_dir = dir.path().join("ledger dir");
    std::fs::create_dir_all(&ledger_dir).unwrap();
    let tmp = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let db = ledger_dir.join("ledger.db");
    // The schema exists before anything runs, so a test can read the ledger at any point.
    lumen_core::meter::connect_db(&db).unwrap();
    Rig {
        spool: dir.path().join("faults.jsonl"),
        dir,
        db,
        tmp,
    }
}

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

    fn ledger_dir(&self) -> &Path {
        self.db.parent().unwrap()
    }

    fn record(&self, call_id: &str) -> PathBuf {
        self.ledger_dir().join(format!("lumen_call_{call_id}.json"))
    }

    fn records(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.ledger_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("lumen_call_"))
            .collect();
        v.sort();
        v
    }

    fn hook(&self, db: &Path, env: &[(&str, &str)], payload: &serde_json::Value) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.args(["hook", "intercept"])
            .env("LUMEN_DB", db)
            .env("LUMEN_FAULT_SPOOL", &self.spool)
            .env("TMPDIR", &self.tmp)
            .env("TMP", &self.tmp)
            .env("TEMP", &self.tmp)
            .env_remove("LUMEN_HOOK_ENABLED")
            .env_remove("LUMEN_LINE_THRESHOLD")
            .env_remove("LUMEN_CAPTURE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.stdout.is_empty(), "the hook printed to stdout");
        Ran {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// The PreToolUse payload Claude Code sends for a Lumen tool call. A subagent's call has
    /// the same `session_id` as its parent's and adds `agent_id` and `agent_type`.
    fn payload(
        &self,
        tool: &str,
        session: &str,
        agent: Option<&str>,
        call_id: &str,
        path: &str,
    ) -> serde_json::Value {
        let mut p = serde_json::json!({
            "session_id": session,
            "transcript_path": self.dir.path().join("t.jsonl").to_string_lossy(),
            "cwd": self.dir.path().to_string_lossy(),
            "permission_mode": "default",
            "hook_event_name": "PreToolUse",
            "tool_name": format!("mcp__lumen__{tool}"),
            "tool_input": {"path": path},
            "tool_use_id": call_id,
        });
        if let Some(a) = agent {
            p["agent_id"] = a.into();
            p["agent_type"] = "general-purpose".into();
        }
        p
    }

    fn announce(&self, session: &str, agent: Option<&str>, call_id: &str, path: &str) -> Ran {
        let r = self.hook(
            &self.db,
            &[],
            &self.payload("smart_read", session, agent, call_id, path),
        );
        assert_eq!(r.code, 0, "{}", r.stderr);
        r
    }

    /// `(session_id, repeat_plan, routed_via)` of every smart_read / recall_file row.
    fn rows(&self) -> Vec<(Option<String>, Option<String>, String)> {
        let conn = lumen_core::meter::open_read_only(&self.db).unwrap();
        let mut st = conn
            .prepare(
                "SELECT session_id, repeat_plan, routed_via FROM read_events \
                 WHERE tool LIKE '%smart_read' OR tool LIKE '%recall_file' ORDER BY rowid",
            )
            .unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn agents(&self) -> Vec<Option<String>> {
        let conn = lumen_core::meter::open_read_only(&self.db).unwrap();
        let mut st = conn
            .prepare(
                "SELECT agent_id FROM read_events \
                 WHERE tool LIKE '%smart_read' OR tool LIKE '%recall_file' ORDER BY rowid",
            )
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn faults(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(&self.spool)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn start(&self, env: &[(&str, &str)]) -> Live {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.env("LUMEN_DB", &self.db)
            .env("LUMEN_FAULT_SPOOL", &self.spool)
            .env("LUMEN_PROJECTS_DIR", self.dir.path().join("projects"))
            .env_remove("LUMEN_RANKED_OUTLINE")
            .env_remove("LUMEN_REPEAT_READS")
            .env("CLAUDE_CODE_SESSION_ID", STALE)
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

    /// A `tools/call` as Claude Code sends it: the tool-use id rides in `_meta`.
    fn call(&mut self, call_id: Option<&str>, path: &str) -> Reply {
        let mut params = serde_json::json!({"name": "smart_read", "arguments": {"path": path}});
        if let Some(id) = call_id {
            params["_meta"] = serde_json::json!({"claudecode/toolUseId": id, "progressToken": 7});
        }
        let v = self.rpc("tools/call", params);
        // The server replies first and writes its ledger row after; it is sequential, so once
        // this answer arrives that write is done.
        self.rpc("tools/list", serde_json::json!({}));
        Reply {
            text: v["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("no text in {v}"))
                .to_string(),
        }
    }

    /// What Claude Code does for one Lumen tool call: the hook first, then the request.
    fn step(
        &mut self,
        rig: &Rig,
        session: &str,
        agent: Option<&str>,
        call_id: &str,
        path: &str,
    ) -> Reply {
        rig.announce(session, agent, call_id, path);
        self.call(Some(call_id), path)
    }

    fn end(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn is_full(r: &Reply) -> bool {
    r.text.contains("— outline")
}

fn is_stub(r: &Reply) -> bool {
    r.text.starts_with("Unchanged since")
}

fn plans(rig: &Rig) -> Vec<String> {
    rig.rows()
        .into_iter()
        .map(|(_, p, _)| p.unwrap_or_else(|| "-".into()))
        .collect()
}

fn sessions(rig: &Rig) -> Vec<Option<String>> {
    rig.rows().into_iter().map(|(s, _, _)| s).collect()
}

#[test]
fn after_clear_the_same_file_is_answered_in_full_and_the_row_names_the_new_conversation() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);

    assert!(is_full(&s.step(&rig, S1, None, "toolu_01", &f)));
    let again = s.step(&rig, S1, None, "toolu_02", &f);
    assert!(
        is_stub(&again),
        "the model holds the outline: {}",
        again.text
    );

    // /clear: the server is the same process, the conversation is not. A stub here is a
    // reference to a read the model no longer has.
    let after = s.step(&rig, S2, None, "toolu_03", &f);
    assert!(
        is_full(&after),
        "a cleared model was told its outline is unchanged: {}",
        after.text
    );
    assert!(is_stub(&s.step(&rig, S2, None, "toolu_04", &f)));
    s.end();

    assert_eq!(plans(&rig), ["first", "unchanged", "first", "unchanged"]);
    let want = [S1, S1, S2, S2].map(|x| Some(x.to_string()));
    assert_eq!(sessions(&rig), want, "rows carry the live session");
}

#[test]
fn resuming_an_earlier_conversation_in_the_same_process_is_a_new_conversation() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.step(&rig, S1, None, "toolu_01", &f);
    assert!(is_stub(&s.step(&rig, S1, None, "toolu_02", &f)));
    s.step(&rig, S2, None, "toolu_03", &f);
    // /resume S1. The base for S1 was freed when the conversation changed, so nothing is
    // left to be wrong about, whatever the resumed transcript still holds.
    let back = s.step(&rig, S1, None, "toolu_04", &f);
    assert!(is_full(&back), "{}", back.text);
    assert!(is_stub(&s.step(&rig, S1, None, "toolu_05", &f)));
    s.end();
    assert_eq!(
        plans(&rig),
        ["first", "unchanged", "first", "first", "unchanged"]
    );
}

/// #42: the intercept asks the ledger whether smart_read already worked for this session and
/// path. After `/clear` the rows must carry the session the hook is now being told about.
#[test]
fn the_escape_valve_still_finds_the_smart_read_that_worked_after_a_clear() {
    let rig = rig();
    let f = rig.file("big.rs", &source(120, 8));
    let read = |session: &str| {
        rig.hook(
            &rig.db,
            &[],
            &serde_json::json!({
                "session_id": session, "hook_event_name": "PreToolUse", "tool_name": "Read",
                "tool_input": {"file_path": f}, "cwd": rig.dir.path().to_string_lossy(),
            }),
        )
    };
    let mut s = rig.start(&[]);
    s.step(&rig, S1, None, "toolu_01", &f);
    // /clear. The model is redirected, follows the redirect, and smart_read works.
    assert_eq!(read(S2).code, 2, "the first Read is redirected");
    s.step(&rig, S2, None, "toolu_02", &f);
    s.end();

    let again = read(S2);
    assert_eq!(
        again.code, 2,
        "the retry after a working smart_read was released whole: {}",
        again.stderr
    );
    assert!(again.stderr.contains("recall_file"), "{}", again.stderr);
}

/// Every way the identity can fail to arrive ends the same: the whole answer, a ledger row
/// that says the session is unknown, and never the session the process started under.
#[test]
fn unknown_identity_is_answered_in_full_and_recorded_as_unknown() {
    let long_ago =
        r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"__S__","agent_id":null,"ts_ms":1}"#;
    type Setup = Box<dyn Fn(&Rig)>;
    let cases: Vec<(&str, Setup, Option<&str>)> = vec![
        // The hook is not registered, so nothing ran before the call.
        ("hook missing", Box::new(|_| {}), Some("toolu_x")),
        // The client sent no id at all.
        (
            "no tool-use id in the request",
            Box::new(|r| {
                r.announce(S1, None, "toolu_x", "/nowhere");
            }),
            None,
        ),
        // The record is for another call.
        (
            "record for another call",
            Box::new(|r| {
                r.announce(S1, None, "toolu_other", "/nowhere");
            }),
            Some("toolu_x"),
        ),
        (
            "malformed record",
            Box::new(|r| std::fs::write(r.record("toolu_x"), "{not json").unwrap()),
            Some("toolu_x"),
        ),
        (
            "record with no session",
            Box::new(|r| {
                std::fs::write(
                    r.record("toolu_x"),
                    format!(
                        r#"{{"v":1,"tool":"mcp__lumen__smart_read","session_id":"","ts_ms":{}}}"#,
                        now_ms()
                    ),
                )
                .unwrap()
            }),
            Some("toolu_x"),
        ),
        (
            "record from a format this build does not know",
            Box::new(|r| {
                std::fs::write(
                    r.record("toolu_x"),
                    format!(
                        r#"{{"v":99,"tool":"mcp__lumen__smart_read","session_id":"{S1}","ts_ms":{}}}"#,
                        now_ms()
                    ),
                )
                .unwrap()
            }),
            Some("toolu_x"),
        ),
        (
            "stale record",
            Box::new(move |r| {
                std::fs::write(r.record("toolu_x"), long_ago.replace("__S__", S1)).unwrap()
            }),
            Some("toolu_x"),
        ),
        (
            "record for a different tool",
            Box::new(|r| {
                std::fs::write(
                    r.record("toolu_x"),
                    format!(
                        r#"{{"v":1,"tool":"mcp__lumen__recall_file","session_id":"{S1}","ts_ms":{}}}"#,
                        now_ms()
                    ),
                )
                .unwrap()
            }),
            Some("toolu_x"),
        ),
    ];

    for (name, setup, id) in cases {
        let rig = rig();
        let f = rig.file("a.rs", &source(40, 6));
        setup(&rig);
        let mut s = rig.start(ON);
        let a = s.call(id, &f);
        let b = s.call(id, &f);
        let c = s.call(id, &f);
        s.end();
        for (i, r) in [&a, &b, &c].into_iter().enumerate() {
            assert!(
                is_full(r) && !is_stub(r),
                "{name}: call {i} was not the full answer: {}",
                r.text
            );
        }
        assert_eq!(
            sessions(&rig),
            [None, None, None],
            "{name}: the row named a session"
        );
        assert!(
            plans(&rig).iter().all(|p| p == "unidentified"),
            "{name}: {:?}",
            plans(&rig)
        );
    }
}

/// A record names exactly one call, and CRLF in it is only whitespace.
#[test]
fn a_record_written_with_windows_line_endings_is_read() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let ts = now_ms();
    let rec = format!(
        "{{\r\n  \"v\": 1,\r\n  \"tool\": \"mcp__lumen__smart_read\",\r\n  \"session_id\": \"{S1}\",\r\n  \"agent_id\": null,\r\n  \"ts_ms\": {ts}\r\n}}\r\n"
    );
    std::fs::write(rig.record("toolu_a"), rec.clone()).unwrap();
    std::fs::write(rig.record("toolu_b"), rec).unwrap();
    let mut s = rig.start(ON);
    assert!(is_full(&s.call(Some("toolu_a"), &f)));
    assert!(is_stub(&s.call(Some("toolu_b"), &f)));
    s.end();
    assert_eq!(sessions(&rig), [Some(S1.to_string()), Some(S1.to_string())]);
}

/// What the model holds is only known while the identity is. A call the server could not
/// place may have been made in a conversation that was cleared, and the next call in the old
/// session cannot be told apart from one that was not.
#[test]
fn an_unidentified_call_costs_the_bases_it_cannot_vouch_for() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    s.step(&rig, S1, None, "toolu_01", &f);
    assert!(is_stub(&s.step(&rig, S1, None, "toolu_02", &f)));
    assert!(is_full(&s.call(Some("toolu_unannounced"), &f)));
    let after = s.step(&rig, S1, None, "toolu_03", &f);
    assert!(is_full(&after), "{}", after.text);
    s.end();
    assert_eq!(plans(&rig), ["first", "unchanged", "unidentified", "first"]);
}

/// Parallel tool calls in one turn, with the server answering in another order than the hooks
/// ran: the id, not the order, says whose call it is.
#[test]
fn concurrent_calls_with_different_identities_do_not_mix() {
    let rig = rig();
    let a = rig.file("a.rs", &source(40, 6));
    let b = rig.file("b.rs", &source(41, 6));
    rig.announce(S1, None, "toolu_a", &a);
    rig.announce(S2, None, "toolu_b", &b);
    let mut s = rig.start(ON);
    s.call(Some("toolu_b"), &b);
    s.call(Some("toolu_a"), &a);
    s.end();
    assert_eq!(sessions(&rig), [S2, S1].map(|x| Some(x.to_string())));
}

/// A record is for one call. Resending the id gets no second use of it.
#[test]
fn a_record_is_consumed_by_the_call_it_names() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    rig.announce(S1, None, "toolu_a", &f);
    assert_eq!(rig.records(), ["lumen_call_toolu_a.json"]);
    let mut s = rig.start(ON);
    assert!(is_full(&s.call(Some("toolu_a"), &f)));
    assert!(rig.records().is_empty(), "{:?}", rig.records());
    assert!(is_full(&s.call(Some("toolu_a"), &f)));
    s.end();
    assert_eq!(sessions(&rig), [Some(S1.to_string()), None]);
}

/// Claude Code gives a subagent the parent's `session_id` and an `agent_id` of its own, and
/// the subagent's context does not hold what the parent read.
#[test]
fn a_subagent_is_not_told_that_the_main_agents_read_is_in_its_context() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut s = rig.start(ON);
    assert!(is_full(&s.step(&rig, S1, None, "toolu_01", &f)));
    assert!(is_stub(&s.step(&rig, S1, None, "toolu_02", &f)));

    let sub = s.step(&rig, S1, Some("agent-aaa"), "toolu_03", &f);
    assert!(
        is_full(&sub),
        "a subagent with an empty context was told its outline is unchanged: {}",
        sub.text
    );
    assert!(is_stub(&s.step(
        &rig,
        S1,
        Some("agent-aaa"),
        "toolu_04",
        &f
    )));
    // Another subagent is another empty context.
    assert!(is_full(&s.step(
        &rig,
        S1,
        Some("agent-bbb"),
        "toolu_05",
        &f
    )));
    // The main agent's own base is where it was.
    assert!(is_stub(&s.step(&rig, S1, None, "toolu_06", &f)));
    s.end();

    assert_eq!(
        plans(&rig),
        [
            "first",
            "unchanged",
            "first",
            "unchanged",
            "first",
            "unchanged"
        ]
    );
    assert_eq!(
        rig.agents(),
        [
            None,
            None,
            Some("agent-aaa"),
            Some("agent-aaa"),
            Some("agent-bbb"),
            None
        ]
        .map(|a: Option<&str>| a.map(str::to_string))
    );
}

/// A subagent's smart_read is not the main agent's. The escape valve releases a Read whole
/// unless a smart_read worked in the context that is asking, so the subagent that was served is
/// sent on to recall_file and the main agent, which was not, is let through.
#[test]
fn a_subagents_smart_read_counts_for_the_subagent_and_not_for_the_main_agent() {
    let rig = rig();
    let f = rig.file("big.rs", &source(120, 8));
    let read = |agent: Option<&str>| {
        let mut p = serde_json::json!({
            "session_id": S1, "hook_event_name": "PreToolUse", "tool_name": "Read",
            "tool_input": {"file_path": f}, "cwd": rig.dir.path().to_string_lossy(),
        });
        if let Some(a) = agent {
            p["agent_id"] = a.into();
            p["agent_type"] = "general-purpose".into();
        }
        rig.hook(&rig.db, &[], &p)
    };
    assert_eq!(read(Some("agent-aaa")).code, 2);
    assert_eq!(read(None).code, 2);
    let mut s = rig.start(&[]);
    s.step(&rig, S1, Some("agent-aaa"), "toolu_01", &f);
    s.end();

    let sub = read(Some("agent-aaa"));
    assert_eq!(sub.code, 2, "{}", sub.stderr);
    assert!(sub.stderr.contains("recall_file"), "{}", sub.stderr);
    let main = read(None);
    assert_eq!(
        main.code, 0,
        "the main agent was sent on from a read it does not hold: {}",
        main.stderr
    );
}

/// The hook that cannot record says so on stderr and in the spool, exits 0, and the call is
/// answered in full.
#[test]
fn a_hook_that_cannot_write_its_record_says_so_and_loses_nothing_else() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    // Hooks never create directories: a ledger directory that is not there is Lumen not
    // being installed where the hook was told it is.
    let missing = rig.dir.path().join("not there").join("ledger.db");
    let r = rig.hook(
        &missing,
        &[],
        &rig.payload("smart_read", S1, None, "toolu_01", &f),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stderr.contains("lumen:"),
        "stderr said nothing: {r:?}",
        r = r.stderr
    );
    assert!(
        !missing.parent().unwrap().exists(),
        "the hook made a directory"
    );
    let faults = rig.faults();
    assert!(
        faults.iter().any(|f| f["kind"] == "identity_unrecorded"),
        "{faults:?}"
    );

    let mut s = rig.start(ON);
    assert!(is_full(&s.call(Some("toolu_01"), &f)));
    assert!(is_full(&s.call(Some("toolu_01"), &f)));
    s.end();
}

/// The hook fires before the permission prompt, so a call that is then denied leaves its record
/// behind. They are pruned by age as the next hook runs, and a count cap holds the directory.
#[test]
fn records_of_calls_that_never_reached_the_server_are_pruned() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let old =
        r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"x","agent_id":null,"ts_ms":1}"#;
    std::fs::write(rig.record("toolu_denied"), old).unwrap();
    std::fs::write(rig.record("toolu_garbage"), "{").unwrap();
    // An unrelated file in the ledger directory is not the hook's to touch.
    std::fs::write(rig.ledger_dir().join("faults.jsonl"), "").unwrap();
    // A fresh record of another call is kept: its permission prompt may still be open.
    rig.announce(S1, None, "toolu_waiting", &f);
    rig.announce(S1, None, "toolu_next", &f);
    let left = rig.records();
    assert!(
        !left.contains(&"lumen_call_toolu_denied.json".to_string()),
        "{left:?}"
    );
    assert!(left.contains(&"lumen_call_toolu_waiting.json".to_string()));
    assert!(left.contains(&"lumen_call_toolu_next.json".to_string()));
    assert!(rig.ledger_dir().join("faults.jsonl").exists());
}

/// A server that starts clears out what earlier ones left.
#[test]
fn a_starting_server_prunes_records_nobody_claimed() {
    let rig = rig();
    let old =
        r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"x","agent_id":null,"ts_ms":1}"#;
    std::fs::write(rig.record("toolu_denied"), old).unwrap();
    let s = rig.start(ON);
    s.end();
    assert!(rig.records().is_empty(), "{:?}", rig.records());
}

/// `LUMEN_HOOK_ENABLED=0` switches the Read intercept off. Whose call this is has nothing to do
/// with that, and a repeat-read answer for the wrong conversation is not something a switch for
/// the redirect should risk.
#[test]
fn switching_the_read_intercept_off_does_not_switch_off_the_identity() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let r = rig.hook(
        &rig.db,
        &[("LUMEN_HOOK_ENABLED", "0")],
        &rig.payload("smart_read", S1, None, "toolu_01", &f),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let mut s = rig.start(ON);
    s.call(Some("toolu_01"), &f);
    s.end();
    assert_eq!(sessions(&rig), [Some(S1.to_string())]);
}

/// A hook that sees a tool that is not Lumen's writes nothing.
#[test]
fn only_lumen_tool_calls_are_recorded() {
    let rig = rig();
    let f = rig.file("a.rs", &source(3, 3));
    let mut p = rig.payload("smart_read", S1, None, "toolu_01", &f);
    p["tool_name"] = "mcp__other__smart_read".into();
    assert_eq!(rig.hook(&rig.db, &[], &p).code, 0);
    p["tool_name"] = "Bash".into();
    assert_eq!(rig.hook(&rig.db, &[], &p).code, 0);
    assert!(rig.records().is_empty(), "{:?}", rig.records());
}

/// Two Claude windows are two Claude processes, and a stdio server belongs to the process
/// that spawned it. What one window announces is not the other's: their ids differ.
#[test]
fn two_servers_on_one_ledger_do_not_see_each_others_conversations() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40, 6));
    let mut one = rig.start(ON);
    let mut two = rig.start(ON);
    one.step(&rig, S1, None, "toolu_w1_1", &f);
    two.step(&rig, S2, None, "toolu_w2_1", &f);
    assert!(is_stub(&one.step(&rig, S1, None, "toolu_w1_2", &f)));
    assert!(is_stub(&two.step(&rig, S2, None, "toolu_w2_2", &f)));
    // Window two is cleared; window one is not affected.
    let cleared = two.step(&rig, S2, None, "toolu_w2_3", &f);
    assert!(is_stub(&cleared));
    assert!(is_stub(&one.step(&rig, S1, None, "toolu_w1_3", &f)));
    one.end();
    two.end();
}
