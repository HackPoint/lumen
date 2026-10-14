//! The intercept's block, as the hook says it and as a transcript records it.
//!
//! The PreToolUse hook (`lumen-mcp hook intercept`) blocks a large built-in Read by exiting 2 with a
//! message on stderr, and Claude Code hands that message to the model as the Read's result, an
//! error. A block writes no ledger row (only the marker file the hook keeps for itself), so the
//! transcript of the agent that tried the Read is the one record of it: the assistant's `tool_use`
//! (`Read`, with its `file_path`) and, in a later user record, its `tool_result`, flagged
//! `is_error`, whose text holds the message (Claude Code puts its own words before it).
//!
//! The hook builds the first line of every message it blocks with from [`block_line`], and the
//! daemon reads it back with [`parse_block`]: one definition, so the two cannot drift, and a test
//! in the hook holds each message it writes to the parser. Every form a release wrote is read:
//! the shell hooks of 0.1.0 to 1.5.x and the Rust hook since 1.6.0 wrote the same first line,
//! with the path as the Read named it (joined to the payload's `cwd` when relative, from 1.6.0),
//! and 1.8.0 added [`BlockKind::Served`]. One difference in the count: the hooks Setup wrote up to
//! 1.4.x took it from `wc -l` as it printed it, which on macOS is padded to eight columns
//! (`is      412 lines.`), so spaces before the count are read past. A message whose first line
//! is not one of these is not a block. [`MATCHER`] names this reading and is recorded with each
//! transcript the daemon scans.
//!
//! [`Scanner`] reads a transcript line by line, in file order, and returns each blocked Read it
//! finds: an `is_error` result of a `Read` this transcript asked for, whose text holds a block line
//! naming the path that Read asked for. A line holding neither a `Read` call nor the block prefix
//! is passed over on a substring test, without being parsed.

use std::collections::VecDeque;
use std::path::Path;

use serde::Deserialize;

/// What every block message starts with.
pub const BLOCK_PREFIX: &str = "Lumen intercept: ";

/// The version of [`parse_block`] and [`Scanner`]: bumped when either reads something it did not,
/// so a transcript scanned by an older one can be told apart (`transcript_scans.matcher`).
pub const MATCHER: i64 = 1;

/// Which message a block was: what follows `is <n> lines` on its first line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// A source file, sent to smart_read and recall_file.
    Redirect,
    /// A log or output file, sent to compress_logs.
    Log,
    /// Blocked again after smart_read or recall_file had answered for the path (from 1.8.0): the
    /// escape valve's fault for the Read it then lets through says `after_lumen_read`.
    Served,
}

impl BlockKind {
    /// Longest tail first: a log block's line also ends in `.`.
    pub const ALL: [BlockKind; 3] = [BlockKind::Served, BlockKind::Log, BlockKind::Redirect];

    /// What the first line ends with after `is <n> lines`.
    pub fn tail(self) -> &'static str {
        match self {
            BlockKind::Redirect => ".",
            BlockKind::Log => " (log/output file).",
            BlockKind::Served => ", and smart_read or recall_file has already",
        }
    }

    /// As `intercept_blocks.kind` holds it.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockKind::Redirect => "redirect",
            BlockKind::Log => "log",
            BlockKind::Served => "served",
        }
    }
}

/// The first line of a block message, without its newline:
/// `Lumen intercept: <path> is <lines> lines<tail>`.
pub fn block_line(path: &str, lines: i64, kind: BlockKind) -> String {
    format!("{BLOCK_PREFIX}{path} is {lines} lines{}", kind.tail())
}

/// A block line read back: the path the hook named, the lines it counted, and which message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedBlock<'a> {
    pub path: &'a str,
    pub lines: i64,
    pub kind: BlockKind,
}

/// The first block line in `text`, wherever it starts (Claude Code writes its own words before
/// the hook's), ending at a newline or the end of the text, a carriage return before it dropped.
/// `None` when no line is one [`block_line`] could have written.
pub fn parse_block(text: &str) -> Option<ParsedBlock<'_>> {
    let mut rest = text;
    while let Some(i) = rest.find(BLOCK_PREFIX) {
        let after = &rest[i + BLOCK_PREFIX.len()..];
        let line = after.split('\n').next().map(|l| l.trim_end_matches('\r'));
        if let Some(parsed) = line.and_then(parse_head) {
            return Some(parsed);
        }
        rest = after;
    }
    None
}

/// `<path> is <lines> lines<tail>`, the prefix already taken off. The count may be padded with
/// spaces, as macOS `wc -l` prints it.
fn parse_head(line: &str) -> Option<ParsedBlock<'_>> {
    BlockKind::ALL.into_iter().find_map(|kind| {
        let body = line.strip_suffix(kind.tail())?.strip_suffix(" lines")?;
        let (path, n) = body.rsplit_once(" is ")?;
        let n = n.trim_start_matches(' ');
        if path.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(ParsedBlock {
            path,
            lines: n.parse().ok()?,
            kind,
        })
    })
}

/// A Read the intercept blocked, found in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedBlock {
    /// The `tool_result` record's `sessionId`.
    pub session_id: String,
    /// The `tool_result` record's `uuid`.
    pub uuid: String,
    /// The Read's `tool_use` id: the block's identity, which a copied transcript keeps.
    pub tool_use_id: String,
    /// The `tool_result` record's timestamp, unix ms.
    pub ts_ms: i64,
    /// As the hook named it, which is the Read's `file_path` (joined to the record's `cwd` when
    /// relative), and the path its escape-valve fault names.
    pub path: String,
    pub lines: i64,
    pub kind: BlockKind,
}

/// How many Reads a transcript asked for are kept waiting for their result, the oldest let go
/// first. Claude Code writes a call's result in the round that made it, after at most the results
/// of the other calls made beside it, so a block's call is among the last few Reads when its
/// result arrives; 64 leaves room for any batch of parallel calls. A result more Reads than that
/// after its call is not matched, and its block is not recorded.
const PENDING_READS: usize = 64;

#[derive(Debug, Clone)]
struct PendingRead {
    id: String,
    file_path: String,
    /// The record's `cwd`, kept only for a relative `file_path`.
    cwd: Option<String>,
}

/// Reads one transcript, in file order, for blocked Reads. One per transcript, kept across reads
/// of it: a Read's call and its result can arrive in different reads of a growing file.
#[derive(Debug, Default)]
pub struct Scanner {
    reads: VecDeque<PendingRead>,
}

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type")]
    ty: Option<String>,
    uuid: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<String>,
    cwd: Option<String>,
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    content: Option<Content>,
}

/// Parts first: an untagged enum takes the first variant that fits, and anything fits the other.
#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Blocks(Vec<Part>),
    Other(#[allow(dead_code)] serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    ty: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Input>,
    tool_use_id: Option<String>,
    is_error: Option<bool>,
    content: Option<ResultContent>,
}

#[derive(Deserialize)]
struct Input {
    file_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResultContent {
    Text(String),
    Parts(Vec<TextPart>),
}

#[derive(Deserialize)]
struct TextPart {
    text: Option<String>,
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one transcript line, in file order: the Reads it asks for, and the blocked Reads whose
    /// result it holds. The daemon, which parses every assistant line already, gives the Reads it
    /// finds there to [`Scanner::read_called`] and each line to [`Scanner::results`] instead,
    /// which is this less a second look at every line.
    pub fn feed(&mut self, line: &str) -> Vec<RecordedBlock> {
        if line.contains("\"Read\"")
            && line.contains("\"tool_use\"")
            && let Ok(rec) = serde_json::from_str::<Rec>(line)
            && rec.ty.as_deref() == Some("assistant")
            && let Some(Content::Blocks(parts)) = rec.message.and_then(|m| m.content)
        {
            for part in parts {
                if let (Some("tool_use"), Some("Read"), Some(id), Some(file_path)) = (
                    part.ty.as_deref(),
                    part.name.as_deref(),
                    part.id.as_deref(),
                    part.input.as_ref().and_then(|i| i.file_path.as_deref()),
                ) {
                    self.read_called(id, file_path, rec.cwd.as_deref());
                }
            }
        }
        self.results(line)
    }

    /// The transcript's assistant asked for a Read of `file_path`, as `tool_use` `id`, in the
    /// record whose `cwd` is given.
    pub fn read_called(&mut self, id: &str, file_path: &str, cwd: Option<&str>) {
        if self.reads.len() == PENDING_READS {
            self.reads.pop_front();
        }
        self.reads.push_back(PendingRead {
            id: id.to_string(),
            file_path: file_path.to_string(),
            cwd: Path::new(file_path)
                .is_relative()
                .then(|| cwd.map(str::to_string))
                .flatten(),
        });
    }

    /// The blocked Reads whose result `line` holds. A line without the block message's prefix is
    /// passed over on a substring test, never parsed.
    pub fn results(&mut self, line: &str) -> Vec<RecordedBlock> {
        if !line.contains(BLOCK_PREFIX) {
            return Vec::new();
        }
        let Ok(rec) = serde_json::from_str::<Rec>(line) else {
            return Vec::new();
        };
        let Some(Content::Blocks(parts)) = rec.message.and_then(|m| m.content) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for part in parts {
            if part.ty.as_deref() != Some("tool_result") || part.is_error != Some(true) {
                continue;
            }
            let Some(id) = part.tool_use_id else {
                continue;
            };
            let Some(at) = self.reads.iter().position(|r| r.id == id) else {
                continue;
            };
            let text = match part.content {
                Some(ResultContent::Text(t)) => t,
                Some(ResultContent::Parts(p)) => p
                    .into_iter()
                    .filter_map(|p| p.text)
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => continue,
            };
            let Some(parsed) = parse_block(&text) else {
                continue;
            };
            if !names_the_read(parsed.path, &self.reads[at]) {
                continue;
            }
            let (Some(session_id), Some(uuid), Some(ts_ms)) = (
                rec.session.clone(),
                rec.uuid.clone(),
                rec.timestamp
                    .as_deref()
                    .and_then(crate::events::parse_ts_ms),
            ) else {
                continue;
            };
            self.reads.remove(at);
            found.push(RecordedBlock {
                session_id,
                uuid,
                tool_use_id: id,
                ts_ms,
                path: parsed.path.to_string(),
                lines: parsed.lines,
                kind: parsed.kind,
            });
        }
        found
    }
}

/// Whether the path a block line names is the one the Read asked for: the same text, or, for a
/// relative `file_path`, that path joined to the record's `cwd`, as the hook resolves it.
fn names_the_read(named: &str, read: &PendingRead) -> bool {
    if named == read.file_path {
        return true;
    }
    match &read.cwd {
        Some(cwd) => Path::new(cwd).join(&read.file_path) == Path::new(named),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-10-13T09:00:00.250Z";
    const TS_MS: i64 = 1_791_882_000_250;

    /// The assistant's call: one `Read` of `path`.
    fn read_call(id: &str, path: &str) -> String {
        serde_json::json!({
            "type": "assistant", "uuid": format!("a-{id}"), "sessionId": "s", "timestamp": TS,
            "cwd": "/r",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": id, "name": "Read", "input": {"file_path": path}}
            ]}
        })
        .to_string()
    }

    /// Its result, `content` as given, `is_error` as given.
    fn result(id: &str, content: serde_json::Value, is_error: Option<bool>) -> String {
        let mut part =
            serde_json::json!({"type": "tool_result", "tool_use_id": id, "content": content});
        if let Some(e) = is_error {
            part["is_error"] = e.into();
        }
        serde_json::json!({
            "type": "user", "uuid": format!("u-{id}"), "sessionId": "s", "timestamp": TS,
            "cwd": "/r",
            "message": {"role": "user", "content": [part]}
        })
        .to_string()
    }

    /// What Claude Code hands the model for a PreToolUse hook that exits 2: its own words, then the
    /// hook's stderr.
    ///
    /// An assumption: the repository holds captured hook payloads
    /// (`crates/lumen-mcp/tests/fixtures/hooks`) but no captured transcript of a blocked Read. What
    /// is assumed of one is that the Read's `tool_result` has `is_error: true` and holds the
    /// hook's stderr, as a string or as text parts; the words Claude Code puts before it are not
    /// relied on, since the message is found wherever it starts.
    fn hook_error(message: &str) -> String {
        format!("PreToolUse:Read hook error: [lumen-mcp hook intercept]: {message}")
    }

    fn scan(lines: &[String]) -> Vec<RecordedBlock> {
        let mut s = Scanner::new();
        lines.iter().flat_map(|l| s.feed(l)).collect()
    }

    #[test]
    fn every_message_the_hook_writes_parses_back() {
        for kind in BlockKind::ALL {
            for path in ["/r/a.rs", "/r/dir is odd/x is 3 lines.rs", "C:\\r\\a b.rs"] {
                let line = block_line(path, 412, kind);
                assert_eq!(
                    parse_block(&format!("{line}\nInstead of ...\n")),
                    Some(ParsedBlock {
                        path,
                        lines: 412,
                        kind
                    }),
                    "{line}"
                );
            }
        }
    }

    #[test]
    fn the_first_line_of_each_release_parses() {
        // 0.1.0 to 1.5.x (the shell hook) and 1.6.0 on: the same first lines.
        assert_eq!(
            parse_block("Lumen intercept: /p/big.rs is 420 lines.\nInstead").map(|p| p.kind),
            Some(BlockKind::Redirect)
        );
        assert_eq!(
            parse_block("Lumen intercept: /p/x.log is 9000 lines (log/output file).\nBefore")
                .map(|p| (p.path, p.kind)),
            Some(("/p/x.log", BlockKind::Log))
        );
        // 1.8.0: blocked again after smart_read answered.
        assert_eq!(
            parse_block(
                "Lumen intercept: /p/big.rs is 420 lines, and smart_read or recall_file has \
                 already\nanswered for it"
            )
            .map(|p| p.kind),
            Some(BlockKind::Served)
        );
    }

    /// The hooks Setup wrote up to 1.4.x counted lines with `wc -l < "$FILE_PATH"` and no
    /// `tr -d`, so on macOS, whose `wc` pads its count to eight columns, the message read
    /// `is      412 lines.`.
    #[test]
    fn a_count_padded_by_macos_wc_reads_back() {
        for lines in [412_i64, 12_345_678] {
            let padded = format!(" {lines:>7}");
            for (tail, kind) in [
                (".", BlockKind::Redirect),
                (" (log/output file).", BlockKind::Log),
            ] {
                let text = format!("Lumen intercept: /r/a b.rs is {padded} lines{tail}\nInstead");
                assert_eq!(
                    parse_block(&text),
                    Some(ParsedBlock {
                        path: "/r/a b.rs",
                        lines,
                        kind
                    }),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn a_line_that_is_not_one_the_hook_writes_does_not_parse() {
        for text in [
            "Lumen intercept: a stand-in for the redirect",
            "Lumen intercept: /p/a.rs is many lines.",
            "Lumen intercept: /p/a.rs is 12 lines",
            "Lumen intercept:  is 12 lines.",
            "Lumen intercept: /p/a.rs is -3 lines.",
            "lumen: cannot count lines in /p/a.rs (denied); the read was let through",
            "Lumen intercept: /p/a.rs is 12 lines. And more",
        ] {
            assert_eq!(parse_block(text), None, "{text}");
        }
    }

    #[test]
    fn a_carriage_return_and_words_before_the_message_are_read_past() {
        let text = hook_error(
            "lumen: a warning first\r\nLumen intercept: /r/a.rs is 400 lines.\r\nInstead",
        );
        assert_eq!(
            parse_block(&text),
            Some(ParsedBlock {
                path: "/r/a.rs",
                lines: 400,
                kind: BlockKind::Redirect
            })
        );
    }

    #[test]
    fn a_blocked_read_is_found_with_its_content_as_a_string_or_as_parts() {
        let msg = hook_error(&format!(
            "{}\nInstead of reading",
            block_line("/r/a.rs", 400, BlockKind::Redirect)
        ));
        for content in [
            serde_json::Value::from(msg.clone()),
            serde_json::json!([{"type": "text", "text": msg}]),
        ] {
            let found = scan(&[
                read_call("t1", "/r/a.rs"),
                result("t1", content, Some(true)),
            ]);
            assert_eq!(
                found,
                [RecordedBlock {
                    session_id: "s".into(),
                    uuid: "u-t1".into(),
                    tool_use_id: "t1".into(),
                    ts_ms: TS_MS,
                    path: "/r/a.rs".into(),
                    lines: 400,
                    kind: BlockKind::Redirect
                }]
            );
        }
    }

    #[test]
    fn a_result_that_is_not_an_error_or_not_a_reads_or_another_paths_is_no_block() {
        let msg = block_line("/r/a.rs", 400, BlockKind::Redirect);
        // A Read that went through, of a file that holds a block message.
        assert!(
            scan(&[
                read_call("t1", "/r/a.rs"),
                result("t1", msg.clone().into(), None)
            ])
            .is_empty()
        );
        assert!(
            scan(&[
                read_call("t1", "/r/a.rs"),
                result("t1", msg.clone().into(), Some(false))
            ])
            .is_empty()
        );
        // A failing command that printed one (a test of the hook, say): not a Read's result.
        let bash = serde_json::json!({
            "type": "assistant", "uuid": "a-b", "sessionId": "s", "timestamp": TS,
            "message": {"content": [{"type": "tool_use", "id": "b1", "name": "Bash", "input": {"command": "cargo test"}}]}
        })
        .to_string();
        assert!(scan(&[bash, result("b1", msg.clone().into(), Some(true))]).is_empty());
        // A Read of another file whose error quotes this one's block.
        assert!(
            scan(&[
                read_call("t2", "/r/log.txt"),
                result("t2", msg.into(), Some(true))
            ])
            .is_empty()
        );
        // Another error of a Read.
        assert!(
            scan(&[
                read_call("t3", "/r/a.rs"),
                result("t3", "File does not exist.".into(), Some(true))
            ])
            .is_empty()
        );
    }

    #[test]
    fn a_relative_path_is_matched_as_the_hook_resolves_it() {
        let msg = block_line(
            &Path::new("/r").join("src/a.rs").to_string_lossy(),
            400,
            BlockKind::Redirect,
        );
        let found = scan(&[
            read_call("t1", "src/a.rs"),
            result("t1", msg.into(), Some(true)),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_retried_read_after_a_block_is_another_call_and_no_second_block() {
        let msg = block_line("/r/a.rs", 400, BlockKind::Redirect);
        let found = scan(&[
            read_call("t1", "/r/a.rs"),
            result("t1", msg.clone().into(), Some(true)),
            // The retry the valve lets through, and a stray repeat of the first result.
            read_call("t2", "/r/a.rs"),
            result("t2", "     1\tfn main() {}".into(), None),
            result("t1", msg.into(), Some(true)),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_line_that_names_neither_a_read_nor_a_block_is_not_parsed() {
        let mut s = Scanner::new();
        assert!(s.feed("not json, and no marker").is_empty());
        assert!(s.feed("{\"Lumen intercept: \" broken").is_empty());
        assert!(s.reads.is_empty());
    }

    #[test]
    fn the_reads_waiting_for_a_result_are_bounded() {
        let mut s = Scanner::new();
        for n in 0..(PENDING_READS + 10) {
            s.feed(&read_call(&format!("t{n}"), "/r/a.rs"));
        }
        assert_eq!(s.reads.len(), PENDING_READS);
        assert_eq!(s.reads.front().map(|r| r.id.as_str()), Some("t10"));
    }
}
