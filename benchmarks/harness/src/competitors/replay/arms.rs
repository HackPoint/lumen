//! The arms, each driven through the mechanism it really uses.

use super::echo::{self, EchoUpstream};
use super::hooks::{self, Hook};
use super::install::{self, Headroom};
use super::shim::{Shim, ShimDir};
use super::{
    Arm, CAVEMAN_NA, Case, Followup, Outcome, Received, Step, ToolCall, apply_head_limit,
    apply_read_window, missing_facts, output_text, post_payload, pre_payload, tool_input,
};
use crate::builds;
use crate::driver::{Identity, Server};
use crate::manifest::{Tools, VersionsManifest, host_platform};
use crate::sandbox::{Sandbox, free_port};
use crate::util::{Ctx, Res};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LUMEN_VERSION: &str = "1.9.0";

/// The path of the item as the arm's tool sees it: written under the case's working directory.
pub fn materialize(sb: &Sandbox, case: &Case) -> Res<(PathBuf, PathBuf)> {
    let work = sb.cwd.join("work");
    let file = work.join(case.rel_path());
    std::fs::create_dir_all(file.parent().unwrap_or(&work)).ctx("create the work directory")?;
    std::fs::write(&file, &case.raw).ctx("write the item")?;
    Ok((work, file))
}

fn path_sep() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

fn real_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

// ---------------------------------------------------------------------------------- none, n/a

/// The raw output, as Claude Code with nothing installed hands it to the model.
pub struct NoneArm;

impl Arm for NoneArm {
    fn id(&self) -> &str {
        "none"
    }
    fn replay(&mut self, case: &Case) -> Res<Outcome> {
        let t = Instant::now();
        let bytes = case.raw.clone();
        let mut r = Received::plain(bytes, t.elapsed());
        r.exit_code = matches!(case.call, ToolCall::Bash { .. }).then_some(case.exit_code);
        Ok(Outcome::Received(r))
    }
}

/// An arm whose every cell is `n/a`, with the reason.
pub struct NaArm {
    pub id: String,
    pub reason: String,
}

impl Arm for NaArm {
    fn id(&self) -> &str {
        &self.id
    }
    fn replay(&mut self, _case: &Case) -> Res<Outcome> {
        Ok(Outcome::Na(self.reason.clone()))
    }
}

// ---------------------------------------------------------------------------------- squeez, RTK

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Squeez,
    Rtk,
}

/// A tool that registers Claude Code hooks: installed into a fresh scratch HOME for each case (its
/// session state and history are per HOME, so one item never sees another), then driven through
/// the hooks its installer wrote into `settings.json`.
pub struct HookedArm {
    pub kind: Kind,
    pub id: String,
    /// The tool's binary in the cache.
    pub bin: PathBuf,
    /// Set in every process the tool runs (`telemetry_env` of tools.toml).
    pub env: Vec<(String, String)>,
    pub root: PathBuf,
    pub seq: u32,
}

impl HookedArm {
    fn sandbox(&mut self) -> Res<Sandbox> {
        self.seq += 1;
        let mut sb = Sandbox::create(&self.root.join(format!("{}-{}", self.id, self.seq)))?;
        for (k, v) in &self.env {
            sb = sb.set(k, v);
        }
        Ok(sb)
    }

    /// Run the tool's own installer in `sb`. Returns the directory that holds the tool's binary.
    pub fn install(&self, sb: &Sandbox) -> Res<PathBuf> {
        std::fs::create_dir_all(sb.home.join(".claude"))
            .ctx("create ~/.claude in the scratch HOME")?;
        let (dir, mut cmd) = match self.kind {
            Kind::Squeez => {
                let dir = sb.home.join(".claude").join("squeez").join("bin");
                std::fs::create_dir_all(&dir).ctx("create the squeez bin directory")?;
                let to = dir.join("squeez");
                std::fs::copy(&self.bin, &to).ctx("put squeez in place")?;
                make_executable(&to)?;
                let mut c = Command::new(&to);
                c.arg("setup");
                (dir, c)
            }
            Kind::Rtk => {
                let dir = sb.root.join("bin");
                std::fs::create_dir_all(&dir).ctx("create the rtk bin directory")?;
                let to = dir.join("rtk");
                std::fs::copy(&self.bin, &to).ctx("put rtk in place")?;
                make_executable(&to)?;
                let mut c = Command::new(&to);
                c.args(["init", "-g", "--auto-patch"]);
                (dir, c)
            }
        };
        sb.apply(&mut cmd);
        cmd.env(
            "PATH",
            format!("{}{}{}", dir.display(), path_sep(), real_path()),
        )
        .stdin(Stdio::null());
        let out = cmd.output().ctx("run the installer")?;
        if !out.status.success() {
            return Err(format!(
                "the {} installer exited with {}: {}",
                self.id,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(dir)
    }
}

fn make_executable(p: &Path) -> Res<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
            .ctx(&format!("chmod {}", p.display()))?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

/// What a Bash call's shim is for: the first word of the command is the program, and for `git`
/// and `cargo` the second is the subcommand the shim answers.
fn bash_shim(command: &str, case: &Case) -> Res<Shim> {
    let mut w = command.split_whitespace();
    let program = w.next().ok_or("an empty command")?.to_string();
    let first_arg = matches!(program.as_str(), "git" | "cargo")
        .then(|| w.next().map(str::to_string))
        .flatten();
    Ok(Shim {
        program,
        first_arg,
        stdout: case.raw.clone(),
        exit_code: case.exit_code,
    })
}

/// How a tool lets the model get back what it dropped, at the tool's documented defaults.
///
/// - squeez: its marker names the `squeez_retrieve` MCP tool, which `squeez setup` does not
///   register (README: `claude mcp add squeez -- squeez mcp` is a separate, manual step), so at
///   defaults there is nothing to call: [`Recovery::None`]. [`HookedArm::install`] checks that
///   no squeez MCP server was registered, and stops if one was.
/// - RTK: its marker `[full output: rtk recall <id>]` is a command the model runs through Bash,
///   on from the first install (the recall store is in HOME): [`Recovery::RtkRecall`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    None,
    RtkRecall,
}

/// The recovery path each hook-driven tool has at its defaults.
pub fn recovery_for(kind: Kind) -> Recovery {
    match kind {
        Kind::Squeez => Recovery::None,
        Kind::Rtk => Recovery::RtkRecall,
    }
}

/// The ids in `[full output: rtk recall <id>]` markers.
pub fn rtk_recall_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("rtk recall ") {
        rest = &rest[at + "rtk recall ".len()..];
        let id: String = rest
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if !id.is_empty() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// A hook-driven replay of one case in `sb`, with the hooks already registered. The pure part of
/// [`HookedArm`], so a test can drive it with a stub tool.
pub fn replay_with_hooks(
    arm_id: &str,
    case: &Case,
    sb: &Sandbox,
    hooks: &[Hook],
    tool_bin: &Path,
    recovery: Recovery,
) -> Res<Outcome> {
    let (work, file) = materialize(sb, case)?;
    let session = format!("replay-{}", case.id);
    let rest = format!("{}{}{}", tool_bin.display(), path_sep(), real_path());
    let (shims, path) = match &case.call {
        ToolCall::Bash { command } => {
            let s = bash_shim(command, case)?;
            let d = ShimDir::create(&sb.root.join("shims"), &[s], &rest)?;
            let p = d.path_with(&rest);
            (Some(d), p)
        }
        _ => (None, rest),
    };
    let t0 = Instant::now();
    let mut notes = Vec::new();
    let original = tool_input(&case.call, &file);
    let pre = hooks::run_event(
        hooks,
        "PreToolUse",
        case.call.tool(),
        &pre_payload(case, &file, &session, &work),
        sb,
        &path,
        &work,
    )?;
    if let Some(msg) = &pre.blocked {
        let bytes = msg.clone().into_bytes();
        let mut r = Received::plain(bytes, t0.elapsed());
        r.notes.push("a hook blocked the call".into());
        return Ok(Outcome::Received(r));
    }
    let input = pre
        .updated_input
        .clone()
        .unwrap_or_else(|| original.clone());
    let rewrote_input = input != original;
    notes.push(format!(
        "PreToolUse: {}",
        if rewrote_input {
            "rewrote the input"
        } else {
            "left the input alone"
        }
    ));
    let total_lines = case.text().lines().count();
    let mut exit_code = None;
    let executed: Vec<u8> = match &case.call {
        ToolCall::Read { .. } => apply_read_window(&case.text(), &input).into_bytes(),
        ToolCall::Grep { .. } => apply_head_limit(&case.text(), &input).into_bytes(),
        ToolCall::Bash { command } => {
            let cmd_line = input["command"].as_str().unwrap_or(command);
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd_line);
            sb.apply(&mut c);
            c.env("PATH", &path).current_dir(&work).stdin(Stdio::null());
            let out = c.output().ctx("run the Bash command")?;
            exit_code = out.status.code();
            let d = shims.as_ref().ok_or("no shim for a Bash call")?;
            let calls = d.calls()?;
            let answered = calls.iter().filter(|c| c.answered).count();
            if answered == 0 {
                let seen: Vec<String> = calls
                    .iter()
                    .map(|c| format!("{} {}", c.program, c.args))
                    .collect();
                return Ok(Outcome::Na(format!(
                    "n/a — {arm_id} ran `{cmd_line}` without calling `{}` the way the recording \
                     was made (calls seen: {})",
                    d.log
                        .file_name()
                        .map_or(String::new(), |_| bash_program(command)),
                    if seen.is_empty() {
                        "none".to_string()
                    } else {
                        seen.join("; ")
                    }
                )));
            }
            let passed = calls.len() - answered;
            if passed > 0 {
                notes.push(format!("{passed} other call(s) went to the real program"));
            }
            let mut b = out.stdout;
            b.extend_from_slice(&out.stderr);
            b
        }
    };
    let exec_text = String::from_utf8_lossy(&executed).into_owned();
    let post = hooks::run_event(
        hooks,
        "PostToolUse",
        case.call.tool(),
        &post_payload(case, &input, &exec_text, total_lines, &session, &work),
        sb,
        &path,
        &work,
    )?;
    let bytes = match &post.updated_output {
        Some(v) => {
            notes.push("PostToolUse: rewrote the output".into());
            output_text(v)?.into_bytes()
        }
        None => {
            notes.push("PostToolUse: left the output alone".into());
            executed
        }
    };
    // The ideal asker: while a needle's fact is missing, follow the tool's marker, one round each.
    let mut followups: Vec<Followup> = Vec::new();
    if recovery == Recovery::RtkRecall {
        let mut held = bytes.clone();
        for id in rtk_recall_ids(&String::from_utf8_lossy(&bytes)) {
            if missing_facts(&held, &case.needles, &case.category).is_empty() {
                break;
            }
            let mut c = Command::new(tool_bin.join("rtk"));
            sb.apply(&mut c);
            c.args(["recall", &id])
                .current_dir(&work)
                .stdin(Stdio::null());
            let out = c.output().ctx("run rtk recall")?;
            if !out.status.success() {
                notes.push(format!("rtk recall {id} exited {}", out.status));
                break;
            }
            held.push(b'\n');
            held.extend_from_slice(&out.stdout);
            followups.push(Followup::plain(&format!("rtk recall {id}"), out.stdout));
        }
    }
    let wall = t0.elapsed();
    Ok(Outcome::Received(Received {
        steps: vec![Step::of("output", &bytes)],
        bytes,
        first_rounds: 0,
        followups,
        exit_code,
        notes,
        wall,
    }))
}

fn bash_program(command: &str) -> String {
    command.split_whitespace().next().unwrap_or("").to_string()
}

impl Arm for HookedArm {
    fn id(&self) -> &str {
        &self.id
    }
    fn replay(&mut self, case: &Case) -> Res<Outcome> {
        let sb = self.sandbox()?;
        let bin_dir = self.install(&sb)?;
        let hooks = hooks::load(&sb.home.join(".claude").join("settings.json"))?;
        let recovery = recovery_for(self.kind);
        if self.kind == Kind::Squeez {
            // The claim that squeez has no recovery at defaults holds only while its MCP server
            // is not registered: look where `claude mcp add` would have written it.
            let cj = sb.home.join(".claude.json");
            let registered = std::fs::read_to_string(&cj).is_ok_and(|t| t.contains("squeez"));
            if registered {
                return Err(
                    "squeez registered an MCP server at install; its recovery path is no longer \
                            the default METHOD.md describes"
                        .to_string(),
                );
            }
        }
        let out = replay_with_hooks(&self.id, case, &sb, &hooks, &bin_dir, recovery);
        std::fs::remove_dir_all(&sb.root).ok();
        out
    }
}

// ---------------------------------------------------------------------------------- Lumen

/// Lumen through its real hook and its real `lumen-mcp`.
pub struct LumenArm {
    pub id: String,
    pub bin: PathBuf,
    pub identity: Identity,
    pub root: PathBuf,
    pub seq: u32,
}

/// 1-based inclusive line span of `needle` in `text`.
pub fn needle_span(text: &str, needle: &str) -> Option<(usize, usize)> {
    let at = text.find(needle)?;
    let start = text[..at].matches('\n').count() + 1;
    Some((start, start + needle.matches('\n').count()))
}

impl LumenArm {
    /// What `lumen-mcp hook intercept` does with a `Read` payload: its exit status and stderr.
    fn intercept(&self, sb: &Sandbox, payload: &Value) -> Res<(i32, String)> {
        let mut c = Command::new(&self.bin);
        sb.apply(&mut c);
        c.args(["hook", "intercept"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = c.spawn().ctx("spawn the intercept hook")?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().ok_or("the hook has no stdin")?;
            stdin
                .write_all(payload.to_string().as_bytes())
                .ctx("write the hook payload")?;
        }
        let out = child.wait_with_output().ctx("wait for the hook")?;
        let code = out
            .status
            .code()
            .ok_or_else(|| format!("the hook did not exit normally: {}", out.status))?;
        Ok((code, String::from_utf8_lossy(&out.stderr).into_owned()))
    }
}

impl Arm for LumenArm {
    fn id(&self) -> &str {
        &self.id
    }

    fn replay(&mut self, case: &Case) -> Res<Outcome> {
        self.seq += 1;
        let sb = Sandbox::create(&self.root.join(format!("lumen-{}", self.seq)))?;
        let (work, file) = materialize(&sb, case)?;
        let session = format!("lumen-bench-replay-{}", case.id);
        let abs = file.to_string_lossy().into_owned();
        let t0 = Instant::now();
        let out = match &case.call {
            ToolCall::Read { .. } => {
                let (code, stderr) =
                    self.intercept(&sb, &pre_payload(case, &file, &session, &work))?;
                match code {
                    0 => {
                        let mut r = Received::plain(case.raw.clone(), t0.elapsed());
                        r.notes.push(
                            "the intercept let the Read through (under 300 lines, or not a source kind)".into(),
                        );
                        Outcome::Received(r)
                    }
                    2 => self.after_block(case, &sb, &work, &session, &abs, stderr, t0)?,
                    n => return Err(format!("the intercept hook exited {n}: {stderr}")),
                }
            }
            ToolCall::Bash { .. } if matches!(case.category.as_str(), "test_log" | "build_log") => {
                let mut srv = Server::start(&self.bin, &work, &sb, &session, true, self.identity)?;
                let reply = srv.call("compress_logs", json!({ "path": abs }))?;
                let wall = t0.elapsed();
                let bytes = reply.text.into_bytes();
                let mut r = Received::plain(bytes, wall);
                r.steps = vec![Step::of("compress_logs", &r.bytes)];
                r.exit_code = Some(case.exit_code);
                r.notes.push(
                    "the model calls compress_logs on the log (CLAUDE.md's own routing)".into(),
                );
                Outcome::Received(r)
            }
            _ => {
                let mut r = Received::plain(case.raw.clone(), t0.elapsed());
                r.exit_code = matches!(case.call, ToolCall::Bash { .. }).then_some(case.exit_code);
                r.notes
                    .push("Lumen has no tool for this kind of output: it passes through".into());
                Outcome::Received(r)
            }
        };
        std::fs::remove_dir_all(&sb.root).ok();
        Ok(out)
    }
}

impl LumenArm {
    #[allow(clippy::too_many_arguments)]
    fn after_block(
        &self,
        case: &Case,
        sb: &Sandbox,
        work: &Path,
        session: &str,
        abs: &str,
        block: String,
        t0: Instant,
    ) -> Res<Outcome> {
        let mut srv = Server::start(&self.bin, work, sb, session, true, self.identity)?;
        let mut held: Vec<u8> = block.clone().into_bytes();
        let mut steps = vec![Step::of("block", block.as_bytes())];
        let outline = srv.smart_read(abs)?;
        steps.push(Step::of("smart_read", outline.text.as_bytes()));
        held.push(b'\n');
        held.extend_from_slice(outline.text.as_bytes());
        // The model asks for what the outline left out: one recall_file per missing needle, at
        // the lines the needle is on; each is a round, counted. `first_answer` is the block and the
        // outline.
        let text = case.text();
        let mut asked: Vec<(usize, usize)> = Vec::new();
        let first = held.clone();
        let mut followups: Vec<Followup> = Vec::new();
        for n in missing_facts(&held, &case.needles, &case.category) {
            let Some((s, e)) = needle_span(&text, &n) else {
                continue;
            };
            if asked.iter().any(|&(a, b)| a <= s && e <= b) {
                continue;
            }
            let r = srv.call(
                "recall_file",
                json!({"path": abs, "start_line": s, "end_line": e}),
            )?;
            followups.push(Followup::plain(
                &format!("recall_file {s}-{e}"),
                r.text.into_bytes(),
            ));
            asked.push((s, e));
        }
        let wall = t0.elapsed();
        Ok(Outcome::Received(Received {
            bytes: first,
            steps,
            // The redirect to smart_read is a round of its own.
            first_rounds: 1,
            followups,
            exit_code: None,
            notes: vec![format!(
                "blocked, then smart_read, then {} recall_file call(s) for needles the outline lacked",
                asked.len()
            )],
            wall,
        }))
    }
}

// ---------------------------------------------------------------------------------- Headroom

/// Headroom's proxy in front of a local echo upstream.
pub struct HeadroomArm {
    pub echo: EchoUpstream,
    proxy: Option<Child>,
    addr: std::net::SocketAddr,
    seq: u32,
    _sb: Option<Sandbox>,
    log: Option<PathBuf>,
}

/// The request Claude Code would send after running `case`'s tool: the prompt, the model's tool
/// call, and the tool's result as the newest message. `nonce` is in the prompt so Headroom's
/// response cache (on by default) cannot answer from an earlier, identical request.
pub fn proxy_request(case: &Case, nonce: &str) -> Value {
    let id = format!("toolu_{}", &nonce[nonce.len().saturating_sub(12)..]);
    let fake = PathBuf::from("/work").join(case.rel_path());
    json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 64,
        "messages": [
            {"role": "user", "content": format!("Look at the output and tell me what it says. run {nonce}")},
            {"role": "assistant", "content": [{"type": "tool_use", "id": id,
                "name": case.call.tool(), "input": tool_input(&case.call, &fake)}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": case.text()}]}
        ]
    })
}

/// The newest `tool_result`'s text in a request body.
pub fn forwarded_tool_result(body: &[u8]) -> Res<String> {
    let v: Value = serde_json::from_slice(body).ctx("the forwarded body is not JSON")?;
    let msgs = v["messages"]
        .as_array()
        .ok_or("no messages in the forwarded body")?;
    let block = msgs
        .iter()
        .rev()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|b| b["type"] == "tool_result")
        .ok_or("no tool_result in the forwarded body")?;
    output_text(&block["content"])
}

impl HeadroomArm {
    /// Start the proxy as `headroom wrap claude` does (`headroom proxy --port N --workers 1`),
    /// pointed at a fresh echo upstream, and wait until it and its Kompress model are ready.
    pub fn start(inst: &Headroom, root: &Path, env: &[(String, String)]) -> Res<HeadroomArm> {
        let echo = EchoUpstream::start()?;
        let port = free_port()?;
        let mut sb = Sandbox::create(&root.join("headroom"))?
            .set("ANTHROPIC_TARGET_API_URL", echo.url())
            // The documented air-gap switch: Headroom itself dials nothing but its upstream.
            .set("HEADROOM_OFFLINE", "1")
            .set("HF_HUB_OFFLINE", "1")
            .set("HF_HOME", inst.hf_home.as_os_str())
            .set("LITELLM_LOCAL_MODEL_COST_MAP", "True");
        for (k, v) in env {
            sb = sb.set(k, v);
        }
        let log = std::fs::File::create(sb.root.join("proxy.log")).ctx("create the proxy log")?;
        let mut c = Command::new(&inst.python);
        sb.apply(&mut c);
        c.args([
            "-m",
            "headroom.cli",
            "proxy",
            "--port",
            &port.to_string(),
            "--workers",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(log.try_clone().ctx("clone the log")?)
        .stderr(log);
        let proxy = c.spawn().ctx("start the Headroom proxy")?;
        let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port));
        let mut arm = HeadroomArm {
            echo,
            proxy: Some(proxy),
            addr,
            seq: 0,
            log: Some(sb.root.join("proxy.log")),
            _sb: Some(sb),
        };
        arm.wait_ready(Duration::from_secs(300))?;
        Ok(arm)
    }

    /// A replay against a proxy somebody else runs at `addr` whose upstream is `echo`: how the
    /// tests plant a proxy that rewrites a result.
    pub fn attach(echo: EchoUpstream, addr: std::net::SocketAddr) -> HeadroomArm {
        HeadroomArm {
            echo,
            proxy: None,
            addr,
            seq: 0,
            _sb: None,
            log: None,
        }
    }

    /// The proxy's own warnings and errors, for an error message.
    fn log_tail(&self) -> String {
        let Some(text) = self
            .log
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
        else {
            return String::new();
        };
        let hot: Vec<&str> = text
            .lines()
            .filter(|l| {
                l.contains("WARNING") || l.contains("ERROR") || l.to_lowercase().contains("retry")
            })
            .filter(|l| !l.contains("proxy_offline_mode"))
            .collect();
        let tail: Vec<&str> = hot.iter().rev().take(6).rev().copied().collect();
        format!(" [proxy log: {}]", tail.join(" | "))
    }

    fn wait_ready(&mut self, limit: Duration) -> Res<()> {
        let t = Instant::now();
        let mut last = String::from("no answer yet");
        while t.elapsed() < limit {
            let exited = self
                .proxy
                .as_mut()
                .and_then(|p| p.try_wait().ok().flatten());
            if let Some(status) = exited {
                return Err(format!(
                    "the Headroom proxy exited early ({status}); see its proxy.log"
                ));
            }
            if let Ok((200, body)) = echo::get(self.addr, "/readyz", Duration::from_secs(5)) {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let k = &v["checks"]["kompress"];
                last = format!("kompress {}", k["status"]);
                // Until the model has loaded the proxy forwards requests uncompressed.
                if v["ready"] == json!(true)
                    && (k["status"] == "healthy" || k["enabled"] == json!(false))
                {
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        Err(format!(
            "the Headroom proxy was not ready after {} s ({last})",
            limit.as_secs()
        ))
    }
}

impl Drop for HeadroomArm {
    fn drop(&mut self) {
        if let Some(p) = self.proxy.as_mut() {
            p.kill().ok();
            p.wait().ok();
        }
    }
}

impl Arm for HeadroomArm {
    fn id(&self) -> &str {
        "headroom"
    }

    fn replay(&mut self, case: &Case) -> Res<Outcome> {
        self.seq += 1;
        let nonce = format!(
            "replay-{}-{}-{}",
            std::process::id(),
            self.seq,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let body = proxy_request(case, &nonce).to_string();
        let t0 = Instant::now();
        let (status, _) = echo::post_json(
            self.addr,
            "/v1/messages",
            body.as_bytes(),
            Duration::from_secs(120),
        )?;
        let wall = t0.elapsed();
        if status != 200 {
            return Err(format!("the proxy answered {status}"));
        }
        let seen: Vec<_> = self
            .echo
            .requests()
            .into_iter()
            .filter(|q| q.method == "POST" && echo::contains(&q.body, nonce.as_bytes()))
            .collect();
        if seen.len() != 1 {
            return Err(format!(
                "expected one forwarded request for this item, the echo upstream saw {}: {}",
                seen.len(),
                seen.iter()
                    .map(|q| format!("{} {} ({} bytes)", q.method, q.path, q.body.len()))
                    .collect::<Vec<_>>()
                    .join("; ")
                    + &self.log_tail()
            ));
        }
        let got = [seen[0].body.clone()];
        let text = forwarded_tool_result(&got[0])?;
        let mut r = Received::plain(text.clone().into_bytes(), wall);
        r.exit_code = matches!(case.call, ToolCall::Bash { .. }).then_some(case.exit_code);
        r.notes
            .push("the tool_result of the request the proxy forwarded".into());
        // The ideal asker: while a needle's fact is missing, `headroom_retrieve` the marker's hash
        // (the MCP server `headroom wrap claude` registers by default, which asks this proxy's
        // /v1/retrieve), one round each. Retrieval is by hash and returns the whole original.
        let t1 = Instant::now();
        let mut held = text.clone().into_bytes();
        for hash in retrieval_hashes(&text) {
            if missing_facts(&held, &case.needles, &case.category).is_empty() {
                break;
            }
            let body = json!({ "hash": hash }).to_string();
            let (status, resp) = echo::post_json(
                self.addr,
                "/v1/retrieve",
                body.as_bytes(),
                Duration::from_secs(30),
            )?;
            if status != 200 {
                r.notes.push(format!(
                    "headroom_retrieve {hash}: the proxy answered {status}"
                ));
                break;
            }
            let mut v: Value = serde_json::from_slice(&resp).ctx("the retrieve response")?;
            let original = v["original_content"]
                .as_str()
                .ok_or("a retrieve response with no original_content")?
                .to_string();
            // What the MCP tool hands the model: the result as indented JSON, tagged by source.
            v["source"] = json!("proxy");
            let shown = serde_json::to_string_pretty(&v).ctx("format the retrieve result")?;
            held.push(b'\n');
            held.extend_from_slice(original.as_bytes());
            r.followups.push(Followup {
                label: "headroom_retrieve".into(),
                received: shown.into_bytes(),
                facts: original.into_bytes(),
            });
        }
        r.wall += t1.elapsed();
        Ok(Outcome::Received(r))
    }
}

/// The hashes in `Retrieve more: hash=<hex>` and `Retrieve original: hash=<hex>` markers.
pub fn retrieval_hashes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("hash=") {
        rest = &rest[at + 5..];
        let h: String = rest.chars().take_while(char::is_ascii_hexdigit).collect();
        if h.len() >= 8 && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

// ---------------------------------------------------------------------------------- building

fn want(names: &[String], n: &str) -> bool {
    names.iter().any(|x| x == n)
}

/// The arms asked for, installed from their pins. `prefetch` allows the network.
pub fn build(
    names: &[String],
    cache: &Path,
    repo: &Path,
    scratch: &Path,
    prefetch: bool,
) -> Res<Vec<Box<dyn Arm>>> {
    for n in names {
        if !["none", "lumen", "squeez", "rtk", "headroom", "caveman"].contains(&n.as_str()) {
            return Err(format!("unknown arm {n:?}"));
        }
    }
    let tools = Tools::load_committed()?;
    let mut out: Vec<Box<dyn Arm>> = Vec::new();
    if want(names, "none") {
        out.push(Box::new(NoneArm));
    }
    if want(names, "lumen") {
        out.push(lumen(&tools, cache, repo, scratch, prefetch)?);
    }
    for (name, kind, file) in [
        ("squeez", Kind::Squeez, "squeez"),
        ("rtk", Kind::Rtk, "rtk"),
    ] {
        if !want(names, name) {
            continue;
        }
        out.push(
            match install::asset_for(&tools, name, cache, repo, prefetch)? {
                Err(na) => Box::new(NaArm {
                    id: name.into(),
                    reason: na,
                }),
                Ok(a) => {
                    let bin = match kind {
                        Kind::Squeez => a.path.clone(),
                        Kind::Rtk => install::unpacked(&a, cache, file)?,
                    };
                    let arm = tools.arm(name)?;
                    Box::new(HookedArm {
                        kind,
                        id: name.into(),
                        bin,
                        env: arm
                            .telemetry_env
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        root: scratch.to_path_buf(),
                        seq: 0,
                    })
                }
            },
        );
    }
    if want(names, "headroom") {
        out.push(match install::headroom(&tools, cache, repo, prefetch)? {
            Err(na) => Box::new(NaArm {
                id: "headroom".into(),
                reason: na,
            }),
            Ok(inst) => {
                let arm = tools.arm("headroom")?;
                let env: Vec<(String, String)> = arm
                    .telemetry_env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                Box::new(HeadroomArm::start(&inst, scratch, &env)?)
            }
        });
    }
    if want(names, "caveman") {
        out.push(Box::new(NaArm {
            id: "caveman".into(),
            reason: CAVEMAN_NA.into(),
        }));
    }
    Ok(out)
}

fn lumen(
    tools: &Tools,
    cache: &Path,
    repo: &Path,
    scratch: &Path,
    prefetch: bool,
) -> Res<Box<dyn Arm>> {
    let id = format!("lumen-{LUMEN_VERSION}");
    let m = VersionsManifest::load_committed()?;
    // The two manifests name the same release.
    let arm = tools.arm(&id)?;
    let entry = m.get(LUMEN_VERSION)?;
    if arm.commit.as_deref() != Some(entry.commit.as_str()) {
        return Err(format!(
            "tools.toml and versions/manifest.toml disagree on the commit of Lumen {LUMEN_VERSION}"
        ));
    }
    let Some(platform) = host_platform() else {
        return Ok(Box::new(NaArm {
            id,
            reason: install::na_platform("lumen", None),
        }));
    };
    let asset = match m.asset(LUMEN_VERSION, platform)? {
        Ok(a) => a,
        Err(why) => {
            return Ok(Box::new(NaArm {
                id,
                reason: format!("n/a — {why}"),
            }));
        }
    };
    if !prefetch
        && !cache
            .join("assets")
            .join(&asset.sha256)
            .join(&asset.name)
            .is_file()
    {
        return Err(format!(
            "{} is not in the cache; run `competitors replay --prefetch` first",
            asset.name
        ));
    }
    let dest = cache
        .join("lumen")
        .join(format!("{LUMEN_VERSION}-{platform}"));
    let build = builds::from_asset(&m, LUMEN_VERSION, platform, cache, repo, &dest)?
        .map_err(|why| format!("lumen: {why}"))?;
    let caps = build
        .caps
        .as_ref()
        .ok_or("an asset build has capabilities")?;
    let identity = caps.probe_identity(&build.bin, &scratch.join("probe"))?;
    Ok(Box::new(LumenArm {
        id,
        bin: build.bin,
        identity,
        root: scratch.to_path_buf(),
        seq: 0,
    }))
}
