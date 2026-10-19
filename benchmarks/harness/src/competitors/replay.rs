//! C2: Layer 1 replay. The same recorded bytes go through each arm the way that arm really hooks
//! into Claude Code, and what the model would receive is measured.
//!
//! `lumen-bench competitors replay` takes every item of the C1 corpus (`corpus/manifest.toml`) and,
//! for each arm, records the bytes that would reach the model, their cl100k count with the
//! calibration applied, whether every needle of the item survived byte for byte, and the wall time.
//!
//! **What an item is, to Claude Code.** The corpus holds output, not the call that made it, so
//! each item is given the tool call a model would have made ([`call_for`]): a file read is a
//! `Read`, a `git grep` result is a `Grep`, a shell listing, a test log, a build log and a
//! downloaded JSON document are `Bash` commands. The table is in the function; it is the same for
//! every arm. Claude Code's own wrapping of a result (the `cat -n` line numbers of a `Read`, the
//! `Exit code N` line of a failed `Bash`) is not added for any arm: the same text goes in at the
//! top of every path, so what differs between arms is what the arm did.
//!
//! **Arms**, each driven through its real mechanism ([`arms`]):
//! - `none`: the raw bytes.
//! - `lumen-1.9.0`: the release asset's `lumen-mcp`. A `Read` goes through the real
//!   `lumen-mcp hook intercept` (a file of 300 lines or more of a source kind is blocked), then
//!   `smart_read`, then a `recall_file` for each needle the outline left out. A log goes to
//!   `compress_logs`. Output Lumen has no tool for passes through.
//! - `squeez`, `rtk`: installed by their own installers into a scratch HOME; the hooks the
//!   installer wrote into `settings.json` are run with the JSON Claude Code sends
//!   ([`hooks`], [`payload`]). A Bash command's replacement runs with PATH shims that print the
//!   recorded stdout and exit with the recorded status ([`shim`]).
//! - `headroom`: its proxy in front of a local echo upstream ([`echo`]); the compressed
//!   `tool_result` in the request body the proxy forwards is the output.
//! - `caveman`: `n/a`, see [`CAVEMAN_NA`].
//!
//! Nothing here uses the network except the explicit `--prefetch` step, which downloads each
//! pinned asset and checks its sha256 first. Every installed tool lives in the cache directory or
//! in a per-case scratch HOME, never in the repository, never in the real `~/.claude`.

pub mod arms;
pub mod echo;
pub mod hooks;
pub mod install;
pub mod shim;

use crate::competitors::record::{Item, Manifest};
use crate::sandbox::RealLedgerGuard;
use crate::util::{Ctx, Res, sha256_hex};
use lumen_core::calibration::{self, ContentType, Estimate, Model, Text};
use lumen_core::tokenizer::count_tokens;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What a Caveman row says, and why.
pub const CAVEMAN_NA: &str =
    "n/a — changes the model's replies, not tool output (measured in Layer 2)";

/// The long form for METHOD.md and the totals footer: what was checked about Caveman 4.0.1 and
/// why Layer 1 does not replay its input-side proxy.
pub const CAVEMAN_NOTE: &str = "Caveman's output-style skill changes the model's replies, so it has no tool output to \
    compress. Its 4.0.x input-side proxy (caveman-proxy, installed by `cave setup --install`) does \
    compress tool output, but `cave setup --json` says that without the agent-side recovery MCP \
    server (caveman-mcp) streaming requests pass through uncompressed, and Claude Code always \
    streams. A faithful replay needs a Claude Code session that registers that server, which is \
    Layer 2; Layer 1 does not guess.";

// ---------------------------------------------------------------------------------- cases

/// The tool call a model would have made to get an item's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    /// `Read` of a file; `rel` is its path in the repository it came from.
    Read {
        rel: String,
    },
    Bash {
        command: String,
    },
    Grep {
        pattern: String,
    },
}

impl ToolCall {
    /// The tool's name as Claude Code reports it to a hook.
    pub fn tool(&self) -> &'static str {
        match self {
            ToolCall::Read { .. } => "Read",
            ToolCall::Bash { .. } => "Bash",
            ToolCall::Grep { .. } => "Grep",
        }
    }
}

/// One corpus item, ready to replay.
#[derive(Debug, Clone)]
pub struct Case {
    pub id: String,
    pub category: String,
    pub raw: Vec<u8>,
    pub call: ToolCall,
    /// The exit status the command that printed this would have had (a `Bash` call only).
    pub exit_code: i32,
    pub needles: Vec<String>,
    pub content_type: ContentType,
}

impl Case {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }
    /// A file name for the item on disk, with the extension of the file it came from.
    pub fn file_name(&self) -> String {
        match &self.call {
            ToolCall::Read { rel } => rel.rsplit('/').next().unwrap_or(rel).to_string(),
            _ => format!("{}.log", self.id),
        }
    }
    /// The path the item has under a working directory.
    pub fn rel_path(&self) -> String {
        match &self.call {
            ToolCall::Read { rel } => rel.clone(),
            _ => format!("{}.log", self.id),
        }
    }
}

/// `<40 hex>:<path>` in a recipe: the blob a file read came from.
fn blob_path(recipe: &str) -> Option<String> {
    recipe.split_whitespace().find_map(|t| {
        let (sha, path) = t.split_once(':')?;
        (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| path.trim_end_matches(',').to_string())
    })
}

/// The tool call and exit status of an item. The same for every arm.
///
/// | category | call |
/// |---|---|
/// | file read, JSON file of a repository | `Read` of the file |
/// | grep (`git grep`) | `Grep` with the pattern of the recorded search |
/// | shell (`git ls-tree`, `git show --stat`) | `Bash` with the recorded command at `HEAD` |
/// | JSON document downloaded | `Bash` `curl -s <url>` |
/// | test log | `Bash` `cargo test` or `pytest` |
/// | build log | `Bash` `cargo build` or `tsc --noEmit` |
///
/// A build log is a list of errors, so its command failed: 101 for cargo, 2 for tsc. A test log
/// exits 101 (cargo) or 1 (pytest) when it reports a failure, else 0. Everything else exits 0.
pub fn call_for(item: &Item, text: &str) -> Res<(ToolCall, i32)> {
    let bad = |why: &str| Err(format!("{}: {why} (recipe {:?})", item.id, item.recipe));
    Ok(match item.category.as_str() {
        "file_read" => match blob_path(&item.recipe) {
            Some(rel) => (ToolCall::Read { rel }, 0),
            None => return bad("no blob path in the recipe"),
        },
        "json" => match blob_path(&item.recipe) {
            Some(rel) => (ToolCall::Read { rel }, 0),
            None => (
                ToolCall::Bash {
                    command: format!("curl -s {}", item.source),
                },
                0,
            ),
        },
        "shell" => {
            if !item.recipe.starts_with("git ") || !item.recipe.contains("<commit>") {
                return bad("not a recorded git command");
            }
            (
                ToolCall::Bash {
                    command: item.recipe.replace("<commit>", "HEAD"),
                },
                0,
            )
        }
        "grep" => {
            let r = &item.recipe;
            let (Some(a), Some(b)) = (r.find("-e "), r.find("<commit>")) else {
                return bad("no pattern in the recipe");
            };
            let p = &r[a + 3..b];
            (
                ToolCall::Grep {
                    pattern: p.strip_suffix(' ').unwrap_or(p).to_string(),
                },
                0,
            )
        }
        "test_log" => {
            let failed =
                text.contains("FAILED") || text.contains("failed") || text.contains("panicked");
            if item.id.starts_with("testlog-cargo") {
                (
                    ToolCall::Bash {
                        command: "cargo test".into(),
                    },
                    if failed { 101 } else { 0 },
                )
            } else if item.id.starts_with("testlog-pytest") {
                (
                    ToolCall::Bash {
                        command: "pytest".into(),
                    },
                    if failed { 1 } else { 0 },
                )
            } else {
                return bad("an unknown test log");
            }
        }
        "build_log" => {
            if item.id.starts_with("buildlog-rustc") {
                (
                    ToolCall::Bash {
                        command: "cargo build".into(),
                    },
                    101,
                )
            } else if item.id.starts_with("buildlog-tsc") {
                (
                    ToolCall::Bash {
                        command: "tsc --noEmit".into(),
                    },
                    2,
                )
            } else {
                return bad("an unknown build log");
            }
        }
        other => return bad(&format!("category {other:?} has no tool call")),
    })
}

fn content_type_of(item: &Item, call: &ToolCall) -> ContentType {
    match (item.category.as_str(), call) {
        (_, ToolCall::Read { rel }) => ContentType::from_path(rel),
        ("json", _) => ContentType::Json,
        ("test_log" | "build_log", _) => ContentType::Log,
        _ => ContentType::Other,
    }
}

/// Load the corpus: every item read, its sha256 checked against the manifest, its text required
/// to be UTF-8 (a proxy request is JSON), and given its tool call. `only` keeps the items whose
/// id contains one of the strings.
pub fn load_cases(dir: &Path, only: &[String]) -> Res<Vec<Case>> {
    let text =
        std::fs::read_to_string(dir.join("manifest.toml")).ctx("read the corpus manifest")?;
    let m: Manifest = toml::from_str(&text).ctx("parse the corpus manifest")?;
    let mut out = Vec::new();
    for it in &m.item {
        if !only.is_empty() && !only.iter().any(|o| it.id.contains(o.as_str())) {
            continue;
        }
        let raw = std::fs::read(dir.join(&it.file)).ctx(&format!("read {}", it.file))?;
        if sha256_hex(&raw) != it.sha256 {
            return Err(format!(
                "REFUSED: {} does not match its sha256 in the manifest",
                it.file
            ));
        }
        let t = std::str::from_utf8(&raw)
            .map_err(|e| format!("{}: not UTF-8 ({e})", it.id))?
            .to_string();
        let (call, exit_code) = call_for(it, &t)?;
        out.push(Case {
            id: it.id.clone(),
            category: it.category.clone(),
            content_type: content_type_of(it, &call),
            raw,
            call,
            exit_code,
            needles: it.needles.clone(),
        });
    }
    if out.is_empty() {
        return Err("no corpus item selected".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------- payloads

/// The JSON Claude Code gives a PreToolUse hook for `case`'s call.
///
/// Fields (Claude Code hooks reference): `session_id`, `transcript_path`, `cwd`,
/// `permission_mode`, `hook_event_name`, `tool_name`, `tool_input`, `tool_use_id`. The
/// `tool_input` shapes are the tools' own: `Read {file_path}`, `Bash {command}`,
/// `Grep {pattern, output_mode, -n}`.
pub fn pre_payload(case: &Case, file: &Path, session: &str, cwd: &Path) -> Value {
    json!({
        "session_id": session, "transcript_path": "/dev/null",
        "cwd": cwd.to_string_lossy(), "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": case.call.tool(),
        "tool_input": tool_input(&case.call, file),
        "tool_use_id": "toolu_replay"
    })
}

pub fn tool_input(call: &ToolCall, file: &Path) -> Value {
    match call {
        ToolCall::Read { .. } => json!({"file_path": file.to_string_lossy()}),
        ToolCall::Bash { command } => json!({"command": command}),
        ToolCall::Grep { pattern } => {
            json!({"pattern": pattern, "output_mode": "content", "-n": true})
        }
    }
}

/// The JSON for the PostToolUse hook: the PreToolUse fields plus `tool_response`, in the shape
/// the tool's result has. The shapes are the ones squeez's own PostToolUse hook documents and
/// parses (`hooks/posttooluse.sh` and `src/commands/compress_output.rs` at the pinned commit):
/// for `Read` `{"type":"text","file":{"filePath","content","numLines","startLine","totalLines"}}`;
/// for `Grep` `{"mode":"content","numFiles","filenames","content","numLines"}`; for `Bash`
/// `{"stdout","stderr","interrupted","isImage"}`.
pub fn post_payload(
    case: &Case,
    input: &Value,
    text: &str,
    total_lines: usize,
    session: &str,
    cwd: &Path,
) -> Value {
    let lines = text.lines().count();
    let response = match &case.call {
        ToolCall::Read { .. } => json!({"type": "text", "file": {
            "filePath": input["file_path"], "content": text, "numLines": lines,
            "startLine": 1, "totalLines": total_lines}}),
        ToolCall::Grep { .. } => json!({"mode": "content", "numFiles": 0, "filenames": [],
            "content": text, "numLines": lines}),
        ToolCall::Bash { .. } => {
            json!({"stdout": text, "stderr": "", "interrupted": false, "isImage": false})
        }
    };
    json!({
        "session_id": session, "transcript_path": "/dev/null",
        "cwd": cwd.to_string_lossy(), "permission_mode": "default",
        "hook_event_name": "PostToolUse",
        "tool_name": case.call.tool(),
        "tool_input": input, "tool_response": response,
        "tool_use_id": "toolu_replay"
    })
}

/// The text of an `updatedToolOutput`: a string, a `Read` result, a result with `content`, or a
/// list of text blocks.
pub fn output_text(v: &Value) -> Res<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Object(o) => {
            if let Some(c) = v.pointer("/file/content").and_then(Value::as_str) {
                return Ok(c.to_string());
            }
            for k in ["content", "stdout"] {
                if let Some(c) = o.get(k) {
                    return output_text(c);
                }
            }
            Err(format!("an updatedToolOutput with no text: {v}"))
        }
        Value::Array(a) => {
            let mut s = String::new();
            for b in a {
                s.push_str(b.get("text").and_then(Value::as_str).unwrap_or(""));
            }
            Ok(s)
        }
        _ => Err(format!("an updatedToolOutput of an unknown shape: {v}")),
    }
}

/// Apply the `offset` / `limit` of a hook's `updatedInput` to a `Read` result, as the Read tool
/// does: lines from `offset` (1-based), at most `limit` of them. A line keeps its own ending.
pub fn apply_read_window(text: &str, input: &Value) -> String {
    let limit = input.get("limit").and_then(Value::as_u64);
    let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
    if limit.is_none() && offset == 0 {
        return text.to_string();
    }
    let skip = offset.saturating_sub(1) as usize;
    let take = limit.map_or(usize::MAX, |l| l as usize);
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

/// Apply the `head_limit` of a hook's `updatedInput` to a `Grep` result: its first N lines.
pub fn apply_head_limit(text: &str, input: &Value) -> String {
    match input.get("head_limit").and_then(Value::as_u64) {
        Some(n) if n > 0 => text.split_inclusive('\n').take(n as usize).collect(),
        _ => text.to_string(),
    }
}

// ---------------------------------------------------------------------------------- outcomes

/// One step of what the model received (a block message, an outline, a recall).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: String,
    pub bytes: usize,
    pub tokens: u64,
}

impl Step {
    pub fn of(label: &str, bytes: &[u8]) -> Step {
        Step {
            label: label.to_string(),
            bytes: bytes.len(),
            tokens: count_tokens(&String::from_utf8_lossy(bytes)) as u64,
        }
    }
}

/// A call the model makes after the first answer, to get back something it needs.
#[derive(Debug, Clone)]
pub struct Followup {
    /// What the call was (`recall_file 907-907`, `headroom_retrieve`, `rtk recall`).
    pub label: String,
    /// What the model is handed back, as it is handed (counted in tokens).
    pub received: Vec<u8>,
    /// The content in it that a fact is looked for in. The same bytes as `received`, except where
    /// the tool wraps content in JSON (Headroom's retrieve), where it is the content decoded: the
    /// model reads through the quoting, a byte match must too.
    pub facts: Vec<u8>,
}

impl Followup {
    pub fn plain(label: &str, received: Vec<u8>) -> Followup {
        Followup {
            label: label.to_string(),
            facts: received.clone(),
            received,
        }
    }
}

/// What an arm made of an item.
#[derive(Debug, Clone)]
pub struct Received {
    /// The first answer: what the model is handed for the call, before it asks for anything for
    /// the sake of a missing fact. For Lumen that is the block message and the outline (the
    /// redirect to `smart_read` is a round, counted in `first_rounds`).
    pub bytes: Vec<u8>,
    /// Round trips the first answer cost beyond the original call.
    pub first_rounds: u32,
    pub steps: Vec<Step>,
    /// The follow-ups an ideal asker makes, one round each, for every needle the first answer
    /// lacks, through the recovery path the tool has at its documented defaults. Empty for a tool
    /// with none.
    pub followups: Vec<Followup>,
    pub exit_code: Option<i32>,
    pub notes: Vec<String>,
    /// The time of the replay itself: hooks, the command, the tool calls. Not installation.
    pub wall: Duration,
}

impl Received {
    pub fn plain(bytes: Vec<u8>, wall: Duration) -> Received {
        Received {
            steps: vec![Step::of("output", &bytes)],
            bytes,
            first_rounds: 0,
            followups: Vec::new(),
            exit_code: None,
            notes: Vec::new(),
            wall,
        }
    }

    /// Everything the model was handed, the first answer and then every follow-up.
    pub fn total_bytes(&self) -> Vec<u8> {
        let mut all = self.bytes.clone();
        for f in &self.followups {
            all.push(b'\n');
            all.extend_from_slice(&f.received);
        }
        all
    }

    /// The content facts are looked for in, first answer and follow-ups.
    pub fn total_facts(&self) -> Vec<u8> {
        let mut all = self.bytes.clone();
        for f in &self.followups {
            all.push(b'\n');
            all.extend_from_slice(&f.facts);
        }
        all
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Received(Received),
    /// A cell that cannot exist, with the reason. Never a zero.
    Na(String),
}

/// One arm of the comparison.
pub trait Arm {
    fn id(&self) -> &str;
    fn replay(&mut self, case: &Case) -> Res<Outcome>;
}

/// The measurements of one item through one arm, twice: the first answer alone, and with the
/// follow-ups an ideal asker makes.
#[derive(Debug, Clone)]
pub struct Measured {
    pub bytes: usize,
    pub needles: usize,
    pub raw_tokens: u64,
    /// cl100k tokens of the first answer.
    pub tokens: u64,
    /// Calibrated for the model: the median ratio, with the band the calibration table gives.
    pub estimate: Estimate,
    pub raw_estimate: Estimate,
    /// Every needle's fact present in the first answer.
    pub kept: bool,
    pub missing: Vec<String>,
    /// cl100k tokens of everything handed over, follow-ups included.
    pub tokens_total: u64,
    pub estimate_total: Estimate,
    pub kept_total: bool,
    pub missing_total: Vec<String>,
    /// Round trips beyond the original call: the first answer's and one per follow-up.
    pub rounds: u32,
    pub unchanged: bool,
    pub wall: Duration,
    pub exit_code: Option<i32>,
    pub steps: Vec<Step>,
    pub notes: Vec<String>,
    /// What was received (first answer, then follow-ups), kept for `--dump` and for tests.
    pub received: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Cell {
    Na(String),
    Done(Box<Measured>),
}

#[derive(Debug, Clone)]
pub struct Record {
    pub item: String,
    pub category: String,
    pub arm: String,
    pub cell: Cell,
}

/// Whitespace is not a fact: both sides are compared with every whitespace character removed.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A `git grep` needle split into the file and the rest: `path:LINE:text`, `path-LINE-text` (a
/// context line) and `path:COUNT`. A tool that groups hits under their file shows `LINE:text`
/// below `path`, which is the same fact.
pub fn grep_parts(n: &str) -> Option<(&str, &str)> {
    for (i, c) in n.char_indices() {
        if c != ':' && c != '-' {
            continue;
        }
        let rest = &n[i + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if i > 0
            && digits > 0
            && (digits == rest.len() || matches!(rest.as_bytes()[digits], b':' | b'-'))
        {
            return Some((&n[..i], rest));
        }
    }
    None
}

fn fact_in_line(hay_squashed: &str, needle: &str, grep: bool) -> bool {
    let n = squash(needle);
    if n.is_empty() || hay_squashed.contains(&n) {
        return true;
    }
    if grep && let Some((path, rest)) = grep_parts(needle) {
        let (path, rest) = (squash(path), squash(rest));
        // The text and its line number, after the file's name somewhere before it.
        return hay_squashed
            .match_indices(&rest)
            .any(|(at, _)| hay_squashed[..at].contains(&path));
    }
    false
}

/// Whether the fact a needle states is in `hay`. The rule (METHOD.md, "Needles"):
/// - whitespace is not part of a fact;
/// - a needle of several lines is a set: each line must be present, in any order;
/// - for a `grep` item, a hit grouped under its file's name still states the fact.
///
/// Anything else (a changed number, a changed word, a line that is gone) is a lost fact.
pub fn fact_present(hay: &[u8], needle: &str, category: &str) -> bool {
    let hay = squash(&String::from_utf8_lossy(hay));
    let grep = category == "grep";
    let lines: Vec<&str> = needle.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 1 {
        return lines.iter().all(|l| fact_in_line(&hay, l, grep));
    }
    fact_in_line(&hay, needle, grep)
}

/// Needles whose fact is not in `bytes`.
pub fn missing_facts(bytes: &[u8], needles: &[String], category: &str) -> Vec<String> {
    needles
        .iter()
        .filter(|n| !fact_present(bytes, n, category))
        .cloned()
        .collect()
}

pub fn measure(case: &Case, r: &Received, model: Option<Model>) -> Measured {
    let text = String::from_utf8_lossy(&r.bytes);
    let tokens = count_tokens(&text) as u64;
    let total = r.total_bytes();
    let tokens_total = count_tokens(&String::from_utf8_lossy(&total)) as u64;
    let raw_tokens = count_tokens(&case.text()) as u64;
    let unchanged = r.bytes == case.raw;
    // A reply that is the file's own text is calibrated as a file; anything an arm made from it
    // is an answer, whose ratio the calibration never measured (a wider band).
    let kind = if unchanged { Text::File } else { Text::Answer };
    let missing = missing_facts(&r.bytes, &case.needles, &case.category);
    let missing_total = missing_facts(&r.total_facts(), &case.needles, &case.category);
    let mut steps = r.steps.clone();
    steps.extend(r.followups.iter().map(|f| Step::of(&f.label, &f.received)));
    Measured {
        bytes: r.bytes.len(),
        needles: case.needles.len(),
        raw_tokens,
        tokens,
        estimate: calibration::observed(tokens, case.content_type, model, kind),
        raw_estimate: calibration::observed(raw_tokens, case.content_type, model, Text::File),
        kept: missing.is_empty(),
        missing,
        tokens_total,
        estimate_total: calibration::observed(
            tokens_total,
            case.content_type,
            model,
            if r.followups.is_empty() {
                kind
            } else {
                Text::Answer
            },
        ),
        kept_total: missing_total.is_empty(),
        missing_total,
        rounds: r.first_rounds + r.followups.len() as u32,
        unchanged,
        wall: r.wall,
        exit_code: r.exit_code,
        steps,
        notes: r.notes.clone(),
        received: total,
    }
}

// ---------------------------------------------------------------------------------- needle mix

/// What kind of fact a needle states. A tool that keeps names and signatures (an outline) keeps
/// the first kind and loses most of the others, so the mix is part of what a row means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NeedleKind {
    /// A function, class, struct or component definition line.
    Signature,
    /// An error, panic, failed-assertion or summary line.
    ErrorLine,
    /// A test's name, with or without its outcome.
    TestName,
    /// A value: a JSON field, a checksum, a size, a count, a listing row.
    Value,
    /// An import, a comment, a statement inside a body, a context line.
    Other,
}

impl NeedleKind {
    pub const ALL: [NeedleKind; 5] = [
        NeedleKind::Signature,
        NeedleKind::ErrorLine,
        NeedleKind::TestName,
        NeedleKind::Value,
        NeedleKind::Other,
    ];
    pub fn name(self) -> &'static str {
        match self {
            NeedleKind::Signature => "signature",
            NeedleKind::ErrorLine => "error line",
            NeedleKind::TestName => "test name",
            NeedleKind::Value => "value",
            NeedleKind::Other => "other",
        }
    }
}

const DEF_STARTS: &[&str] = &[
    "def ",
    "class ",
    "fn ",
    "pub fn",
    "pub(crate) fn",
    "function ",
    "async function",
    "export function",
    "export const",
    "const App",
    "struct ",
    "pub struct",
    "impl ",
    "interface ",
];

fn is_definition(t: &str) -> bool {
    DEF_STARTS.iter().any(|d| t.starts_with(d))
}

pub fn needle_kind(category: &str, needle: &str) -> NeedleKind {
    let t = needle.trim();
    let lower = t.to_ascii_lowercase();
    match category {
        "shell" | "json" => NeedleKind::Value,
        "build_log" => NeedleKind::ErrorLine,
        "test_log" => {
            if t.contains("::") || t.starts_with("test ") {
                NeedleKind::TestName
            } else {
                NeedleKind::ErrorLine
            }
        }
        "grep" => match grep_parts(t) {
            Some((_, rest)) => {
                let body = rest
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .get(1..)
                    .unwrap_or("");
                if body.is_empty() {
                    NeedleKind::Value
                } else if is_definition(body.trim()) {
                    NeedleKind::Signature
                } else {
                    NeedleKind::Other
                }
            }
            None => NeedleKind::Other,
        },
        _ => {
            let _ = lower;
            if is_definition(t) {
                NeedleKind::Signature
            } else {
                NeedleKind::Other
            }
        }
    }
}

/// Needles per category and kind.
pub fn needle_mix(cases: &[Case]) -> std::collections::BTreeMap<(String, NeedleKind), usize> {
    let mut m = std::collections::BTreeMap::new();
    for c in cases {
        for n in &c.needles {
            *m.entry((c.category.clone(), needle_kind(&c.category, n)))
                .or_insert(0) += 1;
        }
    }
    m
}

/// Replay one case through one arm. A failure of the arm stops the run (an arm that errors is not
/// a measurement); `Na` is the arm's own statement that the cell cannot exist.
pub fn run_case(arm: &mut dyn Arm, case: &Case, model: Option<Model>) -> Res<Record> {
    let outcome = arm
        .replay(case)
        .map_err(|e| format!("{} on {}: {e}", arm.id(), case.id))?;
    Ok(Record {
        item: case.id.clone(),
        category: case.category.clone(),
        arm: arm.id().to_string(),
        cell: match outcome {
            Outcome::Na(why) => Cell::Na(why),
            Outcome::Received(r) => Cell::Done(Box::new(measure(case, &r, model))),
        },
    })
}

// ---------------------------------------------------------------------------------- guards

/// `~/.claude/settings.json` and `~/.claude.json` as they were, taken read-only, to show after a
/// run that no tool changed them.
///
/// `settings.json` is where every tool registers its hooks, and it is compared whole: size, mtime
/// and sha256. `~/.claude.json` is where `claude mcp add` registers servers, but it is also the
/// file a running Claude Code rewrites all the time (startup counts, cached feature flags), so a
/// whole-file comparison fires whenever the maintainer has a session open. What a tool would change
/// in it is `mcpServers` and `hooks` (and the settings that steer a session: `env`, `permissions`,
/// `model`), and those are compared as a canonical JSON fingerprint; any other difference is
/// reported as bookkeeping by a running Claude Code, not as a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeState {
    home: PathBuf,
    settings: Option<(u64, u128, String)>,
    claude_json: Option<(u64, u128, String)>,
    /// The canonical form of the parts of `~/.claude.json` a tool would change.
    config: Option<String>,
}

/// The keys of `~/.claude.json` that a tool installing itself would write.
const CONFIG_KEYS: [&str; 5] = ["mcpServers", "hooks", "env", "permissions", "model"];

fn file_state(p: &Path) -> Res<Option<(u64, u128, String)>> {
    match std::fs::metadata(p) {
        Ok(md) => {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let bytes = std::fs::read(p).ctx(&format!("read {}", p.display()))?;
            Ok(Some((md.len(), mtime, sha256_hex(&bytes))))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat {}: {e}", p.display())),
    }
}

/// The fingerprint of the parts of a `~/.claude.json` a tool would change.
pub fn config_fingerprint(text: &str) -> Res<String> {
    let v: Value = serde_json::from_str(text).ctx("parse ~/.claude.json")?;
    let mut keep = serde_json::Map::new();
    for k in CONFIG_KEYS {
        if let Some(x) = v.get(k) {
            keep.insert(k.to_string(), x.clone());
        }
    }
    // serde_json's map is ordered by key, so equal content is an equal string.
    Ok(Value::Object(keep).to_string())
}

impl ClaudeState {
    pub fn take_at(home: &Path) -> Res<ClaudeState> {
        let cj = home.join(".claude.json");
        let config = match std::fs::read_to_string(&cj) {
            Ok(t) => Some(config_fingerprint(&t)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("read {}: {e}", cj.display())),
        };
        Ok(ClaudeState {
            home: home.to_path_buf(),
            settings: file_state(&home.join(".claude").join("settings.json"))?,
            claude_json: file_state(&cj)?,
            config,
        })
    }

    /// The home this process runs under.
    pub fn take() -> Res<ClaudeState> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|h| !h.is_empty())
            .ok_or("no HOME to take the Claude state of")?;
        Self::take_at(Path::new(&home))
    }

    /// Stop when `settings.json` changed, or when what a tool would write to `~/.claude.json`
    /// differs. `Ok(Some(note))` when `~/.claude.json` was rewritten but only in the parts a
    /// running Claude Code rewrites on its own.
    pub fn check_report(&self) -> Res<Option<String>> {
        let now = Self::take_at(&self.home)?;
        if now.settings != self.settings {
            return Err(
                "REFUSED: ~/.claude/settings.json changed during the replay; a tool wrote to the real \
                 Claude Code configuration, so nothing it measured can be trusted"
                    .to_string(),
            );
        }
        if now.config != self.config || now.claude_json.is_some() != self.claude_json.is_some() {
            return Err(
                "REFUSED: mcpServers, hooks or settings in ~/.claude.json changed during the replay; a \
                 tool wrote to the real Claude Code configuration, so nothing it measured can be trusted"
                    .to_string(),
            );
        }
        Ok((now.claude_json != self.claude_json).then(|| {
            "~/.claude.json was rewritten during the replay in its bookkeeping only (mcpServers, \
             hooks, env, permissions and model are identical): a running Claude Code does that"
                .to_string()
        }))
    }

    pub fn check(&self) -> Res<()> {
        self.check_report().map(|_| ())
    }
}

// ---------------------------------------------------------------------------------- report

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

pub const TSV_HEADER: &str = "item\tcategory\tarm\tstatus\tbytes\traw_cl100k\t\
first_cl100k\tfirst_calibrated\tfirst_cal_low\tfirst_cal_high\tneedles\tfirst_needles_kept\t\
total_cl100k\ttotal_calibrated\ttotal_cal_low\ttotal_cal_high\ttotal_needles_kept\trounds\t\
unchanged\twall_ms\texit\tsteps\tnote";

/// One TSV line per record. A cell that cannot exist says why and carries no figure.
pub fn tsv(records: &[Record]) -> String {
    let mut s = format!("{TSV_HEADER}\n");
    let cols = TSV_HEADER.split('\t').count();
    for r in records {
        match &r.cell {
            Cell::Na(why) => {
                // item, category, arm, status, then empty columns up to the note.
                let mut row = vec![
                    r.item.clone(),
                    r.category.clone(),
                    r.arm.clone(),
                    "n/a".into(),
                ];
                row.extend(std::iter::repeat_n(String::new(), cols - 5));
                row.push(why.clone());
                s.push_str(&row.join("\t"));
                s.push('\n');
            }
            Cell::Done(m) => {
                let row: Vec<String> = vec![
                    r.item.clone(),
                    r.category.clone(),
                    r.arm.clone(),
                    "done".into(),
                    m.bytes.to_string(),
                    m.raw_tokens.to_string(),
                    m.tokens.to_string(),
                    m.estimate.value.to_string(),
                    m.estimate.low.to_string(),
                    m.estimate.high.to_string(),
                    m.needles.to_string(),
                    (m.needles - m.missing.len()).to_string(),
                    m.tokens_total.to_string(),
                    m.estimate_total.value.to_string(),
                    m.estimate_total.low.to_string(),
                    m.estimate_total.high.to_string(),
                    (m.needles - m.missing_total.len()).to_string(),
                    m.rounds.to_string(),
                    m.unchanged.to_string(),
                    ms(m.wall),
                    m.exit_code.map_or(String::new(), |c| c.to_string()),
                    m.steps
                        .iter()
                        .map(|st| format!("{}={}", st.label, st.tokens))
                        .collect::<Vec<_>>()
                        .join(","),
                    m.notes.join("; ").replace(['\t', '\n'], " "),
                ];
                s.push_str(&row.join("\t"));
                s.push('\n');
            }
        }
    }
    s
}

/// Totals per arm over the items it has a figure for, in two columns: the first answer alone, and
/// with the follow-ups (their tokens included; `rounds` is the extra round trips). Signed: a tool
/// that sent more than the raw output shows a negative saving. Needles are counted as facts
/// present out of facts asked for.
pub fn totals(records: &[Record]) -> String {
    let mut arms: Vec<&str> = Vec::new();
    for r in records {
        if !arms.contains(&r.arm.as_str()) {
            arms.push(&r.arm);
        }
    }
    let mut s = String::from(
        "arm\titems\tn/a\traw_cl100k\t\
         first_cl100k\tfirst_saved_pct\tfirst_calibrated\tfirst_needles\t\
         total_cl100k\ttotal_saved_pct\ttotal_calibrated\ttotal_needles\trounds\twall_ms\n",
    );
    for a in arms {
        let mine: Vec<&Record> = records.iter().filter(|r| r.arm == a).collect();
        let na = mine
            .iter()
            .filter(|r| matches!(r.cell, Cell::Na(_)))
            .count();
        let done: Vec<&Measured> = mine
            .iter()
            .filter_map(|r| match &r.cell {
                Cell::Done(m) => Some(&**m),
                Cell::Na(_) => None,
            })
            .collect();
        if done.is_empty() {
            s.push_str(&format!("{a}\t0\t{na}\t\t\t\t\t\t\t\t\t\t\t\n"));
            continue;
        }
        let raw: u64 = done.iter().map(|m| m.raw_tokens).sum();
        let first: u64 = done.iter().map(|m| m.tokens).sum();
        let total: u64 = done.iter().map(|m| m.tokens_total).sum();
        let cal1: u64 = done.iter().map(|m| m.estimate.value).sum();
        let cal2: u64 = done.iter().map(|m| m.estimate_total.value).sum();
        let asked: usize = done.iter().map(|m| m.needles).sum();
        let k1: usize = done.iter().map(|m| m.needles - m.missing.len()).sum();
        let k2: usize = done.iter().map(|m| m.needles - m.missing_total.len()).sum();
        let rounds: u32 = done.iter().map(|m| m.rounds).sum();
        let wall: Duration = done.iter().map(|m| m.wall).sum();
        let pct = |got: u64| 100.0 * (raw as f64 - got as f64) / raw.max(1) as f64;
        s.push_str(&format!(
            "{a}\t{}\t{na}\t{raw}\t{first}\t{:.1}\t{cal1}\t{k1}/{asked}\t{total}\t{:.1}\t{cal2}\t{k2}/{asked}\t{rounds}\t{}\n",
            done.len(),
            pct(first),
            pct(total),
            ms(wall),
        ));
    }
    s
}

// ---------------------------------------------------------------------------------- command line

pub const HELP: &str = "\
usage: benchmarks/run.sh competitors replay [options]

  Layer 1: each corpus item goes through each arm the way that arm hooks into Claude Code, and
  the bytes the model would receive are counted (cl100k, calibrated), checked for needles and
  timed. No model and no API: Headroom's proxy forwards to a local echo upstream.

  --corpus DIR   the C1 corpus (default: benchmarks/competitors/corpus)
  --cache DIR    assets, installs and scratch (default: <system temp>/lumen-bench-cache); never
                 inside the repository
  --arms LIST    comma-separated: none,lumen,squeez,rtk,headroom,caveman (default: all)
  --items LIST   only items whose id contains one of these (comma-separated)
  --dump DIR     also write what each arm delivered, one file per item and arm, for reading
  --out FILE     write the per-item TSV here (default: stdout). Results are not committed by
                 this task.
  --prefetch     download and verify every pinned asset, install the tools into the cache and
                 fetch Headroom's model weights. The only step that uses the network. Without it
                 a missing asset or install is an error.
";

struct Opts {
    corpus: PathBuf,
    cache: PathBuf,
    arms: Vec<String>,
    items: Vec<String>,
    out: Option<PathBuf>,
    dump: Option<PathBuf>,
    prefetch: bool,
}

fn parse_opts(args: &[String]) -> Res<Opts> {
    let mut o = Opts {
        corpus: crate::competitors::record::default_dir(),
        cache: std::env::temp_dir().join("lumen-bench-cache"),
        arms: ["none", "lumen", "squeez", "rtk", "headroom", "caveman"]
            .map(String::from)
            .to_vec(),
        items: Vec::new(),
        out: None,
        dump: None,
        prefetch: false,
    };
    let list = |s: &str| {
        s.split(',')
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().cloned().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--corpus" => o.corpus = PathBuf::from(val("--corpus")?),
            "--cache" => o.cache = PathBuf::from(val("--cache")?),
            "--arms" => o.arms = list(&val("--arms")?),
            "--items" => o.items = list(&val("--items")?),
            "--dump" => o.dump = Some(PathBuf::from(val("--dump")?)),
            "--out" => o.out = Some(PathBuf::from(val("--out")?)),
            "--prefetch" => o.prefetch = true,
            other => return Err(format!("replay: unknown argument {other}\n{HELP}")),
        }
    }
    Ok(o)
}

/// `competitors replay`.
pub fn run(args: &[String]) -> Res<()> {
    let o = parse_opts(args)?;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    crate::assets::ensure_outside_repo(&o.cache, &repo)?;
    let guard = RealLedgerGuard::new()?;
    guard.check("before")?;
    let state = ClaudeState::take()?;
    let result = run_inner(&o, &repo);
    // Checked even when the run failed: a tool that wrote somewhere it should not is the
    // headline, not the error that followed.
    let after = guard.check("after").and(state.check_report());
    let records = result?;
    if let Some(note) = after? {
        eprintln!("replay: {note}");
    }
    let table = tsv(&records);
    match &o.out {
        Some(p) => std::fs::write(p, &table).ctx(&format!("write {}", p.display()))?,
        None => print!("{table}"),
    }
    if let Some(d) = &o.dump {
        for r in &records {
            if let Cell::Done(m) = &r.cell {
                let dir = d.join(&r.arm);
                std::fs::create_dir_all(&dir).ctx("create the dump directory")?;
                std::fs::write(dir.join(format!("{}.txt", r.item)), &m.received)
                    .ctx("write a dump")?;
            }
        }
    }
    eprintln!("\n{}", totals(&records));
    if o.arms.iter().any(|a| a == "caveman") {
        eprintln!("caveman: {CAVEMAN_NOTE}");
    }
    Ok(())
}

fn run_inner(o: &Opts, repo: &Path) -> Res<Vec<Record>> {
    let cases = load_cases(&o.corpus, &o.items)?;
    let tools = crate::manifest::Tools::load_committed()?;
    let model = Model::from_model(&tools.meta.model);
    let scratch = tempfile::Builder::new()
        .prefix("replay-")
        .tempdir_in(ensure_dir(&o.cache)?)
        .ctx("create the scratch directory")?;
    let mut arms = arms::build(&o.arms, &o.cache, repo, scratch.path(), o.prefetch)?;
    let mut records = Vec::new();
    for case in &cases {
        for arm in arms.iter_mut() {
            eprintln!("replay: {} / {}", case.id, arm.id());
            records.push(run_case(arm.as_mut(), case, model)?);
        }
    }
    Ok(records)
}

fn ensure_dir(p: &Path) -> Res<&Path> {
    std::fs::create_dir_all(p).ctx(&format!("create {}", p.display()))?;
    Ok(p)
}
