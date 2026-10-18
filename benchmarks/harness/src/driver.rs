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

use crate::sandbox::Sandbox;
use crate::util::{Ctx, Res};
use crate::versions::caps::saved_agrees;
use lumen_core::tokenizer::count_tokens;
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
pub fn records_for(iso: &Sandbox, tool_use_id: &str) -> Vec<PathBuf> {
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
pub fn run_intercept(bin: &Path, iso: &Sandbox, payload: &[u8]) -> Res<i32> {
    let mut cmd = Command::new(bin);
    iso.apply(&mut cmd);
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
    let iso = Sandbox::create(root)?;
    let id = "toolu_bench_probe";
    run_intercept(bin, &iso, &lumen_call_payload("bench-probe", id))?;
    Ok(if records_for(&iso, id).is_empty() {
        Identity::Env
    } else {
        Identity::Hook
    })
}

/// A tool call's answer.
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub full: i64,
    pub returned: i64,
    /// The server's own `saved_tokens`, as reported. For a version the map says clamps it is
    /// recorded and never used.
    pub saved_reported: i64,
    /// `full - returned`, signed. For a clamped version `returned` is the harness's re-count of
    /// the reply text, so a reply longer than the file is a negative figure, as it is for every
    /// other version.
    pub saved_signed: i64,
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
    iso: Sandbox,
    session: String,
    identity: Identity,
    /// Distinguishes this server's tool-use ids from any other's in the same ledger directory.
    tag: u64,
    calls: u64,
    /// The capability map says this version floors `saved_tokens` at zero.
    clamped: bool,
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
        iso: &Sandbox,
        session: &str,
        repeat_reads: bool,
        identity: Identity,
    ) -> Res<Server> {
        let mut cmd = Command::new(bin);
        iso.apply(&mut cmd);
        if identity == Identity::Env {
            cmd.env("CLAUDE_CODE_SESSION_ID", session);
        }
        // Always said, never left to the build's own default: from 1.8.0 on repeat reads are on
        // unless turned off, so an arm that calls itself "repeat reads off" has to ask for it.
        // Every older build reads "off" as it reads an unset variable (1.7.0: anything
        // unrecognised is off), so the arm means the same thing on every build compared.
        cmd.env(
            "LUMEN_REPEAT_READS",
            if repeat_reads { "on" } else { "off" },
        );
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
            clamped: false,
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

    /// Say what the capability map says about this version's `saved_tokens`: clamped at zero
    /// (1.0.0) or signed. Set before the first call; the default is signed.
    pub fn set_clamped(&mut self, clamped: bool) {
        self.clamped = clamped;
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
        // The figure is judged as the map says the version computes it. A clamped version must
        // report exactly `max(0, full - returned)`; anything else means the map is wrong about
        // this build, which stops the run. Its signed saving is then taken from the re-counted
        // text, never from what it reported.
        if !saved_agrees(self.clamped, full, returned, saved) {
            return Err(format!(
                "{tool}: _meta is inconsistent: {full} - {returned} != {saved}{}",
                if self.clamped {
                    " (and not the zero floor the capability map says this version applies)"
                } else {
                    ""
                }
            ));
        }
        let saved_signed = if self.clamped {
            full - count_tokens(&text) as i64
        } else {
            saved
        };
        Ok(Reply {
            text,
            full,
            returned,
            saved_reported: saved,
            saved_signed,
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
    /// [`NO_ROUTE`] when this ledger has no `routed_via` column.
    pub routed_via: String,
    pub returned: i64,
    pub full: i64,
    /// `None` when the row has none, or the ledger has no `repeat_plan` column.
    pub repeat_plan: Option<String>,
}

/// What `routed_via` says in a ledger that has no such column: a figure that cannot exist, said
/// so, and never an empty string that would pass for a route.
pub const NO_ROUTE: &str = "n/a — no routed_via column";

/// The columns of `read_events` that this ledger has. Old ledgers lack columns that newer ones
/// carry, and a query that names one fails on them: the cross-check asks what exists first.
pub fn read_event_columns(db: &Path) -> Res<Vec<String>> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    let mut st = conn
        .prepare("PRAGMA table_info(read_events)")
        .ctx("read the table's columns")?;
    let cols = st
        .query_map([], |r| r.get::<_, String>(1))
        .ctx("read the table's columns")?;
    cols.collect::<Result<Vec<_>, _>>()
        .ctx("read a column name")
}

/// The SELECT list for the columns this ledger has: a column it lacks is a typed NULL, so the row
/// keeps its shape. The three a cross-check cannot do without are required.
fn read_row_query(cols: &[String]) -> Res<String> {
    let has = |c: &str| cols.iter().any(|x| x == c);
    for need in ["tool", "tokens_returned", "full_tokens"] {
        if !has(need) {
            return Err(format!(
                "the ledger's read_events has no {need} column (it has {}): the harness cannot \
                 cross-check this build",
                cols.join(", ")
            ));
        }
    }
    let opt = |c: &str| {
        if has(c) {
            c.to_string()
        } else {
            "NULL".to_string()
        }
    };
    Ok(format!(
        "SELECT tool, {}, tokens_returned, full_tokens, {} FROM read_events \
         WHERE tool LIKE '%smart_read' OR tool LIKE '%recall_file' ORDER BY rowid",
        opt("routed_via"),
        opt("repeat_plan")
    ))
}

/// Every Lumen read row, oldest first. Only the columns the ledger has are queried.
pub fn ledger_rows(db: &Path) -> Res<Vec<LedgerRow>> {
    let sql = read_row_query(&read_event_columns(db)?)?;
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger")?;
    let mut st = conn.prepare(&sql).ctx("prepare the ledger query")?;
    let rows = st
        .query_map([], |r| {
            Ok(LedgerRow {
                tool: r.get(0)?,
                routed_via: r
                    .get::<_, Option<String>>(1)?
                    .unwrap_or_else(|| NO_ROUTE.to_string()),
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
        let iso = Sandbox::create(dir.path()).unwrap();
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

    /// A stand-in for a server: answers `initialize` and every `tools/call` with fixed text and
    /// the `_meta` figures given, and exits when its stdin closes. A version of 1.0.0's kind:
    /// a reply longer than the file reports `saved_tokens` 0, the floor.
    #[cfg(unix)]
    fn server_stub(dir: &Path, full: i64, returned: i64, saved: i64) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let body = format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"serverInfo":{{"name":"s","version":"0.2.0"}}}}}}\n' "$id";;
    *'"tools/call"'*) printf '{{"jsonrpc":"2.0","id":%s,"result":{{"isError":false,"content":[{{"type":"text","text":"alpha beta gamma delta epsilon zeta eta theta"}}],"_meta":{{"full_tokens":{full},"returned_tokens":{returned},"saved_tokens":{saved}}}}}}}\n' "$id";;
    *) printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id";;
  esac
done
"#
        );
        let path = dir.join(format!("srv-{full}-{returned}-{saved}"));
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn call_stub(bin: &Path, root: &Path, clamped: bool) -> Res<Reply> {
        let iso = Sandbox::create(root).unwrap();
        let mut s = Server::start(bin, root, &iso, "bench-stub", false, Identity::Env)?;
        s.set_clamped(clamped);
        s.smart_read("x.rs")
    }

    #[cfg(unix)]
    #[test]
    fn a_clamped_version_does_not_abort_and_its_saving_is_recomputed_from_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        // The file is 3 tokens, the reply says it is 9 and reports a saving of 0: the floor.
        let bin = server_stub(dir.path(), 3, 9, 0);
        let r = call_stub(&bin, &dir.path().join("a"), true).unwrap();
        assert_eq!(r.saved_reported, 0, "the self-reported value is recorded");
        let recount = count_tokens(&r.text) as i64;
        assert_eq!(r.saved_signed, 3 - recount, "and the signed one recomputed");
        assert!(r.saved_signed < 0, "a reply longer than the file is a loss");
        // The same reply from a build the map says is signed is a disagreement: stop.
        let e = call_stub(&bin, &dir.path().join("b"), false).unwrap_err();
        assert!(e.contains("_meta is inconsistent"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn a_clamped_version_that_reports_neither_the_floor_nor_the_saving_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        // Clamping says 0 for a loss: -6 and 7 are both wrong for full 3, returned 9.
        for saved in [-6, 7] {
            let bin = server_stub(dir.path(), 3, 9, saved);
            let e = call_stub(&bin, &dir.path().join(format!("c{saved}")), true).unwrap_err();
            assert!(e.contains("zero floor"), "{e}");
        }
        // A saving that is positive is the same either way.
        let bin = server_stub(dir.path(), 20, 9, 11);
        let r = call_stub(&bin, &dir.path().join("d"), true).unwrap();
        assert_eq!(r.saved_signed, 20 - count_tokens(&r.text) as i64);
    }

    /// A ledger as an old build left it: no `routed_via`, no `repeat_plan`.
    fn old_ledger(dir: &Path, with_routed_via: bool) -> PathBuf {
        let db = dir.join("old.db");
        let c = lumen_core::meter::connect_db(&db).unwrap();
        let extra = if with_routed_via {
            ", routed_via TEXT"
        } else {
            ""
        };
        c.execute_batch(&format!(
            "DROP TABLE read_events; \
             CREATE TABLE read_events (ts TEXT, tool TEXT, path TEXT, tokens_returned INTEGER, \
             full_tokens INTEGER, saved_tokens INTEGER{extra}); \
             INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens) \
             VALUES ('t', 'smart_read', 'a.rs', 9, 3, 0), ('t', 'Read', 'b.rs', 5, 5, 0), \
             ('t', 'mcp__lumen__recall_file', 'a.rs', 2, 3, 1);"
        ))
        .unwrap();
        db
    }

    #[test]
    fn the_ledger_check_asks_only_for_the_columns_the_ledger_has() {
        let dir = tempfile::tempdir().unwrap();
        let db = old_ledger(dir.path(), false);
        let cols = read_event_columns(&db).unwrap();
        assert!(cols.contains(&"tool".to_string()));
        assert!(!cols.contains(&"repeat_plan".to_string()));
        assert!(!cols.contains(&"routed_via".to_string()));
        // The current query would fail on this table; the column-aware one does not.
        let conn = lumen_core::meter::open_read_only(&db).unwrap();
        assert!(
            conn.prepare("SELECT routed_via, repeat_plan FROM read_events")
                .is_err()
        );
        let rows = ledger_rows(&db).unwrap();
        assert_eq!(rows.len(), 2, "the Read row is not a Lumen read");
        assert_eq!(rows[0].returned, 9);
        assert_eq!(rows[0].full, 3);
        assert_eq!(rows[0].repeat_plan, None);
        assert_eq!(rows[0].routed_via, NO_ROUTE);
        assert_eq!(rows[1].tool, "mcp__lumen__recall_file");
    }

    #[test]
    fn a_ledger_without_the_columns_a_cross_check_needs_is_refused_by_name() {
        let cols: Vec<String> = ["tool", "full_tokens"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let e = read_row_query(&cols).unwrap_err();
        assert!(e.contains("tokens_returned"), "{e}");
        // With the columns present the query names exactly those.
        let all: Vec<String> = ["tool", "tokens_returned", "full_tokens", "routed_via"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let q = read_row_query(&all).unwrap();
        assert!(q.contains("routed_via") && !q.contains("repeat_plan, ") && q.contains("NULL"));
    }

    #[test]
    fn a_ledger_with_the_columns_reads_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("new.db");
        let c = lumen_core::meter::connect_db(&db).unwrap();
        c.execute_batch(
            "INSERT INTO read_events (ts, tool, path, tokens_returned, full_tokens, saved_tokens, \
             routed_via) VALUES ('t', 'smart_read', 'a.rs', 9, 30, 21, 'smart_read')",
        )
        .unwrap();
        drop(c);
        let rows = ledger_rows(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].routed_via, "smart_read");
        assert_eq!((rows[0].returned, rows[0].full), (9, 30));
    }
}
