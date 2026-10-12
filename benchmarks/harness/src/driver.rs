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
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// No call on a corpus file takes anywhere near this long; a server that has not answered by
/// then has hung, and the run stops saying so.
const RPC_TIMEOUT: Duration = Duration::from_secs(120);

/// How the server learns which session it is serving: the ONE place a run sets identity.
///
/// Today (1.7.0 and main at the pinned commit) the server reads `CLAUDE_CODE_SESSION_ID` once,
/// from its environment at spawn, and keys every repeat-read base on it for its whole life. So
/// a run sets it here, before the child exists, and calls this nowhere else.
///
/// If identity moves to a live id handed over by a hook before each call, this is the function
/// to adapt: spawn without the variable (or with the first id) and have [`Server::call`] run
/// the identity hook for each call, which is also what the "per-call identity hook overhead"
/// latency slot would then time. Hook payloads carry their own `session_id`; the latency
/// module generates a fresh one per run for the intercept's once-per-session marker.
pub fn set_session_identity(cmd: &mut Command, session: &str) {
    cmd.env("CLAUDE_CODE_SESSION_ID", session);
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
}

impl Server {
    /// Spawn the binary with `cwd` as its working directory and run `initialize`.
    ///
    /// Paths sent to the server are relative to `cwd`, so the path embedded in every reply
    /// (an outline carries it three times) is the same on every machine.
    pub fn start(
        bin: &Path,
        cwd: &Path,
        iso: &Isolation,
        session: &str,
        repeat_reads: bool,
    ) -> Res<Server> {
        let mut cmd = Command::new(bin);
        isolate(&mut cmd, iso);
        set_session_identity(&mut cmd, session);
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
    pub fn call(&mut self, tool: &str, args: Value) -> Res<Reply> {
        let v = self.rpc("tools/call", json!({"name": tool, "arguments": args}))?;
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
