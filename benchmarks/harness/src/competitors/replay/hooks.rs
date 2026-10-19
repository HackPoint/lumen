//! The hooks a tool registered in Claude Code's `settings.json`, run the way Claude Code runs them.
//!
//! A tool's installer writes its hooks into `$HOME/.claude/settings.json`. Replaying through what
//! the installer really wrote (and not through a list kept here) means a change in the tool's
//! registration (a new matcher, a different command) changes the replay with it.
//!
//! Claude Code's contract, from its hooks documentation: for each event, every hook whose
//! `matcher` matches the tool name runs as a shell command with the event's JSON on stdin and
//! the project directory as its working directory. Exit 0 with JSON on stdout is a decision:
//! `hookSpecificOutput.updatedInput` (PreToolUse) replaces the tool's input, and
//! `hookSpecificOutput.updatedToolOutput` (PostToolUse) replaces the tool's result. Exit 2
//! blocks. Any other status is a non-blocking error and the call proceeds unchanged.

use crate::sandbox::Sandbox;
use crate::util::{Ctx, Res};
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// One registered hook command and the tools it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub event: String,
    /// `None` matches every tool.
    pub matcher: Option<Vec<String>>,
    pub command: String,
}

/// What one hook run produced.
#[derive(Debug, Clone)]
pub struct HookRun {
    pub command: String,
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// What the hooks of one event decided, together.
#[derive(Debug, Clone, Default)]
pub struct Decision {
    /// The last `updatedInput` a hook gave.
    pub updated_input: Option<Value>,
    /// The last `updatedToolOutput` a hook gave.
    pub updated_output: Option<Value>,
    /// A hook exited 2: the call is blocked, and this is what the model is told.
    pub blocked: Option<String>,
    pub runs: Vec<HookRun>,
}

/// Parse the hooks of a `settings.json`. A matcher is a `|` alternation of exact tool names, or
/// `*`, or empty; anything else (a real regular expression) is refused rather than guessed at.
pub fn parse(settings: &str) -> Res<Vec<Hook>> {
    let v: Value = serde_json::from_str(settings).ctx("parse settings.json")?;
    let mut out = Vec::new();
    let Some(events) = v.get("hooks").and_then(Value::as_object) else {
        return Ok(out);
    };
    for (event, entries) in events {
        for e in entries.as_array().into_iter().flatten() {
            let matcher = match e.get("matcher").and_then(Value::as_str) {
                None | Some("") | Some("*") => None,
                Some(m) => {
                    if !m
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'|'))
                    {
                        return Err(format!(
                            "the {event} matcher {m:?} is a regular expression, which this \
                             replay does not interpret"
                        ));
                    }
                    Some(m.split('|').map(str::to_string).collect())
                }
            };
            for h in e
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if h.get("type").and_then(Value::as_str) != Some("command") {
                    continue;
                }
                if let Some(c) = h.get("command").and_then(Value::as_str) {
                    out.push(Hook {
                        event: event.clone(),
                        matcher: matcher.clone(),
                        command: c.to_string(),
                    });
                }
            }
        }
    }
    Ok(out)
}

pub fn load(settings: &Path) -> Res<Vec<Hook>> {
    let text = std::fs::read_to_string(settings).ctx(&format!(
        "read {} (did the installer run?)",
        settings.display()
    ))?;
    parse(&text)
}

impl Hook {
    pub fn matches(&self, event: &str, tool: &str) -> bool {
        self.event == event
            && self
                .matcher
                .as_ref()
                .is_none_or(|m| m.iter().any(|t| t == tool))
    }
}

/// Run every hook of `event` that matches `tool`, in settings order, each on `payload`.
///
/// `path` is the `PATH` the hooks see (the shim directory first, then the tool's own `bin`).
/// `cwd` is the project directory.
pub fn run_event(
    hooks: &[Hook],
    event: &str,
    tool: &str,
    payload: &Value,
    sb: &Sandbox,
    path: &str,
    cwd: &Path,
) -> Res<Decision> {
    let mut d = Decision::default();
    let body = payload.to_string();
    for h in hooks.iter().filter(|h| h.matches(event, tool)) {
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        cmd.arg(if cfg!(windows) { "/C" } else { "-c" })
            .arg(&h.command);
        sb.apply(&mut cmd);
        cmd.env("PATH", path)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().ctx(&format!("spawn the hook {}", h.command))?;
        if let Some(mut stdin) = child.stdin.take() {
            // A hook that exits without reading closes the pipe; that is its business.
            let _ = stdin.write_all(body.as_bytes());
        }
        let out = child.wait_with_output().ctx("wait for the hook")?;
        let run = HookRun {
            command: h.command.clone(),
            exit: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        match run.exit {
            Some(0) => {
                if let Ok(v) = serde_json::from_str::<Value>(run.stdout.trim()) {
                    let hso = &v["hookSpecificOutput"];
                    if let Some(i) = hso.get("updatedInput") {
                        d.updated_input = Some(i.clone());
                    }
                    if let Some(o) = hso.get("updatedToolOutput") {
                        d.updated_output = Some(o.clone());
                    }
                }
            }
            Some(2) => d.blocked = Some(run.stderr.clone()),
            _ => {}
        }
        d.runs.push(run);
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `squeez setup` wrote at the pinned release (paths shortened).
    const SQUEEZ: &str = r#"{"hooks":{
      "PreToolUse":[{"matcher":"Bash|Read|Grep|Glob|Agent|Task","hooks":[{"type":"command","command":"bash pre.sh"}]}],
      "PostToolUse":[{"hooks":[{"type":"command","command":"bash post.sh"}]},
                     {"matcher":"Edit|Write|Bash","hooks":[{"type":"command","command":"bash buddy.sh"}]}],
      "Stop":[{"hooks":[{"type":"command","command":"bash stop.sh"}]}]}}"#;

    /// What `rtk init -g --auto-patch` wrote at v1.0.1.
    const RTK: &str = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"rtk hook claude"}]}]}}"#;

    #[test]
    fn the_registrations_of_squeez_and_rtk_select_the_tools_they_hook() {
        let sq = parse(SQUEEZ).unwrap();
        let names = |h: &[Hook], ev: &str, tool: &str| -> Vec<String> {
            h.iter()
                .filter(|x| x.matches(ev, tool))
                .map(|x| x.command.clone())
                .collect()
        };
        assert_eq!(names(&sq, "PreToolUse", "Read"), ["bash pre.sh"]);
        assert_eq!(names(&sq, "PreToolUse", "WebFetch"), Vec::<String>::new());
        assert_eq!(names(&sq, "PostToolUse", "Read"), ["bash post.sh"]);
        assert_eq!(
            names(&sq, "PostToolUse", "Bash"),
            ["bash post.sh", "bash buddy.sh"]
        );
        let rtk = parse(RTK).unwrap();
        // RTK hooks Bash and nothing else: a Read or a Grep never reaches it.
        assert_eq!(names(&rtk, "PreToolUse", "Bash"), ["rtk hook claude"]);
        assert!(names(&rtk, "PreToolUse", "Read").is_empty());
        assert!(names(&rtk, "PostToolUse", "Bash").is_empty());
    }

    #[test]
    fn a_regular_expression_matcher_is_refused_not_guessed() {
        let s = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash.*","hooks":[{"type":"command","command":"x"}]}]}}"#;
        assert!(parse(s).unwrap_err().contains("regular expression"));
    }

    #[test]
    fn settings_with_no_hooks_have_none() {
        assert!(parse("{}").unwrap().is_empty());
    }
}
