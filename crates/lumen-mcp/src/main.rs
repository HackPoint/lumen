// lumen-mcp — MCP stdio server.
// STDOUT: JSON-RPC 2.0 frames only (newline-delimited).
// STDERR: all logging / diagnostics.

use lumen_core::{
    compress::compress_logs,
    meter::{detect_channel, insert_read_event},
    structure::{CodeItem, detect_lang, outline},
    tokenizer::count_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::Path;

// ── JSON-RPC wire types ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Request {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    result: Value,
}

#[derive(Serialize)]
struct ErrorResponse {
    jsonrpc: &'static str,
    id: Value,
    error: RpcError,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn send(id: Value, result: Value) {
    let out = serde_json::to_string(&Response {
        jsonrpc: "2.0",
        id,
        result,
    })
    .expect("serialization never fails");
    println!("{out}");
    io::stdout().flush().ok();
}

fn send_err(id: Value, code: i32, msg: impl Into<String>) {
    let out = serde_json::to_string(&ErrorResponse {
        jsonrpc: "2.0",
        id,
        error: RpcError {
            code,
            message: msg.into(),
        },
    })
    .expect("serialization never fails");
    println!("{out}");
    io::stdout().flush().ok();
}

fn ok_result(text: String, full_tokens: usize, returned_tokens: usize) -> Value {
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

#[allow(clippy::too_many_arguments)]
/// Send the JSON-RPC response AND write a metering row to the DB.
/// Called instead of `send(id, ok_result(...))` for every lumen tool response.
/// stdout is written first so the response is never delayed by a DB write.
fn send_and_meter(
    id: Value,
    text: String,
    full_tokens: usize,
    returned_tokens: usize,
    tool_name: &str,
    routed_via: &str,
    path: &str,
    lines: Option<i64>,
) {
    send(id, ok_result(text, full_tokens, returned_tokens));
    let saved = full_tokens.saturating_sub(returned_tokens) as i64;
    let channel = detect_channel();
    insert_read_event(
        path,
        lines,
        returned_tokens as i64,
        full_tokens as i64,
        saved,
        routed_via,
        channel,
        tool_name,
    );
}

/// Sandbox: path must exist and be a regular file.
fn safe_read(path: &str) -> Result<String, String> {
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

fn handle_initialize(id: Value) {
    send(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "lumen", "version": "0.2.0" }
        }),
    );
}

fn handle_tools_list(id: Value) {
    send(
        id,
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
        ]}),
    );
}

// ── tool: lumen_ping ─────────────────────────────────────────────────────────

fn tool_ping(id: Value, args: &Value) {
    let echo = args.get("echo").and_then(Value::as_str).unwrap_or("pong");
    send(
        id,
        json!({
            "content": [{ "type": "text", "text": format!("lumen-mcp alive: {echo}") }],
            "isError": false
        }),
    );
}

// ── tool: smart_read ─────────────────────────────────────────────────────────

fn tool_smart_read(id: Value, args: &Value) {
    let path = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return send_err(id, -32602, "smart_read: missing required parameter 'path'"),
    };

    let src = match safe_read(path) {
        Ok(s) => s,
        Err(e) => return send_err(id, -32602, e),
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
        return send_and_meter(
            id,
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

    send_and_meter(
        id,
        text,
        full_tokens,
        returned_tokens,
        "mcp__lumen__smart_read",
        "smart_read",
        path,
        Some(line_count as i64),
    );
}

fn format_outline(path: &str, line_count: usize, full_tokens: usize, items: &[CodeItem]) -> String {
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

fn tool_recall_file(id: Value, args: &Value) {
    let path = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return send_err(id, -32602, "recall_file: missing required parameter 'path'"),
    };

    let src = match safe_read(path) {
        Ok(s) => s,
        Err(e) => return send_err(id, -32602, e),
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
            return send_and_meter(
                id,
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
    send_and_meter(
        id,
        text,
        full_tokens,
        returned_tokens,
        "mcp__lumen__recall_file",
        "recall_file",
        path,
        Some(line_count as i64),
    );
}

const CTX_LINES: usize = 3;

fn format_items_excerpt(path: &str, src_lines: &[&str], items: &[&CodeItem]) -> String {
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

fn tool_compress_logs(id: Value, args: &Value) {
    let (src, label, meter_path) = if let Some(path) = args.get("path").and_then(Value::as_str) {
        match safe_read(path) {
            Ok(s) => (s, path.to_string(), path.to_string()),
            Err(e) => return send_err(id, -32602, e),
        }
    } else if let Some(text) = args.get("text").and_then(Value::as_str) {
        (
            text.to_string(),
            "(inline text)".to_string(),
            "(inline)".to_string(),
        )
    } else {
        return send_err(id, -32602, "compress_logs: provide either 'path' or 'text'");
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
    send_and_meter(
        id,
        text,
        result.original_tokens,
        result.compressed_tokens,
        "mcp__lumen__compress_logs",
        "compress_logs",
        &meter_path,
        Some(orig_lines),
    );
}

// ── dispatch ─────────────────────────────────────────────────────────────────

fn handle_tools_call(id: Value, params: Option<Value>) {
    let params = params.unwrap_or_else(|| json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match name {
        "lumen_ping" => tool_ping(id, &args),
        "smart_read" => tool_smart_read(id, &args),
        "recall_file" => tool_recall_file(id, &args),
        "compress_logs" => tool_compress_logs(id, &args),
        other => send_err(id, -32601, format!("Tool not found: {other}")),
    }
}

// ── main loop ────────────────────────────────────────────────────────────────

fn main() {
    eprintln!("lumen-mcp v0.2.0 starting (stdio transport)");

    for line in io::stdin().lock().lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            Ok(_) => continue,
            Err(e) => {
                eprintln!("stdin read error: {e}");
                break;
            }
        };

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("JSON parse error: {e}");
                continue;
            }
        };

        let id = match req.id {
            Some(id) => id,
            None => {
                eprintln!("notification: {}", req.method);
                continue;
            }
        };

        eprintln!("→ {} (id={})", req.method, id);

        match req.method.as_str() {
            "initialize" => handle_initialize(id),
            "ping" => send(id, json!({})),
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, req.params),
            other => {
                eprintln!("unknown method: {other}");
                send_err(id, -32601, format!("Method not found: {other}"));
            }
        }
    }

    eprintln!("lumen-mcp exiting");
}
