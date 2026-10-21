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
// The intercept also runs in front of Lumen's own tools (smart_read, recall_file,
// compress_logs), where it blocks nothing: it writes a small record, named by the call's
// tool_use_id, saying which conversation (and which subagent) made the call. The MCP
// request carries only that id, so this is the one place the server can learn whose call
// it is, and with it whether the model still holds what an earlier result sent. A call
// with no record is answered in full.
//
// Every way a hook can lose an event leaves a fault in the spool and a line on
// stderr. Neither hook creates directories: a missing data directory means Lumen is
// not installed where the hook was told it is, and creating one would hide that.

use lumen_core::{
    coverage::{DEFAULT_LINE_THRESHOLD, INTERCEPTED_SOURCE_EXTS, LOG_EXTS, ext_of},
    faults::{self, FaultRecord},
    identity,
    intercept::{BlockKind, block_line},
    meter::{self, HookReadEvent, METER_WRITE_FAILED},
    repeat,
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
    /// [`faults::spool_path`], over the injected environment: both go through the redirect, so a
    /// spool an old shim exported into the previous folder lands in the new one, and an empty
    /// variable counts as unset.
    fn spool_path(&self) -> Option<PathBuf> {
        let home = self.get("HOME").or_else(|| self.get("USERPROFILE"));
        let env_spool = self.get("LUMEN_FAULT_SPOOL");
        faults::spool_path_with(
            env_spool.as_deref().map(std::ffi::OsStr::new),
            self.db_path().as_deref(),
            home.as_deref(),
        )
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
    /// The subagent the event belongs to; `None` for the main agent. A subagent shares its
    /// parent's `session_id` and has a context of its own.
    agent: Option<String>,
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
            agent: None,
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
        // A subagent blocks and is let through in its own right: which one it was is what lets
        // its block be counted apart from its parent's on the same file.
        rec.agent_id = self.agent.clone();
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
    run.agent = str_at(&d, "/agent_id").map(str::to_string);
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
    let agent = run.agent.clone();
    run.insert(&HookReadEvent {
        tool: "Read",
        path: &shown,
        lines: Some(lines),
        tokens_returned: tokens,
        full_tokens: tokens,
        routed_via: "builtin_read",
        channel,
        session_id: session.as_deref(),
        agent_id: agent.as_deref(),
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
    let agent = run.agent.clone();
    run.insert(&HookReadEvent {
        tool: "Bash",
        path: &label,
        lines: None,
        tokens_returned: 0,
        full_tokens: tokens,
        routed_via: "bash_output",
        channel,
        session_id: session.as_deref(),
        agent_id: agent.as_deref(),
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

// ── intercept (PreToolUse on Read, and on Lumen's own tools) ─────────────────

/// Fault kind for a Lumen tool call whose conversation the hook could not record: the server
/// then has nobody to attribute it to and answers it in full.
pub const IDENTITY_UNRECORDED: &str = "identity_unrecorded";

fn intercept(input: &[u8], env: &Env) -> Outcome {
    let mut run = Run::intercept(env);
    // Silent here: a payload that is not JSON is reported below, once, if this is the Read
    // intercept; a switched-off intercept stays quiet as it always was.
    let parsed = serde_json::from_slice::<Value>(input)
        .ok()
        .filter(Value::is_object);
    // Whose call a Lumen tool call is has nothing to do with the Read redirect, so
    // `LUMEN_HOOK_ENABLED=0` does not reach it: the repeat-read gate would then have to guess.
    if let Some(d) = &parsed
        && let Some(tool) = str_at(d, "/tool_name").filter(|t| identity::is_lumen_tool(t))
    {
        return record_identity(env, d, tool);
    }
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
    let Some(d) = parsed.or_else(|| run.payload(input)) else {
        return run.out;
    };
    run.session = str_at(&d, "/session_id").map(str::to_string);
    run.agent = str_at(&d, "/agent_id").map(str::to_string);
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
    //
    // The exception is a model that came back because the route *worked*: smart_read or
    // recall_file already answered for this path in this session, and it wants the code.
    // Releasing then costs the whole file, and Claude Code's own unchanged-file dedup
    // cannot help, because the blocked first Read never entered its cache (#36). That
    // Read gets one more redirect, to recall_file, and only one: a model that really
    // needs the file whole (to Edit it, say) retries and gets it, so nothing here can
    // strand a model. The valve still exists for the case it was written for. With no
    // successful smart_read or recall_file recorded for the path (tools down, denied, or
    // never called) the retry is released exactly as before.
    let marker = env
        .state_dir
        .join(marker_name(run.session.as_deref(), run.agent.as_deref()));
    // No marker yet is no text. An unreadable one fails the append below, which fails open.
    let text = std::fs::read_to_string(&marker).ok();
    let has_line = |want: &str| {
        text.as_deref()
            .is_some_and(|t| t.lines().any(|l| l == want))
    };
    let seen = has_line(&shown);
    let pointed_line = format!("{POINTED_PREFIX}{shown}");
    let pointed = has_line(&pointed_line);
    let served = !is_log && lumen_served(env, run.session.as_deref(), run.agent.as_deref(), &shown);
    if seen && (pointed || !served) {
        let detail = served.then(|| AFTER_LUMEN_READ.to_string());
        run.fault("retry_escape_valve", Some(&shown), Some(lines), detail);
        return run.out;
    }
    let mut record = String::new();
    if !seen {
        record.push_str(&shown);
        record.push('\n');
    }
    if served {
        record.push_str(&pointed_line);
        record.push('\n');
    }
    if let Err(e) = append_marker(&marker, &record) {
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
    run.say(if served {
        served_message(&shown, lines)
    } else {
        redirect_message(&shown, lines, is_log)
    });
    run.out
}

/// Marker line prefix: this path was already sent to recall_file after a working smart_read.
/// A prefix no path starts with, so it cannot be mistaken for the path lines.
const POINTED_PREFIX: &str = "after-lumen-read\t";

/// `detail` on the `retry_escape_valve` fault of a Read released after smart_read or
/// recall_file had answered for the path, so those can be counted apart from the valve's
/// intended use.
const AFTER_LUMEN_READ: &str = "after_lumen_read";

/// Whether smart_read or recall_file already answered for `path` in this context: the session's,
/// or, for a subagent, its own (it does not hold what its parent read). The hook cannot see the
/// server; the ledger the server writes is the only place that records that a call worked. Fails
/// open: no session, no ledger, or an unreadable one answer "no", and the valve releases the
/// retry as it always did. See [`meter::lumen_answered`].
fn lumen_served(env: &Env, session: Option<&str>, agent: Option<&str>, path: &str) -> bool {
    let (Some(session), Some(db)) = (session, env.db_path()) else {
        return false;
    };
    meter::lumen_answered(&db, session, agent, path)
}

/// The intercept marker's file name. The main agent's is `lumen_intercept_<session>`; a
/// subagent has a context of its own, so it has a marker of its own beside it. `.` cannot
/// occur in either key, so no two contexts share a name.
fn marker_name(session: Option<&str>, agent: Option<&str>) -> String {
    match agent {
        Some(a) => format!(
            "lumen_intercept_{}.{}",
            session_key(session),
            session_key(Some(a))
        ),
        None => format!("lumen_intercept_{}", session_key(session)),
    }
}

/// A Lumen tool call is about to be made: leave the conversation it belongs to where the server
/// will find it, keyed by the call's `tool_use_id`. The tool request itself carries only that id.
///
/// Exits 0 whatever happens: a record that is not written costs the call its repeat-read
/// treatment (the server answers an unidentified call in full) and nothing else. It is said on
/// stderr and in the spool, and no directory is created to make the write succeed.
fn record_identity(env: &Env, d: &Value, tool: &str) -> Outcome {
    let mut run = Run::new(
        env,
        IDENTITY_UNRECORDED,
        "this call will be answered in full",
    );
    run.session = str_at(d, "/session_id").map(str::to_string);
    run.agent = str_at(d, "/agent_id").map(str::to_string);
    let Some(db) = env.db_path() else {
        run.say(format!(
            "lumen: no database path resolved (LUMEN_DB unset, no home directory); {}",
            run.consequence
        ));
        run.fault("no_db_path", None, None, None);
        return run.out;
    };
    let dir = identity::record_dir(&db);
    let call_id = str_at(d, "/tool_use_id").unwrap_or("");
    let now = repeat::now_ms();
    let written = match run.session.clone() {
        Some(session) => {
            let who = identity::Identity {
                session,
                agent: run.agent.clone(),
            };
            identity::write(dir, tool, call_id, &who, now)
        }
        None => Err(identity::WriteError::Invalid("the session_id is missing")),
    };
    if let Err(e) = written {
        let variant = match e {
            identity::WriteError::Invalid(_) => "unusable_ids",
            identity::WriteError::Io(_) => "record_unwritable",
        };
        run.say(format!(
            "lumen: cannot record who is calling {tool} in {} ({e}); {}",
            dir.display(),
            run.consequence
        ));
        run.fault(variant, None, None, Some(e.to_string()));
        return run.out;
    }
    // The hook fires before the permission prompt, so a call that is then refused leaves its
    // record behind. Clearing the old ones here is what keeps the directory bounded.
    for (path, e) in identity::prune(dir, now).failed {
        run.say(format!("lumen: cannot remove {}: {e}", path.display()));
    }
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

fn append_marker(marker: &Path, lines: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(marker)?;
    f.write_all(lines.as_bytes())
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
///
/// The first line is [`block_line`]'s, which the daemon reads back out of the transcript to count
/// the block (`lumen_core::intercept`): change it there, not here.
fn redirect_message(path: &str, lines: i64, is_log: bool) -> String {
    if is_log {
        format!(
            "{}\n\
             Before reading the full file, call:\n  \
             lumen:compress_logs(path=\"{path}\")\n\
             This collapses repeated lines and stack frames deterministically. Analyze the\n\
             compressed output; the full file is still readable via smart_read(mode=\"full\")\n\
             if needed.\n\n{RETRY}",
            block_line(path, lines, BlockKind::Log)
        )
    } else {
        format!(
            "{}\n\
             Instead of reading the full file, call:\n  \
             1. lumen:smart_read(path=\"{path}\") → structural outline with line ranges\n  \
             2. lumen:recall_file(path=\"{path}\", names=[\"<item>\"]) → fetch only what you need\n\
             Each call reports the tokens it saved in _meta.saved_tokens.\n\
             Use smart_read(mode=\"full\") only if you truly need every line.\n\n{RETRY}",
            block_line(path, lines, BlockKind::Redirect)
        )
    }
}

/// What the model is told when it reads, whole, a file that smart_read or recall_file has
/// already answered for. No figures, for the reason given on [`redirect_message`]; its first line
/// is [`block_line`]'s, for the reason given there.
fn served_message(path: &str, lines: i64) -> String {
    format!(
        "{}\n\
         answered for it in this session. Reading it whole puts every line in context again.\n\
         Fetch only what you need, using the item names from the outline you already have:\n  \
         lumen:recall_file(path=\"{path}\", names=[\"<item>\"])\n  \
         lumen:recall_file(path=\"{path}\", start_line=N, end_line=M)\n\n\
         If you do need the whole file (to Edit it, for example), retry this exact Read once\n\
         more — it will be allowed through.",
        block_line(path, lines, BlockKind::Served)
    )
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

    /// `Env::spool_path` is `faults::spool_path`'s rule over the injected environment: an empty
    /// variable is unset, and a spool an old shim exported into the previous folder lands in N
    /// once the marker says where that folder went.
    #[test]
    fn the_spool_goes_through_the_redirect_and_an_empty_variable_is_unset() {
        use lumen_core::legacy::marker::{FORMAT, MarkerV2, STATE_MIGRATED, write_marker};
        let home = tempfile::TempDir::new().unwrap();
        let h = home.path().to_string_lossy().into_owned();
        let n = meter::app_data_dir_in(home.path());
        let prev = n.parent().unwrap().join("example.previous.app");
        std::fs::create_dir_all(&prev).unwrap();
        std::fs::create_dir_all(&n).unwrap();
        write_marker(
            &n,
            &MarkerV2 {
                format: FORMAT,
                state: STATE_MIGRATED.into(),
                source: "example.previous.app".into(),
                source_dir: prev.to_string_lossy().into_owned(),
                backup: "legacy-backup-2026-10-19".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let state = home.path().join("state");
        let spool_for = |pairs: &[(&str, &str)]| {
            let var = vars(pairs);
            Env {
                var: &var,
                state_dir: &state,
            }
            .spool_path()
        };
        let stale = prev.join("faults.jsonl");
        assert_eq!(
            spool_for(&[
                ("HOME", &h),
                ("LUMEN_FAULT_SPOOL", &stale.to_string_lossy())
            ]),
            Some(n.join("faults.jsonl")),
            "red before the redirect: the stale path was used as it stood"
        );
        let stale_db = prev.join("lumen.db");
        assert_eq!(
            spool_for(&[
                ("HOME", &h),
                ("LUMEN_DB", &stale_db.to_string_lossy()),
                ("LUMEN_FAULT_SPOOL", ""),
            ]),
            Some(n.join("faults.jsonl")),
            "empty is unset: beside the database, which is N's"
        );
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
    fn the_served_message_carries_no_figures() {
        let m = served_message("/p/x.rs", 400);
        assert!(!m.contains('%'), "{m}");
        assert!(!m.contains("typically"), "{m}");
        assert!(!m.contains("~"), "{m}");
        assert!(m.contains("allowed through"), "{m}");
    }

    #[test]
    fn smart_read_before_the_first_read_gets_one_redirect_to_recall_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let var = ledger_env(tmp.path());
        seed_row(&tmp.path().join("lumen.db"), "s-1", &file, "smart_read");
        let payload = read_payload(&file, "s-1");

        let first = hook("intercept", &payload, &var, tmp.path());
        assert_eq!(first.exit, 2, "{first:?}");
        assert!(first.stderr[0].contains("already"), "{first:?}");
        assert!(
            first.stderr[0].contains("lumen:recall_file(path="),
            "{first:?}"
        );
        let retry = hook("intercept", &payload, &var, tmp.path());
        assert_eq!((retry.exit, retry.stderr.len()), (0, 0), "{retry:?}");
    }

    /// The daemon counts a block by reading its message back out of the transcript
    /// (`lumen_core::intercept::parse_block`). Every message this hook blocks with must read back
    /// as the block it is, with the path and the count it named.
    #[test]
    fn every_message_the_intercept_blocks_with_reads_back_as_its_block() {
        use lumen_core::intercept::{ParsedBlock, parse_block};
        for path in ["/p/x.rs", "/p/dir is odd/a is 3 lines.rs", "C:\\a b\\y.log"] {
            for (message, kind) in [
                (redirect_message(path, 412, false), BlockKind::Redirect),
                (redirect_message(path, 412, true), BlockKind::Log),
                (served_message(path, 412), BlockKind::Served),
            ] {
                assert_eq!(
                    parse_block(&format!("PreToolUse:Read hook error: [lumen]: {message}")),
                    Some(ParsedBlock {
                        path,
                        lines: 412,
                        kind
                    }),
                    "{message}"
                );
            }
        }
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

    /// A ledger row as the MCP server writes one: `routed_via` is the route it took, and a
    /// declined call carries a route of its own, so only the two served routes count.
    fn seed_row(db: &Path, session: &str, path: &Path, routed_via: &str) {
        lumen_core::meter::insert_read_event_at(
            db,
            &path.to_string_lossy(),
            Some(400),
            300,
            4000,
            3700,
            routed_via,
            "cli",
            &format!("mcp__lumen__{routed_via}"),
            Some(session),
            None,
            None,
            &lumen_core::meter::RankedMeta::default(),
        )
        .unwrap();
    }

    /// An environment pointing the ledger and the fault spool into `dir`.
    fn ledger_env(dir: &Path) -> impl Fn(&str) -> Option<String> + use<> {
        vars(&[
            ("LUMEN_DB", dir.join("lumen.db").to_str().unwrap()),
            (
                "LUMEN_FAULT_SPOOL",
                dir.join("faults.jsonl").to_str().unwrap(),
            ),
        ])
    }

    #[test]
    fn a_retry_after_a_working_smart_read_is_sent_to_recall_file_not_released() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let var = ledger_env(tmp.path());
        let payload = read_payload(&file, "s-1");

        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        // The model did as told and smart_read worked.
        seed_row(&tmp.path().join("lumen.db"), "s-1", &file, "smart_read");

        let again = hook("intercept", &payload, &var, tmp.path());
        assert_eq!(
            again.exit, 2,
            "released a full read after smart_read: {again:?}"
        );
        let msg = again.stderr.join("\n");
        assert!(msg.contains("lumen:recall_file(path="), "{msg}");
        assert!(msg.contains("names=["), "{msg}");
        assert!(msg.contains("smart_read"), "{msg}");
        assert!(msg.contains("retry"), "must say how to get past it: {msg}");
        let spool = tmp.path().join("faults.jsonl");
        assert!(spool_lines(&spool).is_empty(), "a block is not a fault");
    }

    #[test]
    fn a_model_that_insists_is_never_blocked_a_third_time() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let var = ledger_env(tmp.path());
        let payload = read_payload(&file, "s-1");
        let spool = tmp.path().join("faults.jsonl");

        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        seed_row(&tmp.path().join("lumen.db"), "s-1", &file, "smart_read");
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);

        // It needs the whole file (to Edit it, say). The valve holds as before.
        for _ in 0..3 {
            let out = hook("intercept", &payload, &var, tmp.path());
            assert_eq!((out.exit, out.stderr.len()), (0, 0), "{out:?}");
        }
        let recs = spool_lines(&spool);
        assert_eq!(recs.len(), 3, "{recs:?}");
        assert_eq!(recs[0]["variant"], "retry_escape_valve");
        assert_eq!(recs[0]["detail"], "after_lumen_read");
    }

    #[test]
    fn recall_file_alone_also_counts_as_a_working_route() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let var = ledger_env(tmp.path());
        let payload = read_payload(&file, "s-1");
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        seed_row(&tmp.path().join("lumen.db"), "s-1", &file, "recall_file");
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
    }

    #[test]
    fn a_path_with_no_successful_smart_read_is_still_released_on_the_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let other = tmp.path().join("other.rs");
        let var = ledger_env(tmp.path());
        let db = tmp.path().join("lumen.db");
        let payload = read_payload(&file, "s-1");
        let spool = tmp.path().join("faults.jsonl");

        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        // None of these show the tools working for this path in this session: another
        // path, another session, a declined call, and a builtin read of the very path.
        seed_row(&db, "s-1", &other, "smart_read");
        seed_row(&db, "s-2", &file, "smart_read");
        seed_row(&db, "s-1", &file, "would_inflate");
        seed_row(&db, "s-1", &file, "builtin_read");

        let retry = hook("intercept", &payload, &var, tmp.path());
        assert_eq!((retry.exit, retry.stderr.len()), (0, 0), "{retry:?}");
        let recs = spool_lines(&spool);
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0]["variant"], "retry_escape_valve");
        assert!(recs[0]["detail"].is_null(), "{recs:?}");
    }

    #[test]
    fn a_ledger_that_cannot_be_read_releases_the_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let payload = read_payload(&file, "s-1");

        // No ledger at all, and the hook does not create one.
        let var = ledger_env(tmp.path());
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 0);
        assert!(!tmp.path().join("lumen.db").exists());

        // A ledger that is not a database.
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let payload = read_payload(&file, "s-1");
        std::fs::write(tmp.path().join("lumen.db"), "this is not sqlite").unwrap();
        let var = ledger_env(tmp.path());
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 0);

        // A ledger with no read_events table.
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let payload = read_payload(&file, "s-1");
        rusqlite::Connection::open(tmp.path().join("lumen.db"))
            .unwrap()
            .execute_batch("CREATE TABLE unrelated(x)")
            .unwrap();
        let var = ledger_env(tmp.path());
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 0);
    }

    #[test]
    fn a_log_retry_is_unchanged_by_the_ledger() {
        // compress_logs is not smart_read; the change is for source files.
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("build.log");
        std::fs::write(&log, "line\n".repeat(500)).unwrap();
        let var = ledger_env(tmp.path());
        let payload = read_payload(&log, "s-1");
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 2);
        seed_row(&tmp.path().join("lumen.db"), "s-1", &log, "smart_read");
        assert_eq!(hook("intercept", &payload, &var, tmp.path()).exit, 0);
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

    fn lumen_call_payload(tool: &str, session: &str, agent: Option<&str>, id: &str) -> Vec<u8> {
        let mut p = serde_json::json!({
            "session_id": session,
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": {"path": "/x/a.rs"},
            "tool_use_id": id,
        });
        if let Some(a) = agent {
            p["agent_id"] = a.into();
            p["agent_type"] = "general-purpose".into();
        }
        serde_json::to_vec(&p).unwrap()
    }

    fn claim(dir: &Path, tool: &str, id: &str) -> identity::Lookup {
        identity::claim(dir, tool, Some(id), repeat::now_ms())
    }

    #[test]
    fn a_lumen_tool_call_leaves_its_conversation_under_its_tool_use_id() {
        let tmp = tempfile::tempdir().unwrap();
        let var = ledger_env(tmp.path());
        for tool in identity::MATCHERS
            .into_iter()
            .chain(identity::PLUGIN_MATCHERS)
        {
            let short = identity::short_name(tool).unwrap();
            let out = hook(
                "intercept",
                &lumen_call_payload(tool, "s-1", None, &format!("toolu_{short}")),
                &var,
                tmp.path(),
            );
            assert_eq!((out.exit, out.stderr.is_empty()), (0, true), "{out:?}");
            match claim(tmp.path(), short, &format!("toolu_{short}")) {
                identity::Lookup::Known(who) => {
                    assert_eq!((who.session.as_str(), who.agent), ("s-1", None));
                }
                other => panic!("{tool}: {other:?}"),
            }
        }
        assert!(spool_lines(&tmp.path().join("faults.jsonl")).is_empty());
    }

    #[test]
    fn a_subagents_call_carries_its_agent_id_beside_the_shared_session() {
        let tmp = tempfile::tempdir().unwrap();
        let var = ledger_env(tmp.path());
        let p = lumen_call_payload("mcp__lumen__smart_read", "s-1", Some("agent-a"), "toolu_1");
        assert_eq!(hook("intercept", &p, &var, tmp.path()).exit, 0);
        match claim(tmp.path(), "smart_read", "toolu_1") {
            identity::Lookup::Known(who) => {
                assert_eq!(who.owner(), "s-1\0agent-a");
                assert_eq!(who.agent.as_deref(), Some("agent-a"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn switching_the_read_intercept_off_leaves_the_identity_on() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("lumen.db");
        let spool = tmp.path().join("faults.jsonl");
        let var = vars(&[
            ("LUMEN_DB", db.to_str().unwrap()),
            ("LUMEN_FAULT_SPOOL", spool.to_str().unwrap()),
            ("LUMEN_HOOK_ENABLED", "0"),
        ]);
        let p = lumen_call_payload("mcp__lumen__recall_file", "s-1", None, "toolu_1");
        assert_eq!(hook("intercept", &p, &var, tmp.path()).exit, 0);
        assert!(matches!(
            claim(tmp.path(), "recall_file", "toolu_1"),
            identity::Lookup::Known(_)
        ));
        // And the Read redirect is still off.
        let file = big_rs(tmp.path(), 400);
        let read = hook("intercept", &read_payload(&file, "s-1"), &var, tmp.path());
        assert_eq!((read.exit, read.stderr.is_empty()), (0, true), "{read:?}");
    }

    #[test]
    fn tools_that_are_not_lumens_are_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let var = ledger_env(tmp.path());
        for tool in [
            "Bash",
            "mcp__other__smart_read",
            "mcp__lumen__ping",
            "smart_read",
        ] {
            let p = lumen_call_payload(tool, "s-1", None, "toolu_1");
            let out = hook("intercept", &p, &var, tmp.path());
            assert_eq!(
                (out.exit, out.stderr.is_empty()),
                (0, true),
                "{tool}: {out:?}"
            );
        }
        assert!(
            !identity::record_path(tmp.path(), "toolu_1").exists(),
            "a record was written for a tool that is not Lumen's"
        );
    }

    #[test]
    fn a_record_that_cannot_be_written_is_said_and_recorded_and_the_call_goes_on() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        // The ledger's directory is not there, and the hook does not make it.
        let db = tmp.path().join("gone").join("lumen.db");
        let var = vars(&[
            ("LUMEN_DB", db.to_str().unwrap()),
            ("LUMEN_FAULT_SPOOL", spool.to_str().unwrap()),
        ]);
        let p = lumen_call_payload("mcp__lumen__smart_read", "s-1", None, "toolu_1");
        let out = hook("intercept", &p, &var, tmp.path());
        assert_eq!(out.exit, 0, "{out:?}");
        assert!(out.stderr[0].contains("answered in full"), "{out:?}");
        let faults = spool_lines(&spool);
        assert_eq!(faults[0]["kind"], IDENTITY_UNRECORDED, "{faults:?}");
        assert_eq!(faults[0]["variant"], "record_unwritable");
        assert_eq!(faults[0]["session_id"], "s-1");
        assert!(!db.parent().unwrap().exists(), "the hook made a directory");
    }

    #[test]
    fn a_payload_that_cannot_name_its_call_or_conversation_is_a_fault_not_a_guess() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("faults.jsonl");
        let var = ledger_env(tmp.path());
        let no_id = br#"{"session_id":"s-1","tool_name":"mcp__lumen__smart_read"}"#;
        let no_session = br#"{"tool_name":"mcp__lumen__smart_read","tool_use_id":"toolu_1"}"#;
        let bad_id =
            br#"{"session_id":"s-1","tool_name":"mcp__lumen__smart_read","tool_use_id":"../x"}"#;
        for payload in [&no_id[..], &no_session[..], &bad_id[..]] {
            let out = hook("intercept", payload, &var, tmp.path());
            assert_eq!(out.exit, 0, "{out:?}");
            assert!(out.stderr[0].starts_with("lumen:"), "{out:?}");
        }
        let faults = spool_lines(&spool);
        assert_eq!(faults.len(), 3, "{faults:?}");
        assert!(
            faults
                .iter()
                .all(|f| f["kind"] == IDENTITY_UNRECORDED && f["variant"] == "unusable_ids"),
            "{faults:?}"
        );
        let written = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("lumen_call_"));
        assert!(!written, "a record was written for an id that is no id");
    }

    #[test]
    fn a_payload_that_is_not_json_is_still_a_fault_for_the_read_intercept() {
        let tmp = tempfile::tempdir().unwrap();
        let var = ledger_env(tmp.path());
        let out = hook("intercept", b"not json", &var, tmp.path());
        assert_eq!(out.exit, 0);
        assert!(out.stderr[0].contains("not JSON"), "{out:?}");
        let faults = spool_lines(&tmp.path().join("faults.jsonl"));
        assert_eq!(faults[0]["variant"], "bad_payload");
    }

    #[test]
    fn each_context_has_its_own_intercept_marker() {
        assert_eq!(marker_name(Some("s-1"), None), "lumen_intercept_s-1");
        assert_eq!(
            marker_name(Some("s-1"), Some("a1")),
            "lumen_intercept_s-1.a1"
        );
        assert_ne!(
            marker_name(Some("s-1"), Some("a1")),
            marker_name(Some("s-1"), Some("a2"))
        );
        // A session id that spells out a subagent's name is not that subagent's marker.
        assert_ne!(
            marker_name(Some("s-1_a1"), None),
            marker_name(Some("s-1"), Some("a1"))
        );
        assert_eq!(marker_name(None, None), "lumen_intercept_nosession");
    }

    #[test]
    fn a_subagent_is_redirected_once_in_its_own_right() {
        let tmp = tempfile::tempdir().unwrap();
        let file = big_rs(tmp.path(), 400);
        let var = ledger_env(tmp.path());
        let read = |agent: Option<&str>| {
            let mut p: Value = serde_json::from_slice(&read_payload(&file, "s-1")).unwrap();
            if let Some(a) = agent {
                p["agent_id"] = a.into();
            }
            hook(
                "intercept",
                &serde_json::to_vec(&p).unwrap(),
                &var,
                tmp.path(),
            )
            .exit
        };
        assert_eq!(read(None), 2);
        // The subagent was never told about Lumen, whatever its parent was told.
        assert_eq!(read(Some("agent-a")), 2);
        assert_eq!(read(Some("agent-a")), 0);
        assert_eq!(read(None), 0);
        // Each release names whose Read it was, so the two blocks are counted apart: the
        // subagent's fault carries its id, and the main agent's line is what it always was.
        let recs = spool_lines(&tmp.path().join("faults.jsonl"));
        assert_eq!(recs.len(), 2, "{recs:?}");
        assert_eq!(recs[0]["variant"], "retry_escape_valve");
        assert_eq!(recs[0]["agent_id"], "agent-a");
        assert_eq!(recs[1]["variant"], "retry_escape_valve");
        assert!(recs[1].get("agent_id").is_none(), "{:?}", recs[1]);
    }
}
