// lumen-mcp — MCP protocol core.
//
// This crate holds the pure request→reply logic. Nothing here touches stdout or
// the database: handlers return an `Outcome` describing what to send and what
// (if anything) to meter. The `lumen-mcp` binary owns both side effects, which
// keeps the "stdout first, DB write after" ordering in exactly one place and
// makes every handler assertable in a unit test.

use lumen_core::{
    compress::compress_logs,
    meter::{detect_channel, insert_read_event},
    structure::{CodeItem, detect_lang, outline},
    tokenizer::count_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub const SERVER_NAME: &str = "lumen";
pub const SERVER_VERSION: &str = "0.2.0";
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC error codes we emit.
pub const INVALID_PARAMS: i32 = -32602;
pub const NOT_FOUND: i32 = -32601;

// ── JSON-RPC wire types ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub result: Value,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    pub error: RpcError,
}

#[derive(Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

// ── Outcome: what to send, and what to meter ─────────────────────────────────

/// One `read_events` row, deferred so the caller can write it *after* the
/// JSON-RPC frame has already gone out on stdout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterRow {
    pub path: String,
    pub lines: Option<i64>,
    pub returned_tokens: i64,
    pub full_tokens: i64,
    pub saved_tokens: i64,
    pub routed_via: String,
    pub tool_name: String,
}

impl MeterRow {
    /// Write this row to the metering DB. Resolves the channel at call time so
    /// it reflects the live environment.
    pub fn record(&self) {
        insert_read_event(
            &self.path,
            self.lines,
            self.returned_tokens,
            self.full_tokens,
            self.saved_tokens,
            &self.routed_via,
            detect_channel(),
            &self.tool_name,
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Ok(Value),
    Err { code: i32, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub payload: Payload,
    pub meter: Option<MeterRow>,
}

impl Outcome {
    pub fn ok(result: Value) -> Self {
        Outcome {
            payload: Payload::Ok(result),
            meter: None,
        }
    }

    pub fn err(code: i32, message: impl Into<String>) -> Self {
        Outcome {
            payload: Payload::Err {
                code,
                message: message.into(),
            },
            meter: None,
        }
    }

    /// Convenience for tests and callers that only care about the success value.
    pub fn result(&self) -> Option<&Value> {
        match &self.payload {
            Payload::Ok(v) => Some(v),
            Payload::Err { .. } => None,
        }
    }

    pub fn error_code(&self) -> Option<i32> {
        match &self.payload {
            Payload::Err { code, .. } => Some(*code),
            Payload::Ok(_) => None,
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match &self.payload {
            Payload::Err { message, .. } => Some(message),
            Payload::Ok(_) => None,
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

pub fn ok_result(text: String, full_tokens: usize, returned_tokens: usize) -> Value {
    let saved = full_tokens.saturating_sub(returned_tokens);
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
        "_meta": {
            "full_tokens": full_tokens,
            "returned_tokens": returned_tokens,
            "saved_tokens": saved
        }
    })
}

/// Build a metered success outcome: the JSON-RPC result plus the `read_events`
/// row it earned.
#[allow(clippy::too_many_arguments)]
fn metered(
    text: String,
    full_tokens: usize,
    returned_tokens: usize,
    tool_name: &str,
    routed_via: &str,
    path: &str,
    lines: Option<i64>,
) -> Outcome {
    let saved = full_tokens.saturating_sub(returned_tokens) as i64;
    Outcome {
        payload: Payload::Ok(ok_result(text, full_tokens, returned_tokens)),
        meter: Some(MeterRow {
            path: path.to_string(),
            lines,
            returned_tokens: returned_tokens as i64,
            full_tokens: full_tokens as i64,
            saved_tokens: saved,
            routed_via: routed_via.to_string(),
            tool_name: tool_name.to_string(),
        }),
    }
}

/// Sandbox: path must exist and be a regular file.
pub fn safe_read(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("file not found: {path}"));
    }
    if !p.is_file() {
        return Err(format!("not a regular file: {path}"));
    }
    std::fs::read_to_string(p).map_err(|e| format!("failed to read {path}: {e}"))
}

// ── MCP handlers ─────────────────────────────────────────────────────────────

pub fn handle_initialize() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
    })
}

pub fn handle_tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "lumen_ping",
            "description": "Ping the Lumen MCP server to verify connectivity. Returns 'lumen-mcp alive: <echo>'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "echo": { "type": "string", "description": "Optional string to echo back." }
                }
            }
        },
        {
            "name": "smart_read",
            "description": "Read a source-code file structure-first. Returns a compact outline — \
    functions, classes, structs, interfaces, and imports with their exact line ranges — WITHOUT \
    reading all the bodies. Use this INSTEAD OF the built-in Read tool whenever you need to \
    understand what a source file contains, especially for files ≥100 lines. Follow with \
    recall_file to fetch only the specific items you need. Always reports full_tokens vs \
    returned_tokens so the savings are verifiable. Mode 'full' is available as a fallback when \
    the entire file body is genuinely needed.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the source file to read."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["outline", "full"],
                        "default": "outline",
                        "description": "'outline' (default): return structural outline with line ranges only, saving tokens. 'full': return complete file content."
                    }
                }
            },
            "annotations": {
                "readOnlyHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "recall_file",
            "description": "Fetch specific named items (functions, classes, structs, methods) or \
    an explicit line range from a source file WITHOUT reading the whole file. Resolves names via \
    tree-sitter AST — exact match on the function/class name you saw in smart_read's outline. \
    Use after smart_read once you know which items you need. If names don't match, falls back \
    to the outline so you can correct the query. Always reports full_tokens vs returned_tokens.",
            "inputSchema": {
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the source file."
                    },
                    "names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names of items to retrieve (e.g. [\"parse_args\", \"Config\"]). Matched case-insensitively against the outline."
                    },
                    "start_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line of an explicit range to retrieve (1-based, inclusive). Use with end_line."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last line of an explicit range to retrieve (1-based, inclusive). Use with start_line."
                    }
                }
            },
            "annotations": {
                "readOnlyHint": true,
                "openWorldHint": false
            }
        },
        {
            "name": "compress_logs",
            "description": "Deterministically compact a log file or text dump — collapses \
    consecutive identical lines, stack trace runs (Java/Node/Python/Rust), and blank-line noise \
    into annotated short form with exact counts. Not LLM summarization: fully reversible, no \
    information loss, just compaction. Use BEFORE analyzing error logs, crash dumps, verbose \
    build output, or any large repetitive text to reduce what you need to read. Accepts a file \
    path OR inline text. Reports original vs compressed tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to a log file to read and compress."
                    },
                    "text": {
                        "type": "string",
                        "description": "Inline text to compress (use instead of path)."
                    }
                }
            },
            "annotations": {
                "readOnlyHint": true,
                "openWorldHint": false
            }
        }
    ]})
}

// ── tool: lumen_ping ─────────────────────────────────────────────────────────

pub fn tool_ping(args: &Value) -> Outcome {
    let echo = args.get("echo").and_then(Value::as_str).unwrap_or("pong");
    Outcome::ok(json!({
        "content": [{ "type": "text", "text": format!("lumen-mcp alive: {echo}") }],
        "isError": false
    }))
}

// ── tool: smart_read ─────────────────────────────────────────────────────────

pub fn tool_smart_read(args: &Value) -> Outcome {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Outcome::err(
            INVALID_PARAMS,
            "smart_read: missing required parameter 'path'",
        );
    };

    let src = match safe_read(path) {
        Ok(s) => s,
        Err(e) => return Outcome::err(INVALID_PARAMS, e),
    };

    let full_tokens = count_tokens(&src);
    let line_count = src.lines().count();
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("outline");

    if mode == "full" {
        let text = format!(
            "# {path} (full, {line_count} lines, {full_tokens} tokens)\n\n{}",
            src
        );
        let returned_tokens = count_tokens(&text);
        return metered(
            text,
            full_tokens,
            returned_tokens,
            "mcp__lumen__smart_read",
            "smart_read",
            path,
            Some(line_count as i64),
        );
    }

    // outline mode
    let lang = detect_lang(path);
    let items = outline(&src, lang);
    let text = format_outline(path, line_count, full_tokens, &items);
    let returned_tokens = count_tokens(&text);

    metered(
        text,
        full_tokens,
        returned_tokens,
        "mcp__lumen__smart_read",
        "smart_read",
        path,
        Some(line_count as i64),
    )
}

pub fn format_outline(
    path: &str,
    line_count: usize,
    full_tokens: usize,
    items: &[CodeItem],
) -> String {
    let mut buf = format!(
        "# {path} — outline\n\
         # {line_count} lines | {full_tokens} full tokens | {} items\n\
         # Use recall_file to fetch specific items by name or line range.\n\n",
        items.len()
    );

    for (i, item) in items.iter().enumerate() {
        let name = item.name.as_deref().unwrap_or("(anonymous)");
        buf.push_str(&format!(
            "{:>3}. {:<14} {:<32} L{}-{}\n",
            i + 1,
            item.kind,
            name,
            item.start_line,
            item.end_line
        ));
    }

    buf.push_str(&format!(
        "\n# Example: recall_file(path=\"{path}\", names=[\"<name from above>\"])\n\
         # Or:      recall_file(path=\"{path}\", start_line=N, end_line=M)\n"
    ));

    buf
}

// ── tool: recall_file ────────────────────────────────────────────────────────

pub fn tool_recall_file(args: &Value) -> Outcome {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Outcome::err(
            INVALID_PARAMS,
            "recall_file: missing required parameter 'path'",
        );
    };

    let src = match safe_read(path) {
        Ok(s) => s,
        Err(e) => return Outcome::err(INVALID_PARAMS, e),
    };

    let full_tokens = count_tokens(&src);
    let src_lines: Vec<&str> = src.lines().collect();
    let line_count = src_lines.len();

    let names: Option<Vec<String>> = args.get("names").and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .map(|s| s.to_lowercase())
            .collect()
    });

    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|n| n as usize);

    let text = if let Some(queries) = names {
        // Name-based recall
        let lang = detect_lang(path);
        let items = outline(&src, lang);

        let matched: Vec<&CodeItem> = items
            .iter()
            .filter(|item| {
                item.name
                    .as_ref()
                    .map(|n| {
                        let nl = n.to_lowercase();
                        queries.iter().any(|q| nl == *q || nl.contains(q.as_str()))
                    })
                    .unwrap_or(false)
            })
            .collect();

        if matched.is_empty() {
            // Honest: name not found → return the outline so the caller can retry
            let outline_text = format_outline(path, line_count, full_tokens, &items);
            let msg = format!(
                "# recall_file: no items matched {queries:?} in {path}\n\
                 # Available items:\n\n{outline_text}"
            );
            let returned_tokens = count_tokens(&msg);
            return metered(
                msg,
                full_tokens,
                returned_tokens,
                "mcp__lumen__recall_file",
                "recall_file",
                path,
                Some(line_count as i64),
            );
        }

        format_items_excerpt(path, &src_lines, &matched)
    } else if let (Some(start), Some(end)) = (start_line, end_line) {
        // Explicit line range
        let start0 = start.saturating_sub(1);
        let end0 = end.min(line_count);
        let ctx_start = start0.saturating_sub(3);
        let ctx_end = (end0 + 3).min(line_count);

        let mut buf = format!("# {path} — L{start}-{end} (+3 lines context)\n\n");
        for (i, &line) in src_lines[ctx_start..ctx_end].iter().enumerate() {
            let lineno = ctx_start + i + 1;
            buf.push_str(&format!("{lineno:>5}: {line}\n"));
        }
        buf
    } else {
        // No selector — honest no-op: return full file
        format!(
            "# {path} — full (no selector given, {line_count} lines, {full_tokens} tokens)\n\n{}",
            src
        )
    };

    let returned_tokens = count_tokens(&text);
    metered(
        text,
        full_tokens,
        returned_tokens,
        "mcp__lumen__recall_file",
        "recall_file",
        path,
        Some(line_count as i64),
    )
}

const CTX_LINES: usize = 3;

pub fn format_items_excerpt(path: &str, src_lines: &[&str], items: &[&CodeItem]) -> String {
    let names: Vec<String> = items
        .iter()
        .map(|i| i.name.as_deref().unwrap_or("(anonymous)").to_string())
        .collect();

    let mut buf = format!("# {path} — {} item(s): {}\n", items.len(), names.join(", "));

    for item in items {
        let name = item.name.as_deref().unwrap_or("(anonymous)");
        // Convert 1-based inclusive to 0-based indices
        let start0 = item.start_line.saturating_sub(1);
        let end0 = item.end_line.min(src_lines.len());
        let ctx_start = start0.saturating_sub(CTX_LINES);
        let ctx_end = (end0 + CTX_LINES).min(src_lines.len());

        buf.push('\n');
        buf.push_str(&format!(
            "## {} {} [L{}-{}]\n",
            item.kind, name, item.start_line, item.end_line
        ));

        if ctx_start < start0 {
            buf.push_str(&format!("### context [L{}-{}]\n", ctx_start + 1, start0));
            for (i, &line) in src_lines[ctx_start..start0].iter().enumerate() {
                let lineno = ctx_start + i + 1;
                buf.push_str(&format!("{lineno:>5}: {line}\n"));
            }
        }

        buf.push_str(&format!(
            "### body [L{}-{}]\n",
            item.start_line, item.end_line
        ));
        for (i, &line) in src_lines[start0..end0].iter().enumerate() {
            let lineno = start0 + i + 1;
            buf.push_str(&format!("{lineno:>5}: {line}\n"));
        }

        if end0 < ctx_end {
            buf.push_str(&format!("### context [L{}-{}]\n", end0 + 1, ctx_end));
            for (i, &line) in src_lines[end0..ctx_end].iter().enumerate() {
                let lineno = end0 + i + 1;
                buf.push_str(&format!("{lineno:>5}: {line}\n"));
            }
        }
    }

    buf
}

// ── tool: compress_logs ──────────────────────────────────────────────────────

pub fn tool_compress_logs(args: &Value) -> Outcome {
    let (src, label, meter_path) = if let Some(path) = args.get("path").and_then(Value::as_str) {
        match safe_read(path) {
            Ok(s) => (s, path.to_string(), path.to_string()),
            Err(e) => return Outcome::err(INVALID_PARAMS, e),
        }
    } else if let Some(text) = args.get("text").and_then(Value::as_str) {
        (
            text.to_string(),
            "(inline text)".to_string(),
            "(inline)".to_string(),
        )
    } else {
        return Outcome::err(
            INVALID_PARAMS,
            "compress_logs: provide either 'path' or 'text'",
        );
    };

    let result = compress_logs(&src);

    let header = format!(
        "# {label} — compressed\n\
         # {orig_lines} lines → {comp_lines} lines | {orig_tok} tokens → {comp_tok} tokens | saved {saved}\n\n",
        orig_lines = result.original_lines,
        comp_lines = result.compressed_lines,
        orig_tok = result.original_tokens,
        comp_tok = result.compressed_tokens,
        saved = result
            .original_tokens
            .saturating_sub(result.compressed_tokens),
    );

    let text = format!("{}{}", header, result.text);
    let orig_lines = result.original_lines as i64;
    // Compare raw compressed vs raw original (header is constant overhead, not savings).
    metered(
        text,
        result.original_tokens,
        result.compressed_tokens,
        "mcp__lumen__compress_logs",
        "compress_logs",
        &meter_path,
        Some(orig_lines),
    )
}

// ── dispatch ─────────────────────────────────────────────────────────────────

pub fn handle_tools_call(params: Option<Value>) -> Outcome {
    let params = params.unwrap_or_else(|| json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "lumen_ping" => tool_ping(&args),
        "smart_read" => tool_smart_read(&args),
        "recall_file" => tool_recall_file(&args),
        "compress_logs" => tool_compress_logs(&args),
        other => Outcome::err(NOT_FOUND, format!("Tool not found: {other}")),
    }
}

/// Route a JSON-RPC method to its handler. The caller has already established
/// that the request carries an `id` (notifications get no reply).
pub fn dispatch(method: &str, params: Option<Value>) -> Outcome {
    match method {
        "initialize" => Outcome::ok(handle_initialize()),
        "ping" => Outcome::ok(json!({})),
        "tools/list" => Outcome::ok(handle_tools_list()),
        "tools/call" => handle_tools_call(params),
        other => Outcome::err(NOT_FOUND, format!("Method not found: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    /// Write `body` to `name` inside a fresh tempdir and hand back both so the
    /// directory outlives the test body.
    fn fixture(name: &str, body: &str) -> (TempDir, String) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).expect("create fixture");
        f.write_all(body.as_bytes()).expect("write fixture");
        let s = path.to_string_lossy().to_string();
        (dir, s)
    }

    // alpha and beta are separated by more than CTX_LINES of filler so that a
    // single-item recall of `alpha` cannot bleed beta's body in via the ±3-line
    // context window.
    const RUST_SRC: &str = "\
use std::io;

fn alpha(x: i32) -> i32 {
    x + 1
}

// filler
// filler
// filler
// filler

fn beta() {
    println!(\"BETA_BODY_MARKER\");
}
";

    /// A file shaped like real source: few items, substantial bodies. This is
    /// the shape smart_read's outline is meant to win on.
    fn realistic_rust(fn_count: usize, body_lines: usize) -> String {
        let mut s = String::from("use std::collections::HashMap;\n\n");
        for i in 0..fn_count {
            s.push_str(&format!("fn operation_{i}(input: &str) -> String {{\n"));
            for j in 0..body_lines {
                s.push_str(&format!(
                    "    let step_{j} = input.trim().to_string(); // work {j}\n"
                ));
            }
            s.push_str("    input.to_string()\n}\n\n");
        }
        s
    }

    fn text_of(outcome: &Outcome) -> String {
        outcome.result().expect("ok result")["content"][0]["text"]
            .as_str()
            .expect("text field")
            .to_string()
    }

    fn meta(outcome: &Outcome, key: &str) -> u64 {
        outcome.result().expect("ok result")["_meta"][key]
            .as_u64()
            .expect("numeric meta field")
    }

    // ── ok_result ────────────────────────────────────────────────────────────

    #[test]
    fn ok_result_reports_saved_as_full_minus_returned() {
        let v = ok_result("body".into(), 1000, 250);
        assert_eq!(v["_meta"]["full_tokens"], 1000);
        assert_eq!(v["_meta"]["returned_tokens"], 250);
        assert_eq!(v["_meta"]["saved_tokens"], 750);
        assert_eq!(v["isError"], false);
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "body");
    }

    #[test]
    fn ok_result_saturates_when_returned_exceeds_full() {
        // Outline of a tiny file can be longer than the file; savings must not wrap.
        let v = ok_result("body".into(), 10, 400);
        assert_eq!(v["_meta"]["saved_tokens"], 0, "must saturate, never wrap");
    }

    // ── safe_read ────────────────────────────────────────────────────────────

    #[test]
    fn safe_read_returns_contents_for_a_regular_file() {
        let (_d, path) = fixture("a.txt", "hello");
        assert_eq!(safe_read(&path).unwrap(), "hello");
    }

    #[test]
    fn safe_read_rejects_missing_path() {
        let err = safe_read("/nonexistent/lumen/definitely-not-here.txt").unwrap_err();
        assert!(err.starts_with("file not found:"), "got: {err}");
    }

    #[test]
    fn safe_read_rejects_a_directory() {
        let dir = TempDir::new().unwrap();
        let err = safe_read(&dir.path().to_string_lossy()).unwrap_err();
        assert!(err.starts_with("not a regular file:"), "got: {err}");
    }

    // ── initialize / tools/list ──────────────────────────────────────────────

    #[test]
    fn initialize_advertises_protocol_and_server_identity() {
        let v = handle_initialize();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["serverInfo"]["name"], SERVER_NAME);
        assert_eq!(v["serverInfo"]["version"], SERVER_VERSION);
        assert!(v["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_advertises_exactly_the_four_tools() {
        let v = handle_tools_list();
        let names: Vec<&str> = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["lumen_ping", "smart_read", "recall_file", "compress_logs"]
        );
    }

    #[test]
    fn tools_list_marks_every_read_tool_read_only() {
        let v = handle_tools_list();
        for tool in v["tools"].as_array().unwrap() {
            if tool["name"] == "lumen_ping" {
                continue; // ping carries no annotations by design
            }
            assert_eq!(
                tool["annotations"]["readOnlyHint"], true,
                "{} must be annotated read-only",
                tool["name"]
            );
            assert_eq!(tool["annotations"]["openWorldHint"], false);
        }
    }

    #[test]
    fn tools_list_declares_path_required_where_it_is_mandatory() {
        let v = handle_tools_list();
        for tool in v["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let required = tool["inputSchema"]["required"].as_array();
            match name {
                "smart_read" | "recall_file" => {
                    let req = required.expect("must declare required");
                    assert_eq!(req, &vec![json!("path")], "{name}");
                }
                // ping takes nothing; compress_logs accepts either path or text.
                _ => assert!(required.is_none(), "{name} must not require a field"),
            }
        }
    }

    // ── lumen_ping ───────────────────────────────────────────────────────────

    #[test]
    fn ping_echoes_the_supplied_string() {
        let out = tool_ping(&json!({ "echo": "marco" }));
        assert_eq!(text_of(&out), "lumen-mcp alive: marco");
        assert!(out.meter.is_none(), "ping must not write a metering row");
    }

    #[test]
    fn ping_defaults_to_pong_when_echo_absent() {
        assert_eq!(
            text_of(&tool_ping(&json!({}))),
            "lumen-mcp alive: pong",
            "missing echo falls back to pong"
        );
    }

    #[test]
    fn ping_ignores_a_non_string_echo() {
        assert_eq!(
            text_of(&tool_ping(&json!({ "echo": 42 }))),
            "lumen-mcp alive: pong"
        );
    }

    // ── smart_read ───────────────────────────────────────────────────────────

    #[test]
    fn smart_read_without_path_is_invalid_params() {
        let out = tool_smart_read(&json!({}));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
        assert!(out.error_message().unwrap().contains("missing required"));
        assert!(out.meter.is_none());
    }

    #[test]
    fn smart_read_on_missing_file_is_invalid_params() {
        let out = tool_smart_read(&json!({ "path": "/nope/missing.rs" }));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
        assert!(out.error_message().unwrap().starts_with("file not found:"));
    }

    #[test]
    fn smart_read_outline_lists_the_functions_with_line_ranges() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_smart_read(&json!({ "path": &path }));
        let text = text_of(&out);
        assert!(text.contains("— outline"), "got: {text}");
        assert!(text.contains("alpha"), "outline must name alpha: {text}");
        assert!(text.contains("beta"), "outline must name beta: {text}");
        assert!(
            text.contains("recall_file(path="),
            "must suggest the follow-up call"
        );
    }

    #[test]
    fn smart_read_outline_saves_tokens_on_realistically_shaped_source() {
        // The whole point of the tool: for a normally-shaped file (few items,
        // real bodies) the outline must cost far less than the file.
        let (_d, path) = fixture("big.rs", &realistic_rust(8, 40));
        let out = tool_smart_read(&json!({ "path": &path }));
        let (full, returned) = (meta(&out, "full_tokens"), meta(&out, "returned_tokens"));
        assert!(
            returned < full,
            "outline ({returned}) must cost less than the file ({full})"
        );
        assert!(
            returned * 4 < full,
            "outline should be a small fraction of the file, got {returned} vs {full}"
        );
        assert_eq!(meta(&out, "saved_tokens"), full - returned);
    }

    #[test]
    fn smart_read_reports_zero_savings_rather_than_inventing_them() {
        // A file that is almost entirely declarations produces an outline as long
        // as the source. The tool must report that honestly — saturating to zero
        // — instead of claiming a saving it did not achieve.
        let mut decls = String::new();
        for i in 0..60 {
            decls.push_str(&format!("fn f{i}() {{}}\n"));
        }
        let (_d, path) = fixture("decls.rs", &decls);
        let out = tool_smart_read(&json!({ "path": &path }));
        let (full, returned) = (meta(&out, "full_tokens"), meta(&out, "returned_tokens"));
        assert!(
            returned > full,
            "this shape is expected to produce a larger outline ({returned} vs {full})"
        );
        assert_eq!(
            meta(&out, "saved_tokens"),
            0,
            "no savings must be reported as 0, never as a wrapped huge number"
        );
    }

    #[test]
    fn smart_read_full_mode_returns_the_whole_body() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_smart_read(&json!({ "path": &path, "mode": "full" }));
        let text = text_of(&out);
        assert!(
            text.contains("(full,"),
            "header must mark full mode: {text}"
        );
        assert!(text.contains("BETA_BODY_MARKER"), "body must be present");
    }

    #[test]
    fn smart_read_unknown_mode_falls_back_to_outline() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_smart_read(&json!({ "path": &path, "mode": "sideways" }));
        assert!(text_of(&out).contains("— outline"));
    }

    #[test]
    fn smart_read_meters_the_path_and_line_count() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_smart_read(&json!({ "path": &path }));
        let m = out.meter.expect("smart_read must meter");
        assert_eq!(m.path, path);
        assert_eq!(m.tool_name, "mcp__lumen__smart_read");
        assert_eq!(m.routed_via, "smart_read");
        assert_eq!(m.lines, Some(RUST_SRC.lines().count() as i64));
        // Saturating: a tiny file's outline can exceed the file, and the metered
        // saving must then be 0 rather than negative.
        assert_eq!(m.saved_tokens, (m.full_tokens - m.returned_tokens).max(0));
        assert!(m.saved_tokens >= 0, "a metered saving is never negative");
    }

    #[test]
    fn smart_read_handles_an_empty_file() {
        let (_d, path) = fixture("empty.rs", "");
        let out = tool_smart_read(&json!({ "path": &path }));
        assert_eq!(meta(&out, "full_tokens"), 0);
        assert_eq!(
            meta(&out, "saved_tokens"),
            0,
            "nothing to save from nothing"
        );
    }

    #[test]
    fn smart_read_handles_an_unknown_extension() {
        let (_d, path) = fixture("notes.xyz", "some free text\nmore text\n");
        let out = tool_smart_read(&json!({ "path": &path }));
        // Unknown languages yield a single whole-file item rather than an error.
        assert!(text_of(&out).contains("— outline"));
    }

    // ── recall_file ──────────────────────────────────────────────────────────

    #[test]
    fn recall_file_without_path_is_invalid_params() {
        let out = tool_recall_file(&json!({}));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
        assert!(out.error_message().unwrap().contains("missing required"));
    }

    #[test]
    fn recall_file_on_missing_file_is_invalid_params() {
        let out = tool_recall_file(&json!({ "path": "/nope/missing.rs" }));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
    }

    #[test]
    fn recall_file_by_name_returns_only_that_item() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(&json!({ "path": &path, "names": ["alpha"] }));
        let text = text_of(&out);
        assert!(text.contains("1 item(s): alpha"), "got: {text}");
        assert!(text.contains("x + 1"), "alpha's body must be included");
        assert!(
            !text.contains("BETA_BODY_MARKER"),
            "beta's body must NOT be included: {text}"
        );
    }

    #[test]
    fn recall_file_by_name_is_case_insensitive() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(&json!({ "path": &path, "names": ["ALPHA"] }));
        assert!(text_of(&out).contains("alpha"));
    }

    #[test]
    fn recall_file_can_fetch_several_names_at_once() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(&json!({ "path": &path, "names": ["alpha", "beta"] }));
        let text = text_of(&out);
        assert!(text.contains("2 item(s)"), "got: {text}");
        assert!(text.contains("x + 1") && text.contains("BETA_BODY_MARKER"));
    }

    #[test]
    fn recall_file_unmatched_name_falls_back_to_the_outline() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(&json!({ "path": &path, "names": ["gamma"] }));
        let text = text_of(&out);
        assert!(text.contains("no items matched"), "got: {text}");
        assert!(text.contains("Available items:"));
        assert!(text.contains("alpha"), "the outline must still be offered");
        assert!(
            out.meter.is_some(),
            "a fallback still returned content, so it still meters"
        );
    }

    #[test]
    fn recall_file_by_line_range_includes_three_lines_of_context() {
        let numbered: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        let (_d, path) = fixture("nums.txt", &numbered);
        let out = tool_recall_file(&json!({ "path": &path, "start_line": 10, "end_line": 12 }));
        let text = text_of(&out);
        assert!(text.contains("L10-12 (+3 lines context)"), "got: {text}");
        assert!(text.contains("line7"), "3 lines of leading context");
        assert!(!text.contains("line6"), "but not a 4th: {text}");
        assert!(text.contains("line15"), "3 lines of trailing context");
        assert!(!text.contains("line16"), "but not a 4th: {text}");
    }

    #[test]
    fn recall_file_range_clamps_at_the_start_of_file() {
        let numbered: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        let (_d, path) = fixture("nums.txt", &numbered);
        let out = tool_recall_file(&json!({ "path": &path, "start_line": 1, "end_line": 2 }));
        assert!(text_of(&out).contains("line1"), "must not underflow");
    }

    #[test]
    fn recall_file_range_clamps_past_the_end_of_file() {
        let numbered: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        let (_d, path) = fixture("nums.txt", &numbered);
        let out = tool_recall_file(&json!({ "path": &path, "start_line": 8, "end_line": 9999 }));
        let text = text_of(&out);
        assert!(text.contains("line10"), "must include the last real line");
    }

    #[test]
    fn recall_file_with_no_selector_returns_the_whole_file() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(&json!({ "path": &path }));
        let text = text_of(&out);
        assert!(text.contains("no selector given"), "got: {text}");
        assert!(text.contains("x + 1") && text.contains("BETA_BODY_MARKER"));
    }

    #[test]
    fn recall_file_names_take_precedence_over_a_range() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let out = tool_recall_file(
            &json!({ "path": &path, "names": ["alpha"], "start_line": 1, "end_line": 2 }),
        );
        assert!(text_of(&out).contains("1 item(s): alpha"));
    }

    #[test]
    fn recall_file_meters_with_its_own_tool_name() {
        let (_d, path) = fixture("src.rs", RUST_SRC);
        let m = tool_recall_file(&json!({ "path": &path, "names": ["alpha"] }))
            .meter
            .expect("recall_file must meter");
        assert_eq!(m.tool_name, "mcp__lumen__recall_file");
        assert_eq!(m.routed_via, "recall_file");
    }

    // ── compress_logs ────────────────────────────────────────────────────────

    #[test]
    fn compress_logs_without_path_or_text_is_invalid_params() {
        let out = tool_compress_logs(&json!({}));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
        assert!(
            out.error_message()
                .unwrap()
                .contains("either 'path' or 'text'")
        );
    }

    #[test]
    fn compress_logs_collapses_repeated_lines_from_inline_text() {
        let text = "boom\n".repeat(50);
        let out = tool_compress_logs(&json!({ "text": text }));
        let body = text_of(&out);
        assert!(body.contains("(inline text)"), "got: {body}");
        assert!(
            meta(&out, "returned_tokens") < meta(&out, "full_tokens"),
            "50 identical lines must compress"
        );
        assert!(meta(&out, "saved_tokens") > 0);
    }

    #[test]
    fn compress_logs_reads_from_a_path() {
        let (_d, path) = fixture("run.log", &"repeat me\n".repeat(30));
        let out = tool_compress_logs(&json!({ "path": &path }));
        assert!(text_of(&out).contains(&path), "header must name the file");
        assert_eq!(out.meter.unwrap().path, path);
    }

    #[test]
    fn compress_logs_on_missing_path_is_invalid_params() {
        let out = tool_compress_logs(&json!({ "path": "/nope/missing.log" }));
        assert_eq!(out.error_code(), Some(INVALID_PARAMS));
    }

    #[test]
    fn compress_logs_meters_inline_text_under_a_sentinel_path() {
        let m = tool_compress_logs(&json!({ "text": "a\na\na\n" }))
            .meter
            .expect("compress_logs must meter");
        assert_eq!(m.path, "(inline)", "inline text has no real path");
        assert_eq!(m.tool_name, "mcp__lumen__compress_logs");
    }

    #[test]
    fn compress_logs_prefers_path_when_both_are_given() {
        let (_d, path) = fixture("run.log", "from the file\n");
        let out = tool_compress_logs(&json!({ "path": &path, "text": "from inline" }));
        let body = text_of(&out);
        assert!(body.contains("from the file"), "path wins: {body}");
        assert!(!body.contains("from inline"));
    }

    #[test]
    fn compress_logs_is_honest_when_nothing_compresses() {
        let out = tool_compress_logs(&json!({ "text": "one\ntwo\nthree\n" }));
        // No repetition to collapse — savings must be reported as ~nothing, not invented.
        assert_eq!(
            meta(&out, "saved_tokens"),
            0,
            "incompressible input must claim zero savings"
        );
    }

    // ── dispatch ─────────────────────────────────────────────────────────────

    #[test]
    fn dispatch_routes_initialize() {
        let out = dispatch("initialize", None);
        assert_eq!(out.result().unwrap()["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn dispatch_routes_ping_to_an_empty_object() {
        assert_eq!(*dispatch("ping", None).result().unwrap(), json!({}));
    }

    #[test]
    fn dispatch_routes_tools_list() {
        assert_eq!(
            dispatch("tools/list", None).result().unwrap()["tools"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn dispatch_rejects_an_unknown_method() {
        let out = dispatch("tools/teleport", None);
        assert_eq!(out.error_code(), Some(NOT_FOUND));
        assert!(out.error_message().unwrap().contains("Method not found"));
    }

    #[test]
    fn dispatch_rejects_an_unknown_tool() {
        let out = dispatch(
            "tools/call",
            Some(json!({ "name": "nope", "arguments": {} })),
        );
        assert_eq!(out.error_code(), Some(NOT_FOUND));
        assert!(
            out.error_message()
                .unwrap()
                .contains("Tool not found: nope")
        );
    }

    #[test]
    fn dispatch_tools_call_without_params_reports_an_empty_tool_name() {
        let out = dispatch("tools/call", None);
        assert_eq!(out.error_code(), Some(NOT_FOUND));
        assert!(out.error_message().unwrap().contains("Tool not found: "));
    }

    #[test]
    fn dispatch_tools_call_defaults_missing_arguments_to_empty() {
        // No "arguments" key at all — ping must still answer rather than panic.
        let out = dispatch("tools/call", Some(json!({ "name": "lumen_ping" })));
        assert_eq!(text_of(&out), "lumen-mcp alive: pong");
    }

    #[test]
    fn dispatch_routes_every_advertised_tool() {
        // Whatever tools/list advertises must actually be dispatchable — no
        // tool may be advertised and then answer "Tool not found".
        for tool in handle_tools_list()["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let out = dispatch("tools/call", Some(json!({ "name": name })));
            assert_ne!(
                out.error_code(),
                Some(NOT_FOUND),
                "{name} is advertised but not dispatchable"
            );
        }
    }

    // ── Outcome accessors ────────────────────────────────────────────────────

    #[test]
    fn outcome_accessors_discriminate_ok_from_err() {
        let ok = Outcome::ok(json!({"a": 1}));
        assert!(ok.result().is_some());
        assert_eq!(ok.error_code(), None);
        assert_eq!(ok.error_message(), None);

        let err = Outcome::err(INVALID_PARAMS, "bad");
        assert!(err.result().is_none());
        assert_eq!(err.error_code(), Some(INVALID_PARAMS));
        assert_eq!(err.error_message(), Some("bad"));
    }
}
