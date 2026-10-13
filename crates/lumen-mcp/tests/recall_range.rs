// Black-box tests of recall_file line ranges: after each bad request the server must still answer.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use tempfile::TempDir;

const LINES: usize = 200;

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    _db: TempDir,
}

impl Server {
    fn start() -> Self {
        let db = TempDir::new().expect("tempdir");
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

    /// Send one request and read the frame it produces; a server that died fails here with its stderr.
    fn call(&mut self, req: Value) -> Value {
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("read stdout");
        if n == 0 {
            let status = self.child.wait().expect("wait");
            let mut stderr = String::new();
            self.child
                .stderr
                .take()
                .expect("stderr")
                .read_to_string(&mut stderr)
                .expect("read stderr");
            panic!("the server closed stdout without answering ({status}); stderr:\n{stderr}");
        }
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout must be valid JSON, got {line:?}: {e}"))
    }

    /// The process is running and answers: a valid range returns its lines plus three of context.
    fn assert_still_serving(&mut self, f: &Fixture) {
        assert!(
            self.child.try_wait().expect("try_wait").is_none(),
            "the server has exited"
        );
        let frame = self.call(recall(900, &f.path, json!(100), json!(104)));
        let text = text_of(&frame);
        assert!(text.contains("L100-104 (+3 lines context)"), "{text}");
        assert!(
            text.contains("line97 ") && text.contains("line107 "),
            "{text}"
        );
        assert!(
            !text.contains("line96 ") && !text.contains("line108 "),
            "{text}"
        );
        assert_eq!(self.call(rpc(901, "ping"))["result"], json!({}));
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait");
        assert!(status.success(), "server exited with {status}");
    }
}

struct Fixture {
    _dir: TempDir,
    path: String,
}

impl Fixture {
    fn with(body: &str) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("sample.txt");
        std::fs::write(&path, body).expect("write fixture");
        Fixture {
            path: path.to_string_lossy().into_owned(),
            _dir: dir,
        }
    }

    /// Long enough that a ten-line window is a real saving, not the whole-file fallback.
    fn numbered() -> Self {
        let body: String = (1..=LINES)
            .map(|i| format!("line{i} // padding to make this file worth a partial read\n"))
            .collect();
        Self::with(&body)
    }
}

fn rpc(id: i64, method: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method })
}

fn recall(id: i64, path: &str, start: Value, end: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "recall_file", "arguments": {
            "path": path, "start_line": start, "end_line": end
        } }
    })
}

fn text_of(frame: &Value) -> &str {
    frame["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {frame}"))
}

/// An error reply that says how long the file is and what was asked for.
fn assert_refused(frame: &Value, start: &str, end: &str, lines: usize) {
    assert_eq!(frame["error"]["code"], -32602, "{frame}");
    let msg = frame["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message in {frame}"));
    assert!(msg.contains(&format!("{lines} lines")), "line count: {msg}");
    assert!(
        msg.contains(&format!("L{start}-{end}")),
        "the request: {msg}"
    );
}

#[test]
fn a_start_far_past_the_end_is_refused_and_the_server_keeps_serving() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    let frame = s.call(recall(1, &f.path, json!(100_000), json!(100_010)));
    assert_refused(&frame, "100000", "100010", LINES);
    s.assert_still_serving(&f);
    s.shutdown();
}

#[test]
fn a_start_just_past_the_last_line_is_refused_and_the_server_keeps_serving() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    for start in [LINES + 1, LINES + 4, LINES + 5] {
        let end = start + 5;
        let frame = s.call(recall(1, &f.path, json!(start), json!(end)));
        assert_refused(&frame, &start.to_string(), &end.to_string(), LINES);
        s.assert_still_serving(&f);
    }
    s.shutdown();
}

#[test]
fn a_start_after_the_end_is_refused_and_the_server_keeps_serving() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    for (start, end) in [(150, 10), (12, 10), (LINES, 1)] {
        let frame = s.call(recall(1, &f.path, json!(start), json!(end)));
        assert_refused(&frame, &start.to_string(), &end.to_string(), LINES);
        s.assert_still_serving(&f);
    }
    s.shutdown();
}

#[test]
fn huge_line_numbers_are_refused_and_the_server_keeps_serving() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    for (start, end) in [
        (u64::MAX, u64::MAX),
        (i64::MAX as u64, i64::MAX as u64),
        (u64::MAX, 1),
        (i64::MAX as u64, 1),
        (u64::MAX - 1, u64::MAX),
    ] {
        let frame = s.call(recall(1, &f.path, json!(start), json!(end)));
        assert_refused(&frame, &start.to_string(), &end.to_string(), LINES);
        s.assert_still_serving(&f);
    }
    s.shutdown();
}

#[test]
fn an_end_past_the_file_clamps_to_its_last_line() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    for (start, end) in [
        (LINES - 5, LINES as u64 + 1_000),
        (LINES - 5, i64::MAX as u64),
        (LINES - 5, u64::MAX),
    ] {
        let frame = s.call(recall(1, &f.path, json!(start), json!(end)));
        assert!(
            frame["error"].is_null(),
            "an end past the file is not an error: {frame}"
        );
        let text = text_of(&frame);
        assert!(text.contains(&format!("line{LINES} ")), "{text}");
        s.assert_still_serving(&f);
    }
    s.shutdown();
}

#[test]
fn a_start_of_zero_is_served_from_the_first_line() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    let frame = s.call(recall(1, &f.path, json!(0), json!(5)));
    let text = text_of(&frame);
    assert!(text.contains("L0-5 (+3 lines context)"), "{text}");
    assert!(text.contains("line1 ") && text.contains("line8 "), "{text}");
    assert!(!text.contains("line9 "), "{text}");
    s.assert_still_serving(&f);
    s.shutdown();
}

#[test]
fn an_empty_file_has_no_line_to_start_at() {
    let f = Fixture::with("");
    let mut s = Server::start();
    for (start, end) in [(1, 5), (1, 1)] {
        let frame = s.call(recall(1, &f.path, json!(start), json!(end)));
        assert_refused(&frame, &start.to_string(), &end.to_string(), 0);
    }
    assert_eq!(s.call(rpc(2, "ping"))["result"], json!({}));
    s.shutdown();
}

#[test]
fn numbers_that_are_not_line_numbers_get_the_outline_and_the_server_keeps_serving() {
    let f = Fixture::numbered();
    let mut s = Server::start();
    for (start, end) in [
        (json!(-1), json!(5)),
        (json!(1.5), json!(5)),
        (json!("10"), json!("20")),
        (json!(null), json!(null)),
    ] {
        let frame = s.call(recall(1, &f.path, start, end));
        assert!(text_of(&frame).contains("no selector given"), "{frame}");
        s.assert_still_serving(&f);
    }
    s.shutdown();
}
