//! Compaction and rewind, detected in the Claude Code transcript.
//!
//! Both mean something the model was told earlier may no longer be in its context, which is
//! what makes a repeat-read answer ("unchanged since your earlier read") untrue. The daemon
//! feeds every complete line of a transcript through a [`Tracker`] and persists each event it
//! returns, one row per event (`session_events`).
//!
//! Compaction is an explicit record: `{"type":"system","subtype":"compact_boundary"}`.
//!
//! Rewind has no record of its own: the transcript is a tree, and rewinding makes the next
//! prompt a child of an older message. "A parentUuid that is not the latest message" is
//! wrong, though — parallel tool calls make every tool_result point at an earlier assistant
//! record. What distinguishes a rewind is depth counted in real prompts: each record's depth
//! is its parent's, plus one when it is a real prompt. A real prompt whose parent is shallower
//! than the tip of the conversation has branched back past turns the model had already seen.
//! Sidechains (subagents) are separate conversations and are skipped.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Compaction,
    Rewind,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Compaction => "compaction",
            Kind::Rewind => "rewind",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub session_id: String,
    /// The transcript record that caused it: with the kind, the event's identity, so
    /// ingesting the same file twice records it once.
    pub uuid: String,
    /// The record's own timestamp, unix ms. `None` when it is missing or unparseable.
    pub ts_ms: Option<i64>,
}

#[derive(Deserialize)]
struct Head {
    uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    parent: Option<String>,
    #[serde(rename = "isSidechain", default)]
    sidechain: bool,
    #[serde(rename = "isMeta", default)]
    meta: bool,
    #[serde(rename = "type")]
    ty: Option<String>,
    subtype: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session: Option<String>,
}

#[derive(Deserialize)]
struct Body {
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(#[allow(dead_code)] String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    ty: Option<String>,
}

/// One per transcript file. Memory is a uuid and a depth per record of the file.
#[derive(Default)]
pub struct Tracker {
    depth: HashMap<String, i64>,
    tip: i64,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one transcript line, in file order. Lines that are not JSON, or carry no uuid,
    /// are ignored: they cannot be part of the tree.
    pub fn feed(&mut self, line: &str) -> Option<Event> {
        let h: Head = serde_json::from_str(line).ok()?;
        let uuid = h.uuid.clone()?;
        if h.sidechain {
            return None;
        }
        let ty = h.ty.as_deref().unwrap_or("");

        let prompt = ty == "user" && !h.meta && is_prompt(line);
        let parent_depth = h.parent.as_ref().and_then(|p| self.depth.get(p).copied());
        let base = parent_depth.unwrap_or(self.tip);
        let depth = base + prompt as i64;
        self.depth.insert(uuid.clone(), depth);

        // A prompt whose parent is missing or unknown (a rewind to the root) cannot be placed on
        // the branch we were on, so once a prompt has been seen it counts as a rewind: a false
        // positive costs one full outline, a false negative serves a stub with no base in context.
        let rewound = prompt
            && match parent_depth {
                Some(d) => self.tip > d,
                None => self.tip > 0,
            };
        self.tip = depth;

        let kind = if ty == "system" && h.subtype.as_deref() == Some("compact_boundary") {
            Kind::Compaction
        } else if rewound {
            Kind::Rewind
        } else {
            return None;
        };
        Some(Event {
            kind,
            session_id: h.session?,
            uuid,
            ts_ms: h.timestamp.as_deref().and_then(parse_ts_ms),
        })
    }
}

/// A user record that is a person's prompt, not a tool result handed back to the model.
fn is_prompt(line: &str) -> bool {
    match serde_json::from_str::<Body>(line) {
        Ok(Body {
            message:
                Some(Message {
                    content: Some(Content::Blocks(b)),
                }),
        }) => !b.iter().any(|b| b.ty.as_deref() == Some("tool_result")),
        Ok(_) => true,
        Err(_) => false,
    }
}

/// `2026-06-01T16:14:05.157Z` → unix ms. Only UTC (`Z`) and numeric offsets, which is all
/// Claude Code writes.
pub fn parse_ts_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0;
    if b.get(i) == Some(&b'.') {
        let digits = s[i + 1..].bytes().take_while(u8::is_ascii_digit).count();
        let frac = &s[i + 1..i + 1 + digits.min(3)];
        ms = frac.parse::<i64>().ok()? * 10_i64.pow(3 - frac.len() as u32);
        i += 1 + digits;
    }
    let offset = match b.get(i) {
        Some(b'Z') | None => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = num(i + 1, i + 3)?;
            // "+HH" alone is a valid offset; "+HH:MM" with unreadable minutes is not.
            let om = if s.len() > i + 4 {
                num(i + 4, i + 6)?
            } else {
                0
            };
            let o = (oh * 60 + om) * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec - offset) * 1000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ty: &str, uuid: &str, parent: Option<&str>, extra: &str) -> String {
        let p = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        format!(
            r#"{{"type":"{ty}","uuid":"{uuid}","parentUuid":{p},"sessionId":"s","timestamp":"2026-06-01T16:14:05.157Z"{extra}}}"#
        )
    }
    fn prompt(uuid: &str, parent: Option<&str>) -> String {
        rec("user", uuid, parent, r#","message":{"content":"hi"}"#)
    }
    fn result(uuid: &str, parent: &str) -> String {
        rec(
            "user",
            uuid,
            Some(parent),
            r#","message":{"content":[{"type":"tool_result","content":"x"}]}"#,
        )
    }
    fn assistant(uuid: &str, parent: &str) -> String {
        rec(
            "assistant",
            uuid,
            Some(parent),
            r#","message":{"content":[]}"#,
        )
    }
    fn run(lines: &[String]) -> Vec<Event> {
        let mut t = Tracker::new();
        lines.iter().filter_map(|l| t.feed(l)).collect()
    }

    #[test]
    fn timestamps_parse() {
        assert_eq!(parse_ts_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_ts_ms("2026-06-01T16:14:05.157Z"),
            Some(1_780_330_445_157)
        );
        assert_eq!(
            parse_ts_ms("2026-06-01T16:14:05.5Z"),
            Some(1_780_330_445_500)
        );
        assert_eq!(
            parse_ts_ms("2026-06-01T18:14:05+02:00"),
            Some(1_780_330_445_000)
        );
        assert_eq!(parse_ts_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(parse_ts_ms("nonsense"), None);
        assert_eq!(parse_ts_ms("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn a_linear_conversation_with_tool_calls_is_not_a_rewind() {
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            result("r1", "a1"),
            assistant("a2", "r1"),
            prompt("u2", Some("a2")),
        ]);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn parallel_tool_results_pointing_at_one_assistant_are_not_a_rewind() {
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            result("r1", "a1"),
            assistant("a2", "r1"),
            result("r2", "a1"), // the second parallel result: parent is back at a1
            assistant("a3", "r2"),
            prompt("u2", Some("a3")),
        ]);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_prompt_branching_back_past_a_turn_is_a_rewind() {
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            prompt("u2", Some("a1")),
            assistant("a2", "u2"),
            prompt("u3", Some("a2")),
            assistant("a3", "u3"),
            // The user rewinds to before u2 and says something else.
            prompt("u4", Some("a1")),
            assistant("a4", "u4"),
            // And the conversation carries on from there: not another rewind.
            prompt("u5", Some("a4")),
        ]);
        assert_eq!(
            ev,
            [Event {
                kind: Kind::Rewind,
                session_id: "s".into(),
                uuid: "u4".into(),
                ts_ms: Some(1_780_330_445_157)
            }]
        );
    }

    #[test]
    fn a_rewind_to_the_conversation_root_is_a_rewind() {
        // Esc-Esc on the first message: the new prompt has no parent at all.
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            prompt("u2", Some("a1")),
            assistant("a2", "u2"),
            prompt("u3", None),
            assistant("a3", "u3"),
            // The conversation carries on from the new root: not another rewind.
            prompt("u4", Some("a3")),
        ]);
        assert_eq!(
            ev,
            [Event {
                kind: Kind::Rewind,
                session_id: "s".into(),
                uuid: "u3".into(),
                ts_ms: Some(1_780_330_445_157)
            }]
        );
    }

    #[test]
    fn a_prompt_whose_parent_is_unknown_is_a_rewind_once_a_prompt_was_seen() {
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            prompt("u2", Some("never-seen")),
        ]);
        assert_eq!(ev.len(), 1, "{ev:?}");
        assert_eq!(ev[0].kind, Kind::Rewind);
        assert_eq!(ev[0].uuid, "u2");
    }

    #[test]
    fn the_first_prompt_of_a_transcript_is_never_a_rewind() {
        // A resumed transcript can open with a parent that lives in another file.
        let ev = run(&[prompt("u1", Some("elsewhere")), assistant("a1", "u1")]);
        assert!(ev.is_empty(), "{ev:?}");
        let ev = run(&[assistant("a0", "elsewhere"), prompt("u1", None)]);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_sidechain_branching_is_not_a_rewind() {
        let side = |u: &str, p: &str| {
            rec(
                "user",
                u,
                Some(p),
                r#","isSidechain":true,"message":{"content":"task"}"#,
            )
        };
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            prompt("u2", Some("a1")),
            assistant("a2", "u2"),
            side("s1", "a1"),
            side("s2", "a1"),
            prompt("u3", Some("a2")),
        ]);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_meta_user_record_is_not_a_prompt() {
        let meta = rec(
            "user",
            "m1",
            Some("a1"),
            r#","isMeta":true,"message":{"content":"x"}"#,
        );
        let ev = run(&[
            prompt("u1", None),
            assistant("a1", "u1"),
            prompt("u2", Some("a1")),
            meta,
            prompt("u3", Some("u2")),
        ]);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn a_compact_boundary_is_a_compaction_and_a_quoted_one_is_not() {
        let boundary = rec("system", "c1", None, r#","subtype":"compact_boundary""#);
        let quoted = rec(
            "user",
            "q1",
            Some("a1"),
            r#","message":{"content":"grep compact_boundary src"}"#,
        );
        let ev = run(&[prompt("u1", None), assistant("a1", "u1"), quoted, boundary]);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].kind, Kind::Compaction);
        assert_eq!(ev[0].uuid, "c1");
    }

    #[test]
    fn unparseable_and_uuidless_lines_are_ignored() {
        let mut t = Tracker::new();
        assert!(t.feed("not json").is_none());
        assert!(t.feed(r#"{"type":"summary"}"#).is_none());
        assert!(t.feed("").is_none());
    }
}
