//! `lumen-mcp hook`, driven the way Claude Code drives it: a payload Claude Code really
//! sent, on stdin, to the real binary, in the environment a hook process gets.
//!
//! The payloads are the captures under `fixtures/hooks/` (Claude Code 2.1.270; the
//! README there says how they were taken). Each test aims a payload at a temp file on
//! the machine running it — `cwd`, `tool_input.file_path`, `tool_response.file.filePath`
//! — and passes every other field through untouched. A payload written by hand agrees
//! with the hook's own assumptions, and that is the failure this suite exists to catch.
//!
//! Every run is sealed into a temp directory: the ledger, the fault spool, HOME and
//! USERPROFILE, and TMPDIR/TMP/TEMP, where the intercept keeps its markers. Variables
//! that change what a hook does are removed, so the shell running the tests cannot
//! colour a result.

use lumen_core::tokenizer::count_tokens;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, UNIX_EPOCH};
use tempfile::TempDir;

/// `hello.rs` from the capture session, byte for byte.
const HELLO: &str = "fn main() {\n    println!(\"hello\");\n}\n";

/// The session ids inside the captured payloads.
const READ_SESSION: &str = "a2dc779f-f8bf-44c5-9016-7a087d832e3f";
const BASH_SESSION: &str = "7c159cf4-d8a2-4de3-a430-c2b250f22c84";

/// Every file the sandbox writes gets this modification time, so a row's `file_mtime`
/// can be checked against the file and not merely against "some recent second".
const MTIME: i64 = 1_700_000_000;

const SCRUBBED: &[&str] = &[
    "LUMEN_CAPTURE",
    "LUMEN_HOOK_ENABLED",
    "LUMEN_LINE_THRESHOLD",
    "LUMEN_DEBUG",
    "LUMEN_METER_BASH",
    "LUMEN_SESSION_ID",
    "LUMEN_CHANNEL",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "VSCODE_PID",
    "VSCODE_CWD",
];

struct Sandbox {
    _dir: TempDir,
    home: PathBuf,
    tmp: PathBuf,
    proj: PathBuf,
    db: PathBuf,
    spool: PathBuf,
}

/// What one hook process did.
#[derive(Debug)]
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
struct Row {
    tool: String,
    path: String,
    lines: Option<i64>,
    tokens_returned: i64,
    full_tokens: i64,
    saved_tokens: i64,
    routed_via: String,
    channel: String,
    session_id: Option<String>,
    file_mtime: Option<i64>,
    req_key: Option<String>,
    writer_hook: Option<String>,
    token_source: Option<String>,
}

fn sandbox() -> Sandbox {
    let dir = TempDir::new().expect("tempdir");
    let mk = |name: &str| {
        let p = dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    };
    let (home, tmp, proj, data) = (mk("home"), mk("tmp"), mk("proj"), mk("data"));
    let db = data.join("lumen.db");
    // Isolation asserted, not assumed: a hook that ignored LUMEN_DB would write to the
    // developer's own ledger and every test here would still pass.
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the test ledger must live under the temp dir, got {}",
        db.display()
    );
    Sandbox {
        spool: data.join("faults.jsonl"),
        db,
        home,
        tmp,
        proj,
        _dir: dir,
    }
}

impl Sandbox {
    fn hook(&self, args: &[&str], payload: &[u8], env: &[(&str, &str)]) -> Ran {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
        cmd.arg("hook")
            .args(args)
            .env("LUMEN_DB", &self.db)
            .env("LUMEN_FAULT_SPOOL", &self.spool)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("TMPDIR", &self.tmp)
            .env("TMP", &self.tmp)
            .env("TEMP", &self.tmp);
        for k in SCRUBBED {
            cmd.env_remove(k);
        }
        cmd.envs(env.iter().copied());
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn lumen-mcp");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload)
            .expect("write the payload");
        let out = child.wait_with_output().expect("wait for lumen-mcp");
        Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// The meter as the installed shim runs it.
    fn meter(&self, payload: &Value, env: &[(&str, &str)]) -> Ran {
        let ran = self.hook(
            &["meter", "--writer", "lumen_meter.sh"],
            &serde_json::to_vec(payload).unwrap(),
            env,
        );
        // On every path: Claude Code parses a hook's stdout as JSON control output.
        assert_eq!(ran.stdout, "", "the meter printed to stdout: {ran:?}");
        assert_eq!(
            ran.code, 0,
            "the meter must never fail the tool call: {ran:?}"
        );
        ran
    }

    fn intercept(&self, payload: &Value, env: &[(&str, &str)]) -> Ran {
        let ran = self.hook(&["intercept"], &serde_json::to_vec(payload).unwrap(), env);
        assert_eq!(ran.stdout, "", "the intercept printed to stdout: {ran:?}");
        ran
    }

    /// A file in the project directory, stamped with [`MTIME`].
    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let p = self.proj.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(MTIME as u64))
            .unwrap();
        p
    }

    /// Every row in the ledger, oldest first. Opened read-only, so looking cannot create
    /// a ledger that the hook did not.
    fn rows(&self) -> Vec<Row> {
        let conn = Connection::open_with_flags(&self.db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|e| panic!("open {}: {e}", self.db.display()));
        let mut stmt = conn
            .prepare(
                "SELECT tool, path, lines, tokens_returned, full_tokens, saved_tokens, \
                 routed_via, channel, session_id, file_mtime, req_key, writer_hook, \
                 token_source FROM read_events ORDER BY rowid",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok(Row {
                tool: r.get(0)?,
                path: r.get(1)?,
                lines: r.get(2)?,
                tokens_returned: r.get(3)?,
                full_tokens: r.get(4)?,
                saved_tokens: r.get(5)?,
                routed_via: r.get(6)?,
                channel: r.get(7)?,
                session_id: r.get(8)?,
                file_mtime: r.get(9)?,
                req_key: r.get(10)?,
                writer_hook: r.get(11)?,
                token_source: r.get(12)?,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    /// Every record in the fault spool. A missing spool is no faults.
    fn faults(&self) -> Vec<Value> {
        match std::fs::read_to_string(&self.spool) {
            Ok(text) => text
                .lines()
                .map(|l| serde_json::from_str(l).expect("each spool line is JSON"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// A captured payload aimed at the sandbox. Only `cwd` and, given `file`, the two file
/// path fields change.
fn captured(name: &str, cwd: &Path, file: Option<&Path>) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hooks")
        .join(name);
    let mut v: Value =
        serde_json::from_str(&std::fs::read_to_string(&p).expect("fixture")).expect("JSON");
    v["cwd"] = Value::from(cwd.to_string_lossy().as_ref());
    if let Some(file) = file {
        let s = Value::from(file.to_string_lossy().as_ref());
        v["tool_input"]["file_path"] = s.clone();
        if let Some(f) = v.pointer_mut("/tool_response/file") {
            f["filePath"] = s;
        }
    }
    v
}

fn source_lines(n: usize) -> String {
    (0..n).map(|i| format!("fn f{i}() {{}}\n")).collect()
}

// ── meter ───────────────────────────────────────────────────────────────────

/// A full Read becomes one `builtin_read` row with every provenance column filled, and
/// leaves the spool untouched: the negative control for the fault tests below.
#[test]
fn a_captured_read_lands_as_a_row_with_its_provenance() {
    let s = sandbox();
    let file = s.file("hello.rs", HELLO);
    let ran = s.meter(&captured("read_post.json", &s.proj, Some(&file)), &[]);
    assert_eq!(ran.stderr, "");

    let rows = s.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    let path = file.to_string_lossy();
    let tokens = count_tokens(HELLO) as i64;
    assert!(tokens > 0);
    assert_eq!((r.tool.as_str(), r.path.as_str()), ("Read", path.as_ref()));
    assert_eq!(r.lines, Some(3), "`wc -l` semantics: three newline bytes");
    assert_eq!(
        (r.tokens_returned, r.full_tokens, r.saved_tokens),
        (tokens, tokens, 0)
    );
    assert_eq!(r.routed_via, "builtin_read");
    assert_eq!(
        r.channel, "unknown",
        "no entrypoint and no VS Code variables"
    );
    assert_eq!(r.session_id.as_deref(), Some(READ_SESSION));
    assert_eq!(r.file_mtime, Some(MTIME));
    assert_eq!(r.req_key.as_deref(), Some(path.as_ref()));
    assert_eq!(r.writer_hook.as_deref(), Some("lumen_meter.sh"));
    assert_eq!(r.token_source.as_deref(), Some("measured"));
    assert!(
        !s.spool.exists(),
        "a successful insert wrote a fault: {:?}",
        s.faults()
    );
}

/// A Read of one line out of four is recorded at the whole file's size, as the shell
/// meter recorded it. Pinned so that changing it is a decision and not a drift: the
/// payload carries the slice (`tool_response.file.content`), so an exact figure exists.
#[test]
fn a_partial_read_is_recorded_at_the_whole_file() {
    let s = sandbox();
    let file = s.file("hello.rs", HELLO);
    let payload = captured("read_post_partial.json", &s.proj, Some(&file));
    assert_eq!(payload["tool_input"]["limit"], 1, "premise: a sliced Read");
    s.meter(&payload, &[]);

    let r = &s.rows()[0];
    assert_eq!(r.full_tokens, count_tokens(HELLO) as i64);
    assert_eq!(r.tokens_returned, r.full_tokens);
    assert_eq!(r.lines, Some(3));
}

/// A file that is not UTF-8, an image say, has no token count, and its row says so: 0,
/// labelled `unsupported`. A PNG once went into the ledger as 0 `measured`, in the one
/// column there to tell those apart. Until 1.6.0 a test held the shell meter to this;
/// it went when the shell meter did. The capture is a text Read aimed at a PNG: the
/// meter counts the file on disk and never reads the response, so an image Read takes
/// the same path.
#[test]
fn a_file_that_is_not_utf8_is_recorded_as_unsupported_with_no_count() {
    let s = sandbox();
    // A PNG's first bytes. 0x89 cannot begin a UTF-8 sequence.
    let png = s.file("logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
    let ran = s.meter(&captured("read_post.json", &s.proj, Some(&png)), &[]);
    assert_eq!(ran.stderr, "");
    // The control: the same bytes without the 0x89 are UTF-8, and measured.
    let text = s.file("logo.txt", b"PNG\r\n\x1a\n\0\0\0\rIHDR");
    s.meter(&captured("read_post.json", &s.proj, Some(&text)), &[]);

    let rows = s.rows();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (r, control) = (&rows[0], &rows[1]);
    assert_eq!(r.token_source.as_deref(), Some("unsupported"));
    assert_eq!(
        (r.tokens_returned, r.full_tokens, r.saved_tokens),
        (0, 0, 0)
    );
    assert_eq!(control.token_source.as_deref(), Some("measured"));
    assert!(control.full_tokens > 0, "{control:?}");
    assert!(!s.spool.exists(), "{:?}", s.faults());
}

/// Bash output is observed, never intercepted: one `bash_output` row, labelled with the
/// program and subcommand only.
#[test]
fn a_captured_bash_call_lands_as_a_bash_output_row() {
    let s = sandbox();
    let payload = captured("bash_post.json", &s.proj, None);
    let stdout = payload["tool_response"]["stdout"]
        .as_str()
        .unwrap()
        .to_string();
    let ran = s.meter(&payload, &[]);
    assert_eq!(ran.stderr, "");

    let rows = s.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let r = &rows[0];
    assert_eq!((r.tool.as_str(), r.path.as_str()), ("Bash", "ls -la"));
    assert_eq!(r.lines, None);
    assert_eq!(
        (r.tokens_returned, r.full_tokens, r.saved_tokens),
        (0, count_tokens(&stdout) as i64, 0)
    );
    assert!(r.full_tokens > 0);
    assert_eq!(r.routed_via, "bash_output");
    assert_eq!(r.session_id.as_deref(), Some(BASH_SESSION));
    assert_eq!((r.file_mtime, r.req_key.as_deref()), (None, None));
    assert_eq!(r.token_source.as_deref(), Some("measured"));
    assert!(!s.spool.exists(), "{:?}", s.faults());
}

/// Lumen's own tools meter themselves, so the hook must not count them a second time.
/// Only `tool_name` differs from the Read capture, and `tool_input.file_path` stays: a
/// meter that took any tool with a path for a Read would have a file to count.
#[test]
fn a_lumen_tool_call_is_not_metered_twice() {
    let s = sandbox();
    let file = s.file("hello.rs", HELLO);
    for tool in [
        "mcp__lumen__smart_read",
        "mcp__lumen__recall_file",
        "mcp__lumen__compress_logs",
    ] {
        let mut payload = captured("read_post.json", &s.proj, Some(&file));
        payload["tool_name"] = Value::from(tool);
        let ran = s.meter(&payload, &[]);
        assert_eq!(ran.stderr, "", "{tool}");
    }
    assert!(!s.db.exists(), "a lumen tool call was metered by the hook");
    assert!(!s.spool.exists(), "{:?}", s.faults());

    // The control: the same payload as a Read is a row.
    s.meter(&captured("read_post.json", &s.proj, Some(&file)), &[]);
    assert_eq!(s.rows().len(), 1);
}

/// The command line is not stored. A leading assignment is the commonest way a secret
/// rides along.
#[test]
fn a_secret_on_the_command_line_never_reaches_the_ledger() {
    let s = sandbox();
    let mut payload = captured("bash_post.json", &s.proj, None);
    payload["tool_input"]["command"] =
        Value::from("AWS_SECRET=abc123 cargo test --workspace -- --token=xyz");
    s.meter(&payload, &[]);

    let r = &s.rows()[0];
    assert_eq!(r.path, "cargo test");
    let conn = Connection::open_with_flags(&s.db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let leaked: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM read_events WHERE path LIKE '%abc123%' \
             OR IFNULL(req_key, '') LIKE '%abc123%' OR path LIKE '%xyz%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
}

/// LUMEN_METER_BASH=0 records no command output, and nothing else changes: Read is
/// still metered. It is the Bash opt-out that survives Setup, which keeps
/// settings.json's `env` but puts a removed Bash entry back.
#[test]
fn lumen_meter_bash_zero_records_no_command_output_and_still_meters_reads() {
    let s = sandbox();
    let off = [("LUMEN_METER_BASH", "0")];
    let bash = captured("bash_post.json", &s.proj, None);
    let ran = s.meter(&bash, &off);
    assert_eq!(
        (ran.code, ran.stdout.as_str(), ran.stderr.as_str()),
        (0, "", "")
    );
    assert!(!s.db.exists(), "nothing was metered, so no ledger");

    let file = s.file("hello.rs", HELLO);
    s.meter(&captured("read_post.json", &s.proj, Some(&file)), &off);
    let routes: Vec<_> = s.rows().into_iter().map(|r| r.routed_via).collect();
    assert_eq!(routes, ["builtin_read"]);

    // The control: the same payload without it is a row.
    s.meter(&bash, &[]);
    let routes: Vec<_> = s.rows().into_iter().map(|r| r.routed_via).collect();
    assert_eq!(routes, ["builtin_read", "bash_output"]);
    assert!(!s.spool.exists(), "{:?}", s.faults());
}

/// R3, first half. A ledger from before the provenance columns used to swallow every
/// hook insert: the shell meter's INSERT named columns that did not exist and its
/// errors went to /dev/null. The hook opens the ledger through the migrations, so the
/// row lands.
#[test]
fn a_pre_migration_ledger_is_migrated_and_the_row_lands() {
    let s = sandbox();
    {
        let conn = Connection::open(&s.db).unwrap();
        conn.execute_batch(
            "CREATE TABLE read_events (
                 ts              TEXT NOT NULL,
                 tool            TEXT NOT NULL,
                 path            TEXT NOT NULL,
                 lines           INTEGER,
                 tokens_returned INTEGER NOT NULL,
                 full_tokens     INTEGER NOT NULL,
                 saved_tokens    INTEGER NOT NULL,
                 routed_via      TEXT NOT NULL
             );",
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('read_events') \
                 WHERE name IN ('session_id', 'writer_hook', 'token_source')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "premise: the legacy table has no provenance columns");
    }
    let file = s.file("hello.rs", HELLO);
    let ran = s.meter(&captured("read_post.json", &s.proj, Some(&file)), &[]);
    assert_eq!(ran.stderr, "");

    let rows = s.rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].session_id.as_deref(), Some(READ_SESSION));
    assert_eq!(rows[0].token_source.as_deref(), Some("measured"));
    assert!(!s.spool.exists(), "{:?}", s.faults());
}

/// R3, second half. No data directory means Lumen is not installed where the hook was
/// told it is. The hook says so twice — the event was lost, and so was the record of
/// losing it, because the spool lives in that directory too — and creates nothing.
#[test]
fn a_missing_data_directory_is_said_and_nothing_is_created() {
    let mut s = sandbox();
    let absent = s.home.join("not-installed");
    s.db = absent.join("lumen.db");
    let file = s.file("hello.rs", HELLO);
    // The spool defaults to beside the ledger, as it does for a real install.
    let ran = s.meter(
        &captured("read_post.json", &s.proj, Some(&file)),
        &[("LUMEN_FAULT_SPOOL", "")],
    );
    eprintln!("raw stderr, missing data directory:\n{}", ran.stderr);

    let lines: Vec<&str> = ran.stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains("read_events open failed") && lines[0].ends_with("was not metered"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("could not write the fault spool"),
        "{lines:?}"
    );
    assert!(!absent.exists(), "the hook created {}", absent.display());
}

/// B1. A failed INSERT is a fault in the spool, not a silence; and the falsification —
/// the same payload against the restored ledger lands a row and adds no fault.
#[test]
fn an_unwritable_ledger_is_a_fault_and_restoring_it_lands_the_row() {
    let s = sandbox();
    let file = s.file("hello.rs", HELLO);
    let payload = captured("read_post.json", &s.proj, Some(&file));
    s.meter(&payload, &[]);
    assert_eq!(s.rows().len(), 1, "premise: a working ledger");

    set_readonly(&s.db, true);
    // As root the permission bit is ignored and there is nothing to test; say so rather
    // than pass.
    assert!(
        std::fs::File::options().append(true).open(&s.db).is_err(),
        "premise: {} must be unwritable to this user",
        s.db.display()
    );
    let broken = s.meter(&payload, &[]);
    let after_broken = s.faults();
    set_readonly(&s.db, false);
    eprintln!(
        "raw stderr, read-only ledger:\n{}raw spool:\n{}",
        broken.stderr,
        std::fs::read_to_string(&s.spool).unwrap_or_default()
    );

    assert!(
        broken.stderr.contains("was not metered"),
        "{:?}",
        broken.stderr
    );
    assert_eq!(after_broken.len(), 1, "{after_broken:?}");
    let f = &after_broken[0];
    assert_eq!(f["kind"], "meter_write_failed");
    assert_eq!(f["variant"], "insert");
    assert_eq!(f["path"], file.to_string_lossy().as_ref());
    assert_eq!(f["lines"], 3);
    assert_eq!(f["session_id"], READ_SESSION);
    assert!(
        f["detail"].as_str().is_some_and(|d| d.contains("readonly")),
        "{f}"
    );
    assert_eq!(s.rows().len(), 1, "the failed insert must not have landed");

    let restored = s.meter(&payload, &[]);
    eprintln!("raw stderr, restored ledger: {:?}", restored.stderr);
    assert_eq!(restored.stderr, "");
    assert_eq!(s.rows().len(), 2, "the restored ledger takes the row");
    assert_eq!(s.faults().len(), 1, "and no new fault: {:?}", s.faults());
}

fn set_readonly(p: &Path, on: bool) {
    let mut perms = std::fs::metadata(p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(on);
    std::fs::set_permissions(p, perms).unwrap();
}

/// A5. The fault names the surface the session ran on and the version that wrote it,
/// both read at run time: a recorder that always says `cli` folds every VS Code fault
/// into the wrong bucket.
#[test]
fn a_fault_carries_the_running_channel_and_version() {
    let s = sandbox();
    let gone = s.proj.join("deleted-before-the-meter-ran.rs");
    let payload = captured("read_post.json", &s.proj, Some(&gone));
    for (entrypoint, want) in [
        ("claude-vscode", "vscode"),
        ("cli", "cli"),
        ("sdk-ts", "cli"),
    ] {
        s.meter(&payload, &[("CLAUDE_CODE_ENTRYPOINT", entrypoint)]);
        let f = s.faults().pop().expect("a fault");
        assert_eq!(f["variant"], "read_file", "{f}");
        assert_eq!(f["channel"], want, "entrypoint {entrypoint}: {f}");
        assert_eq!(f["version"], env!("CARGO_PKG_VERSION"), "{f}");
    }
    assert!(
        !s.db.exists(),
        "an unreadable file must not create a ledger"
    );
}

/// The hook takes its session from the payload, which names the session the event
/// belongs to. The environment variable is the fallback, not the override.
#[test]
fn the_payload_session_wins_over_the_environment() {
    let s = sandbox();
    let file = s.file("hello.rs", HELLO);
    let payload = captured("read_post.json", &s.proj, Some(&file));
    s.meter(
        &payload,
        &[("CLAUDE_CODE_SESSION_ID", "from-the-environment")],
    );
    let mut bare = payload.clone();
    bare.as_object_mut().unwrap().remove("session_id");
    s.meter(&bare, &[("CLAUDE_CODE_SESSION_ID", "from-the-environment")]);

    let sessions: Vec<Option<String>> = s.rows().into_iter().map(|r| r.session_id).collect();
    assert_eq!(
        sessions,
        [
            Some(READ_SESSION.to_string()),
            Some("from-the-environment".to_string())
        ]
    );
}

#[test]
fn a_hook_with_no_subcommand_is_a_usage_error() {
    let s = sandbox();
    for args in [&[][..], &["bogus"][..]] {
        let ran = s.hook(args, b"{}", &[]);
        assert_eq!(ran.code, 1, "{args:?}: {ran:?}");
        assert!(ran.stderr.starts_with("usage: lumen-mcp hook"), "{ran:?}");
        assert_eq!(ran.stdout, "");
    }
}

// ── intercept ───────────────────────────────────────────────────────────────

/// The redirect, end to end: a large source Read is blocked once with the redirect on
/// stderr and nothing else there, then the retry the message promises goes through and
/// is recorded as the route having failed.
#[test]
fn a_large_source_read_is_blocked_once_and_the_retry_is_let_through() {
    let s = sandbox();
    let big = s.file("big.rs", source_lines(400));
    let payload = captured("read_pre.json", &s.proj, Some(&big));
    let path = big.to_string_lossy();

    let first = s.intercept(&payload, &[]);
    eprintln!(
        "raw stderr, first Read (exit {}):\n{}",
        first.code, first.stderr
    );
    assert_eq!(first.code, 2, "{first:?}");
    assert!(
        first
            .stderr
            .starts_with(&format!("Lumen intercept: {path} is 400 lines.\n")),
        "on a block, everything on stderr goes to the model: {:?}",
        first.stderr
    );
    assert!(
        first
            .stderr
            .contains(&format!("lumen:smart_read(path=\"{path}\")"))
    );
    assert!(
        !s.spool.exists(),
        "a block is not a fault: {:?}",
        s.faults()
    );
    let marker = s.tmp.join(format!("lumen_intercept_{READ_SESSION}"));
    assert!(
        marker.is_file(),
        "the marker must land in the sandbox's temp dir"
    );

    let retry = s.intercept(&payload, &[]);
    assert_eq!((retry.code, retry.stderr.as_str()), (0, ""), "{retry:?}");
    let faults = s.faults();
    assert_eq!(faults.len(), 1, "{faults:?}");
    let f = &faults[0];
    assert_eq!(
        (&f["kind"], &f["variant"]),
        (
            &Value::from("hook_fail_open"),
            &Value::from("retry_escape_valve")
        )
    );
    assert_eq!(f["path"], path.as_ref());
    assert_eq!(f["lines"], 400);
    assert_eq!(f["session_id"], READ_SESSION);
}

/// `.output` included: until #24 the list stopped at `out`, matching is exact, and a
/// large `.output` was read whole.
#[test]
fn a_large_log_read_is_sent_to_compress_logs() {
    let s = sandbox();
    for name in ["build.log", "task.output"] {
        let log = s.file(name, "ok\n".repeat(400));
        let ran = s.intercept(&captured("read_pre.json", &s.proj, Some(&log)), &[]);
        assert_eq!(ran.code, 2, "{name}: {ran:?}");
        assert!(
            ran.stderr.contains(&format!(
                "lumen:compress_logs(path=\"{}\")",
                log.to_string_lossy()
            )),
            "{name}: {ran:?}"
        );
    }
}

/// Each case the intercept must let through silently: below the threshold, a kind it
/// does not route, switched off, a Read of a missing file, and a Bash call.
#[test]
fn the_intercept_passes_what_it_does_not_route() {
    let s = sandbox();
    let small = s.file("small.rs", source_lines(299));
    let notes = s.file("notes.md", "line\n".repeat(400));
    let big = s.file("big.rs", source_lines(400));
    let missing = s.proj.join("missing.rs");
    let read = |file: &Path| captured("read_pre.json", &s.proj, Some(file));
    let off = [("LUMEN_HOOK_ENABLED", "0")];
    let cases = [
        ("299 lines", read(&small), &[][..]),
        ("markdown", read(&notes), &[]),
        ("disabled", read(&big), &off),
        ("missing file", read(&missing), &[]),
        ("Bash", captured("bash_pre.json", &s.proj, None), &[]),
    ];
    for (name, payload, env) in cases {
        let ran = s.intercept(&payload, env);
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{name}: {ran:?}");
    }
    assert!(!s.spool.exists(), "{:?}", s.faults());
    assert!(!s.db.exists(), "the intercept never writes the ledger");
}
