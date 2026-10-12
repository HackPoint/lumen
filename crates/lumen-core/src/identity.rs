//! Whose call is this? The live conversation behind one MCP tool call (#32).
//!
//! `lumen-mcp` is a stdio server and Claude Code starts it once. `/clear` ends the
//! conversation and starts another in the same Claude Code process, so the server lives on,
//! with the session id it was started under in its environment and the reads it delivered
//! in its memory. Answering "unchanged" to a model whose context was cleared is a reference
//! to nothing, and a ledger row stamped with the old session belongs to nobody.
//!
//! The MCP request does not say who is asking: Claude Code sends `_meta` with a
//! `claudecode/toolUseId` and a progress token, and nothing else. The PreToolUse hook payload
//! does: `session_id`, and inside a subagent `agent_id`, beside the same `tool_use_id`. So the
//! hook leaves one small record per Lumen tool call, named by that id, beside the ledger, and
//! the server claims it when the request arrives.
//!
//! A record names one call and is used once, so parallel calls cannot take each other's, and
//! nothing is guessed from ordering. Everything that can go wrong ends in [`Lookup::Unknown`],
//! which the server answers with the full result and records as unknown: the hook not being
//! registered, not having run, being unable to write, a record that is malformed, old, or for
//! another tool. Nothing falls back to the environment.

use serde_json::{Value, json};
use std::fmt;
use std::path::{Path, PathBuf};

/// Lumen's MCP tools as Claude Code names them when the server is registered as `lumen`,
/// which is how Setup registers it.
pub const TOOL_PREFIX: &str = "mcp__lumen__";

/// The same tools when the server comes from the Claude Code plugin named `lumen`: Claude
/// Code puts the plugin's name in front of the server's.
pub const PLUGIN_TOOL_PREFIX: &str = "mcp__plugin_lumen_lumen__";

/// The tools that read or compress what the model is about to hold.
pub const TOOLS: [&str; 3] = ["smart_read", "recall_file", "compress_logs"];

/// The PreToolUse matchers that carry them for a server Setup registered. Claude Code reads a
/// matcher made only of letters, digits, `_` and `|` as an exact name or a list of them, so
/// each is registered as its own entry. This is the list Setup installs and the validator and
/// `lumen doctor` check.
pub const MATCHERS: [&str; 3] = [
    "mcp__lumen__smart_read",
    "mcp__lumen__recall_file",
    "mcp__lumen__compress_logs",
];

/// The matchers for the server the plugin ships. The plugin's `hooks/hooks.json` registers
/// these and [`MATCHERS`] both: it cannot know which of the two servers a session has.
pub const PLUGIN_MATCHERS: [&str; 3] = [
    "mcp__plugin_lumen_lumen__smart_read",
    "mcp__plugin_lumen_lumen__recall_file",
    "mcp__plugin_lumen_lumen__compress_logs",
];

/// Which of Lumen's tools `tool_name`, as a hook payload spells it, is: `smart_read`,
/// `recall_file` or `compress_logs`, under either server name. Anything else is not Lumen's.
pub fn short_name(tool_name: &str) -> Option<&'static str> {
    let rest = tool_name
        .strip_prefix(TOOL_PREFIX)
        .or_else(|| tool_name.strip_prefix(PLUGIN_TOOL_PREFIX))?;
    TOOLS.into_iter().find(|t| *t == rest)
}

/// Whether `tool_name`, as a hook payload spells it, is one of Lumen's.
pub fn is_lumen_tool(tool_name: &str) -> bool {
    short_name(tool_name).is_some()
}

/// The record format. A record of another version is unknown, not guessed at.
pub const FORMAT: i64 = 1;

/// How long a record is good for, and how long an unclaimed one is kept.
///
/// A record is made when the hook runs, which is before the permission prompt, so one for a
/// call that is then denied is never claimed. An hour covers a prompt left open; past it
/// the call is answered as unknown, which only costs it the repeat-read saving.
pub const MAX_AGE_MS: i64 = 60 * 60 * 1000;

/// Records kept at most. The age bound alone is a rate times an hour, and a model denied
/// the same call in a loop is a high rate.
pub const MAX_RECORDS: usize = 256;

const PREFIX: &str = "lumen_call_";
const SUFFIX: &str = ".json";

/// Longest session or agent id accepted. Claude Code's are UUIDs and short tokens.
const MAX_FIELD: usize = 256;

/// The conversation that made a call: a session, and inside a subagent which one.
///
/// Subagents share their parent's session id and each runs in a context of its own, so the
/// pair is what names a context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub session: String,
    pub agent: Option<String>,
}

impl Identity {
    /// The key a base is held under: one string per context.
    pub fn owner(&self) -> String {
        match &self.agent {
            Some(a) => format!("{}\0{a}", self.session),
            None => self.session.clone(),
        }
    }
}

/// Why a call could not be placed. Said on the server's stderr, never acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The request carried no tool-use id.
    NoCallId,
    /// The id is not one a record could be named for.
    BadCallId,
    /// No record: the hook is not registered, did not run, or could not write.
    NoRecord,
    /// A record that could not be read.
    Unreadable,
    /// A record that is not what this build writes.
    Malformed,
    /// A record older than [`MAX_AGE_MS`].
    Stale,
    /// A record made for a call to another tool.
    OtherTool,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Why::NoCallId => "the request carries no tool-use id",
            Why::BadCallId => "the tool-use id is not one a record can be named for",
            Why::NoRecord => "no PreToolUse hook record for this call",
            Why::Unreadable => "the hook record could not be read",
            Why::Malformed => "the hook record is not in a format this build reads",
            Why::Stale => "the hook record is too old to belong to this call",
            Why::OtherTool => "the hook record is for a call to another tool",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Known(Identity),
    Unknown(Why),
}

/// A tool-use id a record can be named for: letters, digits, `_` and `-`, so it cannot name a
/// path outside the directory it is joined to or a name Windows refuses.
pub fn is_call_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A session or agent id the record will carry: non-empty, bounded, no control characters.
fn is_field(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_FIELD && !s.chars().any(char::is_control)
}

/// The directory records live in: the ledger's, which the hook and the server both resolve
/// from `LUMEN_DB`, the pointer file or the per-OS default by the same rule.
pub fn record_dir(db: &Path) -> &Path {
    match db.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

pub fn record_path(dir: &Path, call_id: &str) -> PathBuf {
    dir.join(format!("{PREFIX}{call_id}{SUFFIX}"))
}

/// Why a record was not written.
#[derive(Debug)]
pub enum WriteError {
    /// The id, session or agent is not one a record can carry.
    Invalid(&'static str),
    Io(std::io::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Invalid(what) => write!(f, "{what}"),
            WriteError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Leave the record for `call_id`. Creates no directory: one that is missing means Lumen is
/// not where the hook was told it is, and making it would hide that.
pub fn write(
    dir: &Path,
    tool: &str,
    call_id: &str,
    who: &Identity,
    now_ms: i64,
) -> Result<(), WriteError> {
    if !is_call_id(call_id) {
        return Err(WriteError::Invalid("the tool_use_id is not usable"));
    }
    if !is_field(&who.session) {
        return Err(WriteError::Invalid("the session_id is not usable"));
    }
    if who.agent.as_deref().is_some_and(|a| !is_field(a)) {
        return Err(WriteError::Invalid("the agent_id is not usable"));
    }
    let body = json!({
        "v": FORMAT,
        "tool": tool,
        "session_id": who.session,
        "agent_id": who.agent,
        "ts_ms": now_ms,
    });
    std::fs::write(record_path(dir, call_id), body.to_string()).map_err(WriteError::Io)
}

/// Take the record for `call_id`: read it and remove it, so it serves this call only.
///
/// A record that cannot be removed is still used, and the failure is said on stderr: the
/// call is the right one, and the leftover is pruned by age like any other.
pub fn claim(dir: &Path, tool: &str, call_id: Option<&str>, now_ms: i64) -> Lookup {
    let Some(call_id) = call_id else {
        return Lookup::Unknown(Why::NoCallId);
    };
    if !is_call_id(call_id) {
        return Lookup::Unknown(Why::BadCallId);
    }
    let path = record_path(dir, call_id);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Lookup::Unknown(Why::NoRecord);
        }
        Err(e) => {
            eprintln!("lumen: cannot read {}: {e}", path.display());
            return Lookup::Unknown(Why::Unreadable);
        }
    };
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!(
            "lumen: cannot remove the identity record {}: {e}",
            path.display()
        );
    }
    parse(&text, tool, now_ms)
}

fn parse(text: &str, tool: &str, now_ms: i64) -> Lookup {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Lookup::Unknown(Why::Malformed);
    };
    if v["v"].as_i64() != Some(FORMAT) {
        return Lookup::Unknown(Why::Malformed);
    }
    let Some(session) = v["session_id"].as_str().filter(|s| is_field(s)) else {
        return Lookup::Unknown(Why::Malformed);
    };
    let agent = match &v["agent_id"] {
        Value::Null => None,
        Value::String(a) if is_field(a) => Some(a.clone()),
        _ => return Lookup::Unknown(Why::Malformed),
    };
    let Some(ts) = v["ts_ms"].as_i64() else {
        return Lookup::Unknown(Why::Malformed);
    };
    if now_ms.saturating_sub(ts) > MAX_AGE_MS {
        return Lookup::Unknown(Why::Stale);
    }
    if v["tool"].as_str().and_then(short_name) != Some(tool) {
        return Lookup::Unknown(Why::OtherTool);
    }
    Lookup::Known(Identity {
        session: session.to_string(),
        agent,
    })
}

/// What a prune did: how many records went, and the removals that failed.
#[derive(Debug, Default)]
pub struct Pruned {
    pub removed: usize,
    pub failed: Vec<(PathBuf, std::io::Error)>,
}

/// Remove the records nobody will claim: those past [`MAX_AGE_MS`], and the oldest beyond
/// [`MAX_RECORDS`]. Only files named like a record are looked at.
///
/// A record's age is the `ts_ms` it carries, or the file's modification time when it cannot be
/// read, so garbage in the directory goes too. A directory that cannot be listed prunes nothing.
pub fn prune(dir: &Path, now_ms: i64) -> Pruned {
    let mut out = Pruned::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut kept: Vec<(i64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
            continue;
        }
        // A directory that happens to be named like a record is not ours to remove.
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let born = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["ts_ms"].as_i64())
            .or_else(|| {
                let m = entry.metadata().ok()?.modified().ok()?;
                let d = m.duration_since(std::time::UNIX_EPOCH).ok()?;
                Some(d.as_millis() as i64)
            });
        // A record whose age cannot be told at all is as good as old.
        match born {
            Some(b) if now_ms.saturating_sub(b) <= MAX_AGE_MS => kept.push((b, path)),
            _ => remove(&path, &mut out),
        }
    }
    if kept.len() > MAX_RECORDS {
        kept.sort();
        let excess = kept.len() - MAX_RECORDS;
        for (_, path) in kept.drain(..excess) {
            remove(&path, &mut out);
        }
    }
    out
}

fn remove(path: &Path, out: &mut Pruned) {
    match std::fs::remove_file(path) {
        Ok(()) => out.removed += 1,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => out.failed.push((path.to_path_buf(), e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const NOW: i64 = 1_800_000_000_000;

    fn who(agent: Option<&str>) -> Identity {
        Identity {
            session: "s-1".into(),
            agent: agent.map(str::to_string),
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn the_matchers_are_the_tools_under_the_server_name() {
        let built: Vec<String> = TOOLS.iter().map(|t| format!("{TOOL_PREFIX}{t}")).collect();
        assert_eq!(built, MATCHERS);
        let plugin: Vec<String> = TOOLS
            .iter()
            .map(|t| format!("{PLUGIN_TOOL_PREFIX}{t}"))
            .collect();
        assert_eq!(plugin, PLUGIN_MATCHERS);
        for (m, tool) in MATCHERS
            .into_iter()
            .chain(PLUGIN_MATCHERS)
            .zip(TOOLS.into_iter().cycle())
        {
            assert!(
                m.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "{m} would be read as a pattern, not as a name"
            );
            assert!(is_lumen_tool(m));
            assert_eq!(short_name(m), Some(tool));
        }
        assert!(!is_lumen_tool("Read"));
        assert!(!is_lumen_tool("mcp__other__smart_read"));
        assert!(!is_lumen_tool("mcp__lumen__ping"));
        assert!(!is_lumen_tool("mcp__plugin_lumen_lumen__ping"));
        assert!(!is_lumen_tool("mcp__plugin_other_lumen__smart_read"));
    }

    #[test]
    fn a_record_made_under_the_plugin_name_serves_the_same_tool() {
        let d = TempDir::new().unwrap();
        write(
            d.path(),
            "mcp__plugin_lumen_lumen__smart_read",
            "toolu_p",
            &who(None),
            NOW,
        )
        .unwrap();
        assert_eq!(
            claim(d.path(), "smart_read", Some("toolu_p"), NOW),
            Lookup::Known(who(None))
        );
        write(
            d.path(),
            "mcp__plugin_lumen_lumen__recall_file",
            "toolu_q",
            &who(None),
            NOW,
        )
        .unwrap();
        assert_eq!(
            claim(d.path(), "smart_read", Some("toolu_q"), NOW),
            Lookup::Unknown(Why::OtherTool)
        );
    }

    #[test]
    fn a_record_round_trips_and_serves_one_call() {
        let d = TempDir::new().unwrap();
        let tool = "mcp__lumen__smart_read";
        write(d.path(), tool, "toolu_01", &who(Some("a1")), NOW).unwrap();
        assert_eq!(
            claim(d.path(), "smart_read", Some("toolu_01"), NOW + 5),
            Lookup::Known(who(Some("a1")))
        );
        assert!(names(d.path()).is_empty(), "claimed means consumed");
        assert_eq!(
            claim(d.path(), "smart_read", Some("toolu_01"), NOW + 6),
            Lookup::Unknown(Why::NoRecord)
        );
    }

    #[test]
    fn a_main_agent_has_no_agent_and_an_agent_is_a_different_owner() {
        assert_eq!(who(None).owner(), "s-1");
        assert_ne!(who(Some("a1")).owner(), who(None).owner());
        assert_ne!(who(Some("a1")).owner(), who(Some("a2")).owner());
        // No id can make one owner look like another's.
        let tricky = Identity {
            session: "s-1\0a1".into(),
            agent: None,
        };
        assert_eq!(tricky.owner(), who(Some("a1")).owner());
        assert!(!is_field(&tricky.session), "a control character is refused");
    }

    #[test]
    fn what_cannot_name_a_file_is_refused_before_it_touches_one() {
        for bad in ["", "../x", "a/b", "a\\b", "a b", "toolu:1", "x\0", "é"] {
            assert!(!is_call_id(bad), "{bad:?}");
        }
        assert!(!is_call_id(&"x".repeat(129)));
        assert!(is_call_id("toolu_01AbC-9"));
        let d = TempDir::new().unwrap();
        let err = write(d.path(), "t", "../escape", &who(None), NOW).unwrap_err();
        assert!(matches!(err, WriteError::Invalid(_)));
        assert!(names(d.path()).is_empty());
        assert_eq!(
            claim(d.path(), "smart_read", Some("../escape"), NOW),
            Lookup::Unknown(Why::BadCallId)
        );
        for session in ["", "a\nb"] {
            let bad = Identity {
                session: session.into(),
                agent: None,
            };
            assert!(write(d.path(), "t", "toolu_01", &bad, NOW).is_err());
        }
        let long = Identity {
            session: "x".repeat(MAX_FIELD + 1),
            agent: None,
        };
        assert!(write(d.path(), "t", "toolu_01", &long, NOW).is_err());
        let blank_agent = Identity {
            session: "s".into(),
            agent: Some(String::new()),
        };
        assert!(write(d.path(), "t", "toolu_01", &blank_agent, NOW).is_err());
    }

    #[test]
    fn every_way_a_record_can_be_wrong_is_unknown() {
        let d = TempDir::new().unwrap();
        let put = |id: &str, body: &str| std::fs::write(record_path(d.path(), id), body).unwrap();
        let good = |tool: &str, ts: i64| {
            format!(r#"{{"v":1,"tool":"{tool}","session_id":"s","agent_id":null,"ts_ms":{ts}}}"#)
        };
        put("a", "");
        put("b", "{");
        put("c", "[]");
        put(
            "d",
            r#"{"v":2,"tool":"mcp__lumen__smart_read","session_id":"s","ts_ms":1800000000000}"#,
        );
        put(
            "e",
            r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"","ts_ms":1800000000000}"#,
        );
        put(
            "f",
            r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":7,"ts_ms":1800000000000}"#,
        );
        put(
            "g",
            r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"s","agent_id":"","ts_ms":1800000000000}"#,
        );
        put(
            "h",
            r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"s","agent_id":3,"ts_ms":1800000000000}"#,
        );
        put(
            "i",
            r#"{"v":1,"tool":"mcp__lumen__smart_read","session_id":"s"}"#,
        );
        put("j", &good("mcp__lumen__smart_read", NOW - MAX_AGE_MS - 1));
        put("k", &good("mcp__lumen__recall_file", NOW));
        put("l", "\u{feff}{}");
        for id in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "l"] {
            assert_eq!(
                claim(d.path(), "smart_read", Some(id), NOW),
                Lookup::Unknown(Why::Malformed),
                "{id}"
            );
        }
        assert_eq!(
            claim(d.path(), "smart_read", Some("j"), NOW),
            Lookup::Unknown(Why::Stale)
        );
        assert_eq!(
            claim(d.path(), "smart_read", Some("k"), NOW),
            Lookup::Unknown(Why::OtherTool)
        );
        assert_eq!(
            claim(d.path(), "smart_read", None, NOW),
            Lookup::Unknown(Why::NoCallId)
        );
        // At the edge of the age it is still good.
        put("m", &good("mcp__lumen__smart_read", NOW - MAX_AGE_MS));
        assert!(matches!(
            claim(d.path(), "smart_read", Some("m"), NOW),
            Lookup::Known(_)
        ));
    }

    #[test]
    fn crlf_is_whitespace() {
        let d = TempDir::new().unwrap();
        let body = format!(
            "{{\r\n \"v\": 1,\r\n \"tool\": \"mcp__lumen__smart_read\",\r\n \"session_id\": \"s-1\",\r\n \"agent_id\": null,\r\n \"ts_ms\": {NOW}\r\n}}\r\n"
        );
        std::fs::write(record_path(d.path(), "x"), body).unwrap();
        assert_eq!(
            claim(d.path(), "smart_read", Some("x"), NOW),
            Lookup::Known(who(None))
        );
    }

    #[test]
    fn a_directory_that_is_not_there_is_an_error_not_a_directory() {
        let d = TempDir::new().unwrap();
        let gone = d.path().join("not there");
        let err = write(&gone, "t", "toolu_01", &who(None), NOW).unwrap_err();
        assert!(matches!(err, WriteError::Io(_)));
        assert!(!gone.exists(), "the writer made a directory");
        assert_eq!(
            claim(&gone, "smart_read", Some("toolu_01"), NOW),
            Lookup::Unknown(Why::NoRecord)
        );
        assert_eq!(prune(&gone, NOW).removed, 0);
    }

    #[test]
    fn the_record_directory_is_the_ledgers() {
        assert_eq!(
            record_dir(Path::new("/a b/c/lumen.db")),
            Path::new("/a b/c")
        );
        assert_eq!(record_dir(Path::new("lumen.db")), Path::new("."));
        assert_eq!(
            record_dir(Path::new(r"C:\Users\me\AppData\Roaming\lumen.db")).file_name(),
            Path::new(r"C:\Users\me\AppData\Roaming\lumen.db")
                .parent()
                .unwrap()
                .file_name()
        );
    }

    #[test]
    fn prune_takes_the_old_and_the_unreadable_and_nothing_else() {
        let d = TempDir::new().unwrap();
        let tool = "mcp__lumen__smart_read";
        // The real clock, because a file's modification time is on it.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        write(d.path(), tool, "old", &who(None), now - MAX_AGE_MS - 1).unwrap();
        write(d.path(), tool, "fresh", &who(None), now - 1000).unwrap();
        std::fs::write(record_path(d.path(), "garbage"), "{").unwrap();
        let aged = std::fs::File::options()
            .write(true)
            .open(record_path(d.path(), "garbage"))
            .unwrap();
        // The file says nothing about its age, so its modification time does.
        aged.set_modified(
            std::time::UNIX_EPOCH
                + std::time::Duration::from_millis((now - MAX_AGE_MS - 5000) as u64),
        )
        .unwrap();
        drop(aged);
        std::fs::write(record_path(d.path(), "garbage_new"), "{").unwrap();
        std::fs::write(d.path().join("faults.jsonl"), "x").unwrap();
        std::fs::write(d.path().join("lumen_call_notes.txt"), "x").unwrap();
        std::fs::create_dir(d.path().join("lumen_call_dir.json")).unwrap();

        let p = prune(d.path(), now);
        assert_eq!(p.removed, 2, "{:?}", p.failed);
        assert_eq!(
            names(d.path()),
            [
                "faults.jsonl",
                "lumen_call_dir.json",
                "lumen_call_fresh.json",
                "lumen_call_garbage_new.json",
                "lumen_call_notes.txt"
            ]
        );
    }

    #[test]
    fn a_denied_loop_cannot_fill_the_directory() {
        let d = TempDir::new().unwrap();
        let tool = "mcp__lumen__smart_read";
        for i in 0..(MAX_RECORDS + 40) {
            write(
                d.path(),
                tool,
                &format!("t{i:04}"),
                &who(None),
                NOW - 5000 + i as i64,
            )
            .unwrap();
        }
        assert_eq!(prune(d.path(), NOW).removed, 40);
        let left = names(d.path());
        assert_eq!(left.len(), MAX_RECORDS);
        // The newest survive: they are the ones whose prompts may still be open.
        assert!(left.contains(&format!("lumen_call_t{:04}.json", MAX_RECORDS + 39)));
        assert!(!left.contains(&"lumen_call_t0000.json".to_string()));
    }
}
