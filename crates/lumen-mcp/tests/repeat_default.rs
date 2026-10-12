//! Repeat reads are on by default (1.8.0), with two ways to turn them off, through the real
//! `lumen-mcp` binary over stdio: `LUMEN_REPEAT_READS=off` and the persisted setting, a small
//! JSON file beside the ledger. The env var wins over the file, the file over the default, and
//! anything that cannot be read as an answer is the safe side (off: every read in full) and a
//! fault row, never a silent flip.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::TempDir;

const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const UNCHANGED: &str = "Unchanged since an earlier read of this file in this session.";

struct Rig {
    dir: TempDir,
    db: std::path::PathBuf,
}

fn rig() -> Rig {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("ledger.db");
    lumen_core::meter::connect_db(&db).unwrap();
    Rig { dir, db }
}

fn source(defs: usize) -> String {
    let mut s = String::from("use std::collections::HashMap;\n\n");
    for i in 0..defs {
        s.push_str(&format!("pub fn operation_{i}(input: &str) -> String {{\n"));
        for j in 0..6 {
            s.push_str(&format!("    let v{j} = input.len() + {j};\n"));
        }
        s.push_str("    input.to_string()\n}\n\n");
    }
    s
}

struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    out: BufReader<ChildStdout>,
    id: i64,
    records: std::path::PathBuf,
    calls: usize,
}

impl Rig {
    fn file(&self, name: &str, body: &str) -> String {
        let p = self.dir.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }

    /// The persisted setting, where the server looks for it: beside the ledger.
    fn setting(&self, body: &str) {
        std::fs::write(self.dir.path().join("lumen_settings.json"), body).unwrap();
    }

    fn start(&self, env: &[(&str, &str)]) -> Live {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.env("LUMEN_DB", &self.db)
            .env("LUMEN_PROJECTS_DIR", self.dir.path().join("projects"))
            .env_remove("LUMEN_RANKED_OUTLINE")
            .env_remove("LUMEN_REPEAT_READS")
            .env_remove("LUMEN_FAULT_SPOOL")
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
            records: self.dir.path().to_path_buf(),
            calls: 0,
        };
        live.rpc(
            "initialize",
            serde_json::json!({"protocolVersion":"2024-11-05","capabilities":{},
                "clientInfo":{"name":"e2e","version":"0"}}),
        );
        live
    }

    fn faults(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.path().join("faults.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
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

    /// One call, placed in `SESSION` the way the PreToolUse hook places it. Returns the reply
    /// text and the tokens the server says it returned.
    fn call(&mut self, tool: &str, args: serde_json::Value) -> (String, i64) {
        self.calls += 1;
        let call_id = format!("toolu_{}", self.calls);
        lumen_core::identity::write(
            &self.records,
            &format!("mcp__lumen__{tool}"),
            &call_id,
            &lumen_core::identity::Identity {
                session: SESSION.to_string(),
                agent: None,
            },
            lumen_core::repeat::now_ms(),
        )
        .unwrap();
        let v = self.rpc(
            "tools/call",
            serde_json::json!({
                "name": tool,
                "arguments": args,
                "_meta": {"claudecode/toolUseId": call_id},
            }),
        );
        let text = v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no text in {v}"))
            .to_string();
        (
            text,
            v["result"]["_meta"]["returned_tokens"].as_i64().unwrap(),
        )
    }

    fn read(&mut self, path: &str) -> (String, i64) {
        self.call("smart_read", serde_json::json!({ "path": path }))
    }

    fn lists_fresh(&mut self) -> bool {
        self.rpc("tools/list", serde_json::json!({}))
            .to_string()
            .contains("fresh")
    }

    fn end(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

#[test]
fn a_fresh_install_answers_a_repeat_with_a_stub() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40));
    let mut s = rig.start(&[]);
    let (first, full) = s.read(&f);
    assert!(first.contains("— outline"), "{first}");
    let (second, stub) = s.read(&f);
    assert!(second.starts_with(UNCHANGED), "{second}");
    assert!(stub * 3 < full, "{stub} vs {full}");
    assert!(s.lists_fresh(), "the escape hatch is advertised when on");
    s.end();
    assert!(rig.faults().is_empty(), "{:?}", rig.faults());
}

#[test]
fn the_setting_on_is_on() {
    let rig = rig();
    rig.setting(r#"{"repeat_reads": true}"#);
    let f = rig.file("a.rs", &source(40));
    let mut s = rig.start(&[]);
    s.read(&f);
    assert!(s.read(&f).0.starts_with(UNCHANGED));
    s.end();
}

/// An upgrade from 1.7.0: the ledger has no `agent_id` column (the server's open adds it) and
/// there is no settings file, because 1.7.0 had no setting. Neither changes the default.
#[test]
fn a_1_7_0_ledger_with_no_setting_file_is_on_and_keeps_its_rows() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40));
    {
        let mut s = rig.start(&[]);
        s.read(&f);
        s.end();
    }
    let conn = lumen_core::meter::connect_db(&rig.db).unwrap();
    conn.execute("ALTER TABLE read_events DROP COLUMN agent_id", [])
        .unwrap();
    let before: i64 = conn
        .query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
        .unwrap();
    drop(conn);
    assert!(before > 0);
    assert!(!rig.dir.path().join("lumen_settings.json").exists());

    let mut s = rig.start(&[]);
    let (first, _) = s.read(&f);
    assert!(
        first.contains("— outline"),
        "a restart lost the base: {first}"
    );
    let (second, _) = s.read(&f);
    assert!(second.starts_with(UNCHANGED), "{second}");
    s.end();

    let conn = lumen_core::meter::connect_db(&rig.db).unwrap();
    let after: i64 = conn
        .query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        after,
        before + 2,
        "the old rows are kept and the new ones land"
    );
    assert!(rig.faults().is_empty(), "{:?}", rig.faults());
}

/// The same two reads of one file, with nothing set and with the switch off: what the default
/// is worth. With it off the second read is the first again (that is also what 1.7.0 sent by
/// default); by default it is the stub.
#[test]
fn a_two_read_scenario_costs_less_by_default_than_with_the_switch_off() {
    // Each run reads its own temp file, and a path of random characters is a few tokens more
    // or fewer, so the first reads are compared as text with the path taken out, not by count.
    let tokens = |env: &[(&str, &str)]| {
        let rig = rig();
        let f = rig.file("a.rs", &source(40));
        let mut s = rig.start(env);
        let (t1, first) = s.read(&f);
        let (_, second) = s.read(&f);
        s.end();
        (t1.replace(&f, "<path>"), first, second)
    };
    let (off_text, off_first, off_second) = tokens(&[("LUMEN_REPEAT_READS", "off")]);
    let (on_text, on_first, on_second) = tokens(&[]);
    eprintln!(
        "two reads of one file: switch off {off_first} + {off_second} = {}; default {on_first} + \
         {on_second} = {}",
        off_first + off_second,
        on_first + on_second
    );
    assert_eq!(
        on_text, off_text,
        "the first read is the same bytes either way"
    );
    assert_eq!(off_second, off_first, "off answers the repeat in full");
    assert!(
        on_second * 3 < off_second,
        "default {on_second} vs off {off_second} for the second read"
    );
}

/// What offering `fresh` costs: every session that has repeat reads on pays for it on two
/// tools, used or not. Pinned so that a longer description is a decision and not a drift.
#[test]
fn offering_fresh_adds_a_few_tokens_to_the_tool_list_and_only_when_on() {
    let rig = rig();
    let list = |env: &[(&str, &str)]| {
        let mut s = rig.start(env);
        let v = s.rpc("tools/list", serde_json::json!({}));
        s.end();
        v["result"].clone()
    };
    let on = list(&[]);
    let off = list(&[("LUMEN_REPEAT_READS", "off")]);
    let (on_bytes, off_bytes) = (on.to_string().len(), off.to_string().len());
    eprintln!(
        "tools/list: off {off_bytes} B, on {on_bytes} B, +{} B (/4 = {}, an estimate)",
        on_bytes - off_bytes,
        (on_bytes - off_bytes) / 4
    );
    assert!(!off.to_string().contains("fresh"), "off offers nothing");
    for tool in on["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let fresh = &tool["inputSchema"]["properties"]["fresh"];
        if matches!(name, "smart_read" | "recall_file") {
            assert_eq!(fresh["type"], "boolean", "{name}");
            let what = fresh["description"].as_str().unwrap();
            assert!(what.contains("Full result"), "{name}: {what}");
            assert!(
                fresh.get("default").is_none(),
                "{name}: false is already the default"
            );
        } else {
            assert!(fresh.is_null(), "{name} takes no fresh option");
        }
    }
    assert!(
        on_bytes - off_bytes <= 200,
        "fresh now adds {} B to every session's tool list; it is paid whether or not it is used",
        on_bytes - off_bytes
    );
}

/// The file edited in one function, so a repeat of it is a delta and not a rewrite.
fn edited(before: &str) -> String {
    before.replacen(
        "    let v3 = input.len() + 3;\n",
        "    let v3 = input.len() + 3; // edited\n",
        1,
    )
}

/// What "off" has to mean, whichever way it was asked for: a repeat is answered exactly as the
/// first read was, an edit is answered with the full outline of the new file, and the tool
/// list does not offer `fresh`.
fn assert_everything_in_full(rig: &Rig, mut s: Live, f: &str) {
    let (first, full) = s.read(f);
    assert!(first.contains("— outline"), "{first}");
    let (again, again_tokens) = s.read(f);
    assert_eq!(
        again, first,
        "a repeat must be answered exactly as the first"
    );
    assert_eq!(again_tokens, full);
    let before = std::fs::read_to_string(f).unwrap();
    rig.file("a.rs", &edited(&before));
    let (after_edit, _) = s.read(f);
    assert!(!after_edit.starts_with("Changed since"), "{after_edit}");
    assert!(!after_edit.starts_with(UNCHANGED), "{after_edit}");
    assert!(after_edit.contains("— outline"), "{after_edit}");
    assert!(!s.lists_fresh(), "off must not change the tool list");
    s.end();
}

#[test]
fn the_env_off_switch_stops_stubs_and_deltas() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40));
    assert_everything_in_full(&rig, rig.start(&[("LUMEN_REPEAT_READS", "off")]), &f);
    assert!(rig.faults().is_empty(), "{:?}", rig.faults());
}

#[test]
fn the_setting_off_stops_stubs_and_deltas() {
    let rig = rig();
    rig.setting(r#"{"repeat_reads": false}"#);
    let f = rig.file("a.rs", &source(40));
    assert_everything_in_full(&rig, rig.start(&[]), &f);
    assert!(rig.faults().is_empty(), "{:?}", rig.faults());
}

#[test]
fn the_env_beats_the_setting_in_both_directions() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40));

    rig.setting(r#"{"repeat_reads": true}"#);
    assert_everything_in_full(&rig, rig.start(&[("LUMEN_REPEAT_READS", "off")]), &f);

    rig.setting(r#"{"repeat_reads": false}"#);
    let mut s = rig.start(&[("LUMEN_REPEAT_READS", "on")]);
    s.read(&f);
    assert!(s.read(&f).0.starts_with(UNCHANGED));
    s.end();
}

#[test]
fn an_edit_is_a_delta_by_default() {
    let rig = rig();
    let before = source(40);
    let f = rig.file("a.rs", &before);
    let mut s = rig.start(&[]);
    let (_, full) = s.read(&f);
    rig.file("a.rs", &edited(&before));
    let (delta, tokens) = s.read(&f);
    assert!(
        delta.starts_with("Changed since an earlier read"),
        "{delta}"
    );
    assert!(tokens * 3 < full, "{tokens} vs {full}");
    s.end();
}

#[test]
fn a_corrupt_setting_answers_in_full_and_says_so() {
    for body in ["{not json", "[]", r#"{"repeat_reads": "off"}"#, ""] {
        let rig = rig();
        rig.setting(body);
        let f = rig.file("a.rs", &source(40));
        assert_everything_in_full(&rig, rig.start(&[]), &f);
        let faults = rig.faults();
        assert_eq!(
            faults.len(),
            1,
            "once, not once per read: {body:?} {faults:?}"
        );
        assert_eq!(faults[0]["kind"], "repeat_reads_config");
        assert_eq!(faults[0]["variant"], "file");
        assert!(
            faults[0]["detail"]
                .as_str()
                .unwrap()
                .contains("lumen_settings.json"),
            "{faults:?}"
        );
    }
}

#[test]
fn an_unreadable_setting_answers_in_full_and_says_so() {
    let rig = rig();
    // A directory where the file belongs: it exists, and cannot be read as a file.
    std::fs::create_dir(rig.dir.path().join("lumen_settings.json")).unwrap();
    let f = rig.file("a.rs", &source(40));
    assert_everything_in_full(&rig, rig.start(&[]), &f);
    assert_eq!(rig.faults().len(), 1, "{:?}", rig.faults());
    assert_eq!(rig.faults()[0]["variant"], "file");
}

#[test]
fn an_unknown_env_value_answers_in_full_says_so_and_does_not_fall_to_the_setting() {
    let rig = rig();
    rig.setting(r#"{"repeat_reads": true}"#);
    let f = rig.file("a.rs", &source(40));
    assert_everything_in_full(&rig, rig.start(&[("LUMEN_REPEAT_READS", "enabled")]), &f);
    let faults = rig.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(faults[0]["kind"], "repeat_reads_config");
    assert_eq!(faults[0]["variant"], "env");
    assert!(
        faults[0]["detail"].as_str().unwrap().contains("enabled"),
        "{faults:?}"
    );
}

#[test]
fn a_change_of_the_setting_applies_to_the_next_read_with_no_restart() {
    let rig = rig();
    let f = rig.file("a.rs", &source(40));
    let mut s = rig.start(&[]);
    let (first, _) = s.read(&f);
    assert!(s.read(&f).0.starts_with(UNCHANGED));

    // Off: the very next read is in full, and so are the ones after it.
    rig.setting(r#"{"repeat_reads": false}"#);
    assert_eq!(s.read(&f).0, first);
    assert_eq!(s.read(&f).0, first);
    assert!(!s.lists_fresh());

    // On again: what was remembered before it was turned off is not trusted, so the first
    // read is in full and the one after it is a stub.
    rig.setting(r#"{"repeat_reads": true}"#);
    assert_eq!(s.read(&f).0, first);
    assert!(s.read(&f).0.starts_with(UNCHANGED));
    assert!(s.lists_fresh());
    s.end();
    assert!(rig.faults().is_empty(), "{:?}", rig.faults());
}
