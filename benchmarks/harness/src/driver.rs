//! The real `lumen-mcp` binary, driven over stdio JSON-RPC the way Claude Code drives it.
//!
//! Newline-delimited JSON-RPC 2.0. Tool results carry `_meta.{full_tokens, returned_tokens,
//! saved_tokens}`, which count the reply text only: the JSON-RPC envelope and the `_meta`
//! block are not counted, exactly as in the meter. The harness re-counts every reply with the
//! same tokenizer and refuses a result whose own figure disagrees.
//!
//! The server answers first and writes its ledger row after. It is sequential, so a request
//! sent after a call is not read until that row is written: an untimed `tools/list` after a
//! timed call is how the next timing is kept free of the previous call's ledger write.

use crate::util::{Ctx, Res};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// No call on a corpus file takes anywhere near this long; a server that has not answered by
/// then has hung, and the run stops saying so.
const RPC_TIMEOUT: Duration = Duration::from_secs(120);

/// How a build learns which conversation a tool call belongs to. Decided per build by
/// [`probe_identity`], from what the build does and not from what its usage text says.
///
/// This module is the one place a run supplies identity: [`Server::start`] sets what a
/// [`Identity::Env`] build reads, and [`Server::call`] does what a [`Identity::Hook`] build needs
/// before every call. A third mechanism would be a third variant here and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// 1.7.0 and main at the pinned commit: the server reads `CLAUDE_CODE_SESSION_ID` once, from
    /// its environment at spawn, and keys every repeat-read base on it for its whole life.
    Env,
    /// Live identity (PR #45): the PreToolUse hook (`hook intercept`, on a payload whose
    /// `tool_name` is one of Lumen's own MCP tools) writes a record beside the ledger, keyed by the
    /// call's `tool_use_id`. The server finds it through `_meta["claudecode/toolUseId"]` on the
    /// `tools/call` request, reads it and removes it. A call with no record is answered in full
    /// and remembers nothing, so a harness that only changed the environment would measure full
    /// outlines where a real session gets stubs and deltas.
    Hook,
}

impl Identity {
    /// The word the results file carries.
    pub fn label(self) -> &'static str {
        match self {
            Identity::Env => "env",
            Identity::Hook => "hook",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Identity::Env => "CLAUDE_CODE_SESSION_ID, read once when the server starts",
            Identity::Hook => {
                "a hook record per call (PreToolUse on the Lumen tool), claimed by the server \
                 through _meta claudecode/toolUseId"
            }
        }
    }
}

/// The tool name a Claude Code PreToolUse payload carries for Lumen's `smart_read`.
const IDENTITY_TOOL: &str = "mcp__lumen__smart_read";

/// A PreToolUse payload for a call of one of Lumen's own MCP tools, as Claude Code hands it to a
/// hook: what a live-identity build needs in order to record who is calling.
pub fn lumen_call_payload(session: &str, tool_use_id: &str) -> Vec<u8> {
    json!({
        "session_id": session, "transcript_path": "/dev/null", "cwd": "/",
        "permission_mode": "default", "hook_event_name": "PreToolUse",
        "tool_name": IDENTITY_TOOL, "tool_input": {"path": "x.rs"},
        "tool_use_id": tool_use_id
    })
    .to_string()
    .into_bytes()
}

/// Files beside the ledger whose name carries `tool_use_id`: the hook's record of that call. The
/// id is unique to the call, so nothing else can match.
pub fn records_for(iso: &Isolation, tool_use_id: &str) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(iso.dir()) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(tool_use_id))
        .map(|e| e.path())
        .collect()
}

/// Run `hook intercept` once on `payload` against the isolated ledger, untimed. Returns its exit
/// code.
pub fn run_intercept(bin: &Path, iso: &Isolation, payload: &[u8]) -> Res<i32> {
    let mut cmd = Command::new(bin);
    isolate(&mut cmd, iso);
    cmd.args(["hook", "intercept"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ctx(&format!("spawn {}", bin.display()))?;
    {
        let mut stdin = child.stdin.take().ok_or("the hook has no stdin")?;
        stdin.write_all(payload).ctx("write the hook payload")?;
    }
    let status = child.wait().ctx("wait for the hook")?;
    status
        .code()
        .ok_or_else(|| format!("the hook did not exit normally: {status}"))
}

/// What a build does about identity, found by asking it: a PreToolUse payload for
/// `mcp__lumen__smart_read` goes to `hook intercept` with an isolated ledger directory. A build
/// that records per-call identity leaves a record beside the ledger; one that does not leaves
/// nothing. Nothing is read from the `hook` usage text, which does not change when a build gains
/// this behaviour.
pub fn probe_identity(bin: &Path, root: &Path) -> Res<Identity> {
    let iso = Isolation::create(root)?;
    let id = "toolu_bench_probe";
    run_intercept(bin, &iso, &lumen_call_payload("bench-probe", id))?;
    Ok(if records_for(&iso, id).is_empty() {
        Identity::Env
    } else {
        Identity::Hook
    })
}

/// Where a run keeps everything it writes, so nothing touches a real ledger or the real
/// temp directory (the intercept hook's markers live under `temp_dir()`).
#[derive(Clone)]
pub struct Isolation {
    pub db: PathBuf,
    pub projects: PathBuf,
    pub tmp: PathBuf,
}

impl Isolation {
    /// The directory the ledger is in: where a hook's per-call records go.
    pub fn dir(&self) -> &Path {
        self.db.parent().unwrap_or_else(|| Path::new("."))
    }

    /// Directories under `root`, created. The ledger file is not: the build under test creates it
    /// with its own schema on its first write, so a baseline build never opens a database laid
    /// out by a newer one.
    pub fn create(root: &Path) -> Res<Isolation> {
        let iso = Isolation {
            db: root.join("ledger.db"),
            projects: root.join("projects"),
            tmp: root.join("tmp"),
        };
        std::fs::create_dir_all(&iso.projects).ctx("create the projects dir")?;
        std::fs::create_dir_all(&iso.tmp).ctx("create the tmp dir")?;
        Ok(iso)
    }
}

/// Strip everything that could change an answer or point at the user's own data, then point
/// the child at the isolated directories. Any `LUMEN_*` or `CLAUDE*` variable inherited from
/// the caller (a ranked-outline flag, a line-number switch, a session id) would silently make
/// two runs measure different things.
pub fn isolate(cmd: &mut Command, iso: &Isolation) {
    for (k, _) in std::env::vars_os() {
        if let Some(k) = k.to_str()
            && (k.starts_with("LUMEN_") || k.starts_with("CLAUDE"))
        {
            cmd.env_remove(k);
        }
    }
    cmd.env("LUMEN_DB", &iso.db)
        .env("LUMEN_PROJECTS_DIR", &iso.projects)
        .env("TMPDIR", &iso.tmp)
        .env("TMP", &iso.tmp)
        .env("TEMP", &iso.tmp);
}

/// A tool call's answer.
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub full: i64,
    pub returned: i64,
}

pub struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<std::io::Result<String>>,
    next_id: i64,
    /// `serverInfo.version` from `initialize`.
    pub version: String,
    /// Monotonic time from just before the last request was written to the moment its reply line
    /// arrived: the round trip a client sees. Request building and reply parsing are outside it.
    pub last_rtt: Duration,
    bin: PathBuf,
    iso: Isolation,
    session: String,
    identity: Identity,
    /// Distinguishes this server's tool-use ids from any other's in the same ledger directory.
    tag: u64,
    calls: u64,
}

/// A source of `Server::tag`s: no two servers of one run share one.
static SERVER_TAG: AtomicU64 = AtomicU64::new(0);

impl Server {
    /// Spawn the binary with `cwd` as its working directory and run `initialize`.
    ///
    /// Paths sent to the server are relative to `cwd`, so the path embedded in every reply
    /// (an outline carries it three times) is the same on every machine.
    ///
    /// Identity: an [`Identity::Env`] build gets `session` as `CLAUDE_CODE_SESSION_ID`. An
    /// [`Identity::Hook`] build does not read it; [`Server::call`] gives each call a hook record
    /// that names `session` instead.
    pub fn start(
        bin: &Path,
        cwd: &Path,
        iso: &Isolation,
        session: &str,
        repeat_reads: bool,
        identity: Identity,
    ) -> Res<Server> {
        let mut cmd = Command::new(bin);
        isolate(&mut cmd, iso);
        if identity == Identity::Env {
            cmd.env("CLAUDE_CODE_SESSION_ID", session);
        }
        if repeat_reads {
            cmd.env("LUMEN_REPEAT_READS", "on");
        }
        cmd.current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().ctx(&format!("spawn {}", bin.display()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("the server has no stdout")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Server {
            child,
            stdin,
            lines,
            next_id: 0,
            version: String::new(),
            last_rtt: Duration::ZERO,
            bin: bin.to_path_buf(),
            iso: iso.clone(),
            session: session.to_string(),
            identity,
            tag: SERVER_TAG.fetch_add(1, Ordering::Relaxed),
            calls: 0,
        };
        let init = s.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "lumen-bench", "version": "0"}}),
        )?;
        s.version = init["result"]["serverInfo"]["version"]
            .as_str()
            .ok_or_else(|| format!("initialize named no server version: {init}"))?
            .to_string();
        Ok(s)
    }

    pub fn rpc(&mut self, method: &str, params: Value) -> Res<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let frame = format!("{req}\n");
        let stdin = self.stdin.as_mut().ok_or("the server's stdin is closed")?;
        let t0 = Instant::now();
        stdin
            .write_all(frame.as_bytes())
            .ctx(&format!("write {method}"))?;
        stdin.flush().ctx(&format!("flush {method}"))?;
        let got = self.lines.recv_timeout(RPC_TIMEOUT);
        self.last_rtt = t0.elapsed();
        let line = match got {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => return Err(format!("{method}: reading the server's stdout: {e}")),
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "{method}: the server did not answer within {} s",
                    RPC_TIMEOUT.as_secs()
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(format!("{method}: the server closed its stdout"));
            }
        };
        let v: Value = serde_json::from_str(&line).ctx(&format!("{method}: reply {line:?}"))?;
        if v["id"] != json!(id) {
            return Err(format!("{method}: reply for another request: {line}"));
        }
        if let Some(e) = v.get("error") {
            return Err(format!("{method}: the server answered with an error: {e}"));
        }
        Ok(v)
    }

    /// One `tools/call`. No sync: the caller decides whether it needs the ledger yet.
    ///
    /// For an [`Identity::Hook`] build the call is placed first, the way Claude Code places it:
    /// the real `hook intercept` runs on a PreToolUse payload for the tool with a fresh
    /// `tool_use_id`, and the request then carries that id in `_meta`. That happens before the
    /// request is written, so it is outside [`Server::last_rtt`]. The call is refused if the hook
    /// left no record, or if the server left the record unclaimed: either way it would not have
    /// been answered as part of this session.
    pub fn call(&mut self, tool: &str, args: Value) -> Res<Reply> {
        self.call_as(tool, args, true)
    }

    /// The same call with no identity: no hook record and no `_meta`. For an
    /// [`Identity::Hook`] build the server answers it in full and remembers nothing; for an
    /// [`Identity::Env`] build it is the ordinary call. Used to time what placing a call costs
    /// the server.
    pub fn call_unplaced(&mut self, tool: &str, args: Value) -> Res<Reply> {
        self.call_as(tool, args, false)
    }

    /// Write the hook's record for the next call; returns the `tool_use_id`.
    fn record_call(&mut self) -> Res<String> {
        self.calls += 1;
        let id = format!("toolu_{}_{}_{}", std::process::id(), self.tag, self.calls);
        let payload = lumen_call_payload(&self.session, &id);
        let code = run_intercept(&self.bin, &self.iso, &payload)?;
        if code != 0 {
            return Err(format!("the identity hook exited {code} for {id}"));
        }
        if records_for(&self.iso, &id).is_empty() {
            return Err(format!(
                "the identity hook left no record for {id} beside {}",
                self.iso.db.display()
            ));
        }
        Ok(id)
    }

    fn call_as(&mut self, tool: &str, args: Value, placed: bool) -> Res<Reply> {
        let mut params = json!({"name": tool, "arguments": args});
        let call_id = if placed && self.identity == Identity::Hook {
            let id = self.record_call()?;
            params["_meta"] = json!({"claudecode/toolUseId": id});
            Some(id)
        } else {
            None
        };
        let v = self.rpc("tools/call", params)?;
        if let Some(id) = &call_id
            && !records_for(&self.iso, id).is_empty()
        {
            return Err(format!(
                "{tool}: the server did not claim the hook's record for {id}, so the call was not \
                 placed in the session"
            ));
        }
        let r = &v["result"];
        if r["isError"] != json!(false) {
            return Err(format!("{tool}: isError is not false: {v}"));
        }
        let text = r["content"][0]["text"]
            .as_str()
            .ok_or_else(|| format!("{tool}: no text in {v}"))?
            .to_string();
        let meta = |k: &str| {
            r["_meta"][k]
                .as_i64()
                .ok_or_else(|| format!("{tool}: _meta.{k} missing in {v}"))
        };
        let (full, returned, saved) = (
            meta("full_tokens")?,
            meta("returned_tokens")?,
            meta("saved_tokens")?,
        );
        if saved != full - returned {
            return Err(format!(
                "{tool}: _meta is inconsistent: {full} - {returned} != {saved}"
            ));
        }
        Ok(Reply {
            text,
            full,
            returned,
        })
    }

    pub fn smart_read(&mut self, path: &str) -> Res<Reply> {
        self.call("smart_read", json!({ "path": path }))
    }

    /// `smart_read` with no identity, see [`Server::call_unplaced`].
    pub fn smart_read_unplaced(&mut self, path: &str) -> Res<Reply> {
        self.call_unplaced("smart_read", json!({ "path": path }))
    }

    pub fn recall(&mut self, path: &str, names: &[&str]) -> Res<Reply> {
        self.call("recall_file", json!({ "path": path, "names": names }))
    }

    /// The server's own no-work round trip: the floor under every other call's latency.
    pub fn ping(&mut self) -> Res<()> {
        self.rpc("ping", json!({})).map(|_| ())
    }

    /// Returns once the previous call's ledger row has been written.
    pub fn sync(&mut self) -> Res<()> {
        self.rpc("tools/list", json!({})).map(|_| ())
    }

    /// Close stdin and require a clean exit: the last ledger write has happened once this
    /// returns.
    pub fn finish(mut self) -> Res<()> {
        drop(self.stdin.take());
        let status = self.child.wait().ctx("wait for the server")?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("the server exited with {status}"))
        }
    }
}

impl Drop for Server {
    /// An error path leaves no server behind.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.child.kill().ok();
            self.child.wait().ok();
        }
    }
}

/// One `smart_read` / `recall_file` row of the ledger, in the order the server wrote them.
#[derive(Debug, Clone)]
pub struct LedgerRow {
    pub tool: String,
    pub routed_via: String,
    pub returned: i64,
    pub full: i64,
    pub repeat_plan: Option<String>,
}

/// Every Lumen read row, oldest first.
pub fn ledger_rows(db: &Path) -> Res<Vec<LedgerRow>> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    let mut st = conn
        .prepare(
            "SELECT tool, routed_via, tokens_returned, full_tokens, repeat_plan FROM read_events \
             WHERE tool LIKE '%smart_read' OR tool LIKE '%recall_file' ORDER BY rowid",
        )
        .ctx("prepare the ledger query")?;
    let rows = st
        .query_map([], |r| {
            Ok(LedgerRow {
                tool: r.get(0)?,
                routed_via: r.get(1)?,
                returned: r.get(2)?,
                full: r.get(3)?,
                repeat_plan: r.get(4)?,
            })
        })
        .ctx("query the ledger")?;
    rows.collect::<Result<Vec<_>, _>>().ctx("read a ledger row")
}

/// Number of rows in `read_events` (all tools).
pub fn ledger_count(db: &Path) -> Res<i64> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    conn.query_row("SELECT COUNT(*) FROM read_events", [], |r| r.get(0))
        .ctx("count ledger rows")
}

/// Number of `read_events` rows written for one hook-metered tool (`Read` or `Bash`).
pub fn ledger_tool_count(db: &Path, tool: &str) -> Res<i64> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    conn.query_row(
        "SELECT COUNT(*) FROM read_events WHERE tool = ?1",
        [tool],
        |r| r.get(0),
    )
    .ctx("count ledger rows for a tool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lumen_call_payload_is_a_pretooluse_for_the_lumen_tool_with_its_ids() {
        let v: Value = serde_json::from_slice(&lumen_call_payload("s-1", "toolu_7")).unwrap();
        assert_eq!(v["hook_event_name"], "PreToolUse");
        assert_eq!(v["tool_name"], "mcp__lumen__smart_read");
        assert_eq!(v["session_id"], "s-1");
        assert_eq!(v["tool_use_id"], "toolu_7");
    }

    #[test]
    fn records_are_found_by_the_call_id_in_their_name() {
        let dir = tempfile::tempdir().unwrap();
        let iso = Isolation::create(dir.path()).unwrap();
        assert!(records_for(&iso, "toolu_1").is_empty());
        std::fs::write(iso.dir().join("lumen_call_toolu_1.json"), "{}").unwrap();
        std::fs::write(iso.dir().join("lumen_call_toolu_2.json"), "{}").unwrap();
        assert_eq!(records_for(&iso, "toolu_1").len(), 1);
        assert_eq!(iso.dir(), dir.path());
    }

    /// A stand-in for a lumen-mcp binary: `hook intercept` reads its payload and, if `records`,
    /// leaves a `lumen_call_<id>.json` beside the ledger the way a live-identity build does.
    #[cfg(unix)]
    fn stub(dir: &Path, name: &str, records: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let body = if records {
            "cat > /dev/null\ntouch \"$(dirname \"$LUMEN_DB\")/lumen_call_toolu_bench_probe.json\"\n"
        } else {
            "cat > /dev/null\n"
        };
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_asks_the_build_what_it_does_and_does_not_read_its_usage_text() {
        let dir = tempfile::tempdir().unwrap();
        let recording = stub(dir.path(), "records", true);
        let silent = stub(dir.path(), "silent", false);
        assert_eq!(
            probe_identity(&recording, &dir.path().join("p1")).unwrap(),
            Identity::Hook
        );
        assert_eq!(
            probe_identity(&silent, &dir.path().join("p2")).unwrap(),
            Identity::Env
        );
    }
}
