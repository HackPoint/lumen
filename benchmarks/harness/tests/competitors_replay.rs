//! C2: the Layer 1 replay adapters, without the network.
//!
//! The shims, the hook runner, the echo upstream and the measuring are tested here against stubs
//! and against the real `lumen-mcp` built from this checkout; the competitors' own binaries are
//! only run by the manual replay (`competitors replay --prefetch`).

use lumen_bench::competitors::record::{self, Item};
use lumen_bench::competitors::replay::arms::{
    HeadroomArm, Kind, LumenArm, NaArm, NoneArm, Recovery, recovery_for, retrieval_hashes,
};
// Used only by the tests of the competitor arms, whose PATH shims are POSIX (n/a on Windows).
#[cfg(unix)]
use lumen_bench::competitors::replay::arms::{replay_with_hooks, rtk_recall_ids};
use lumen_bench::competitors::replay::echo::{self, EchoUpstream};
#[cfg(unix)]
use lumen_bench::competitors::replay::hooks;
use lumen_bench::competitors::replay::{
    Arm, CAVEMAN_NA, Case, Cell, NeedleKind, Outcome, Received, ToolCall, apply_head_limit,
    apply_read_window, call_for, fact_present, load_cases, measure, missing_facts, needle_kind,
    needle_mix, output_text, post_payload, run_case, totals, tsv,
};
use lumen_bench::driver::probe_identity;
use lumen_bench::sandbox::RealLedgerGuard;
#[cfg(unix)]
use lumen_bench::sandbox::Sandbox;
use lumen_core::calibration::{ContentType, Model};
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn model() -> Option<Model> {
    Model::from_model("claude-sonnet-5-5")
}

fn case(id: &str, category: &str, call: ToolCall, raw: &str, needles: &[&str], exit: i32) -> Case {
    Case {
        id: id.into(),
        category: category.into(),
        raw: raw.as_bytes().to_vec(),
        call,
        exit_code: exit,
        needles: needles.iter().map(|s| s.to_string()).collect(),
        content_type: ContentType::Other,
    }
}

fn lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i} of the listing\n"))
        .collect()
}

// ---------------------------------------------------------------------------- proxy and echo

/// A stand-in for a proxy: forwards each POST to `upstream`, after `rewrite` has changed the text
/// of the newest `tool_result`. `None` forwards the body untouched.
fn fake_proxy(upstream: SocketAddr, rewrite: Option<fn(&str) -> String>) -> SocketAddr {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = l.local_addr().unwrap();
    // The original text of the last rewritten result, for `/v1/retrieve`.
    let store: std::sync::Arc<std::sync::Mutex<String>> = Default::default();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(conn) = conn else { continue };
            let mut w = conn.try_clone().unwrap();
            let mut r = std::io::BufReader::new(conn);
            let Ok(Some(req)) = echo::read_request(&mut r, &mut w) else {
                continue;
            };
            use std::io::Write;
            if req.path == "/v1/retrieve" {
                let body = json!({"hash": "x", "original_content": store.lock().unwrap().clone(),
                    "original_item_count": 1, "compressed_item_count": 1, "tool_name": "Bash",
                    "retrieval_count": 1})
                .to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                w.write_all(reply.as_bytes()).unwrap();
                continue;
            }
            let mut v: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            if let Some(f) = rewrite {
                let msgs = v["messages"].as_array_mut().unwrap();
                let last = msgs.last_mut().unwrap();
                let block = &mut last["content"][0];
                let text = block["content"].as_str().unwrap().to_string();
                *store.lock().unwrap() = text.clone();
                block["content"] = json!(f(&text));
            }
            let (st, _) = echo::post_json(
                upstream,
                &req.path,
                v.to_string().as_bytes(),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(st, 200);
            w.write_all(&echo::canned_reply()).unwrap();
        }
    });
    addr
}

fn drop_marker_line(text: &str) -> String {
    text.lines()
        .filter(|l| !l.contains("NEEDLE-LINE"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn proxy_case() -> Case {
    let raw = format!("{}NEEDLE-LINE 42 survives?\n{}", lines(30), lines(30));
    case(
        "shell-fixture",
        "shell",
        ToolCall::Bash {
            command: "git ls-tree -r -l HEAD".into(),
        },
        &raw,
        &["NEEDLE-LINE 42 survives?"],
        0,
    )
}

#[test]
fn the_echo_upstream_captures_what_the_proxy_forwards_byte_for_byte() {
    let echo = EchoUpstream::start().unwrap();
    let proxy = fake_proxy(echo.addr, None);
    let mut arm = HeadroomArm::attach(echo, proxy);
    let c = proxy_case();
    let Outcome::Received(r) = arm.replay(&c).unwrap() else {
        panic!("not received")
    };
    assert_eq!(
        r.bytes, c.raw,
        "a proxy that forwards verbatim changes nothing"
    );
    let m = measure(&c, &r, model());
    assert!(m.kept && m.unchanged && m.missing.is_empty());
    // The upstream really saw the request, once, with the item in it.
    let seen: Vec<_> = arm
        .echo
        .requests()
        .into_iter()
        .filter(|q| q.method == "POST")
        .collect();
    assert_eq!(seen.len(), 1);
    assert!(echo::contains(&seen[0].body, b"NEEDLE-LINE 42 survives?"));
    assert_eq!(seen[0].path, "/v1/messages");
}

#[test]
fn a_proxy_that_rewrites_the_result_is_detected_and_its_lost_needle_flagged() {
    let echo = EchoUpstream::start().unwrap();
    let proxy = fake_proxy(echo.addr, Some(drop_marker_line));
    let mut arm = HeadroomArm::attach(echo, proxy);
    let c = proxy_case();
    let Outcome::Received(r) = arm.replay(&c).unwrap() else {
        panic!("not received")
    };
    assert_ne!(
        r.bytes, c.raw,
        "the planted rewrite must show in the capture"
    );
    let m = measure(&c, &r, model());
    assert!(!m.kept && !m.unchanged);
    assert_eq!(m.missing, vec!["NEEDLE-LINE 42 survives?".to_string()]);
    // The proxy left no marker, so there is nothing to retrieve: the fact stays lost.
    assert!(!m.kept_total && m.rounds == 0);
    assert!(m.tokens < m.raw_tokens);
}

#[test]
fn each_replay_through_a_proxy_is_its_own_request() {
    // The proxy's response cache answers a repeated request itself, so a replay that sent the same
    // body twice would see one forwarded request and an error. The prompt carries a nonce.
    let echo = EchoUpstream::start().unwrap();
    let proxy = fake_proxy(echo.addr, None);
    let mut arm = HeadroomArm::attach(echo, proxy);
    let c = proxy_case();
    for _ in 0..3 {
        assert!(matches!(arm.replay(&c).unwrap(), Outcome::Received(_)));
    }
    assert_eq!(
        arm.echo
            .requests()
            .iter()
            .filter(|q| q.method == "POST")
            .count(),
        3
    );
}

#[test]
fn a_head_probe_gets_headers_only_so_a_kept_alive_connection_stays_in_step() {
    use std::io::{Read, Write};
    let echo = EchoUpstream::start().unwrap();
    let mut s = std::net::TcpStream::connect(echo.addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // HEAD, then a POST on the same connection, as a proxy's upstream probe is followed by traffic.
    s.write_all(b"HEAD / HTTP/1.1\r\nhost: x\r\n\r\nPOST /v1/messages HTTP/1.1\r\nhost: x\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").unwrap();
    let mut all = String::new();
    s.read_to_string(&mut all).unwrap();
    // Two status lines, and the first response holds no body: its headers end where the next
    // status line begins.
    let first = all.find("HTTP/1.1 200 OK").unwrap();
    let second = all[first + 1..]
        .find("HTTP/1.1 200 OK")
        .map(|i| i + first + 1)
        .unwrap();
    assert!(all[first..second].ends_with("\r\n\r\n"), "{all:?}");
}

#[test]
fn the_echo_upstream_reads_chunked_bodies_and_stays_on_loopback() {
    let echo = EchoUpstream::start().unwrap();
    assert!(echo.addr.ip().is_loopback());
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(echo.addr).unwrap();
    s.write_all(b"POST /v1/messages HTTP/1.1\r\nhost: x\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").unwrap();
    let mut reply = String::new();
    s.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert_eq!(echo.requests()[0].body, b"hello world");
}

// ---------------------------------------------------------------------------- hooks and shims

#[cfg(unix)]
mod hooked {
    use super::*;

    /// A sandbox with a stub tool's hooks: `pre` is what the PreToolUse hook prints, `post` what
    /// the PostToolUse hook prints (empty prints nothing).
    pub fn stub_for(root: &Path, pre: &str, post: &str) -> (Sandbox, Vec<hooks::Hook>) {
        stub(root, pre, post)
    }

    fn stub(root: &Path, pre: &str, post: &str) -> (Sandbox, Vec<hooks::Hook>) {
        let sb = Sandbox::create(root).unwrap();
        std::fs::write(root.join("pre.json"), pre).unwrap();
        std::fs::write(root.join("post.json"), post).unwrap();
        // Records where it ran and with what environment: the confinement test reads it.
        std::fs::write(
            root.join("where.sh"),
            "printf '%s\\n' \"$HOME\" \"$TMPDIR\" \"$XDG_CONFIG_HOME\" \"$(pwd -P)\" >> \"$HOME/where.txt\"\n",
        )
        .unwrap();
        let settings = json!({"hooks": {
            "PreToolUse": [{"matcher": "Bash|Read|Grep", "hooks": [
                {"type": "command", "command": format!("sh {0}/where.sh; cat {0}/pre.json", root.display())}]}],
            "PostToolUse": [{"hooks": [
                {"type": "command", "command": format!("cat >/dev/null; cat {}/post.json", root.display())}]}]
        }});
        (sb, hooks::parse(&settings.to_string()).unwrap())
    }

    fn updated_output(text: &str) -> String {
        json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": text}})
            .to_string()
    }

    #[test]
    fn an_arm_that_drops_a_needle_is_flagged() {
        let t = tempfile::tempdir().unwrap();
        let c = case(
            "read-fixture",
            "file_read",
            ToolCall::Read {
                rel: "src/a.txt".into(),
            },
            &format!("{}the NEEDLE sits here\n{}", lines(10), lines(10)),
            &["the NEEDLE sits here", "line 3 of the listing"],
            0,
        );
        // The stub keeps the first five lines: needle 2 (line 3) survives, needle 1 does not.
        let kept: String = c.text().lines().take(5).map(|l| format!("{l}\n")).collect();
        let (sb, hooks) = stub(&t.path().join("s"), "", &updated_output(&kept));
        let Outcome::Received(r) = replay_with_hooks(
            "stub",
            &c,
            &sb,
            &hooks,
            Path::new("/nonexistent"),
            Recovery::None,
        )
        .unwrap() else {
            panic!("not received")
        };
        let m = measure(&c, &r, model());
        assert_eq!(r.bytes, kept.as_bytes());
        assert!(!m.kept, "the dropped needle must be flagged");
        assert_eq!(m.missing, vec!["the NEEDLE sits here".to_string()]);
        // The same arm with a hook that leaves the output alone keeps every needle.
        let (sb2, hooks2) = stub(&t.path().join("s2"), "", "");
        let Outcome::Received(r2) =
            replay_with_hooks("stub", &c, &sb2, &hooks2, Path::new("/x"), Recovery::None).unwrap()
        else {
            panic!("not received")
        };
        assert!(measure(&c, &r2, model()).kept);
        assert!(r2.wall > Duration::ZERO);
    }

    #[test]
    fn a_wrapped_bash_command_replays_the_recorded_bytes_and_status_through_the_shim() {
        let t = tempfile::tempdir().unwrap();
        let raw = "error[E0599]: no method\r\nsecond line, no newline at the end";
        let c = case(
            "buildlog-fixture",
            "build_log",
            ToolCall::Bash {
                command: "cargo build".into(),
            },
            raw,
            &["error[E0599]"],
            101,
        );
        // The stub tool "wraps" the command in a script that runs it and passes its output on, the
        // way `squeez wrap` and `rtk <cmd>` do.
        let root = t.path().join("s");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("wrap.sh"), "sh -c \"$1\"\n").unwrap();
        let pre = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
            "updatedInput": {"command": format!("sh {}/wrap.sh 'cargo build'", root.display())}}}).to_string();
        let (sb, hooks) = stub(&root, &pre, "");
        let Outcome::Received(r) =
            replay_with_hooks("stub", &c, &sb, &hooks, Path::new("/x"), Recovery::None).unwrap()
        else {
            panic!("not received")
        };
        assert_eq!(
            r.bytes,
            raw.as_bytes(),
            "the exact bytes, CRLF and missing final newline included"
        );
        assert_eq!(
            r.exit_code,
            Some(101),
            "the recorded non-zero status survives the wrapper"
        );
        assert!(
            r.notes.iter().any(|n| n.contains("rewrote the input")),
            "{:?}",
            r.notes
        );
    }

    #[test]
    fn a_wrapper_that_never_calls_the_recorded_program_is_na_not_a_measurement() {
        let t = tempfile::tempdir().unwrap();
        let c = case(
            "shell-fixture",
            "shell",
            ToolCall::Bash {
                command: "git ls-tree -r -l HEAD".into(),
            },
            "x\n",
            &["x"],
            0,
        );
        let pre = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse",
            "updatedInput": {"command": "echo substituted"}}})
        .to_string();
        let (sb, hooks) = stub(&t.path().join("s"), &pre, "");
        match replay_with_hooks("stub", &c, &sb, &hooks, Path::new("/x"), Recovery::None).unwrap() {
            Outcome::Na(why) => assert!(
                why.starts_with("n/a —") && why.contains("without calling"),
                "{why}"
            ),
            Outcome::Received(_) => {
                panic!("what `echo substituted` printed is not the tool's output")
            }
        }
    }

    #[test]
    fn nothing_is_written_outside_the_scratch_dir_and_the_claude_state_is_untouched() {
        let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let tree = |p: &Path| -> Vec<(PathBuf, u64)> {
            let mut out = Vec::new();
            let mut stack = vec![p.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    let md = e.metadata().unwrap();
                    if md.is_dir() {
                        if e.file_name() != "target" {
                            stack.push(e.path());
                        }
                    } else {
                        out.push((e.path(), md.len()));
                    }
                }
            }
            out.sort();
            out
        };
        let repo_before = tree(&bench.join("competitors"));
        let claude_before = lumen_bench::competitors::replay::ClaudeState::take().unwrap();

        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("s");
        let c = case(
            "shell-fixture",
            "shell",
            ToolCall::Bash {
                command: "git ls-tree".into(),
            },
            "a\nb\n",
            &["a"],
            0,
        );
        let (sb, hooks) = stub(&root, "", "");
        replay_with_hooks("stub", &c, &sb, &hooks, Path::new("/x"), Recovery::None).unwrap();

        // Every hook ran with HOME, TMPDIR, XDG_CONFIG_HOME and its working directory inside the
        // scratch root, so what it wrote could only land there.
        let canon = std::fs::canonicalize(&root).unwrap();
        let rec = std::fs::read_to_string(sb.home.join("where.txt")).unwrap();
        assert!(!rec.is_empty());
        for l in rec.lines() {
            let p = std::fs::canonicalize(l).unwrap_or_else(|_| PathBuf::from(l));
            assert!(p.starts_with(&canon), "{l} is outside {}", canon.display());
        }
        assert_eq!(
            tree(&bench.join("competitors")),
            repo_before,
            "nothing was written in the repository"
        );
        claude_before
            .check()
            .expect("~/.claude/settings.json and ~/.claude.json are as they were");
    }
}

#[test]
fn the_claude_state_guard_trips_when_a_tool_changes_what_it_would_change() {
    use lumen_bench::competitors::replay::ClaudeState;
    let t = tempfile::tempdir().unwrap();
    let home = t.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude/settings.json");
    let cj = home.join(".claude.json");
    std::fs::write(&settings, "{}").unwrap();
    std::fs::write(&cj, r#"{"numStartups":1,"mcpServers":{}}"#).unwrap();
    let s = ClaudeState::take_at(home).unwrap();
    assert_eq!(s.check_report().unwrap(), None, "nothing changed");
    // A hook registered in settings.json, even one byte of difference: refused.
    std::fs::write(&settings, "{ }").unwrap();
    assert!(s.check().unwrap_err().contains("settings.json"));
    std::fs::write(&settings, "{}").unwrap();
    // An MCP server registered in ~/.claude.json (what `claude mcp add` does): refused.
    let s = ClaudeState::take_at(home).unwrap();
    std::fs::write(
        &cj,
        r#"{"numStartups":1,"mcpServers":{"headroom":{"command":"x"}}}"#,
    )
    .unwrap();
    assert!(s.check().unwrap_err().contains("mcpServers"));
    // A hooks key appearing: refused.
    std::fs::write(&cj, r#"{"numStartups":1,"mcpServers":{},"hooks":{}}"#).unwrap();
    assert!(s.check().is_err());
    // A running Claude Code's own bookkeeping is reported, not refused.
    std::fs::write(&cj, r#"{"numStartups":2,"mcpServers":{}}"#).unwrap();
    let note = s.check_report().unwrap().expect("a note");
    assert!(note.contains("bookkeeping"), "{note}");
    // A file that appeared or went is refused.
    std::fs::remove_file(&cj).unwrap();
    assert!(s.check().is_err(), "~/.claude.json went");
    let s = ClaudeState::take_at(home).unwrap();
    std::fs::write(&cj, "{}").unwrap();
    assert!(s.check().is_err(), "~/.claude.json appeared");
}

// ---------------------------------------------------------------------------- Lumen

fn mcp_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's own path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/<profile>/deps/<test>")
        .to_path_buf();
    let target_dir = profile_dir.parent().expect("target/<profile>");
    let mut build = Command::new(env!("CARGO"));
    build
        .args(["build", "--quiet", "-p", "lumen-mcp", "--target-dir"])
        .arg(target_dir);
    match profile_dir.file_name().and_then(|n| n.to_str()) {
        Some("debug") => {}
        Some("release") => {
            build.arg("--release");
        }
        other => panic!("an unknown cargo profile directory {other:?}"),
    }
    assert!(build.status().expect("cargo build -p lumen-mcp").success());
    let bin = profile_dir.join(if cfg!(windows) {
        "lumen-mcp.exe"
    } else {
        "lumen-mcp"
    });
    assert!(bin.is_file());
    bin
}

fn big_rs() -> String {
    let mut s = String::from("//! A generated fixture.\n\n");
    for i in 0..40 {
        s.push_str(&format!(
            "pub fn step_{i:02}(x: i64) -> i64 {{\n    let a = x + {i};\n    let b = a * 2;\n    \
             let c = b - {i};\n    let d = c / 3;\n    d + a\n}}\n\n"
        ));
    }
    s
}

#[test]
fn the_lumen_arm_goes_through_the_real_hook_and_the_real_server() {
    let guard = RealLedgerGuard::new().unwrap();
    guard.check("before").unwrap();
    let bin = mcp_bin();
    let t = tempfile::tempdir().unwrap();
    let identity = probe_identity(&bin, &t.path().join("probe")).unwrap();
    let mut arm = LumenArm {
        id: "lumen-test".into(),
        bin,
        identity,
        root: t.path().to_path_buf(),
        seq: 0,
    };
    // A source file over the 300-line bar: the Read is blocked, the outline comes, and the body
    // lines the outline does not hold are asked for with recall_file.
    let c = case(
        "read-fixture-big",
        "file_read",
        ToolCall::Read {
            rel: "src/big.rs".into(),
        },
        &big_rs(),
        &["pub fn step_03(x: i64) -> i64 {", "    let c = b - 17;"],
        0,
    );
    let Outcome::Received(r) = arm.replay(&c).unwrap() else {
        panic!("not received")
    };
    let labels: Vec<&str> = r.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels[0], "block");
    assert_eq!(labels[1], "smart_read");
    assert!(
        labels[2..].iter().all(|l| l.starts_with("recall_file")),
        "{labels:?}"
    );
    assert!(
        String::from_utf8_lossy(&r.bytes).contains("smart_read"),
        "the block message names the tool"
    );
    let m = measure(&c, &r, model());
    assert!(
        !m.kept,
        "the first answer (block and outline) lacks the body line"
    );
    assert!(
        m.kept_total,
        "after the recall every needle is there: missing {:?}",
        m.missing_total
    );
    // Every follow-up is counted: its tokens, and a round each (the redirect to smart_read is one).
    assert_eq!(m.rounds as usize, 1 + r.followups.len());
    assert!(
        r.followups
            .iter()
            .all(|f| f.label.starts_with("recall_file"))
    );
    assert!(!r.followups.is_empty());
    assert!(
        m.tokens_total > m.tokens,
        "{} vs {}",
        m.tokens_total,
        m.tokens
    );
    assert!(
        m.tokens_total < m.raw_tokens,
        "{} vs {}",
        m.tokens_total,
        m.raw_tokens
    );

    // Under the bar the Read goes through untouched.
    let small = case(
        "read-fixture-small",
        "file_read",
        ToolCall::Read {
            rel: "src/small.rs".into(),
        },
        "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n",
        &["pub fn add"],
        0,
    );
    let Outcome::Received(r) = arm.replay(&small).unwrap() else {
        panic!("not received")
    };
    assert_eq!(r.bytes, small.raw);
    assert!(r.notes[0].contains("let the Read through"), "{:?}", r.notes);

    // A log goes to compress_logs; a repeated line collapses.
    let log = format!(
        "{}error: aborting due to 1 previous error\n",
        "warning: unused variable `x`\n".repeat(200)
    );
    let lc = case(
        "buildlog-fixture",
        "build_log",
        ToolCall::Bash {
            command: "cargo build".into(),
        },
        &log,
        &["error: aborting due to 1 previous error"],
        101,
    );
    let Outcome::Received(r) = arm.replay(&lc).unwrap() else {
        panic!("not received")
    };
    assert_eq!(r.steps[0].label, "compress_logs");
    let m = measure(&lc, &r, model());
    assert!(
        m.kept && m.tokens < m.raw_tokens / 4,
        "{} vs {}",
        m.tokens,
        m.raw_tokens
    );

    // Output Lumen has no tool for is passed through, and says so.
    let other = case(
        "shell-fixture",
        "shell",
        ToolCall::Bash {
            command: "git ls-tree -r -l HEAD".into(),
        },
        "a\n",
        &["a"],
        0,
    );
    let Outcome::Received(r) = arm.replay(&other).unwrap() else {
        panic!("not received")
    };
    assert_eq!(r.bytes, other.raw);
    guard.check("after").unwrap();
}

// ---------------------------------------------------------------------------- n/a, tool calls, shapes

#[test]
fn caveman_cells_are_na_with_the_reason_and_never_a_zero() {
    let c = case(
        "shell-fixture",
        "shell",
        ToolCall::Bash {
            command: "git ls-tree".into(),
        },
        "a\n",
        &["a"],
        0,
    );
    let mut arm = NaArm {
        id: "caveman".into(),
        reason: CAVEMAN_NA.into(),
    };
    let rec = run_case(&mut arm, &c, model()).unwrap();
    assert!(
        matches!(&rec.cell, Cell::Na(w) if w == "n/a — changes the model's replies, not tool output (measured in Layer 2)")
    );
    let mut none = NoneArm;
    let rec2 = run_case(&mut none, &c, model()).unwrap();
    let table = tsv(&[rec2.clone(), rec.clone()]);
    let na_row = table.lines().find(|l| l.contains("\tcaveman\t")).unwrap();
    assert!(
        na_row.contains("\tn/a\t") && na_row.ends_with(CAVEMAN_NA),
        "{na_row}"
    );
    // No figure on the row: every column between the status and the note is empty.
    assert!(
        !na_row.chars().any(|ch| ch.is_ascii_digit()) || !na_row.contains("\t0\t"),
        "{na_row}"
    );
    let t = totals(&[rec2, rec]);
    let row = t.lines().find(|l| l.starts_with("caveman")).unwrap();
    assert_eq!(row, "caveman\t0\t1\t\t\t\t\t\t\t\t\t\t\t");
}

#[test]
fn every_corpus_item_gets_a_tool_call_and_the_corpus_loads() {
    let cases = load_cases(&record::default_dir(), &[]).unwrap();
    assert_eq!(cases.len(), 46);
    let by = |id: &str| cases.iter().find(|c| c.id == id).unwrap();
    assert_eq!(
        by("shell-ls-tree-lumen").call,
        ToolCall::Bash {
            command: "git ls-tree -r -l HEAD".into()
        }
    );
    assert_eq!(
        by("grep-lumen-pub-fn").call,
        ToolCall::Grep {
            pattern: "pub fn".into()
        }
    );
    assert_eq!(
        by("read-lumen-1000-2999").call,
        ToolCall::Read {
            rel: "crates/lumen-core/src/repeat.rs".into()
        }
    );
    assert!(
        matches!(&by("json-npm-express-4.21.2").call, ToolCall::Bash { command } if command.starts_with("curl -s https://"))
    );
    assert_eq!(by("buildlog-tsc-conditional-types").exit_code, 2);
    assert_eq!(by("buildlog-rustc-item-privacy").exit_code, 101);
    assert!(matches!(
        by("json-file-react-hook-form-package").call,
        ToolCall::Read { .. }
    ));
    // A Read's file keeps the extension of the file it came from.
    assert_eq!(by("read-pytest-3000plus").file_name(), "test_config.py");
    assert!(cases.iter().all(|c| !c.needles.is_empty()));
}

#[test]
fn a_manifest_entry_that_is_not_a_known_kind_is_refused() {
    let it: Item = toml::from_str(
        "id='x'\ncategory='mystery'\nfile='f'\nbytes=1\nlines=1\nsha256='0'\nsource_kind='url'\nsource='u'\nrecipe='r'\nneedles=[]\n",
    )
    .unwrap();
    assert!(call_for(&it, "").unwrap_err().contains("mystery"));
}

#[test]
fn the_payloads_have_the_shapes_the_hooks_parse() {
    let c = case(
        "read-x",
        "file_read",
        ToolCall::Read {
            rel: "src/a.py".into(),
        },
        "a\nb\nc\n",
        &["a"],
        0,
    );
    let input = json!({"file_path": "/w/src/a.py"});
    let p = post_payload(&c, &input, "a\nb\nc\n", 3, "s", Path::new("/w"));
    assert_eq!(p["hook_event_name"], "PostToolUse");
    assert_eq!(p["tool_name"], "Read");
    assert_eq!(p["tool_response"]["type"], "text");
    assert_eq!(p["tool_response"]["file"]["content"], "a\nb\nc\n");
    assert_eq!(p["tool_response"]["file"]["totalLines"], 3);
    assert_eq!(p["tool_input"]["file_path"], "/w/src/a.py");
    let g = case(
        "grep-x",
        "grep",
        ToolCall::Grep {
            pattern: "p".into(),
        },
        "a:1:x\n",
        &["a"],
        0,
    );
    let p = post_payload(
        &g,
        &json!({"pattern": "p"}),
        "a:1:x\n",
        1,
        "s",
        Path::new("/w"),
    );
    assert_eq!(p["tool_response"]["content"], "a:1:x\n");
    assert_eq!(p["tool_response"]["mode"], "content");
    let b = case(
        "sh-x",
        "shell",
        ToolCall::Bash {
            command: "git status".into(),
        },
        "ok\n",
        &["ok"],
        0,
    );
    let p = post_payload(
        &b,
        &json!({"command": "git status"}),
        "ok\n",
        1,
        "s",
        Path::new("/w"),
    );
    assert_eq!(p["tool_response"]["stdout"], "ok\n");
    assert_eq!(p["tool_response"]["interrupted"], false);
}

#[test]
fn a_hooks_limit_and_a_rewritten_output_are_applied_the_way_the_tools_apply_them() {
    let text = "1\n2\n3\n4\n5\n";
    assert_eq!(apply_read_window(text, &json!({"limit": 2})), "1\n2\n");
    assert_eq!(
        apply_read_window(text, &json!({"offset": 3, "limit": 2})),
        "3\n4\n"
    );
    assert_eq!(apply_read_window(text, &json!({})), text);
    assert_eq!(
        apply_head_limit(text, &json!({"head_limit": 3})),
        "1\n2\n3\n"
    );
    assert_eq!(apply_head_limit(text, &json!({})), text);
    assert_eq!(output_text(&json!("plain")).unwrap(), "plain");
    assert_eq!(
        output_text(&json!({"type": "text", "file": {"content": "from a read"}})).unwrap(),
        "from a read"
    );
    assert_eq!(
        output_text(&json!([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]))
            .unwrap(),
        "ab"
    );
    assert!(output_text(&json!(7)).is_err());
}

#[test]
fn a_replay_runs_offline_on_a_fixture_corpus_and_writes_only_where_told() {
    let t = tempfile::tempdir().unwrap();
    let corpus = t.path().join("corpus");
    std::fs::create_dir_all(corpus.join("items")).unwrap();
    let body = "git status output\nline 2\n";
    std::fs::write(corpus.join("items/shell-ls-tree-x.txt"), body).unwrap();
    let sha = lumen_bench::util::sha256_hex(body.as_bytes());
    std::fs::write(
        corpus.join("manifest.toml"),
        format!(
            "[corpus]\nschema=1\nnote='n'\nitems=1\ntotal_bytes={}\n\n[[item]]\nid='shell-ls-tree-x'\ncategory='shell'\nfile='items/shell-ls-tree-x.txt'\nbytes={}\nlines=2\nsha256='{sha}'\nsource_kind='git'\nsource='s'\nrecipe='git ls-tree -r -l <commit>'\nneedles=['line 2']\n",
            body.len(),
            body.len()
        ),
    )
    .unwrap();
    let out = t.path().join("out.tsv");
    let cache = t.path().join("cache");
    let args: Vec<String> = [
        "--corpus",
        corpus.to_str().unwrap(),
        "--cache",
        cache.to_str().unwrap(),
        "--arms",
        "none,caveman",
        "--out",
        out.to_str().unwrap(),
    ]
    .map(String::from)
    .to_vec();
    lumen_bench::competitors::replay::run(&args).unwrap();
    let table = std::fs::read_to_string(&out).unwrap();
    assert!(
        table.contains("shell-ls-tree-x\tshell\tnone\tdone"),
        "{table}"
    );
    assert!(
        table.contains("shell-ls-tree-x\tshell\tcaveman\tn/a"),
        "{table}"
    );
    // A bad sha stops the run before any arm sees the item.
    std::fs::write(corpus.join("items/shell-ls-tree-x.txt"), "changed\n").unwrap();
    assert!(
        lumen_bench::competitors::replay::run(&args)
            .unwrap_err()
            .contains("sha256")
    );
    // The cache must stay out of the repository.
    let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("replay-cache");
    let bad: Vec<String> = ["--cache", inside.to_str().unwrap(), "--arms", "none"]
        .map(String::from)
        .to_vec();
    assert!(
        lumen_bench::competitors::replay::run(&bad)
            .unwrap_err()
            .contains("inside the repository")
    );
}

#[test]
fn received_is_what_a_plain_arm_hands_over_and_none_changes_nothing() {
    let c = case(
        "shell-fixture",
        "shell",
        ToolCall::Bash {
            command: "git ls-tree".into(),
        },
        "a\n",
        &["a"],
        0,
    );
    let r = Received::plain(c.raw.clone(), Duration::from_millis(1));
    let m = measure(&c, &r, model());
    assert!(m.unchanged && m.kept);
    assert_eq!(m.tokens, m.raw_tokens);
    // An arm that sends back more than it was given shows a negative saving, signed.
    let bigger = Received::plain(
        b"a\nplus a header the tool added\n".to_vec(),
        Duration::ZERO,
    );
    let m = measure(&c, &bigger, model());
    assert!(m.tokens > m.raw_tokens && !m.unchanged);
    let rec = lumen_bench::competitors::replay::Record {
        item: c.id.clone(),
        category: c.category.clone(),
        arm: "x".into(),
        cell: Cell::Done(Box::new(m)),
    };
    let row = totals(&[rec]).lines().nth(1).unwrap().to_string();
    let saved: f64 = row.split('\t').nth(5).unwrap().parse().unwrap();
    assert!(saved < 0.0, "{row}");
}

// ---------------------------------------------------------------------------- facts, follow-ups

fn with_marker(text: &str) -> String {
    format!(
        "{}\n[3 lines compressed to 1. Retrieve more: hash=0123456789abcdef]",
        drop_marker_line(text)
    )
}

#[test]
fn headroom_follow_up_is_the_retrieval_its_marker_names_and_it_is_counted() {
    let echo = EchoUpstream::start().unwrap();
    let proxy = fake_proxy(echo.addr, Some(with_marker));
    let mut arm = HeadroomArm::attach(echo, proxy);
    let c = proxy_case();
    let Outcome::Received(r) = arm.replay(&c).unwrap() else {
        panic!("not received")
    };
    let m = measure(&c, &r, model());
    assert!(!m.kept, "the first answer lost the needle");
    assert!(
        m.kept_total,
        "the retrieval brings it back: {:?}",
        m.missing_total
    );
    assert_eq!(r.followups.len(), 1);
    assert_eq!(m.rounds, 1);
    // The model is handed the tool's JSON, and that costs tokens.
    let shown = String::from_utf8_lossy(&r.followups[0].received).into_owned();
    assert!(
        shown.contains("\"original_content\"") && shown.contains("\"source\": \"proxy\""),
        "{shown}"
    );
    assert!(
        m.tokens_total > m.tokens + m.raw_tokens / 2,
        "{} vs {} / {}",
        m.tokens_total,
        m.tokens,
        m.raw_tokens
    );
    assert_eq!(
        retrieval_hashes(
            "a Retrieve more: hash=0123456789abcdef. b hash=zz Retrieve original: hash=feedfacefeed"
        ),
        vec!["0123456789abcdef", "feedfacefeed"]
    );
}

#[cfg(unix)]
#[test]
fn rtk_follow_up_is_the_recall_its_marker_names_squeez_has_none_at_defaults() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("s");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    // A stub `rtk` that answers `recall <id>` with the original text, as the real one does.
    let c = case(
        "shell-fixture",
        "shell",
        ToolCall::Bash {
            command: "git show --stat".into(),
        },
        &format!("{}NEEDLE-LINE 42 survives?\n{}", lines(5), lines(5)),
        &["NEEDLE-LINE 42 survives?"],
        0,
    );
    std::fs::write(
        root.join("bin/rtk"),
        format!(
            "#!/bin/sh\n[ \"$1\" = recall ] && [ \"$2\" = eff26c8959e2 ] && cat {}/orig.txt\n",
            root.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("bin/rtk"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::write(root.join("orig.txt"), &c.raw).unwrap();
    // The tool replaces the output with a summary that names the recall.
    let summary = "5 lines of listing\n[full output: rtk recall eff26c8959e2]\n";
    let post = json!({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": summary}}).to_string();
    let (sb, hooks) = {
        let (sb, h) = hooked::stub_for(&root, "", &post);
        (sb, h)
    };
    for (recovery, followups) in [(Recovery::RtkRecall, 1usize), (Recovery::None, 0)] {
        let Outcome::Received(r) =
            replay_with_hooks("stub", &c, &sb, &hooks, &root.join("bin"), recovery).unwrap()
        else {
            panic!("not received")
        };
        let m = measure(&c, &r, model());
        assert_eq!(r.followups.len(), followups, "{recovery:?}");
        assert!(!m.kept);
        assert_eq!(
            m.kept_total,
            followups == 1,
            "{recovery:?}: a tool with no recovery path keeps the fact lost"
        );
        assert_eq!(m.rounds as usize, followups);
        if followups == 1 {
            assert!(m.tokens_total > m.tokens, "the recall's output is counted");
        }
    }
    assert_eq!(
        rtk_recall_ids("x [full output: rtk recall eff26c8959e2]\n y rtk recall 0a1b"),
        vec!["eff26c8959e2", "0a1b"]
    );
}

#[test]
fn a_fact_survives_a_change_of_form_but_not_of_content() {
    let g = "grep";
    // Whitespace is not a fact.
    assert!(fact_present(
        b"fn  run(a:   i64)\n   -> i64",
        "fn run(a: i64) -> i64",
        "read"
    ));
    assert!(fact_present(
        b"\"name\": \"express\"",
        "\"name\":\"express\"",
        "json"
    ));
    // A grep hit grouped under its file is the same fact (what Headroom does).
    let grouped = b"crates/lumen-cli/src/data.rs\n87:pub fn run(state: Arc<Mutex<AppState>>) {\n306:pub fn x() {}\n";
    assert!(fact_present(
        grouped,
        "crates/lumen-cli/src/data.rs:87:pub fn run(state: Arc<Mutex<AppState>>) {",
        g
    ));
    // ... but not under another file, with another line's text, or with the line gone.
    assert!(!fact_present(
        grouped,
        "crates/other/src/data.rs:87:pub fn run(state: Arc<Mutex<AppState>>) {",
        g
    ));
    assert!(!fact_present(
        grouped,
        "crates/lumen-cli/src/data.rs:88:pub fn run(state: Arc<Mutex<AppState>>) {",
        g
    ));
    assert!(!fact_present(
        grouped,
        "crates/lumen-cli/src/data.rs:400:pub fn gone() {",
        g
    ));
    // The grouped rule belongs to grep items only.
    assert!(!fact_present(
        grouped,
        "crates/lumen-cli/src/data.rs:87:pub fn run(state: Arc<Mutex<AppState>>) {",
        "read"
    ));
    // A set of lines is present in any order, and absent when one line is.
    assert!(fact_present(
        b"b line\na line\nc line",
        "a line\nb line",
        "read"
    ));
    assert!(!fact_present(b"b line\nc line", "a line\nb line", "read"));
    // A changed number or word is a lost fact; so is a word order that says something else.
    assert!(!fact_present(
        b"44 failed in 0.12s",
        "45 failed in 0.12s",
        "test_log"
    ));
    assert!(!fact_present(
        b"error: aborting due to 14 previous errors",
        "error: aborting due to 15 previous errors",
        "build_log"
    ));
    assert!(!fact_present(
        b"previous 15 to due aborting error:",
        "error: aborting due to 15 previous errors",
        "build_log"
    ));
    assert_eq!(
        missing_facts(b"x\ny", &["x".to_string(), "z".to_string()], "read"),
        vec!["z".to_string()]
    );
}

#[test]
fn the_needle_mix_is_stated_by_kind_and_category() {
    let cases = load_cases(&record::default_dir(), &[]).unwrap();
    let mix = needle_mix(&cases);
    let total: usize = mix.values().sum();
    let of = |k: NeedleKind| -> usize {
        mix.iter()
            .filter(|((_, kk), _)| *kk == k)
            .map(|(_, n)| n)
            .sum()
    };
    assert_eq!(total, cases.iter().map(|c| c.needles.len()).sum::<usize>());
    // What an outline keeps (a definition line) is a minority of the facts asked for; METHOD.md
    // gives the table. A drift in the corpus that made it the majority would tilt the rows.
    for ((cat, k), n) in &mix {
        println!("mix\t{cat}\t{}\t{n}", k.name());
    }
    let sig = of(NeedleKind::Signature);
    assert!(sig * 4 < total, "signatures are {sig} of {total} needles");
    for k in NeedleKind::ALL {
        assert!(of(k) > 0, "{} has no needle", k.name());
    }
    assert_eq!(
        needle_kind("read", "pub fn recall_delta("),
        NeedleKind::Signature
    );
    assert_eq!(
        needle_kind("read", "let pre = old.iter();"),
        NeedleKind::Other
    );
    assert_eq!(
        needle_kind("grep", "src/a.rs:9:pub fn run() {"),
        NeedleKind::Signature
    );
    assert_eq!(
        needle_kind("grep", "src/a.rs:9:    raise OSError()"),
        NeedleKind::Other
    );
    assert_eq!(
        needle_kind("test_log", "FAILED t.py::test_a - assert 0"),
        NeedleKind::TestName
    );
    assert_eq!(
        needle_kind("test_log", "44 failed in 0.12s"),
        NeedleKind::ErrorLine
    );
    assert_eq!(needle_kind("shell", "265 files changed"), NeedleKind::Value);
}

#[test]
fn each_tool_has_the_recovery_path_it_has_at_its_defaults_and_squeez_has_none() {
    assert_eq!(recovery_for(Kind::Squeez), Recovery::None);
    assert_eq!(recovery_for(Kind::Rtk), Recovery::RtkRecall);
}
