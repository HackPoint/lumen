//! Tokens and rounds of one run, read from Claude Code's own transcripts.
//!
//! Not from a tool's claims and not from the live stream alone: every `message.usage` in the
//! session transcript and in `subagents/agent-*.jsonl`, deduplicated by `message.id`, then
//! cross-checked against the stream-json `result` line (METHOD.md, "Tokens and dollars").
//!
//! One streamed message is written to a transcript several times. It counts once. When the
//! repeats of one id disagree, the entry with the greatest `output_tokens` is used and the
//! disagreement is counted, so the report can say so.

use crate::util::{Ctx, Res};
use lumen_core::rates::{Spend, Tokens};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A difference above this share, per component, is flagged.
pub const CROSS_CHECK_TOLERANCE: f64 = 0.02;

/// What Claude Code gives a message that no model produced. Not an API request.
pub const SYNTHETIC_MODEL: &str = "<synthetic>";

/// One API message's usage.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageEntry {
    pub id: String,
    pub model: String,
    pub speed: Option<String>,
    pub geo: Option<String>,
    pub tokens: Tokens,
}

fn int(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0).max(0)
}

/// What a transcript line or a stream-json event says about usage.
#[derive(Clone, Debug, PartialEq)]
pub enum Usage {
    /// Not an assistant message with usage.
    None,
    /// An assistant message with usage but no id, so a caller counts it instead of losing it.
    NoId,
    Entry(UsageEntry),
}

/// The usage of a transcript line or a stream-json `assistant` event, if it has one.
pub fn usage_of(line: &Value) -> Usage {
    let Some(msg) = line.get("message") else {
        return Usage::None;
    };
    let is_assistant = line.get("type").and_then(Value::as_str) == Some("assistant")
        || msg.get("role").and_then(Value::as_str) == Some("assistant");
    let Some(usage) = msg.get("usage").filter(|u| u.is_object()) else {
        return Usage::None;
    };
    if !is_assistant {
        return Usage::None;
    }
    let Some(id) = msg
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Usage::NoId;
    };
    // The 1h share exists only when the transcript recorded the split.
    let one_hour = usage
        .get("cache_creation")
        .filter(|c| c.is_object())
        .map(|c| int(c, "ephemeral_1h_input_tokens"));
    let tokens = Tokens::of_turn(
        int(usage, "input_tokens"),
        int(usage, "output_tokens"),
        int(usage, "cache_read_input_tokens"),
        int(usage, "cache_creation_input_tokens"),
        one_hour,
    );
    let text = |k: &str| usage.get(k).and_then(Value::as_str).map(str::to_string);
    Usage::Entry(UsageEntry {
        id: id.to_string(),
        model: msg
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        speed: text("speed"),
        geo: text("inference_geo"),
        tokens,
    })
}

/// Messages by id, keeping the one with the most output when repeats disagree.
#[derive(Clone, Debug, Default)]
pub struct Dedup {
    by_id: BTreeMap<String, UsageEntry>,
    order: Vec<String>,
    pub duplicate_entries: u32,
    /// Ids whose repeats did not carry the same usage.
    pub disagreements: u32,
    /// Assistant lines with usage but no id: counted, not priced.
    pub no_id_entries: u32,
}

impl Dedup {
    pub fn add(&mut self, e: UsageEntry) {
        match self.by_id.get_mut(&e.id) {
            None => {
                self.order.push(e.id.clone());
                self.by_id.insert(e.id.clone(), e);
            }
            Some(old) => {
                self.duplicate_entries += 1;
                if old.tokens != e.tokens {
                    self.disagreements += 1;
                    if e.tokens.output > old.tokens.output {
                        *old = e;
                    }
                }
            }
        }
    }

    pub fn entries(&self) -> impl Iterator<Item = &UsageEntry> {
        self.order.iter().filter_map(|id| self.by_id.get(id))
    }

    /// Entries a model produced: the API requests.
    pub fn requests(&self) -> impl Iterator<Item = &UsageEntry> {
        self.entries().filter(|e| e.model != SYNTHETIC_MODEL)
    }

    pub fn spend(&self) -> Spend {
        let mut s = Spend::default();
        for e in self.requests() {
            s.add(&e.model, e.speed.as_deref(), e.geo.as_deref(), e.tokens);
        }
        s
    }
}

/// What a run used.
#[derive(Clone, Debug, Serialize)]
pub struct Totals {
    pub tokens: Tokens,
    /// API requests: distinct message ids a model produced, subagents included.
    pub rounds: u32,
    /// Dollars at the model's price from lumen-core's table; a floor when `unpriced_tokens > 0`.
    pub usd: f64,
    /// Tokens of a model with no price: never silently zero.
    pub unpriced_tokens: i64,
    pub duplicate_entries: u32,
    pub disagreements: u32,
    pub no_id_entries: u32,
    pub files: u32,
    /// `cache_read` of the first assistant message of the main transcript.
    pub first_turn_cache_read: Option<i64>,
}

pub fn totals_of(d: &Dedup, files: u32, first_turn_cache_read: Option<i64>) -> Totals {
    let spend = d.spend();
    let priced = spend.priced();
    Totals {
        tokens: spend.tokens(),
        rounds: d.requests().count() as u32,
        usd: priced.cost.total(),
        unpriced_tokens: priced.unpriced.total(),
        duplicate_entries: d.duplicate_entries,
        disagreements: d.disagreements,
        no_id_entries: d.no_id_entries,
        files,
        first_turn_cache_read,
    }
}

/// Parse transcript files (main first, then subagents) into [`Totals`]. A line that is not JSON
/// is an error naming the file and line: a transcript that cannot be read is not a zero.
pub fn parse_files(main: &[PathBuf], subagents: &[PathBuf]) -> Res<Totals> {
    let mut d = Dedup::default();
    let mut first: Option<i64> = None;
    for (i, path) in main.iter().chain(subagents.iter()).enumerate() {
        let is_main = i < main.len();
        let text =
            std::fs::read_to_string(path).ctx(&format!("read transcript {}", path.display()))?;
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value =
                serde_json::from_str(line).ctx(&format!("{} line {}", path.display(), n + 1))?;
            match usage_of(&v) {
                Usage::Entry(e) => {
                    if is_main && first.is_none() && e.model != SYNTHETIC_MODEL {
                        first = Some(e.tokens.cache_read);
                    }
                    d.add(e);
                }
                Usage::None => {}
                Usage::NoId => d.no_id_entries += 1,
            }
        }
    }
    Ok(totals_of(&d, (main.len() + subagents.len()) as u32, first))
}

/// The transcripts under `<home>/.claude/projects`: main sessions and `subagents/agent-*.jsonl`,
/// each sorted by path. Read only.
pub fn find_transcripts(home: &Path) -> Res<(Vec<PathBuf>, Vec<PathBuf>)> {
    let root = home.join(".claude").join("projects");
    let (mut main, mut subs) = (Vec::new(), Vec::new());
    if !root.is_dir() {
        return Ok((main, subs));
    }
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).ctx(&format!("read {}", dir.display()))? {
            let e = e.ctx("read a directory entry")?;
            let p = e.path();
            let ft = e.file_type().ctx("stat")?;
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                let in_subagents = p
                    .parent()
                    .and_then(|d| d.file_name())
                    .is_some_and(|n| n == "subagents");
                let agent = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("agent-"));
                if in_subagents && agent {
                    subs.push(p);
                } else if !in_subagents {
                    main.push(p);
                }
            }
        }
    }
    main.sort();
    subs.sort();
    Ok((main, subs))
}

/// Everything under a home's projects directory.
pub fn parse_home(home: &Path) -> Res<Totals> {
    let (main, subs) = find_transcripts(home)?;
    parse_files(&main, &subs)
}

/// The stream-json `result` line.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StreamResult {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    /// What Claude Code estimated. Recorded, never used for dollars.
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<i64>,
    pub is_error: bool,
    pub subtype: String,
}

pub fn stream_result(line: &Value) -> Option<StreamResult> {
    if line.get("type").and_then(Value::as_str) != Some("result") {
        return None;
    }
    let u = line.get("usage").cloned().unwrap_or(Value::Null);
    Some(StreamResult {
        input: int(&u, "input_tokens"),
        output: int(&u, "output_tokens"),
        cache_read: int(&u, "cache_read_input_tokens"),
        cache_creation: int(&u, "cache_creation_input_tokens"),
        total_cost_usd: line.get("total_cost_usd").and_then(Value::as_f64),
        num_turns: line.get("num_turns").and_then(Value::as_i64),
        is_error: line
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        subtype: line
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

/// The transcript's totals against the stream's `result`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CrossCheck {
    /// False when the run printed no `result` line (killed, or crashed): nothing to compare.
    pub checked: bool,
    /// One line per component that differs by more than [`CROSS_CHECK_TOLERANCE`].
    pub mismatches: Vec<String>,
}

impl CrossCheck {
    pub fn flagged(&self) -> bool {
        !self.mismatches.is_empty()
    }
}

pub fn cross_check(t: &Tokens, r: Option<&StreamResult>) -> CrossCheck {
    let Some(r) = r else {
        return CrossCheck {
            checked: false,
            mismatches: Vec::new(),
        };
    };
    let rows = [
        ("input", t.input, r.input),
        ("output", t.output, r.output),
        ("cache_read", t.cache_read, r.cache_read),
        ("cache_creation", t.cache_write, r.cache_creation),
    ];
    let mismatches = rows
        .iter()
        .filter_map(|(name, transcript, stream)| {
            let big = (*transcript).max(*stream);
            let diff = (transcript - stream).abs();
            (big > 0 && diff as f64 / big as f64 > CROSS_CHECK_TOLERANCE)
                .then(|| format!("{name}: transcript {transcript}, stream result {stream}"))
        })
        .collect();
    CrossCheck {
        checked: true,
        mismatches,
    }
}
