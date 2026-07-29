// Black-box tests for the lumen-mcp binary over real stdio.
//
// The unit tests in src/lib.rs cover what each handler returns. These drive the
// actual process the way Claude Code does — spawn it, write newline-delimited
// JSON-RPC to stdin, read frames from stdout — so they also cover the parts the
// library cannot: the read loop, framing, notification handling, and the
// stdout/stderr split that makes the transport usable at all.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

/// A running lumen-mcp process with its pipes.
struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    _db: TempDir,
}

impl Server {
    fn start() -> Self {
        // Point metering at a throwaway DB so tests never write to a real one.
        let db = TempDir::new().expect("tempdir");
        // Isolation is load-bearing, not a convenience. Since the DB fallback became
        // the canonical per-OS path, a forgotten LUMEN_DB no longer lands in the
        // repository root — it lands in the user's real ledger. A test that pollutes
        // production data is worse than one that fails, so assert the redirection
        // rather than trusting it.
        assert!(
            db.path().starts_with(std::env::temp_dir()),
            "the metering DB must live in a temp dir, got {:?}",
            db.path()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
            .env("LUMEN_DB", db.path().join("test.db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn lumen-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
            _db: db,
        }
    }

    /// Send one request and read the single response frame it produces.
    fn call(&mut self, req: Value) -> Value {
        self.send(req);
        self.read_frame()
    }

    fn send(&mut self, req: Value) {
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
    }

    /// Send a raw line, bypassing JSON construction.
    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write raw");
        self.stdin.flush().expect("flush");
    }

    fn read_frame(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read stdout");
        assert!(n > 0, "server closed stdout without answering");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout must be valid JSON, got {line:?}: {e}"))
    }

    /// Close stdin and wait for a clean exit.
    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait");
        assert!(status.success(), "server exited with {status}");
    }
}

fn rpc(id: i64, method: &str, params: Option<Value>) -> Value {
    let mut v = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    if let Some(p) = params {
        v["params"] = p;
    }
    v
}

fn tool_call(id: i64, name: &str, args: Value) -> Value {
    rpc(
        id,
        "tools/call",
        Some(json!({ "name": name, "arguments": args })),
    )
}

/// The text payload of a successful tool result.
fn text_of(frame: &Value) -> &str {
    frame["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {frame}"))
}

// ── handshake ────────────────────────────────────────────────────────────────

#[test]
fn initialize_returns_a_well_formed_jsonrpc_response() {
    let mut s = Server::start();
    let frame = s.call(rpc(1, "initialize", None));
    assert_eq!(frame["jsonrpc"], "2.0");
    assert_eq!(frame["id"], 1);
    assert_eq!(frame["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(frame["result"]["serverInfo"]["name"], "lumen");
    assert!(frame["result"]["capabilities"]["tools"].is_object());
    s.shutdown();
}

#[test]
fn the_response_id_matches_the_request_id() {
    let mut s = Server::start();
    for id in [7i64, 42, 999] {
        assert_eq!(s.call(rpc(id, "ping", None))["id"], id);
    }
    s.shutdown();
}

#[test]
fn a_string_id_is_echoed_back_as_a_string() {
    // JSON-RPC allows string ids; Claude Code uses numbers, but the type must
    // round-trip rather than being coerced.
    let mut s = Server::start();
    let req = json!({ "jsonrpc": "2.0", "id": "abc-123", "method": "ping" });
    assert_eq!(s.call(req)["id"], "abc-123");
    s.shutdown();
}

#[test]
fn ping_answers_with_an_empty_result() {
    let mut s = Server::start();
    assert_eq!(s.call(rpc(1, "ping", None))["result"], json!({}));
    s.shutdown();
}

#[test]
fn tools_list_advertises_the_four_tools_with_schemas() {
    let mut s = Server::start();
    let frame = s.call(rpc(1, "tools/list", None));
    let tools = frame["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        vec!["lumen_ping", "smart_read", "recall_file", "compress_logs"]
    );
    for t in tools {
        assert_eq!(
            t["inputSchema"]["type"], "object",
            "{} needs an object schema",
            t["name"]
        );
        assert!(
            t["description"].as_str().is_some_and(|d| d.len() > 40),
            "{} needs a description the model can act on",
            t["name"]
        );
    }
    s.shutdown();
}

// ── tool calls end to end ────────────────────────────────────────────────────

#[test]
fn lumen_ping_round_trips_through_the_process() {
    let mut s = Server::start();
    let frame = s.call(tool_call(1, "lumen_ping", json!({ "echo": "hello" })));
    assert_eq!(text_of(&frame), "lumen-mcp alive: hello");
    assert_eq!(frame["result"]["isError"], false);
    s.shutdown();
}

#[test]
fn smart_read_outlines_a_real_file_on_disk() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("sample.rs");
    std::fs::write(&file, "fn alpha() {}\nfn beta() {}\n").unwrap();

    let mut s = Server::start();
    let frame = s.call(tool_call(
        1,
        "smart_read",
        json!({ "path": file.to_string_lossy() }),
    ));
    let text = text_of(&frame);
    assert!(text.contains("outline"), "{text}");
    assert!(text.contains("alpha") && text.contains("beta"));
    assert!(frame["result"]["_meta"]["full_tokens"].as_u64().unwrap() > 0);
    s.shutdown();
}

#[test]
fn recall_file_returns_one_item_from_a_real_file() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("sample.rs");
    std::fs::write(
        &file,
        "fn alpha() {\n    let a = 1;\n}\n\n// pad\n// pad\n// pad\n// pad\n\nfn beta() {\n    let b = 2;\n}\n",
    )
    .unwrap();

    let mut s = Server::start();
    let frame = s.call(tool_call(
        1,
        "recall_file",
        json!({ "path": file.to_string_lossy(), "names": ["alpha"] }),
    ));
    let text = text_of(&frame);
    assert!(text.contains("alpha"), "{text}");
    assert!(text.contains("let a = 1"));
    assert!(!text.contains("let b = 2"), "beta must not leak: {text}");
    s.shutdown();
}

#[test]
fn compress_logs_compacts_inline_text() {
    let mut s = Server::start();
    let frame = s.call(tool_call(
        1,
        "compress_logs",
        json!({ "text": "same line\n".repeat(40) }),
    ));
    let meta = &frame["result"]["_meta"];
    assert!(
        meta["returned_tokens"].as_u64().unwrap() < meta["full_tokens"].as_u64().unwrap(),
        "40 identical lines must compress: {meta}"
    );
    s.shutdown();
}

// ── error paths ──────────────────────────────────────────────────────────────

#[test]
fn an_unknown_method_returns_method_not_found() {
    let mut s = Server::start();
    let frame = s.call(rpc(1, "resources/list", None));
    assert_eq!(frame["error"]["code"], -32601);
    assert!(
        frame["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Method not found")
    );
    assert!(
        frame["result"].is_null(),
        "an error frame carries no result"
    );
    s.shutdown();
}

#[test]
fn an_unknown_tool_returns_method_not_found() {
    let mut s = Server::start();
    let frame = s.call(tool_call(1, "definitely_not_a_tool", json!({})));
    assert_eq!(frame["error"]["code"], -32601);
    s.shutdown();
}

#[test]
fn a_missing_required_path_returns_invalid_params() {
    let mut s = Server::start();
    let frame = s.call(tool_call(1, "smart_read", json!({})));
    assert_eq!(frame["error"]["code"], -32602);
    s.shutdown();
}

#[test]
fn reading_a_nonexistent_file_returns_invalid_params_not_a_crash() {
    let mut s = Server::start();
    let frame = s.call(tool_call(
        1,
        "smart_read",
        json!({ "path": "/nope/does/not/exist.rs" }),
    ));
    assert_eq!(frame["error"]["code"], -32602);
    assert!(
        frame["error"]["message"]
            .as_str()
            .unwrap()
            .contains("file not found")
    );
    s.shutdown();
}

// ── transport robustness ─────────────────────────────────────────────────────

#[test]
fn a_malformed_line_is_skipped_and_the_server_keeps_serving() {
    let mut s = Server::start();
    s.send_raw("this is not json at all");
    s.send_raw("{ \"unclosed\": ");
    // No response is produced for either; the next real request must still work.
    let frame = s.call(rpc(1, "ping", None));
    assert_eq!(frame["id"], 1);
    s.shutdown();
}

#[test]
fn a_notification_without_an_id_gets_no_reply() {
    let mut s = Server::start();
    // Per JSON-RPC, a request with no id is a notification and must not be answered.
    s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    // The only frame on stdout must be the answer to the request that follows.
    let frame = s.call(rpc(5, "ping", None));
    assert_eq!(
        frame["id"], 5,
        "the notification must not have produced a frame"
    );
    s.shutdown();
}

#[test]
fn blank_lines_are_ignored() {
    let mut s = Server::start();
    s.send_raw("");
    s.send_raw("   ");
    let frame = s.call(rpc(1, "ping", None));
    assert_eq!(frame["id"], 1);
    s.shutdown();
}

#[test]
fn many_requests_are_answered_in_order_on_one_connection() {
    let mut s = Server::start();
    for id in 1..=25 {
        s.send(rpc(id, "ping", None));
    }
    for id in 1..=25 {
        assert_eq!(
            s.read_frame()["id"],
            id,
            "frames must come back in request order"
        );
    }
    s.shutdown();
}

#[test]
fn one_frame_per_line_with_no_embedded_newlines() {
    // The transport is newline-delimited, so a result containing newlines must
    // still serialise onto exactly one line.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("multi.rs");
    std::fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

    let mut s = Server::start();
    s.send(tool_call(
        1,
        "smart_read",
        json!({ "path": file.to_string_lossy() }),
    ));
    let mut line = String::new();
    s.stdout.read_line(&mut line).unwrap();
    let parsed: Value = serde_json::from_str(&line).expect("exactly one frame per line");
    assert!(
        text_of(&parsed).contains('\n'),
        "the payload does contain newlines"
    );
    s.shutdown();
}

#[test]
fn the_server_exits_cleanly_when_stdin_closes() {
    let s = Server::start();
    // shutdown() drops stdin and asserts a zero exit status.
    s.shutdown();
}

#[test]
fn diagnostics_go_to_stderr_so_stdout_stays_pure_jsonrpc() {
    // A stray log line on stdout would corrupt the transport and break the
    // client, so every frame on stdout must parse as JSON.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("s.rs");
    std::fs::write(&file, "fn a() {}\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
        .env("LUMEN_DB", dir.path().join("test.db"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", rpc(1, "initialize", None)).unwrap();
        writeln!(stdin, "{}", rpc(2, "tools/list", None)).unwrap();
        writeln!(stdin, "garbage line").unwrap();
        writeln!(
            stdin,
            "{}",
            tool_call(3, "smart_read", json!({ "path": file.to_string_lossy() }))
        )
        .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());

    let stdout = String::from_utf8(out.stdout).unwrap();
    let frames: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(frames.len(), 3, "one frame per answered request");
    for f in &frames {
        serde_json::from_str::<Value>(f)
            .unwrap_or_else(|e| panic!("non-JSON on stdout: {f:?} {e}"));
    }

    // And the diagnostics did happen — just on the other stream.
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.contains("starting"),
        "startup banner belongs on stderr"
    );
    assert!(
        stderr.contains("JSON parse error"),
        "so does the parse error"
    );
}

// ── metering side effect ─────────────────────────────────────────────────────

#[test]
fn a_tool_call_writes_a_metering_row_after_answering() {
    let db_dir = TempDir::new().unwrap();
    let db = db_dir.path().join("meter.db");
    let src_dir = TempDir::new().unwrap();
    let file = src_dir.path().join("s.rs");
    std::fs::write(&file, "fn alpha() {}\nfn beta() {}\n").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
        .env("LUMEN_DB", &db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            tool_call(1, "smart_read", json!({ "path": file.to_string_lossy() }))
        )
        .unwrap();
        // lumen_ping is not a read, so it must NOT be metered.
        writeln!(stdin, "{}", tool_call(2, "lumen_ping", json!({}))).unwrap();
    }
    assert!(child.wait_with_output().unwrap().status.success());

    let conn = lumen_core::meter::connect_db(&db).expect("open metering db");
    let (count, via, tool): (i64, String, String) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(routed_via),''), COALESCE(MAX(tool),'')
             FROM read_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(count, 1, "one metered read; ping is not a read");
    assert_eq!(via, "smart_read");
    assert_eq!(tool, "mcp__lumen__smart_read");
}

#[test]
fn a_failed_tool_call_writes_no_metering_row() {
    let db_dir = TempDir::new().unwrap();
    let db = db_dir.path().join("meter.db");

    let mut child = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"))
        .env("LUMEN_DB", &db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            tool_call(1, "smart_read", json!({ "path": "/nope/missing.rs" }))
        )
        .unwrap();
    }
    assert!(child.wait_with_output().unwrap().status.success());

    let conn = lumen_core::meter::connect_db(&db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "a read that failed saved nothing, so it meters nothing"
    );
}
