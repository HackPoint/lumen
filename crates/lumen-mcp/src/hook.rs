// lumen-mcp hook — the Claude Code hooks, in Rust.
//
// Until 1.6.0 the hooks were bash scripts that drove python3, stat, wc, mktemp and
// date. Every one of those was a portability hazard and one of them was fatal: with
// no `python3` on PATH — the python.org layout on Windows, which provides `python`
// and `py` only — the meter wrote nothing and the intercept blocked nothing, and
// neither said so, because the fault recorder needed python3 as well. The installed
// scripts are now shims that exec this; the one thing left in shell is reporting
// that this binary is missing.
//
// Claude Code's contract: the payload arrives as JSON on stdin; exit 0 lets the tool
// call proceed; exit 2 from a PreToolUse hook blocks it and hands stderr to the
// model; any other status is a non-blocking error. So the meter always exits 0 and
// the intercept exits 2 only when it has decided to block.
//
// Every way a hook can lose an event leaves a fault in the spool and a line on
// stderr. Neither hook creates directories: a missing data directory means Lumen is
// not installed where the hook was told it is, and creating one would hide that.

use lumen_core::{
    coverage::{DEFAULT_LINE_THRESHOLD, INTERCEPTED_SOURCE_EXTS, LOG_EXTS, ext_of},
    faults::{self, FaultRecord},
    meter::{self, HookReadEvent, METER_WRITE_FAILED},
    tokenizer::count_tokens,
};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Fault kind for an intercept that let through a read it would have redirected.
pub const HOOK_FAIL_OPEN: &str = "hook_fail_open";

/// `writer_hook` when the caller passes no `--writer`.
pub const DEFAULT_WRITER: &str = "lumen-mcp hook meter";

const USAGE: &str = "usage: lumen-mcp hook <meter|intercept> [--writer NAME] < payload.json";

/// What a hook run decided.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// 0 lets the tool call proceed. 2, from the intercept only, blocks it.
    pub exit: i32,
    /// Written to stderr in order. On exit 2 Claude Code hands this to the model.
    pub stderr: Vec<String>,
}

/// The process state a hook reads, passed in so a test never touches the real
/// environment, the real temp directory or the real ledger.
pub struct Env<'a> {
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// Where the intercept keeps its once-per-session markers.
    pub state_dir: &'a Path,
}

impl Env<'_> {
    /// A variable that is set and non-empty. Empty counts as unset, as `${VAR:-default}`
    /// did in the shell hooks.
    fn get(&self, key: &str) -> Option<String> {
        (self.var)(key).filter(|v| !v.is_empty())
    }

    fn db_path(&self) -> Option<PathBuf> {
        let home = self.get("HOME").or_else(|| self.get("USERPROFILE"));
        meter::resolve_db_path(self.get("LUMEN_DB").as_deref(), home.as_deref())
    }

    /// `LUMEN_FAULT_SPOOL`, else `faults.jsonl` beside the database — the same rule as
    /// [`faults::spool_path`], over the injected environment.
    fn spool_path(&self) -> Option<PathBuf> {
        if let Some(p) = self.get("LUMEN_FAULT_SPOOL") {
            return Some(PathBuf::from(p));
        }
        Some(self.db_path()?.parent()?.join("faults.jsonl"))
    }
}

/// `lumen-mcp hook <meter|intercept> [--writer NAME]`, as a process. Returns the
/// exit status.
pub fn run(args: &[String]) -> i32 {
    let mut input = Vec::new();
    if let Err(e) = std::io::stdin().lock().read_to_end(&mut input) {
        // The empty payload that follows is recorded as a bad payload; this line says why.
        eprintln!("lumen: cannot read the hook payload from stdin: {e}");
        input.clear();
    }
    let var = |k: &str| std::env::var(k).ok();
    let state_dir = std::env::temp_dir();
    let out = run_with(
        args,
        &input,
        &Env {
            var: &var,
            state_dir: &state_dir,
        },
    );
    for line in &out.stderr {
        eprintln!("{line}");
    }
    out.exit
}

/// The hook with every input explicit. A panic is caught and recorded: a hook that
/// dies takes its row with it, and a panic message on stderr alone is seen by nobody.
pub fn run_with(args: &[String], input: &[u8], env: &Env) -> Outcome {
    let sub = args.first().map(String::as_str).unwrap_or("");
    if env.get("LUMEN_DEBUG").as_deref() == Some("1") {
        // How fixtures get captured. Best-effort: a debugging aid must not cost a row.
        let _ = std::fs::write(env.state_dir.join("lumen_hook_dump.json"), input);
    }
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match sub {
        "meter" => meter(input, env, writer(&args[1..])),
        "intercept" => intercept(input, env),
        _ => Outcome {
            exit: 1,
            stderr: vec![USAGE.to_string()],
        },
    }));
    caught.unwrap_or_else(|panic| {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string());
        let mut run = if sub == "intercept" {
            Run::intercept(env)
        } else {
            Run::meter(env)
        };
        run.say(format!(
            "lumen: the {sub} hook panicked: {msg}; {}",
            run.consequence
        ));
        run.fault("panic", None, None, Some(msg));
        run.out
    })
}

fn writer(args: &[String]) -> &str {
    args.iter()
        .position(|a| a == "--writer")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
        .unwrap_or(DEFAULT_WRITER)
}

/// One hook run: the stderr it is building, and where its faults go.
struct Run<'a> {
    env: &'a Env<'a>,
    kind: &'static str,
    /// What losing this event means, for the stderr line.
    consequence: &'static str,
    session: Option<String>,
    out: Outcome,
}

impl<'a> Run<'a> {
    fn meter(env: &'a Env<'a>) -> Self {
        Self::new(env, METER_WRITE_FAILED, "this event was not metered")
    }

    fn intercept(env: &'a Env<'a>) -> Self {
        Self::new(env, HOOK_FAIL_OPEN, "the read was let through")
    }

    fn new(env: &'a Env<'a>, kind: &'static str, consequence: &'static str) -> Self {
        Self {
            env,
            kind,
            consequence,
            session: None,
            out: Outcome::default(),
        }
    }

    fn say(&mut self, line: impl Into<String>) {
        self.out.stderr.push(line.into());
    }

    /// Append one fault to the spool. `LUMEN_CAPTURE=0` drops the record and keeps the
    /// stderr line, as it did in the shell hooks.
    fn fault(
        &mut self,
        variant: &str,
        path: Option<&str>,
        lines: Option<i64>,
        detail: Option<String>,
    ) {
        if self.env.get("LUMEN_CAPTURE").as_deref() == Some("0") {
            return;
        }
        let mut rec = FaultRecord::now_with_env(self.kind, variant, self.env.var);
        rec.path = path.map(str::to_string);
        rec.lines = lines;
        rec.detail = detail;
        // The payload names the session the event belongs to; the environment only
        // names the process that ran the hook.
        if self.session.is_some() {
            rec.session_id = self.session.clone();
        }
        let Some(spool) = self.env.spool_path() else {
            self.say(
                "lumen: no fault spool resolved (LUMEN_FAULT_SPOOL and LUMEN_DB unset, no home \
                 directory); the fault above was not recorded",
            );
            return;
        };
        if let Err(e) = faults::try_record_at(&spool, &rec) {
            self.say(format!(
                "lumen: could not write the fault spool {} either: {e}",
                spool.display()
            ));
        }
    }

    fn bad_payload(&mut self, detail: String) {
        self.say(format!("lumen: {detail}; {}", self.consequence));
        self.fault("bad_payload", None, None, Some(detail));
    }

    /// Parse the payload, or say why not and record it.
    fn payload(&mut self, input: &[u8]) -> Option<Value> {
        match serde_json::from_slice::<Value>(input) {
            Ok(v) if v.is_object() => Some(v),
            Ok(_) => {
                self.bad_payload("the hook payload is not a JSON object".to_string());
                None
            }
            Err(e) => {
                self.bad_payload(format!("the hook payload is not JSON ({e})"));
                None
            }
        }
    }

    fn insert(&mut self, ev: &HookReadEvent) {
        let Some(db) = self.env.db_path() else {
            self.say(format!(
                "lumen: no database path resolved (LUMEN_DB unset, no home directory); this {} \
                 was not metered",
                ev.tool
            ));
            self.fault("no_db_path", Some(ev.path), ev.lines, None);
            return;
        };
        if let Err(e) = meter::insert_hook_event_at(&db, ev) {
            // SQLite names the file in some errors and not in others.
            let shown = db.display().to_string();
            let at = if e.detail.contains(&shown) {
                String::new()
            } else {
                format!(" ({shown})")
            };
            self.say(format!("lumen: {e}{at}; this {} was not metered", ev.tool));
            self.fault(e.stage, Some(ev.path), ev.lines, Some(e.detail));
        }
    }
}

/// A non-empty string at a JSON pointer.
fn str_at<'v>(d: &'v Value, pointer: &str) -> Option<&'v str> {
    d.pointer(pointer)?.as_str().filter(|s| !s.is_empty())
}

/// The payload's path, joined to the payload's `cwd` when relative. Claude Code sends
/// absolute paths; this is for the version that does not.
fn resolve(raw: &str, d: &Value) -> PathBuf {
    let p = Path::new(raw);
    match str_at(d, "/cwd") {
        Some(cwd) if p.is_relative() => Path::new(cwd).join(p),
        _ => p.to_path_buf(),
    }
}

// ── meter (PostToolUse on Read and Bash) ─────────────────────────────────────

fn meter(input: &[u8], env: &Env, writer: &str) -> Outcome {
    let mut run = Run::meter(env);
    let Some(d) = run.payload(input) else {
        return run.out;
    };
    run.session = str_at(&d, "/session_id")
        .map(str::to_string)
        .or_else(|| env.get("CLAUDE_CODE_SESSION_ID"));
    let Some(tool) = str_at(&d, "/tool_name") else {
        run.bad_payload("the hook payload has no tool_name".to_string());
        return run.out;
    };
    let channel = meter::channel_from(env.var);
    match tool {
        "Read" => meter_read(&mut run, &d, channel, writer),
        // LUMEN_METER_BASH=0 records no command output. It goes in the `env` of
        // ~/.claude/settings.json, which Setup keeps; removing the Bash entry instead
        // lasts only until the next Setup run puts it back.
        "Bash" if env.get("LUMEN_METER_BASH").as_deref() != Some("0") => {
            meter_bash(&mut run, &d, channel, writer)
        }
        _ => {}
    }
    run.out
}

/// A built-in Read: the "missed optimization" baseline.
///
/// `full_tokens` is the whole file even when the Read took an `offset`/`limit` slice,
/// exactly as the shell meter recorded it.
fn meter_read(run: &mut Run, d: &Value, channel: &str, writer: &str) {
    let Some(raw) = str_at(d, "/tool_input/file_path") else {
        run.bad_payload("a Read payload with no tool_input.file_path".to_string());
        return;
    };
    let path = resolve(raw, d);
    let shown = path.to_string_lossy().into_owned();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            run.say(format!(
                "lumen: cannot read {shown} to meter it ({e}); this Read was not metered"
            ));
            run.fault("read_file", Some(&shown), None, Some(e.to_string()));
            return;
        }
    };
    // `wc -l`: newline bytes, so a last line without one is not counted. The ledger has
    // always held this figure and `coverage::classify` compares against it.
    let lines = bytes.iter().filter(|&&b| b == b'\n').count() as i64;
    // Not UTF-8 means no token count exists. 0 labelled `unsupported` is that answer;
    // bytes/4 overstated a PNG by ~40x.
    let (tokens, source) = match std::str::from_utf8(&bytes) {
        Ok(text) => (count_tokens(text) as i64, "measured"),
        Err(_) => (0, "unsupported"),
    };
    let session = run.session.clone();
    run.insert(&HookReadEvent {
        tool: "Read",
        path: &shown,
        lines: Some(lines),
        tokens_returned: tokens,
        full_tokens: tokens,
        routed_via: "builtin_read",
        channel,
        session_id: session.as_deref(),
        file_mtime: crate::file_mtime(&shown),
        req_key: Some(&shown),
        writer_hook: writer,
        token_source: source,
    });
}

/// Bash output volume. Observation only: nothing is intercepted or wrapped.
fn meter_bash(run: &mut Run, d: &Value, channel: &str, writer: &str) {
    // Claude Code 2.1.270 merges stderr into `stdout` and sends `"stderr": ""`. Both are
    // read, so a version that separates them again is still counted in full.
    let output = match d.get("tool_response") {
        Some(Value::Object(r)) if r.contains_key("stdout") || r.contains_key("stderr") => {
            let field = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("");
            format!("{}{}", field("stdout"), field("stderr"))
        }
        Some(Value::String(s)) => s.clone(),
        // Not "no output": no output is `"stdout": ""`. A response without the fields
        // means the payload changed shape, and every Bash row would vanish without it.
        _ => {
            run.bad_payload("a Bash payload with no stdout or stderr in tool_response".to_string());
            return;
        }
    };
    let tokens = count_tokens(&output) as i64;
    // A Read is a datum even when the file is empty. A command that printed nothing
    // carries nothing to measure.
    if tokens == 0 {
        return;
    }
    let label = cmd_label(str_at(d, "/tool_input/command").unwrap_or(""));
    let session = run.session.clone();
    run.insert(&HookReadEvent {
        tool: "Bash",
        path: &label,
        lines: None,
        tokens_returned: 0,
        full_tokens: tokens,
        routed_via: "bash_output",
        channel,
        session_id: session.as_deref(),
        file_mtime: None,
        req_key: None,
        writer_hook: writer,
        token_source: "measured",
    });
}

/// Program and subcommand only — `cargo test`, `git status`, `npm run`.
///
/// Not the whole command line: one routinely carries credentials
/// (`curl -H "Authorization: Bearer …"`, `psql postgres://u:p@host`), and this value
/// lands in a database that gets backed up and copied around. Two tokens fully answer
/// "output volume by kind of command". Leading `VAR=value` assignments are dropped
/// first, so `TOKEN=secret curl …` records `curl`.
pub fn cmd_label(command: &str) -> String {
    let words: Vec<&str> = command
        .split_whitespace()
        .skip_while(|t| t.contains('=') && !t.starts_with('-'))
        .take(2)
        .collect();
    words.join(" ").chars().take(60).collect()
}

// ── intercept (PreToolUse on Read) ───────────────────────────────────────────

fn intercept(input: &[u8], env: &Env) -> Outcome {
    let mut run = Run::intercept(env);
    if env.get("LUMEN_HOOK_ENABLED").as_deref() == Some("0") {
        return run.out;
    }
    let threshold = match env.get("LUMEN_LINE_THRESHOLD") {
        None => DEFAULT_LINE_THRESHOLD,
        Some(v) => match v.trim().parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                run.say(format!(
                    "lumen: LUMEN_LINE_THRESHOLD={v} is not a whole number; using \
                     {DEFAULT_LINE_THRESHOLD}"
                ));
                DEFAULT_LINE_THRESHOLD
            }
        },
    };
    let Some(d) = run.payload(input) else {
        return run.out;
    };
    run.session = str_at(&d, "/session_id").map(str::to_string);
    if str_at(&d, "/tool_name") != Some("Read") {
        return run.out;
    }
    let Some(raw) = str_at(&d, "/tool_input/file_path") else {
        return run.out;
    };
    let path = resolve(raw, &d);
    if !path.is_file() {
        return run.out;
    }
    let shown = path.to_string_lossy().into_owned();
    let ext = ext_of(&shown);
    let is_log = if INTERCEPTED_SOURCE_EXTS.contains(&ext.as_str()) {
        false
    } else if LOG_EXTS.contains(&ext.as_str()) {
        true
    } else {
        return run.out;
    };
    let lines = match count_lines(&path) {
        Ok(n) => n,
        // The built-in Read will meet the same error and report it to the model. A gate
        // that cannot see the file has no grounds to block it.
        Err(e) => {
            run.say(format!(
                "lumen: cannot count lines in {shown} ({e}); {}",
                run.consequence
            ));
            return run.out;
        }
    };
    if lines < threshold {
        return run.out;
    }

    // Below the extension and threshold checks, not above them: the escape valve firing
    // means "this read would have been redirected and could not be", so a file that was
    // never going to be intercepted must not be recorded as a routing failure.
    //
    // One redirect per file per session. A second Read of the same file means the model
    // was told about Lumen and came back anyway, so that route failed; blocking again
    // would strand it.
    let marker = env.state_dir.join(format!(
        "lumen_intercept_{}",
        session_key(run.session.as_deref())
    ));
    let seen = match std::fs::read_to_string(&marker) {
        Ok(text) => text.lines().any(|l| l == shown),
        // No marker yet. An unreadable one fails the append below, which fails open.
        Err(_) => false,
    };
    if seen {
        run.fault("retry_escape_valve", Some(&shown), Some(lines), None);
        return run.out;
    }
    if let Err(e) = append_marker(&marker, &shown) {
        // Without the marker the retry the message promises would be blocked too, and
        // the model would loop. The shell hook swallowed this error and blocked anyway.
        run.say(format!(
            "lumen: cannot write the intercept marker {} ({e}); {}",
            marker.display(),
            run.consequence
        ));
        run.fault(
            "state_unwritable",
            Some(&shown),
            Some(lines),
            Some(e.to_string()),
        );
        return run.out;
    }
    run.out.exit = 2;
    run.say(redirect_message(&shown, lines, is_log));
    run.out
}

/// The marker file's name component: the session id reduced to `[A-Za-z0-9_-]`.
fn session_key(session: Option<&str>) -> String {
    let key: String = session
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if key.is_empty() {
        "nosession".to_string()
    } else {
        key
    }
}

fn append_marker(marker: &Path, path: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)?;
    writeln!(f, "{path}")
}

/// Newline bytes in the file, read in chunks: this runs before every Read and must not
/// pull a large log into memory to count it.
fn count_lines(path: &Path) -> std::io::Result<i64> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0i64;
    loop {
        match f.read(&mut buf) {
            Ok(0) => return Ok(n),
            Ok(k) => n += buf[..k].iter().filter(|&&b| b == b'\n').count() as i64,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

const RETRY: &str = "If the lumen tools are unavailable to you (server down, permission denied), retry\n\
                     this exact Read — it will be allowed through. Do not abandon the task.";

/// What the model is told when a read is blocked.
///
/// No figures. This copy used to promise "~5-10% token cost", "typically saves 80-93%"
/// and "typically 40-80% token reduction" while the ledger showed ranked outlines
/// returning more tokens than the legacy outlines they replaced. The model routes on
/// this text, so an inflated number biased routing, not just perception. Each tool
/// reports what it actually saved, per call.
fn redirect_message(path: &str, lines: i64, is_log: bool) -> String {
    if is_log {
        format!(
            "Lumen intercept: {path} is {lines} lines (log/output file).\n\
             Before reading the full file, call:\n  \
             lumen:compress_logs(path=\"{path}\")\n\
             This collapses repeated lines and stack frames deterministically. Analyze the\n\
             compressed output; the full file is still readable via smart_read(mode=\"full\")\n\
             if needed.\n\n{RETRY}"
        )
    } else {
        format!(
            "Lumen intercept: {path} is {lines} lines.\n\
             Instead of reading the full file, call:\n  \
             1. lumen:smart_read(path=\"{path}\") → structural outline with line ranges\n  \
             2. lumen:recall_file(path=\"{path}\", names=[\"<item>\"]) → fetch only what you need\n\
             Each call reports the tokens it saved in _meta.saved_tokens.\n\
             Use smart_read(mode=\"full\") only if you truly need every line.\n\n{RETRY}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An environment holding exactly `pairs`.
    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    fn hook(
        sub: &str,
        input: &[u8],
        var: &dyn Fn(&str) -> Option<String>,
        state: &Path,
    ) -> Outcome {
        run_with(
            &[sub.to_string()],
            input,
            &Env {
                var,
                state_dir: state,
            },
        )
    }

    fn spool_lines(p: &Path) -> Vec<Value> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn read_payload(path: &Path, session: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "tool_name": "Read",
            "tool_input": {"file_path": path.to_string_lossy()},
        }))
        .unwrap()
    }

    fn big_rs(dir: &Path, lines: usize) -> PathBuf {
        let p = dir.join("big.rs");
        std::fs::write(&p, "fn f() {}\n".repeat(lines)).unwrap();
        p
    }

    #[test]
    fn cmd_label_keeps_program_and_subcommand_and_drops_assignments() {
        for (cmd, want) in [
            ("AWS_SECRET=abc123 cargo test --workspace", "cargo test"),
            ("git status", "git status"),
            ("A=1 B=2 ls", "ls"),
            ("ls -la --color=auto", "ls -la"),
            ("-x=1 foo bar", "-x=1 foo"),
            ("cargo\ttest\n--all", "cargo test"),
            ("", ""),
            ("TOKEN=secret", ""),
        ] {
            assert_eq!(cmd_label(cmd), want, "{cmd:?}");
        }
        assert_eq!(
            cmd_label(&format!("{} y", "x".repeat(80))).chars().count(),
            60
        );
    }

    #[test]
    fn session_key_keeps_only_safe_characters() {
        assert_eq!(session_key(Some("abc-123_X")), "abc-123_X");
        assert_eq!(session_key(Some("../../etc")), "etc");
        assert_eq!(session_key(Some("!!!")), "nosession");
        assert_eq!(session_key(None), "nosession");
    }

    #[test]
    fn the_redirect_message_carries_no_figures() {
        for is_log in [false, true] {
            let m = redirect_message("/p/x.rs", 400, is_log);
            assert!(!m.contains('%'), "{m}");
            assert!(!m.contains("typically"), "{m}");
            assert!(!m.contains("~"), "{m}");
            assert!(m.contains("it will be allowed through"), "{m}");
        }
    }

    #[test]
    fn an_unknown_subcommand_is_a_usage_error_not_a_silent_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let out = hook("metre", b"{}", &vars(&[]), tmp.path());
        assert_eq!(out.exit, 1);
        assert!(out.stderr[0].starts_with("usage:"), "{out:?}");
    }

    #[test]
    fn writer_defaults_and_overrides() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(writer(&s(&[])), DEFAULT_WRITER);
        assert_eq!(
            writer(&s(&["--writer", "lumen_meter.sh"])),
            "lumen_meter.sh"
        );
        assert_eq!(writer(&s(&["--writer"])), DEFAULT_WRITER);
    }

    #[test]
    fn intercept_blocks_once_then_lets_the_retry_through_and_records_it() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[("LUMEN_FAULT_SPOOL", spool.to_str().unwrap())]);
        let payload = read_payload(&file, "s-1");

        let first = hook("intercept", &payload, &var, tmp.path());
        assert_eq!(first.exit, 2, "{first:?}");
        assert!(first.stderr[0].contains("is 400 lines"), "{first:?}");
        assert!(spool_lines(&spool).is_empty(), "a block is not a fault");

        let retry = hook("intercept", &payload, &var, tmp.path());
        assert_eq!(retry.exit, 0, "{retry:?}");
        let recs = spool_lines(&spool);
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0]["kind"], HOOK_FAIL_OPEN);
        assert_eq!(recs[0]["variant"], "retry_escape_valve");
        assert_eq!(recs[0]["lines"], 400);
        assert_eq!(recs[0]["session_id"], "s-1");
        assert_eq!(recs[0]["version"], env!("CARGO_PKG_VERSION"));

        // Another session has its own marker, so it is blocked once too.
        let other = hook("intercept", &read_payload(&file, "s-2"), &var, tmp.path());
        assert_eq!(other.exit, 2, "{other:?}");
    }

    #[test]
    fn intercept_passes_small_files_other_kinds_and_when_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let small = big_rs(tmp.path(), 299);
        let md = tmp.path().join("notes.md");
        std::fs::write(&md, "x\n".repeat(1000)).unwrap();
        let var = vars(&[("LUMEN_FAULT_SPOOL", spool.to_str().unwrap())]);

        assert_eq!(
            hook("intercept", &read_payload(&small, "s"), &var, tmp.path()).exit,
            0
        );
        assert_eq!(
            hook("intercept", &read_payload(&md, "s"), &var, tmp.path()).exit,
            0
        );

        let big = big_rs(tmp.path(), 300);
        let off = vars(&[("LUMEN_HOOK_ENABLED", "0")]);
        assert_eq!(
            hook("intercept", &read_payload(&big, "s"), &off, tmp.path()).exit,
            0
        );
        // The same file under the threshold override passes; at the default it blocks.
        let high = vars(&[("LUMEN_LINE_THRESHOLD", "301")]);
        assert_eq!(
            hook("intercept", &read_payload(&big, "s"), &high, tmp.path()).exit,
            0
        );
        assert_eq!(
            hook("intercept", &read_payload(&big, "s"), &var, tmp.path()).exit,
            2
        );
        assert!(spool_lines(&spool).is_empty());
    }

    #[test]
    fn intercept_routes_logs_to_compress_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("build.output");
        std::fs::write(&log, "line\n".repeat(500)).unwrap();
        let out = hook(
            "intercept",
            &read_payload(&log, "s"),
            &vars(&[]),
            tmp.path(),
        );
        assert_eq!(out.exit, 2);
        assert!(out.stderr[0].contains("lumen:compress_logs"), "{out:?}");
    }

    #[test]
    fn an_unwritable_marker_fails_open_instead_of_blocking_forever() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[("LUMEN_FAULT_SPOOL", spool.to_str().unwrap())]);
        // A state directory that does not exist: the marker cannot be created.
        let missing = tmp.path().join("no-such-dir");
        let out = hook("intercept", &read_payload(&file, "s"), &var, &missing);
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(
            out.stderr[0].contains("cannot write the intercept marker"),
            "{out:?}"
        );
        let recs = spool_lines(&spool);
        assert_eq!(recs[0]["variant"], "state_unwritable");
        assert!(!missing.exists(), "the hook must not create directories");
    }

    #[test]
    fn a_bad_payload_is_a_fault_in_both_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[("LUMEN_FAULT_SPOOL", spool.to_str().unwrap())]);
        let m = hook("meter", b"not json", &var, tmp.path());
        let i = hook("intercept", b"[1,2]", &var, tmp.path());
        assert_eq!((m.exit, i.exit), (0, 0));
        assert!(m.stderr[0].contains("not JSON"), "{m:?}");
        assert!(i.stderr[0].contains("not a JSON object"), "{i:?}");
        let recs = spool_lines(&spool);
        let got: Vec<(&str, &str)> = recs
            .iter()
            .map(|r| (r["kind"].as_str().unwrap(), r["variant"].as_str().unwrap()))
            .collect();
        assert_eq!(
            got,
            [
                (METER_WRITE_FAILED, "bad_payload"),
                (HOOK_FAIL_OPEN, "bad_payload")
            ]
        );
    }

    #[test]
    fn lumen_capture_zero_keeps_the_stderr_line_and_drops_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[
            ("LUMEN_FAULT_SPOOL", spool.to_str().unwrap()),
            ("LUMEN_CAPTURE", "0"),
        ]);
        let out = hook("meter", b"not json", &var, tmp.path());
        assert_eq!(out.stderr.len(), 1, "{out:?}");
        assert!(!spool.exists());
    }

    #[test]
    fn the_fault_channel_follows_the_entrypoint() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        for (entrypoint, want) in [("claude-vscode", "vscode"), ("cli", "cli")] {
            let var = vars(&[
                ("LUMEN_FAULT_SPOOL", spool.to_str().unwrap()),
                ("CLAUDE_CODE_ENTRYPOINT", entrypoint),
            ]);
            hook("intercept", b"nope", &var, tmp.path());
            let recs = spool_lines(&spool);
            assert_eq!(recs.last().unwrap()["channel"], want, "{entrypoint}");
        }
    }

    #[test]
    fn no_database_and_no_home_is_said_and_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[("LUMEN_FAULT_SPOOL", spool.to_str().unwrap())]);
        let payload = serde_json::to_vec(&serde_json::json!({
            "session_id": "s", "tool_name": "Read",
            "tool_input": {"file_path": file.to_string_lossy()},
        }))
        .unwrap();
        let out = hook("meter", &payload, &var, tmp.path());
        assert_eq!(out.exit, 0);
        assert!(
            out.stderr[0].contains("no database path resolved"),
            "{out:?}"
        );
        assert_eq!(spool_lines(&spool)[0]["variant"], "no_db_path");
    }

    #[test]
    fn a_bash_response_without_output_fields_is_a_changed_payload_not_silence() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let db = tmp.path().join("lumen.db");
        let var = vars(&[
            ("LUMEN_FAULT_SPOOL", spool.to_str().unwrap()),
            ("LUMEN_DB", db.to_str().unwrap()),
        ]);
        let changed = br#"{"tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"output":"a\nb\n"}}"#;
        let out = hook("meter", changed, &var, tmp.path());
        assert!(out.stderr[0].contains("no stdout or stderr"), "{out:?}");
        assert_eq!(spool_lines(&spool)[0]["variant"], "bad_payload");

        // Genuinely empty output is not a fault and not a row.
        let empty = br#"{"tool_name":"Bash","tool_input":{"command":"true"},"tool_response":{"stdout":"","stderr":""}}"#;
        let out = hook("meter", empty, &var, tmp.path());
        assert!(out.stderr.is_empty(), "{out:?}");
        assert_eq!(spool_lines(&spool).len(), 1);
        assert!(!db.exists(), "an empty output was recorded");
    }
}
