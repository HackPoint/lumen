//! Is Lumen's identity hook reaching the server? One answer, for every place that asks.
//!
//! The PreToolUse hook on Lumen's three tools is what tells the server which conversation a
//! call is from ([`crate::identity`]). Until 1.9.0 three places decided whether it was
//! registered, each its own way: Setup's validator and the app's prompt counted an exact matcher
//! with a Lumen command on it, `lumen doctor` counted any matcher that selects the tool with any
//! command at all, and the savings summary took whichever its surface passed. The same
//! settings.json could be "registered" in the terminal and "missing" in the window. They all call
//! here now.
//!
//! Two accounts, and the ledger's wins. settings.json is the first: [`registered`]. But a plugin
//! install registers its hooks from the plugin, not from that file, so the file alone would
//! accuse a machine that works. What the server has answered is the second: among the newest
//! calls that carry a repeat plan, any one identified means the hook reaches the server, and all
//! of them unidentified means it does not ([`reaches_server`]).

use serde_json::Value;
use std::path::Path;

use crate::identity::{MATCHERS, TOOLS};

/// Whether `command` is one of Lumen's: it names one of the `lumen_` hook scripts. Another
/// tool's hook on the same matcher says nothing about Lumen.
pub fn is_lumen_command(command: &str) -> bool {
    command.contains("lumen_")
}

/// Whether a hook `matcher` selects `tool_name`. Claude Code reads a matcher made only of
/// letters, digits, `_` and `|` as an exact name or a list of them, and anything else as a
/// regular expression; the spellings of "everything" (`""`, `*`, `.*`) and a trailing `.*` are
/// the only regular expressions worth recognising here, since a wrongly raised finding sends
/// the reader after something that is not broken.
pub fn matcher_selects(matcher: &str, tool_name: &str) -> bool {
    let m = matcher.trim();
    if m.is_empty() || m == "*" || m == ".*" {
        return true;
    }
    if let Some(prefix) = m.strip_suffix(".*") {
        return tool_name.starts_with(prefix);
    }
    m.split('|').any(|name| name == tool_name)
}

/// The Lumen tools, as their short names, that no PreToolUse entry in `settings` runs a Lumen
/// command before.
///
/// An entry counts when its `matcher` is a string that selects the tool ([`matcher_selects`])
/// and one of its `hooks` is a command of Lumen's ([`is_lumen_command`]). An entry with no
/// `matcher` key is not read as a catch-all: Setup never writes one, and a file that holds one
/// was edited by hand, so it is not taken as proof. PostToolUse runs after the tool has
/// answered, too late to say whose call it was, and does not count. Read by index, so a file
/// of any shape is an answer and never a panic.
pub fn missing(settings: &Value) -> Vec<&'static str> {
    let none = Vec::new();
    let entries = settings["hooks"]["PreToolUse"].as_array().unwrap_or(&none);
    TOOLS
        .iter()
        .zip(MATCHERS)
        .filter(|(_, full)| {
            !entries.iter().any(|e| {
                let Some(matcher) = e["matcher"].as_str() else {
                    return false;
                };
                matcher_selects(matcher, full)
                    && e["hooks"].as_array().is_some_and(|hooks| {
                        hooks
                            .iter()
                            .any(|h| h["command"].as_str().is_some_and(is_lumen_command))
                    })
            })
        })
        .map(|(short, _)| *short)
        .collect()
}

/// settings.json runs Lumen's hook before each of the three Lumen tools.
pub fn registered(settings: &Value) -> bool {
    missing(settings).is_empty()
}

/// [`registered`] for the settings.json at `path`, read the way Setup reads it: an absent or
/// blank file is a machine never set up, which registers nothing; a file that cannot be read,
/// is not JSON, or is not a JSON object says nothing (`None`).
pub fn registered_in(path: &Path) -> Option<bool> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
        Ok(text) if text.trim().is_empty() => Some(false),
        Ok(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .filter(Value::is_object)
            .map(|v| registered(&v)),
    }
}

/// How many of the newest calls are read: those that carry a repeat plan, through the three
/// Lumen routes.
pub const SAMPLE: usize = 50;

/// `(unidentified, all)` of the newest [`SAMPLE`] Lumen calls that carry a repeat plan, in the
/// ledger. `?1` is [`crate::repeat::PLAN_UNIDENTIFIED`].
pub const RECENT_PLANS_SQL: &str = "SELECT COALESCE(SUM(repeat_plan = ?1), 0), COUNT(*) FROM (
         SELECT repeat_plan FROM read_events
         WHERE routed_via IN ('smart_read', 'recall_file', 'compress_logs')
           AND repeat_plan IS NOT NULL
         ORDER BY rowid DESC LIMIT 50)";

/// [`RECENT_PLANS_SQL`] on the ledger at `db`, opened read-only. `None` when it cannot be opened
/// or has no such column (a ledger from before 1.7.0): that proves nothing either way.
pub fn recent_plans(db: &Path) -> Option<(u64, u64)> {
    let conn = crate::meter::open_read_only(db).ok()?;
    let (unidentified, all): (i64, i64) = conn
        .query_row(RECENT_PLANS_SQL, [crate::repeat::PLAN_UNIDENTIFIED], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()?;
    Some((u64::try_from(unidentified).ok()?, u64::try_from(all).ok()?))
}

/// The same tally over rows already in hand, newest first, as `(routed_via, repeat_plan)`: what
/// the savings engine reads from the rows it loaded, by the rule [`RECENT_PLANS_SQL`] applies.
pub fn tally<'a>(newest_first: impl Iterator<Item = (&'a str, Option<&'a str>)>) -> (u64, u64) {
    let mut unidentified = 0;
    let mut all = 0;
    for (_, plan) in newest_first
        .filter(|(route, plan)| {
            plan.is_some() && ["smart_read", "recall_file", "compress_logs"].contains(route)
        })
        .take(SAMPLE)
    {
        all += 1;
        if plan == Some(crate::repeat::PLAN_UNIDENTIFIED) {
            unidentified += 1;
        }
    }
    (unidentified, all)
}

/// Whether the hook reaches the server. The ledger's evidence wins: any recent call identified
/// is yes, every recent call unidentified is no. With no evidence, settings.json decides, and
/// with neither (`None`) nothing is known.
pub fn reaches_server(registered: Option<bool>, recent: Option<(u64, u64)>) -> Option<bool> {
    match recent {
        Some((unidentified, all)) if all > 0 => Some(unidentified < all),
        _ => registered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const LUMEN: &str = "/Users/me/.claude/lumen/lumen_read_intercept.sh";
    const FOREIGN: &str = "/opt/someone-elses-tool.sh";

    fn entry(matcher: &str, commands: &[&str]) -> Value {
        json!({
            "matcher": matcher,
            "hooks": commands.iter().map(|c| json!({"type": "command", "command": c})).collect::<Vec<_>>(),
        })
    }

    fn pre(entries: Vec<Value>) -> Value {
        json!({"hooks": {"PreToolUse": entries}})
    }

    /// Every shape the CLI's and the app's tests held their predicates to, with the one answer
    /// both now give.
    pub fn shapes() -> Vec<(Value, bool)> {
        let [s, r, c] = MATCHERS;
        vec![
            // What Setup installs.
            (
                pre(vec![
                    entry("Read", &[LUMEN]),
                    entry(s, &[LUMEN]),
                    entry(r, &[LUMEN]),
                    entry(c, &[LUMEN]),
                ]),
                true,
            ),
            // A list, and the catch-alls, with Lumen's command.
            (pre(vec![entry(&format!("{s}|{r}|{c}"), &[LUMEN])]), true),
            (pre(vec![entry("mcp__lumen__.*", &[LUMEN])]), true),
            (pre(vec![entry(".*", &[LUMEN])]), true),
            (pre(vec![entry("*", &[LUMEN])]), true),
            (pre(vec![entry("", &[LUMEN])]), true),
            // Each one missing.
            (pre(vec![entry(&format!("{s}|{c}"), &[LUMEN])]), false),
            (pre(vec![entry(s, &[LUMEN]), entry(r, &[LUMEN])]), false),
            // Another tool's command on all three, and beside Lumen's.
            (
                pre(vec![
                    entry(s, &[FOREIGN]),
                    entry(r, &[FOREIGN]),
                    entry(c, &[FOREIGN]),
                ]),
                false,
            ),
            (
                pre(vec![
                    entry(s, &[FOREIGN, LUMEN]),
                    entry(r, &[LUMEN, FOREIGN]),
                    entry(c, &[LUMEN]),
                ]),
                true,
            ),
            // After the tool, not before it.
            (
                json!({"hooks": {"PostToolUse": [entry(s, &[LUMEN]), entry(r, &[LUMEN]), entry(c, &[LUMEN])]}}),
                false,
            ),
            // Another server's tools; a hook with no command.
            (pre(vec![entry("mcp__other__smart_read", &[LUMEN])]), false),
            (pre(vec![json!({"matcher": s, "hooks": []})]), false),
            // What 1.7.0 left, and nothing.
            (pre(vec![entry("Read", &[LUMEN])]), false),
            (json!({}), false),
            (json!({"hooks": {}}), false),
            // Any shape at all.
            (json!(null), false),
            (json!(7), false),
            (json!([]), false),
            (json!({"hooks": null}), false),
            (json!({"hooks": {"PreToolUse": {}}}), false),
            (
                json!({"hooks": {"PreToolUse": [null, 3, "x", [], {}]}}),
                false,
            ),
            (
                json!({"hooks": {"PreToolUse": [{"matcher": s, "hooks": "x"}]}}),
                false,
            ),
            (
                json!({"hooks": {"PreToolUse": [{"matcher": s, "hooks": [null, 1, {"command": 7}]}]}}),
                false,
            ),
            (
                json!({"hooks": {"PreToolUse": [{"hooks": [{"command": LUMEN}]}]}}),
                false,
            ),
            (
                json!({"hooks": {"PreToolUse": [{"matcher": 7, "hooks": [{"command": LUMEN}]}]}}),
                false,
            ),
        ]
    }

    #[test]
    fn every_shape_gets_the_one_answer() {
        for (v, want) in shapes() {
            assert_eq!(registered(&v), want, "{v}");
        }
    }

    #[test]
    fn the_missing_tools_are_named() {
        let [s, _, c] = MATCHERS;
        assert_eq!(
            missing(&pre(vec![entry(&format!("{s}|{c}"), &[LUMEN])])),
            ["recall_file"]
        );
        assert_eq!(missing(&json!({})), TOOLS);
    }

    #[test]
    fn a_settings_file_is_read_as_setup_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("settings.json");
        assert_eq!(registered_in(&f), Some(false), "absent: never set up");
        for (text, want) in [
            ("", Some(false)),
            ("  \n", Some(false)),
            ("{", None),
            ("[]", None),
            ("{}", Some(false)),
        ] {
            std::fs::write(&f, text).unwrap();
            assert_eq!(registered_in(&f), want, "{text:?}");
        }
        std::fs::write(&f, pre(vec![entry(".*", &[LUMEN])]).to_string()).unwrap();
        assert_eq!(registered_in(&f), Some(true));
    }

    #[test]
    fn the_ledger_overrules_the_settings_file_either_way() {
        assert_eq!(reaches_server(Some(false), Some((49, 50))), Some(true));
        assert_eq!(reaches_server(Some(true), Some((50, 50))), Some(false));
        assert_eq!(reaches_server(Some(true), Some((0, 0))), Some(true));
        assert_eq!(reaches_server(None, None), None);
        let rows = [
            ("smart_read", Some("unidentified")),
            ("builtin_read", Some("first")),
            ("recall_file", None),
        ];
        assert_eq!(tally(rows.into_iter()), (1, 1));
    }
}
