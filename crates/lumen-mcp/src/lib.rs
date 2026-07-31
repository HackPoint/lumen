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
    ranked,
    structure::{CodeItem, detect_lang, outline},
    tokenizer::count_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

pub const SERVER_NAME: &str = "lumen";
// Taken from the crate, not written out. Hardcoded, it said 0.2.0 while the crate was
// 1.5.0 — so the startup banner and every `initialize` response reported a version
// seven releases old, which is worse than reporting none at all when someone is
// trying to work out which build they are talking to.
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
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
///
/// `PartialEq` only, not `Eq`: the ranked decision carries the f64 economics inputs it
/// was computed from, and floats have no total equality. Comparing rows in tests is what
/// this derive is for, and `PartialEq` serves that.
#[derive(Debug, Clone, PartialEq)]
pub struct MeterRow {
    pub path: String,
    pub lines: Option<i64>,
    pub returned_tokens: i64,
    pub full_tokens: i64,
    pub saved_tokens: i64,
    pub routed_via: String,
    pub tool_name: String,
    /// Claude Code's session id, from CLAUDE_CODE_SESSION_ID. Claude Code exports it
    /// to the MCP server it spawns, and because stdio servers are per-session the
    /// value is stable and correct for the whole process lifetime. Without it a read
    /// cannot be tied to the turn that caused it — second-precision timestamps are
    /// ambiguous when several sessions run at once.
    pub session_id: Option<String>,
    /// File modification time at read time, so a re-read of an unchanged file is
    /// distinguishable from a re-read after an edit.
    pub file_mtime: Option<i64>,
    /// Identity of the REQUEST, not the file: two recall_file calls on one file
    /// asking for different items are different requests, so keying dedup on path
    /// alone overstates the opportunity.
    pub req_key: Option<String>,
    /// Ranked-outline decision inputs. Empty for the legacy arm, which has no decision
    /// to record — a zero there would claim one was made.
    pub ranked: lumen_core::meter::RankedMeta,
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
            self.session_id.as_deref(),
            self.file_mtime,
            self.req_key.as_deref(),
            &self.ranked,
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
    // Signed, deliberately. Both operands are usize, so saturating_sub floored at
    // zero and the cast to i64 happened afterwards — a negative saving was
    // structurally unrepresentable. 170 real events returned MORE than the file
    // contained (full-mode reads, whole-file fallbacks, ranges covering most of a
    // file) and every one of them was recorded as a saving of exactly 0, hiding
    // 92,347 tokens of loss and inflating the reported averages.
    let saved = full_tokens as i64 - returned_tokens as i64;
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
/// The session Claude Code told us about, if it did.
fn session_id() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Modification time of `path` in unix seconds, or None when it is not a real file
/// (compress_logs on inline text has no file behind it).
fn file_mtime(path: &str) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

/// Stable identity for a request.
///
/// `smart_read` is identified by its path alone — there is one outline per file. A
/// `recall_file` also depends on *what was asked for*, so the selector is folded in,
/// with names sorted so argument order cannot produce two keys for one request.
pub fn request_key(
    path: &str,
    names: &[String],
    start: Option<usize>,
    end: Option<usize>,
) -> String {
    if names.is_empty() && start.is_none() && end.is_none() {
        return path.to_string();
    }
    let mut sorted = names.to_vec();
    sorted.sort();
    format!(
        "{path}#names={}&range={}-{}",
        sorted.join(","),
        start.map(|v| v.to_string()).unwrap_or_default(),
        end.map(|v| v.to_string()).unwrap_or_default()
    )
}

#[allow(clippy::too_many_arguments)]
fn metered(
    text: String,
    full_tokens: usize,
    returned_tokens: usize,
    tool_name: &str,
    routed_via: &str,
    path: &str,
    lines: Option<i64>,
    req_key: Option<String>,
) -> Outcome {
    metered_with(
        text,
        full_tokens,
        returned_tokens,
        tool_name,
        routed_via,
        path,
        lines,
        req_key,
        lumen_core::meter::RankedMeta::default(),
    )
}

/// How a call site wants inflation handled.
///
/// An explicit parameter rather than a default, so a new tool cannot forget to decide. Before
/// this existed only the ranked path had a guard (`Decline::WouldInflate`); the legacy outline,
/// `mode="full"` and all four `recall_file` branches had none, and between them they spent
/// 92,347 tokens above what a plain read would have cost.
pub enum Inflate<'a> {
    /// If the reply would cost more than the file, return the file instead and say so.
    Guard { fallback: &'a str },
    /// Delivering the whole file *is* the request, so a header-sized overage is expected and
    /// honest. Only `smart_read(mode="full")` uses this.
    Allow,
}

/// The most a guarded reply may exceed the file by: the one explanatory line it adds.
///
/// A fallback cannot cost *less* than the file it returns, so `returned <= full` is not a
/// satisfiable bound — the honest one is `full + NOTE_ALLOWANCE`. Sized with room to spare
/// because the note embeds the path, and a long monorobo path tokenizes to tens of tokens on
/// its own. Note also that the JSON-RPC envelope and `_meta` block are never counted at all, so
/// every recorded `tokens_returned` understates real cost by ~40-60 tokens regardless.
pub const NOTE_ALLOWANCE: i64 = 80;

/// A reply that would cost more than the file is never an optimisation.
///
/// Returns the file plus one line of explanation, not an error and not a truncation:
///   - an error leaves the model with no content and burns a round, which is the exact failure
///     the ranked arm already refuses to cause;
///   - a truncation returns less than was asked for, silently.
///
/// Routed as `would_inflate` so it is visible in the ledger and excluded from the savings
/// headline by route rather than hidden inside a clamp.
fn inflated_fallback(
    fallback: &str,
    attempted: usize,
    full_tokens: usize,
    tool_name: &str,
    path: &str,
    lines: Option<i64>,
    req_key: Option<String>,
) -> Outcome {
    let text = format!(
        "# lumen: the requested view was {attempted} tokens against {full_tokens} for the file, \
         so the file is returned instead.\n\n{fallback}"
    );
    let returned_tokens = count_tokens(&text);
    Outcome {
        payload: Payload::Ok(ok_result(text, full_tokens, returned_tokens)),
        meter: Some(MeterRow {
            path: path.to_string(),
            lines,
            returned_tokens: returned_tokens as i64,
            full_tokens: full_tokens as i64,
            saved_tokens: full_tokens as i64 - returned_tokens as i64,
            routed_via: lumen_core::ranked::Decline::WouldInflate
                .route()
                .to_string(),
            tool_name: tool_name.to_string(),
            session_id: session_id(),
            file_mtime: file_mtime(path),
            req_key: req_key.or_else(|| Some(path.to_string())),
            ranked: lumen_core::meter::RankedMeta::default(),
        }),
    }
}

/// The language could not be parsed, so there is no structure to return.
///
/// Distinct from `inflated_fallback` because the complaint is different: the outline here is
/// *cheap*, it just describes nothing — one synthetic whole-file item. Reporting it as "the view
/// cost more than the file" would be false, and the old behaviour (metering it as a ~95% saving)
/// was worse: a saving on a file the tool never looked inside.
fn undescribable_fallback(
    src: &str,
    full_tokens: usize,
    path: &str,
    lines: Option<i64>,
) -> Outcome {
    let text = format!(
        "# lumen: no structure could be extracted from {path}, so the file is returned instead.\n\n{src}"
    );
    let returned_tokens = count_tokens(&text);
    Outcome {
        payload: Payload::Ok(ok_result(text, full_tokens, returned_tokens)),
        meter: Some(MeterRow {
            path: path.to_string(),
            lines,
            returned_tokens: returned_tokens as i64,
            full_tokens: full_tokens as i64,
            saved_tokens: full_tokens as i64 - returned_tokens as i64,
            routed_via: lumen_core::ranked::Decline::NoDefs.route().to_string(),
            tool_name: "mcp__lumen__smart_read".to_string(),
            session_id: session_id(),
            file_mtime: file_mtime(path),
            req_key: Some(path.to_string()),
            ranked: lumen_core::meter::RankedMeta::default(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn metered_guarded(
    text: String,
    full_tokens: usize,
    tool_name: &str,
    routed_via: &str,
    path: &str,
    lines: Option<i64>,
    req_key: Option<String>,
    inflate: Inflate<'_>,
) -> Outcome {
    let returned_tokens = count_tokens(&text);
    if let Inflate::Guard { fallback } = inflate
        && returned_tokens >= full_tokens
    {
        return inflated_fallback(
            fallback,
            returned_tokens,
            full_tokens,
            tool_name,
            path,
            lines,
            req_key,
        );
    }
    metered(
        text,
        full_tokens,
        returned_tokens,
        tool_name,
        routed_via,
        path,
        lines,
        req_key,
    )
}

#[allow(clippy::too_many_arguments)]
fn metered_with(
    text: String,
    full_tokens: usize,
    returned_tokens: usize,
    tool_name: &str,
    routed_via: &str,
    path: &str,
    lines: Option<i64>,
    req_key: Option<String>,
    ranked: lumen_core::meter::RankedMeta,
) -> Outcome {
    // Signed for the same reason as ok_result above: a loss must be recordable.
    let saved = full_tokens as i64 - returned_tokens as i64;
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
            session_id: session_id(),
            file_mtime: file_mtime(path),
            req_key: req_key.or_else(|| Some(path.to_string())),
            ranked,
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
    understand what a source file contains, especially for files ≥300 lines. Follow with \
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
        // Header shrunk to one short token-cheap line. The old one repeated the absolute path
        // alongside two counts, and on a long monorepo path that alone was ~40 tokens — which
        // is exactly 47 of the 84 recorded smart_read overages, every one of them this header.
        let text = format!("# {path} (full)\n\n{}", src);
        // Routed `smart_read_full`, NOT `smart_read`. Handing over a whole file is a delivery,
        // not an optimisation, and pooling it with outline savings is how a route that can only
        // ever lose tokens ended up inside the savings headline. Deliberately absent from
        // lumen-stats' LUMEN_ROUTES.
        return metered_guarded(
            text,
            full_tokens,
            "mcp__lumen__smart_read",
            "smart_read_full",
            path,
            Some(line_count as i64),
            // One outline per file, so the path is the whole request identity.
            None,
            // Allowed: the caller asked for the file, so returning it plus a header is the
            // honest answer and the small overage is the header they asked to be labelled with.
            Inflate::Allow,
        );
    }

    // outline mode. Which implementation depends on the rollout flag; `Off` is the
    // default and reaches nothing below.
    let arm = ranked::arm_for(ranked::mode_from_env(), path);
    if arm == ranked::Arm::Ranked
        && let Some(o) = ranked_arm(path, &src, full_tokens, line_count)
    {
        return o;
    }

    let lang = detect_lang(path);
    let items = outline(&src, lang);

    // An "outline" that is one synthetic whole-file item describes nothing. `outline` does not
    // fail on a language it cannot parse — it returns `whole_file_item` — so without this the
    // tool would report a ~95% saving on a file it never looked inside. Hand back the file and
    // let the ledger say so.
    if items.iter().all(|i| i.kind == "file") {
        return undescribable_fallback(&src, full_tokens, path, Some(line_count as i64));
    }

    let text = format_outline(path, line_count, full_tokens, &items);
    metered_guarded(
        text,
        full_tokens,
        "mcp__lumen__smart_read",
        "smart_read",
        path,
        Some(line_count as i64),
        None,
        // Guarded: a tiny file, or one that is almost entirely short declarations, can have an
        // outline larger than itself. That is not a saving and must not be recorded as one.
        Inflate::Guard { fallback: &src },
    )
}

/// Economics for this process, resolved once.
///
/// Once, not per call: `Econ::observed` opens the ledger and averages `turns`, which is
/// far too much work to repeat inside a synchronous tool call. An MCP server is
/// per-session and short-lived enough that the means cannot drift meaningfully within
/// one process.
fn econ() -> &'static lumen_core::econ::Econ {
    static ECON: std::sync::OnceLock<lumen_core::econ::Econ> = std::sync::OnceLock::new();
    ECON.get_or_init(|| match lumen_core::meter::db_path() {
        Some(db) => lumen_core::econ::Econ::observed(&db),
        None => lumen_core::econ::Econ::default(),
    })
}

/// Record the decision inputs whether or not an outline was produced.
///
/// A decline is the more interesting row: it says the budget refused a call that would
/// otherwise have happened, and without it the ledger would show only the calls that
/// went ahead — which is the population that makes any gate look unnecessary.
fn ranked_meta(d: &ranked::Decision, k: i64, n: i64) -> lumen_core::meter::RankedMeta {
    lumen_core::meter::RankedMeta {
        budget: Some(d.budget),
        s_min: Some(d.s_min),
        econ_context: Some(d.econ.context_tokens),
        econ_rounds: Some(d.econ.rounds_remaining),
        econ_output: Some(d.econ.output_tokens),
        econ_source: Some(d.econ.source.as_str().to_string()),
        k_selected: Some(k),
        n_total: Some(n),
        coeff_version: Some(d.coeff_version as i64),
        target_outline: Some(lumen_core::econ::target_outline()),
    }
}

/// The ranked arm. `None` means it declined and the caller should fall back.
///
/// A decline returns `None` rather than an error: the model asked for an outline and must
/// get one. The fallback is the legacy outline rather than the truncation the
/// specification names — truncation would be a regression against what already ships,
/// and the decline is still visible because the fallback row carries the decline's route.
fn ranked_arm(path: &str, src: &str, full_tokens: usize, line_count: usize) -> Option<Outcome> {
    let e = econ();
    let stamp = ranked::FileStamp::of(path);
    let d = ranked::ranked_outline_cached(path, src, full_tokens, e, &count_tokens, stamp);

    match &d.outcome {
        Ok(f) => {
            let meta = ranked_meta(&d, f.k as i64, f.n as i64);
            Some(metered_with(
                f.text.clone(),
                full_tokens,
                f.returned_tokens,
                "mcp__lumen__smart_read",
                ranked::ROUTE_RANKED,
                path,
                Some(line_count as i64),
                None,
                meta,
            ))
        }
        Err(decline) => {
            // Fall through to the legacy outline, but record that the ranked arm refused
            // and why, on its own route.
            let lang = detect_lang(path);
            let items = outline(src, lang);
            let text = format_outline(path, line_count, full_tokens, &items);
            let returned = count_tokens(&text);
            let meta = ranked_meta(&d, 0, 0);
            Some(metered_with(
                text,
                full_tokens,
                returned,
                "mcp__lumen__smart_read",
                decline.route(),
                path,
                Some(line_count as i64),
                None,
                meta,
            ))
        }
    }
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

    // Computed before `names` is consumed below. The key must reflect what was
    // asked for, not just which file: two recall_file calls on one file requesting
    // different items are different requests, and keying dedup on the path alone
    // would count the second as redundant when it is not.
    let req_key = request_key(path, names.as_deref().unwrap_or(&[]), start_line, end_line);

    let text = if let Some(queries) = names {
        // Name-based recall
        let lang = detect_lang(path);
        let items = outline(&src, lang);

        // Empty or whitespace-only queries matched every named item under the old substring
        // rule — `names: [""]` returned the entire file, dressed as a saving.
        let queries: Vec<String> = queries
            .iter()
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty())
            .collect();
        if queries.is_empty() {
            return Outcome::err(
                INVALID_PARAMS,
                "recall_file: 'names' contained no usable names (empty strings match nothing)",
            );
        }

        // Exact first. The tool description promised exact matching and the code did
        // `nl == q || nl.contains(q)`, so `names: ["e"]` matched nearly every item in a file and
        // the reply came back larger than the file itself — the mechanism behind 96% of the
        // recorded overshoot.
        let exact: Vec<&CodeItem> = items
            .iter()
            .filter(|item| {
                item.name
                    .as_ref()
                    .map(|n| queries.contains(&n.to_lowercase()))
                    .unwrap_or(false)
            })
            .collect();

        // Substring only as a labelled fallback, and only when it stays small. Sloppy queries
        // are genuinely useful — dropping them outright would break real callers — but they must
        // not be able to select the whole file.
        const SUBSTRING_CAP: usize = 5;
        let mut fuzzy_note = String::new();
        let matched: Vec<&CodeItem> = if !exact.is_empty() {
            exact
        } else {
            let subs: Vec<&CodeItem> = items
                .iter()
                .filter(|item| {
                    item.name
                        .as_ref()
                        .map(|n| {
                            let nl = n.to_lowercase();
                            queries.iter().any(|q| nl.contains(q.as_str()))
                        })
                        .unwrap_or(false)
                })
                .collect();
            if subs.is_empty() || subs.len() > SUBSTRING_CAP {
                // Too broad to answer with bodies. Return the map, not the territory.
                let names: Vec<String> = subs.iter().filter_map(|i| i.name.clone()).collect();
                let filtered: Vec<&CodeItem> = if subs.is_empty() {
                    items.iter().collect()
                } else {
                    subs
                };
                let head = if names.is_empty() {
                    format!(
                        "# recall_file: nothing matched {} in {path}\n# Available items:\n\n",
                        queries.join(", ")
                    )
                } else {
                    format!(
                        "# recall_file: {} matched {} items — too many to return bodies for.\n\
                         # Pick an exact name from below and call again.\n\n",
                        queries.join(", "),
                        names.len()
                    )
                };
                let text = format!(
                    "{head}{}",
                    format_outline_compact(path, line_count, &filtered)
                );
                return metered_guarded(
                    text,
                    full_tokens,
                    "mcp__lumen__recall_file",
                    "recall_file",
                    path,
                    Some(line_count as i64),
                    Some(req_key.clone()),
                    Inflate::Guard { fallback: &src },
                );
            }
            fuzzy_note = format!(
                "# matched by substring, not exact: {} → {}\n",
                queries.join(", "),
                subs.iter()
                    .filter_map(|i| i.name.as_deref())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            subs
        };

        // A selection covering most of the file is a full read with extra steps. Return the
        // outline instead and let the caller narrow it.
        let span: usize = merge_blocks(&matched, src_lines.len())
            .iter()
            .map(|b| b.end - b.start)
            .sum();
        if line_count > 0 && span * 100 / line_count > 60 {
            let text = format!(
                "# recall_file: those names cover {}% of {path} — returning the outline instead.\n\
                 # Ask for fewer names, or use smart_read(mode=\"full\") to read it whole.\n\n{}",
                span * 100 / line_count,
                format_outline_compact(path, line_count, &matched)
            );
            return metered_guarded(
                text,
                full_tokens,
                "mcp__lumen__recall_file",
                "recall_file",
                path,
                Some(line_count as i64),
                Some(req_key.clone()),
                Inflate::Guard { fallback: &src },
            );
        }

        format!(
            "{fuzzy_note}{}",
            format_items_excerpt(path, &src_lines, &matched)
        )
    } else if let (Some(start), Some(end)) = (start_line, end_line) {
        // Explicit line range
        let start0 = start.saturating_sub(1);
        let end0 = end.min(line_count);
        let ctx_start = start0.saturating_sub(3);
        let ctx_end = (end0 + 3).min(line_count);

        let mut buf = format!("# {path} — L{start}-{end} (+3 lines context)\n\n");
        for (i, &line) in src_lines[ctx_start..ctx_end].iter().enumerate() {
            buf.push_str(&gutter_line(ctx_start + i + 1, line));
        }
        buf
    } else {
        // No selector. Returning the file plus a header was guaranteed to cost more than the
        // file — the one branch that could never save anything. Return the outline instead,
        // which is what the caller needs in order to ask a real question. Not an error: an
        // error costs a round and delivers nothing.
        let lang = detect_lang(path);
        let items = outline(&src, lang);
        let refs: Vec<&CodeItem> = items.iter().collect();
        format!(
            "# recall_file: no selector given, so here is the outline.\n\
             # Call again with names=[...] or start_line/end_line, or use \
             smart_read(mode=\"full\") for the whole file.\n\n{}",
            format_outline_compact(path, line_count, &refs)
        )
    };

    metered_guarded(
        text,
        full_tokens,
        "mcp__lumen__recall_file",
        "recall_file",
        path,
        Some(line_count as i64),
        Some(req_key.clone()),
        // Guarded on every branch: a range spanning the file, or an outline of a file with
        // hundreds of tiny declarations, can both exceed the file itself.
        Inflate::Guard { fallback: &src },
    )
}

/// One emitted source line, with its number.
///
/// `{lineno:>5}: ` cost about three tokens of gutter per line — the padding run, the digits, the
/// colon and the space — and applied across most of a file that was the entire mechanism behind
/// the recorded overshoot (mean 2.85 tokens per line of file). `{n}|` drops the padding and the
/// trailing space. Set `LUMEN_LINE_NUMBERS=0` to drop the gutter altogether; the block headers
/// still carry the line ranges, which is how the ranked renderer has always worked.
fn gutter_line(lineno: usize, line: &str) -> String {
    if std::env::var("LUMEN_LINE_NUMBERS").as_deref() == Ok("0") {
        format!("{line}\n")
    } else {
        format!("{lineno}|{line}\n")
    }
}

/// A contiguous run of source lines, and the items that asked for it.
#[derive(Debug, PartialEq)]
pub struct Block {
    pub start: usize,
    pub end: usize,
    pub labels: Vec<String>,
}

/// Merge matched items into non-overlapping blocks, context included.
///
/// Nested and adjacent items previously each emitted their own context, body and three markdown
/// headers, so shared lines went out twice — re-numbered each time. A method inside a matched
/// struct duplicated the struct's own lines.
pub fn merge_blocks(items: &[&CodeItem], total_lines: usize) -> Vec<Block> {
    let mut spans: Vec<(usize, usize, String)> = items
        .iter()
        .map(|i| {
            let start = i.start_line.saturating_sub(1).saturating_sub(CTX_LINES);
            let end = (i.end_line + CTX_LINES).min(total_lines);
            let label = format!(
                "{} {} [L{}-{}]",
                i.kind,
                i.name.as_deref().unwrap_or("(anonymous)"),
                i.start_line,
                i.end_line
            );
            (start, end, label)
        })
        .collect();
    spans.sort_by_key(|(s, e, _)| (*s, *e));

    let mut out: Vec<Block> = Vec::new();
    for (start, end, label) in spans {
        match out.last_mut() {
            // `<=  end + 1` merges touching blocks too: a one-line gap between two runs costs
            // more as a second header than as the line itself.
            Some(prev) if start <= prev.end + 1 => {
                prev.end = prev.end.max(end);
                prev.labels.push(label);
            }
            _ => out.push(Block {
                start,
                end,
                labels: vec![label],
            }),
        }
    }
    out
}

const CTX_LINES: usize = 3;

pub fn format_items_excerpt(path: &str, src_lines: &[&str], items: &[&CodeItem]) -> String {
    let names: Vec<String> = items
        .iter()
        .map(|i| i.name.as_deref().unwrap_or("(anonymous)").to_string())
        .collect();

    // Header kept verbatim — callers and tests read the "N item(s): names" line.
    let mut buf = format!("# {path} — {} item(s): {}\n", items.len(), names.join(", "));

    // One block per contiguous run, one header, and every line exactly once. The old shape
    // emitted `## kind name`, `### context`, `### body` and `### context` per item, which on
    // nested or adjacent matches meant the same lines twice with three extra headers each.
    for block in merge_blocks(items, src_lines.len()) {
        buf.push('\n');
        buf.push_str(&format!("## {}\n", block.labels.join(" + ")));
        for (i, &line) in src_lines[block.start..block.end].iter().enumerate() {
            buf.push_str(&gutter_line(block.start + i + 1, line));
        }
    }

    buf
}

/// An outline with no trailing usage examples.
///
/// `format_outline` ends with two example lines that repeat the absolute path twice more. That
/// is right for `smart_read`, where the caller is deciding what to ask for next. It is pure cost
/// when an outline is returned as a *fallback* from a call the caller has already made and whose
/// shape they already know.
pub fn format_outline_compact(path: &str, line_count: usize, items: &[&CodeItem]) -> String {
    let mut buf = format!(
        "# {path} — outline ({line_count} lines, {} items)\n",
        items.len()
    );
    for (n, item) in items.iter().enumerate() {
        buf.push_str(&format!(
            "{:>3}. {:<14} {:<32} L{}-{}\n",
            n + 1,
            item.kind,
            item.name.as_deref().unwrap_or("(anonymous)"),
            item.start_line,
            item.end_line
        ));
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
        saved = result.original_tokens as i64 - result.compressed_tokens as i64,
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
        None,
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
    //
    // Bodies are substantial on purpose. With one-line bodies this file was 14 lines, and every
    // reply about it legitimately cost more than reading it whole — so the inflation guard fired
    // and twelve behavioural tests started asserting the guard instead of the behaviour they
    // were written for. A fixture has to be shaped like the input the tool is for.
    fn rust_src() -> String {
        let mut s = String::from("use std::io;\n\n");
        s.push_str("fn alpha(x: i32) -> i32 {\n");
        for j in 0..18 {
            s.push_str(&format!(
                "    let a{j} = x.wrapping_mul({j}).wrapping_add(1);\n"
            ));
        }
        s.push_str("    x + 1\n}\n\n");
        for _ in 0..6 {
            s.push_str("// filler\n");
        }
        s.push('\n');
        s.push_str("fn beta() {\n");
        s.push_str("    println!(\"BETA_BODY_MARKER\");\n");
        for j in 0..18 {
            s.push_str(&format!("    let b{j} = {j} * 3 + 1;\n"));
        }
        s.push_str("}\n\n");
        // Six more items so that recalling alpha+beta is a minority of the file. With only two
        // functions, asking for both *is* a full read, and the >60%-coverage guard correctly
        // returned the outline — which made a test about fetching two names assert the guard.
        for k in 0..6 {
            s.push_str(&format!("fn spare_{k}(v: usize) -> usize {{\n"));
            for j in 0..18 {
                s.push_str(&format!("    let s{j} = v.wrapping_add({j});\n"));
            }
            s.push_str("    v\n}\n\n");
        }
        s
    }

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

    /// i64, not u64: `saved_tokens` can now be negative, and `as_u64()` returns
    /// None for a negative — so this helper was itself enforcing the clamp, and
    /// every test reading through it failed with "numeric meta field" rather than
    /// with a value mismatch. The clamp assumption was encoded in four places:
    /// two write sites, the assertions, and here.
    fn meta(outcome: &Outcome, key: &str) -> i64 {
        outcome.result().expect("ok result")["_meta"][key]
            .as_i64()
            .expect("numeric meta field")
    }

    // ── request_key ──────────────────────────────────────────────────────────
    //
    // This is the column that will discount the dedup ceiling to something real. The
    // measured 969,799-token ceiling keys on (route, path), so two recall_file calls
    // for *different items* in one file still count as a repeat. A key that includes
    // the selector is what separates a genuine re-read from a different question.

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_same_request_always_produces_the_same_key() {
        let a = request_key("/x.rs", &names(&["alpha", "beta"]), None, None);
        let b = request_key("/x.rs", &names(&["alpha", "beta"]), None, None);
        assert_eq!(a, b);
    }

    #[test]
    fn argument_order_does_not_change_the_key() {
        // The model may list names in any order; the same question must key alike.
        assert_eq!(
            request_key("/x.rs", &names(&["beta", "alpha"]), None, None),
            request_key("/x.rs", &names(&["alpha", "beta"]), None, None),
        );
    }

    #[test]
    fn different_names_produce_different_keys() {
        assert_ne!(
            request_key("/x.rs", &names(&["alpha"]), None, None),
            request_key("/x.rs", &names(&["beta"]), None, None),
            "asking for a different item is a different request, not a repeat"
        );
    }

    #[test]
    fn a_subset_is_not_the_same_request_as_a_superset() {
        assert_ne!(
            request_key("/x.rs", &names(&["alpha"]), None, None),
            request_key("/x.rs", &names(&["alpha", "beta"]), None, None),
        );
    }

    #[test]
    fn different_line_ranges_produce_different_keys() {
        assert_ne!(
            request_key("/x.rs", &[], Some(1), Some(50)),
            request_key("/x.rs", &[], Some(51), Some(100)),
        );
    }

    #[test]
    fn a_whole_file_request_keys_on_the_path_alone() {
        // No selector means one canonical request per file, so smart_read and an
        // unqualified recall_file agree — which is what makes them comparable.
        assert_eq!(request_key("/x.rs", &[], None, None), "/x.rs");
    }

    #[test]
    fn a_name_request_is_never_confused_with_the_whole_file() {
        assert_ne!(
            request_key("/x.rs", &names(&["alpha"]), None, None),
            request_key("/x.rs", &[], None, None),
        );
    }

    #[test]
    fn different_paths_never_share_a_key() {
        assert_ne!(
            request_key("/a.rs", &names(&["f"]), None, None),
            request_key("/b.rs", &names(&["f"]), None, None),
        );
    }

    #[test]
    fn a_names_request_and_a_range_request_are_distinct() {
        assert_ne!(
            request_key("/x.rs", &names(&["alpha"]), None, None),
            request_key("/x.rs", &[], Some(1), Some(9)),
        );
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
        // Inverted: the whole point of E7's clamp fix is that a read returning more
        // than the file contained reports a NEGATIVE saving instead of a flattering
        // zero. If this ever reads 0 again, the clamp is back.
        assert!(
            v["_meta"]["saved_tokens"].as_i64().unwrap() < 0,
            "returning more than the file contained is a loss, not a zero: {}",
            v["_meta"]["saved_tokens"]
        );
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
        let (_d, path) = fixture("src.rs", &rust_src());
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
    fn smart_read_reports_a_loss_rather_than_a_flattering_zero() {
        // A file that is almost entirely declarations produces an outline as long as
        // the source. That is a real loss and must be reported as one. Reporting 0
        // was the old behaviour: defensible against a wrapped huge number, but it
        // made 170 real losses indistinguishable from "no saving" and inflated every
        // average built on the column.
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
        let saved = meta(&out, "saved_tokens");
        assert_eq!(
            saved,
            full - returned,
            "the loss must be the exact signed difference"
        );
        assert!(
            saved < 0,
            "a bigger outline than the file is a loss: {saved}"
        );
    }

    #[test]
    fn smart_read_full_mode_returns_the_whole_body() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_smart_read(&json!({ "path": &path, "mode": "full" }));
        let text = text_of(&out);
        assert!(
            text.contains("(full)"),
            "header must mark full mode: {text}"
        );
        assert!(text.contains("BETA_BODY_MARKER"), "body must be present");
    }

    #[test]
    fn full_mode_is_routed_separately_so_it_never_pools_with_outline_savings() {
        // Handing over a whole file is a delivery, not an optimisation. Under one shared route,
        // a call that can only ever lose tokens sat inside the savings headline — 47 of the 84
        // recorded smart_read overages were this path and nothing else.
        let (_d, path) = fixture("src.rs", &rust_src());
        let m = tool_smart_read(&json!({ "path": &path, "mode": "full" }))
            .meter
            .expect("full mode still meters");
        assert_eq!(m.routed_via, "smart_read_full");
        assert!(
            !lumen_stats::LUMEN_ROUTES.contains(&m.routed_via.as_str()),
            "smart_read_full must be excluded from the savings routes"
        );
        // The overage is the one-line header and nothing more.
        assert!(
            m.returned_tokens - m.full_tokens < 40,
            "the full-mode header should cost a handful of tokens, not {}",
            m.returned_tokens - m.full_tokens
        );
    }

    #[test]
    fn smart_read_unknown_mode_falls_back_to_outline() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_smart_read(&json!({ "path": &path, "mode": "sideways" }));
        assert!(text_of(&out).contains("— outline"));
    }

    #[test]
    fn smart_read_meters_the_path_and_line_count() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_smart_read(&json!({ "path": &path }));
        let m = out.meter.expect("smart_read must meter");
        assert_eq!(m.path, path);
        assert_eq!(m.tool_name, "mcp__lumen__smart_read");
        assert_eq!(m.routed_via, "smart_read");
        assert_eq!(m.lines, Some(rust_src().lines().count() as i64));
        // Saturating: a tiny file's outline can exceed the file, and the metered
        // saving must then be 0 rather than negative.
        // Inverted: exact signed difference, no floor.
        assert_eq!(
            m.saved_tokens,
            m.full_tokens - m.returned_tokens,
            "the metered saving must be the exact signed difference"
        );
    }

    #[test]
    fn smart_read_handles_an_empty_file() {
        let (_d, path) = fixture("empty.rs", "");
        let out = tool_smart_read(&json!({ "path": &path }));
        assert_eq!(meta(&out, "full_tokens"), 0);
        // An empty file still gets an outline header, so the call costs a few tokens
        // and returns nothing useful. Under the clamp that read as a clean 0; the
        // truth is a small loss, and the whole point of E7 is to stop rounding
        // losses towards the pleasant answer.
        let saved = meta(&out, "saved_tokens");
        assert_eq!(saved, 0 - meta(&out, "returned_tokens"));
        assert!(saved <= 0, "an empty file cannot yield a saving: {saved}");
    }

    #[test]
    fn an_unparseable_language_returns_the_file_rather_than_a_fake_saving() {
        // `outline` does not fail on a language it cannot parse — it returns one synthetic
        // whole-file item. Formatted as an outline that is ~40 tokens describing nothing, and it
        // was metered as a ~95% saving of a file the tool never looked inside. A false saving is
        // worse than a miss, because nothing downstream can tell it from a real one.
        let (_d, path) = fixture("notes.xyz", &"some free text\n".repeat(200));
        let out = tool_smart_read(&json!({ "path": &path }));
        let text = text_of(&out);
        assert!(
            text.contains("no structure could be extracted"),
            "got: {text}"
        );
        assert!(
            text.contains("some free text"),
            "the file itself must come back"
        );
        let m = out.meter.expect("still meters");
        assert_eq!(m.routed_via, "ranked_no_defs");
        assert!(
            m.saved_tokens < 0,
            "it cost more than a plain read, so it is a loss: {}",
            m.saved_tokens
        );
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
        let (_d, path) = fixture("src.rs", &rust_src());
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
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["ALPHA"] }));
        assert!(text_of(&out).contains("alpha"));
    }

    #[test]
    fn recall_file_can_fetch_several_names_at_once() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["alpha", "beta"] }));
        let text = text_of(&out);
        assert!(text.contains("2 item(s)"), "got: {text}");
        assert!(text.contains("x + 1") && text.contains("BETA_BODY_MARKER"));
    }

    #[test]
    fn recall_file_unmatched_name_falls_back_to_the_outline() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["gamma"] }));
        let text = text_of(&out);
        assert!(text.contains("nothing matched"), "got: {text}");
        assert!(text.contains("Available items:"));
        assert!(text.contains("alpha"), "the outline must still be offered");
        assert!(
            out.meter.is_some(),
            "a fallback still returned content, so it still meters"
        );
    }

    #[test]
    fn recall_file_by_line_range_includes_three_lines_of_context() {
        // 400 lines with real content, not 20 tiny ones: a range read of a 20-line file costs
        // more than the file, so the inflation guard fires and the context window never gets
        // exercised. The range branch only means anything on a file worth not reading whole.
        let numbered: String = (1..=400)
            .map(|i| format!("line{i} // padding to make this file worth a partial read\n"))
            .collect();
        let (_d, path) = fixture("nums.txt", &numbered);
        let out = tool_recall_file(&json!({ "path": &path, "start_line": 10, "end_line": 12 }));
        let text = text_of(&out);
        assert!(text.contains("L10-12 (+3 lines context)"), "got: {text}");
        // Anchored on the gutter: a bare "line6" also matches line60..line69 in a 400-line
        // file, so the negative assertions would pass for the wrong reason.
        assert!(
            text.contains("7|line7 "),
            "3 lines of leading context: {text}"
        );
        assert!(!text.contains("6|line6 "), "but not a 4th: {text}");
        assert!(
            text.contains("15|line15 "),
            "3 lines of trailing context: {text}"
        );
        assert!(!text.contains("16|line16 "), "but not a 4th: {text}");
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
    fn recall_file_with_no_selector_returns_the_outline_not_the_file() {
        // Returning the file plus a header made this the one branch that could never save
        // anything — guaranteed inflation, every single call. The outline is what a caller
        // without a selector actually needs, and it is not an error, because an error costs a
        // round and delivers nothing.
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path }));
        let text = text_of(&out);
        assert!(text.contains("no selector given"), "got: {text}");
        assert!(
            text.contains("alpha") && text.contains("beta"),
            "outline must name both: {text}"
        );
        assert!(
            !text.contains("BETA_BODY_MARKER"),
            "bodies must NOT come back: {text}"
        );
        let m = out.meter.expect("meters");
        assert!(
            m.returned_tokens < m.full_tokens,
            "the outline must cost less than the file ({} vs {})",
            m.returned_tokens,
            m.full_tokens
        );
    }

    #[test]
    fn recall_file_names_take_precedence_over_a_range() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(
            &json!({ "path": &path, "names": ["alpha"], "start_line": 1, "end_line": 2 }),
        );
        assert!(text_of(&out).contains("1 item(s): alpha"));
    }

    #[test]
    fn recall_file_meters_with_its_own_tool_name() {
        let (_d, path) = fixture("src.rs", &rust_src());
        let m = tool_recall_file(&json!({ "path": &path, "names": ["alpha"] }))
            .meter
            .expect("recall_file must meter");
        assert_eq!(m.tool_name, "mcp__lumen__recall_file");
        assert_eq!(m.routed_via, "recall_file");
    }

    #[test]
    fn a_single_letter_name_no_longer_selects_the_whole_file() {
        // `nl == q || nl.contains(q)` while the description promised exact matching. `["e"]`
        // matched nearly every item, and the reply came back larger than the file — the
        // mechanism behind 96% of the 92,347 recorded overshoot tokens.
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["a"] }));
        let m = out.meter.expect("meters");
        assert!(
            m.returned_tokens <= m.full_tokens,
            "a one-letter query must never cost more than the file ({} vs {})",
            m.returned_tokens,
            m.full_tokens
        );
    }

    #[test]
    fn an_exact_name_wins_over_substring_matches() {
        // `beta` is also a substring of nothing here, but `spare_0` vs `spare_1`… would be:
        // exact-first is what stops one precise ask fanning out into six bodies.
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["spare_0"] }));
        let text = text_of(&out);
        assert!(text.contains("1 item(s): spare_0"), "got: {text}");
        // One block, one header. `fn spare_1` does appear — it is inside spare_0's trailing
        // three lines of context, which is the documented behaviour — so the claim to test is
        // that only one item was *selected*, not that a sibling name is absent.
        assert_eq!(
            text.matches("\n## ").count(),
            1,
            "exactly one block should be emitted: {text}"
        );
        let m = out.meter.expect("meters");
        assert!(
            m.returned_tokens * 3 < m.full_tokens,
            "one of eight items should be a small fraction of the file ({} vs {})",
            m.returned_tokens,
            m.full_tokens
        );
    }

    #[test]
    fn a_broad_substring_query_returns_the_map_not_the_territory() {
        // "spare" substring-matches six items. Returning six bodies would be most of the file,
        // so the outline comes back with an instruction instead.
        let (_d, path) = fixture("src.rs", &rust_src());
        let out = tool_recall_file(&json!({ "path": &path, "names": ["spare"] }));
        let text = text_of(&out);
        assert!(text.contains("too many to return bodies"), "got: {text}");
        assert!(!text.contains("wrapping_add"), "no bodies: {text}");
        let m = out.meter.expect("meters");
        assert!(
            m.returned_tokens < m.full_tokens,
            "and it must still be cheaper than the file"
        );
    }

    #[test]
    fn a_narrow_substring_match_is_returned_but_labelled_as_inexact() {
        // Sloppy queries are genuinely useful and real callers rely on them; they just must not
        // be able to select the whole file. When they are honoured, the reply says so.
        let (_d, path) = fixture("src.rs", &rust_src());
        let text = text_of(&tool_recall_file(
            &json!({ "path": &path, "names": ["lph"] }),
        ));
        assert!(
            text.contains("matched by substring, not exact"),
            "got: {text}"
        );
        assert!(text.contains("alpha"), "got: {text}");
    }

    #[test]
    fn an_empty_name_is_rejected_rather_than_matching_everything() {
        // `names: [""]` matched every named item under the old rule.
        let (_d, path) = fixture("src.rs", &rust_src());
        for names in [json!([""]), json!(["   "]), json!([])] {
            let out = tool_recall_file(&json!({ "path": &path, "names": names }));
            assert_eq!(out.error_code(), Some(INVALID_PARAMS), "for {names:?}");
        }
    }

    #[test]
    fn names_covering_most_of_the_file_return_the_outline_instead() {
        let (_d, path) = fixture("small.rs", &realistic_rust(2, 30));
        let out =
            tool_recall_file(&json!({ "path": &path, "names": ["operation_0", "operation_1"] }));
        let text = text_of(&out);
        assert!(
            text.contains("returning the outline instead"),
            "got: {text}"
        );
        let m = out.meter.expect("meters");
        assert!(m.returned_tokens < m.full_tokens);
    }

    #[test]
    fn overlapping_items_emit_their_shared_lines_once() {
        // A nested item used to re-emit its parent's lines, re-numbered, under three more
        // markdown headers.
        let items = [
            CodeItem {
                kind: "struct".into(),
                name: Some("Outer".into()),
                start_line: 10,
                end_line: 40,
                start_byte: 0,
                end_byte: 0,
            },
            CodeItem {
                kind: "fn".into(),
                name: Some("inner".into()),
                start_line: 15,
                end_line: 20,
                start_byte: 0,
                end_byte: 0,
            },
        ];
        let refs: Vec<&CodeItem> = items.iter().collect();
        let blocks = merge_blocks(&refs, 100);
        assert_eq!(blocks.len(), 1, "nested items are one block: {blocks:?}");
        assert_eq!(blocks[0].labels.len(), 2, "both are labelled");
    }

    #[test]
    fn items_separated_by_more_than_the_context_window_stay_separate() {
        let items = [
            CodeItem {
                kind: "fn".into(),
                name: Some("a".into()),
                start_line: 1,
                end_line: 5,
                start_byte: 0,
                end_byte: 0,
            },
            CodeItem {
                kind: "fn".into(),
                name: Some("b".into()),
                start_line: 90,
                end_line: 95,
                start_byte: 0,
                end_byte: 0,
            },
        ];
        let refs: Vec<&CodeItem> = items.iter().collect();
        assert_eq!(merge_blocks(&refs, 100).len(), 2);
    }

    #[test]
    fn a_range_spanning_the_whole_file_cannot_cost_more_than_the_file() {
        let (_d, path) = fixture("big.rs", &realistic_rust(8, 40));
        let out = tool_recall_file(&json!({ "path": &path, "start_line": 1, "end_line": 99999 }));
        let m = out.meter.expect("meters");
        assert!(
            m.returned_tokens <= m.full_tokens + NOTE_ALLOWANCE,
            "asking for everything must fall back to the file, not exceed it ({} vs {})",
            m.returned_tokens,
            m.full_tokens
        );
        assert_eq!(m.routed_via, "would_inflate");
    }

    #[test]
    fn every_guarded_path_stays_within_the_file_on_a_realistic_corpus_shape() {
        // The assertion that makes the class impossible rather than merely absent: for each
        // shape a caller can ask for, the reply is never more expensive than reading the file.
        for (label, body) in [
            ("few big items", realistic_rust(8, 40)),
            (
                "many tiny items",
                (0..80).map(|i| format!("fn f{i}() {{}}\n")).collect(),
            ),
            ("one item", realistic_rust(1, 200)),
        ] {
            let (_d, path) = fixture("c.rs", &body);
            let outs = [
                tool_smart_read(&json!({ "path": &path })),
                tool_recall_file(&json!({ "path": &path })),
                tool_recall_file(&json!({ "path": &path, "start_line": 1, "end_line": 99999 })),
            ];
            for out in outs {
                let m = out.meter.expect("meters");
                assert!(
                    m.returned_tokens <= m.full_tokens + NOTE_ALLOWANCE,
                    "[{label}] {} returned {} against {} for the file",
                    m.routed_via,
                    m.returned_tokens,
                    m.full_tokens
                );
            }
        }
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
