//! The shared end-to-end runner (E1), tested with a stub `claude`: no network, no model, no cost.
//!
//! The stub is `lumen-bench-stub-claude` (src/bin/stub_claude.rs), a binary so these tests run on
//! Windows too. Nothing here reads the owner's Claude Code login: every run's HOME is a scratch
//! directory, and a test snapshots the real config files before and after.

use lumen_bench::e2e::arms::{self, Fetcher, InstallCtx};
use lumen_bench::e2e::cap::Outcome;
use lumen_bench::e2e::realhome::{claude_json_view, real_home};
use lumen_bench::e2e::schedule::{self, Suite};
use lumen_bench::e2e::transcript::{self, CROSS_CHECK_TOLERANCE, StreamResult};
use lumen_bench::e2e::{
    self, Authorized, Budget, DockerExecutor, ProgramExecutor, RunCtx, RunInputs, RunSpec,
};
use lumen_bench::manifest::{Arm as ArmEntry, Tasks, ToolAsset, Tools};
use lumen_bench::sandbox::Sandbox;
use lumen_bench::util::sha256_hex;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const STUB: &str = env!("CARGO_BIN_EXE_lumen-bench-stub-claude");
const BENCH: &str = env!("CARGO_BIN_EXE_lumen-bench");

fn tools() -> Tools {
    Tools::load_committed().unwrap()
}
fn tasks() -> Tasks {
    Tasks::load_committed().unwrap()
}

// --- fixtures ------------------------------------------------------------------------------------------

const MODEL: &str = "claude-sonnet-5-5";

/// One transcript line of an assistant message.
#[allow(clippy::too_many_arguments)]
fn msg(
    id: &str,
    model: &str,
    input: i64,
    output: i64,
    read: i64,
    write: i64,
    w5: i64,
    w1h: i64,
) -> String {
    json!({
        "type": "assistant",
        "message": {
            "id": id, "role": "assistant", "model": model,
            "usage": {
                "input_tokens": input, "output_tokens": output,
                "cache_read_input_tokens": read, "cache_creation_input_tokens": write,
                "cache_creation": { "ephemeral_5m_input_tokens": w5, "ephemeral_1h_input_tokens": w1h }
            }
        }
    })
    .to_string()
}

/// Main transcript: A is written three times (the first while streaming, with less output), B
/// twice, a user line, and a synthetic message.
fn main_transcript() -> String {
    [
        json!({"type":"user","message":{"role":"user","content":"hi"}}).to_string(),
        msg("msg_A", MODEL, 10, 20, 1000, 500, 300, 200),
        msg("msg_A", MODEL, 10, 100, 1000, 500, 300, 200),
        msg("msg_A", MODEL, 10, 100, 1000, 500, 300, 200),
        msg("msg_B", MODEL, 5, 50, 1500, 0, 0, 0),
        msg("msg_B", MODEL, 5, 50, 1500, 0, 0, 0),
        msg("msg_S", "<synthetic>", 0, 0, 0, 0, 0, 0),
    ]
    .join("\n")
        + "\n"
}

/// Subagent 1: C, and B again (the same id as in the main transcript).
fn subagent_1() -> String {
    [
        msg("msg_C", MODEL, 7, 70, 200, 100, 100, 0),
        msg("msg_B", MODEL, 5, 50, 1500, 0, 0, 0),
    ]
    .join("\n")
        + "\n"
}

fn subagent_2() -> String {
    msg("msg_D", MODEL, 3, 30, 0, 400, 0, 400) + "\n"
}

/// What the three files add up to, by hand: A (10, 100, 1000, 500 of which 200 for 1h), B (5, 50,
/// 1500), C (7, 70, 200, 100) and D (3, 30, 0, 400 for 1h).
const WANT_INPUT: i64 = 25;
const WANT_OUTPUT: i64 = 250;
const WANT_READ: i64 = 2700;
const WANT_WRITE: i64 = 1000;
const WANT_WRITE_1H: i64 = 600;

fn stream_result_line(input: i64, output: i64, read: i64, creation: i64) -> String {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "num_turns": 4,
        "total_cost_usd": 0.0,
        "usage": { "input_tokens": input, "output_tokens": output,
                   "cache_read_input_tokens": read, "cache_creation_input_tokens": creation }
    })
    .to_string()
}

fn write_file(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

/// Hands back fixtures for the arms' assets: a .deb with the three sidecars, a .tar.gz with an
/// `rtk`, a plain file. Never the network.
struct FixtureFetcher {
    dir: PathBuf,
}

fn ar_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let hdr = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{name}/"),
            0,
            0,
            0,
            "100644",
            data.len()
        );
        out.extend(hdr.as_bytes());
        out.extend(*data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

fn tar_gz(tree: &Path, out: &Path) {
    let st = Command::new("tar")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(tree)
        .arg(".")
        .status()
        .unwrap();
    assert!(st.success());
}

impl Fetcher for FixtureFetcher {
    fn fetch(&self, _arm: &ArmEntry, asset: &ToolAsset) -> Result<PathBuf, String> {
        let at = self.dir.join(&asset.name);
        if asset.name.ends_with(".deb") {
            let tree = self.dir.join(format!("{}-tree", asset.name));
            let bin = tree.join("usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            for s in lumen_bench::assets::SIDECARS {
                std::fs::write(bin.join(s), format!("not a binary: {s}\n")).unwrap();
            }
            let data = self.dir.join(format!("{}-data.tar.gz", asset.name));
            tar_gz(&tree, &data);
            std::fs::write(
                &at,
                ar_bytes(&[
                    ("debian-binary", b"2.0\n"),
                    ("control.tar.gz", b"ctl"),
                    ("data.tar.gz", &std::fs::read(&data).unwrap()),
                ]),
            )
            .unwrap();
        } else if asset.name.ends_with(".tar.gz") {
            let tree = self.dir.join(format!("{}-tree", asset.name));
            std::fs::create_dir_all(&tree).unwrap();
            std::fs::write(tree.join("rtk"), "not a binary: rtk\n").unwrap();
            tar_gz(&tree, &at);
        } else {
            std::fs::write(&at, format!("not a binary: {}\n", asset.name)).unwrap();
        }
        Ok(at)
    }
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn spec(arm: &str, run_id: &str, cap: f64) -> RunSpec {
    RunSpec {
        run_id: run_id.to_string(),
        arm: arm.to_string(),
        task: "django__django-14915".into(),
        image: "example.invalid/swe:instance".into(),
        model: MODEL.into(),
        claude_version: "2.1.292".into(),
        prompt: e2e::build_prompt("The problem statement."),
        timeout: Duration::from_secs(60),
        cap_usd: cap,
        poll_every: Duration::from_millis(50),
    }
}

fn run_with(
    lead: Vec<String>,
    arm_id: &str,
    run_id: &str,
    cap: f64,
    timeout: Option<Duration>,
    root: &Path,
    fixtures: &Path,
) -> e2e::RunRecord {
    let t = tools();
    let arm = arms::arm_for(&t, arm_id).unwrap();
    let mut s = spec(arm_id, run_id, cap);
    if let Some(d) = timeout {
        s.timeout = d;
    }
    let exec = ProgramExecutor {
        program: PathBuf::from(STUB),
        lead_args: lead,
    };
    let hooks = hook_scripts_dir(fixtures);
    e2e::run_one(&RunInputs {
        spec: &s,
        arm: arm.as_ref(),
        fetcher: &FixtureFetcher {
            dir: fixtures.to_path_buf(),
        },
        executor: &exec,
        root,
        api_key: None,
        hook_scripts: Some(&hooks),
    })
    .unwrap()
}

/// Stand-ins for the tag's committed `.claude/hooks`, for the script-hook arm.
fn hook_scripts_dir(base: &Path) -> PathBuf {
    let d = base.join("tag-hooks");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("lumen_meter.sh"), "#!/usr/bin/env bash\nexit 0\n").unwrap();
    std::fs::write(
        d.join("lumen_read_intercept.sh"),
        "#!/usr/bin/env bash\nexit 0\n",
    )
    .unwrap();
    d
}

fn s(v: &str) -> String {
    v.to_string()
}

// --- the transcript parser -----------------------------------------------------------------------------

#[test]
fn a_fixture_with_duplicate_ids_and_subagents_gives_exact_totals() {
    let d = scratch();
    let main = write_file(d.path(), "session-1.jsonl", &main_transcript());
    let a1 = write_file(d.path(), "agent-a1.jsonl", &subagent_1());
    let a2 = write_file(d.path(), "agent-a2.jsonl", &subagent_2());
    let t = transcript::parse_files(&[main], &[a1, a2]).unwrap();

    assert_eq!(t.tokens.input, WANT_INPUT);
    assert_eq!(t.tokens.output, WANT_OUTPUT);
    assert_eq!(t.tokens.cache_read, WANT_READ);
    assert_eq!(t.tokens.cache_write, WANT_WRITE);
    assert_eq!(t.tokens.cache_write_1h, WANT_WRITE_1H);
    assert_eq!(t.tokens.cache_write_unknown, 0);
    // Four messages a model produced; the synthetic one is not an API request.
    assert_eq!(t.rounds, 4);
    // A twice more, B once more in main and once in a subagent.
    assert_eq!(t.duplicate_entries, 4);
    // A's first line had less output than its last.
    assert_eq!(t.disagreements, 1);
    assert_eq!(t.files, 3);
    assert_eq!(t.first_turn_cache_read, Some(1000));
    assert_eq!(t.unpriced_tokens, 0);
    // $2 in, $10 out, $2.50 5-minute write, $4 1-hour write, $0.10 read, per million.
    let want = (25.0 * 2.0 + 250.0 * 10.0 + 2700.0 * 0.1 + 400.0 * 2.5 + 600.0 * 4.0) / 1e6;
    assert!((t.usd - want).abs() < 1e-12, "{} vs {want}", t.usd);
}

#[test]
fn a_transcript_that_is_not_json_is_an_error_not_a_zero() {
    let d = scratch();
    let bad = write_file(
        d.path(),
        "session-1.jsonl",
        "{\"type\":\"user\"}\nnot json\n",
    );
    let e = transcript::parse_files(&[bad], &[]).unwrap_err();
    assert!(e.contains("line 2"), "{e}");
}

#[test]
fn an_assistant_line_without_an_id_is_counted_not_priced() {
    let d = scratch();
    let line = json!({"type":"assistant","message":{"role":"assistant","model":MODEL,
        "usage":{"input_tokens":1,"output_tokens":1}}})
    .to_string();
    let p = write_file(d.path(), "s.jsonl", &format!("{line}\n"));
    let t = transcript::parse_files(&[p], &[]).unwrap();
    assert_eq!((t.no_id_entries, t.rounds, t.tokens.output), (1, 0, 0));
}

#[test]
fn find_transcripts_separates_subagents_and_reads_only_the_projects_dir() {
    let d = scratch();
    let proj = d.path().join(".claude/projects/-work");
    std::fs::create_dir_all(proj.join("session-1/subagents")).unwrap();
    std::fs::write(proj.join("session-1.jsonl"), main_transcript()).unwrap();
    std::fs::write(
        proj.join("session-1/subagents/agent-a1.jsonl"),
        subagent_1(),
    )
    .unwrap();
    std::fs::write(proj.join("session-1/subagents/notes.jsonl"), "{}").unwrap();
    std::fs::write(d.path().join(".claude/history.jsonl"), "{}").unwrap();
    let (main, subs) = transcript::find_transcripts(d.path()).unwrap();
    assert_eq!((main.len(), subs.len()), (1, 1));
}

// --- the cross-check -----------------------------------------------------------------------------------

fn want_tokens() -> lumen_core::rates::Tokens {
    let d = scratch();
    let main = write_file(d.path(), "m.jsonl", &main_transcript());
    let a1 = write_file(d.path(), "a1.jsonl", &subagent_1());
    let a2 = write_file(d.path(), "a2.jsonl", &subagent_2());
    transcript::parse_files(&[main], &[a1, a2]).unwrap().tokens
}

fn result(input: i64, output: i64, read: i64, creation: i64) -> StreamResult {
    let v: Value =
        serde_json::from_str(&stream_result_line(input, output, read, creation)).unwrap();
    transcript::stream_result(&v).unwrap()
}

#[test]
fn a_cross_check_mismatch_is_flagged_and_a_match_is_not() {
    let t = want_tokens();
    let ok = transcript::cross_check(
        &t,
        Some(&result(WANT_INPUT, WANT_OUTPUT, WANT_READ, WANT_WRITE)),
    );
    assert!(ok.checked && !ok.flagged(), "{ok:?}");

    // Output off by 60%: flagged, and it names the component.
    let bad = transcript::cross_check(&t, Some(&result(WANT_INPUT, 625, WANT_READ, WANT_WRITE)));
    assert!(bad.flagged());
    assert_eq!(bad.mismatches.len(), 1);
    assert!(bad.mismatches[0].starts_with("output:"), "{bad:?}");

    // Just inside the tolerance is not flagged; just outside is.
    assert_eq!(CROSS_CHECK_TOLERANCE, 0.02);
    let inside =
        transcript::cross_check(&t, Some(&result(WANT_INPUT, WANT_OUTPUT, 2750, WANT_WRITE)));
    assert!(!inside.flagged(), "{inside:?}");
    let outside =
        transcript::cross_check(&t, Some(&result(WANT_INPUT, WANT_OUTPUT, 2800, WANT_WRITE)));
    assert!(outside.flagged(), "{outside:?}");

    // No result line (a killed run): nothing was checked, which is not a pass.
    let none = transcript::cross_check(&t, None);
    assert!(!none.checked && !none.flagged());
}

// --- a whole run replayed through the stub -------------------------------------------------------------

#[test]
fn a_replayed_run_records_exact_totals_and_the_command_line() {
    let d = scratch();
    let fx = scratch();
    let stream = write_file(
        fx.path(),
        "stream.jsonl",
        &(msg("msg_A", MODEL, 10, 100, 1000, 500, 300, 200)
            + "\n"
            + &msg("msg_B", MODEL, 5, 50, 1500, 0, 0, 0)
            + "\n"
            + &stream_result_line(WANT_INPUT, WANT_OUTPUT, WANT_READ, WANT_WRITE)
            + "\n"),
    );
    let main = write_file(fx.path(), "main.jsonl", &main_transcript());
    let a1 = write_file(fx.path(), "a1.jsonl", &subagent_1());
    let a2 = write_file(fx.path(), "a2.jsonl", &subagent_2());
    let rec_path = fx.path().join("record.json");
    let rec = run_with(
        vec![
            s("--record"),
            rec_path.display().to_string(),
            s("--stream"),
            stream.display().to_string(),
            s("--transcript"),
            main.display().to_string(),
            s("--subagent"),
            a1.display().to_string(),
            s("--subagent"),
            a2.display().to_string(),
        ],
        "none",
        "0000aaaa-0000-4000-a000-000000000001",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(!rec.failure, "{:?}", rec.failure_reason);
    assert_eq!(rec.outcome, Outcome::Exited { code: Some(0) });
    let t = rec.transcript.as_ref().expect("transcripts were read");
    assert_eq!((t.tokens.input, t.tokens.output), (WANT_INPUT, WANT_OUTPUT));
    assert_eq!(t.rounds, 4);
    assert!(rec.cross_check.checked && !rec.cross_check.flagged());
    // The one flag is the fixture's own: message A was first written with less output.
    assert_eq!(
        rec.flags,
        ["1 message id(s) were repeated with different usage"]
    );
    assert_eq!(
        rec.live_rounds, 4,
        "the live total is the main thread plus the subagents"
    );
    assert!((rec.live_usd - t.usd).abs() < 1e-12);
    assert!(rec.usd >= rec.live_usd && (rec.usd - t.usd).abs() < 1e-12);
    assert!(rec.wall_secs > 0.0);

    // What `claude` was given: METHOD.md's command line, with this run's nonce.
    let got: Value = serde_json::from_str(&std::fs::read_to_string(rec_path).unwrap()).unwrap();
    let args: Vec<String> = got["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
    assert_eq!(args[0], "-p");
    assert!(args[1].starts_with("The problem statement."));
    assert!(args[1].ends_with(e2e::INSTRUCTION));
    assert_eq!(args[at("--model") + 1], "claude-sonnet-5-5");
    assert_eq!(args[at("--output-format") + 1], "stream-json");
    assert!(args.contains(&s("--verbose")));
    assert!(args.contains(&s("--dangerously-skip-permissions")));
    assert_eq!(
        args[at("--append-system-prompt") + 1],
        "run 0000aaaa-0000-4000-a000-000000000001"
    );
    // The environment: the scratch HOME, the pin, no Claude Code or login variables.
    assert_eq!(got["env"]["DISABLE_AUTOUPDATER"], "1");
    let home = got["home"].as_str().unwrap();
    assert!(
        home.contains("0000aaaa-0000-4000-a000-000000000001"),
        "{home}"
    );
    let names: Vec<&str> = got["env_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n.to_ascii_uppercase().starts_with("CLAUDE") || n.starts_with("ANTHROPIC")),
        "{names:?}"
    );
}

#[test]
fn a_stream_and_transcript_that_disagree_are_flagged_in_the_record() {
    let d = scratch();
    let fx = scratch();
    // The result says far less output than the transcripts hold.
    let stream = write_file(
        fx.path(),
        "stream.jsonl",
        &(stream_result_line(WANT_INPUT, 100, WANT_READ, WANT_WRITE) + "\n"),
    );
    let main = write_file(fx.path(), "main.jsonl", &main_transcript());
    let rec = run_with(
        vec![
            s("--stream"),
            stream.display().to_string(),
            s("--transcript"),
            main.display().to_string(),
        ],
        "none",
        "0000aaaa-0000-4000-a000-000000000002",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(rec.cross_check.flagged());
    assert!(
        rec.flags.iter().any(|f| f.contains("output")),
        "{:?}",
        rec.flags
    );
}

// --- the cap ---------------------------------------------------------------------------------------------

#[test]
fn a_stub_that_keeps_spending_is_killed_at_the_cap_and_recorded_as_a_failure() {
    let d = scratch();
    let fx = scratch();
    // 100,000 output tokens of Sonnet 5.5 is $1.00 a message; the cap is $3.
    let started = std::time::Instant::now();
    let rec = run_with(
        vec![s("--spend-forever"), s("100000")],
        "lumen-1.9.0",
        "0000aaaa-0000-4000-a000-000000000003",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the cap did not stop the run"
    );
    assert_eq!(rec.outcome, Outcome::CapExceeded { cap_usd: 3.0 });
    assert!(rec.failure);
    assert!(rec.failure_reason.as_deref().unwrap().contains("cap"));
    // The cost so far is recorded and is what the cap saw: at least the cap, not unbounded.
    assert!(
        rec.live_usd >= 3.0 && rec.live_usd < 8.0,
        "{}",
        rec.live_usd
    );
    assert_eq!(rec.usd, rec.live_usd);
    assert!(rec.live_rounds >= 3);
    // Killed before any transcript was written: said so, not zero.
    assert!(rec.transcript.is_none());
    assert!(rec.transcript_error.is_some());
}

#[test]
fn a_run_under_the_cap_is_not_killed() {
    let d = scratch();
    let fx = scratch();
    let stream = write_file(
        fx.path(),
        "stream.jsonl",
        &(msg("msg_A", MODEL, 10, 100, 1000, 500, 300, 200) + "\n"),
    );
    let rec = run_with(
        vec![s("--stream"), stream.display().to_string()],
        "none",
        "0000aaaa-0000-4000-a000-000000000004",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert_eq!(rec.outcome, Outcome::Exited { code: Some(0) });
    assert!(!rec.failure && rec.live_usd > 0.0 && rec.live_usd < 3.0);
}

#[test]
fn a_run_that_outlasts_its_timeout_is_killed_and_recorded_as_a_failure() {
    let d = scratch();
    let fx = scratch();
    let started = std::time::Instant::now();
    let rec = run_with(
        vec![s("--sleep"), s("60")],
        "none",
        "0000aaaa-0000-4000-a000-000000000005",
        3.0,
        Some(Duration::from_millis(700)),
        d.path(),
        fx.path(),
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        matches!(rec.outcome, Outcome::TimedOut { .. }),
        "{:?}",
        rec.outcome
    );
    assert!(rec.failure);
}

#[test]
fn a_model_with_no_price_stops_the_run_because_the_cap_cannot_be_enforced() {
    let d = scratch();
    let fx = scratch();
    let stream = write_file(
        fx.path(),
        "stream.jsonl",
        &(msg("msg_X", "claude-nonexistent-9", 10, 100, 0, 0, 0, 0) + "\n"),
    );
    let rec = run_with(
        vec![
            s("--stream"),
            stream.display().to_string(),
            s("--sleep"),
            s("60"),
        ],
        "none",
        "0000aaaa-0000-4000-a000-000000000006",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(
        matches!(rec.outcome, Outcome::Unpriced { .. }),
        "{:?}",
        rec.outcome
    );
    assert!(rec.failure);
}

#[test]
fn a_nonzero_exit_is_a_failure() {
    let d = scratch();
    let fx = scratch();
    let rec = run_with(
        vec![s("--exit"), s("3")],
        "none",
        "0000aaaa-0000-4000-a000-000000000007",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(rec.failure);
    assert_eq!(rec.outcome, Outcome::Exited { code: Some(3) });
}

// --- no accidental real run ----------------------------------------------------------------------------

#[test]
fn a_real_run_is_refused_without_the_key_and_the_budget_flag() {
    let both = e2e::authorize(None, None).unwrap_err();
    assert!(
        both.contains("ANTHROPIC_API_KEY") && both.contains("--i-have-a-budget"),
        "{both}"
    );

    let no_budget = e2e::authorize(Some("sk-test"), None).unwrap_err();
    assert!(no_budget.contains("--i-have-a-budget") && !no_budget.contains("ANTHROPIC_API_KEY"));

    let no_key = e2e::authorize(None, Some("5")).unwrap_err();
    assert!(no_key.contains("ANTHROPIC_API_KEY") && !no_key.contains("--i-have-a-budget is"));
    assert!(
        e2e::authorize(Some("  "), Some("5")).is_err(),
        "a blank key is no key"
    );

    for bad in ["0", "-1", "abc", "NaN", "inf", ""] {
        assert!(
            e2e::authorize(Some("sk-test"), Some(bad)).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(
        e2e::authorize(Some("sk-test"), Some("12.5")).unwrap(),
        Authorized { budget_usd: 12.5 }
    );
}

#[test]
fn the_command_line_refuses_before_anything_else_and_never_prints_the_key() {
    let run = |key: Option<&str>, extra: &[&str]| {
        let mut c = Command::new(BENCH);
        c.args(["e2e", "run"]).args(extra);
        c.env_remove("ANTHROPIC_API_KEY");
        if let Some(k) = key {
            c.env("ANTHROPIC_API_KEY", k);
        }
        c.output().unwrap()
    };
    // No key, no flag; key without the flag; flag without the key: each refuses, and the refusal
    // is the guard's (the other required flags are not even looked at).
    for (key, extra) in [
        (None, vec![]),
        (Some("sk-secret-value"), vec![]),
        (None, vec!["--i-have-a-budget", "5"]),
    ] {
        let out = run(key, &extra);
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("refusing to start a real run"), "{err}");
        assert!(!err.contains("sk-secret-value"));
    }
}

#[test]
fn the_budget_bounds_the_cap_and_a_spent_budget_starts_nothing() {
    let mut b = Budget::new(&Authorized { budget_usd: 4.0 });
    assert_eq!(b.cap_for_run(3.0).unwrap(), 3.0);
    b.record(2.5);
    assert_eq!(b.cap_for_run(3.0).unwrap(), 1.5);
    b.record(2.0);
    assert!(b.cap_for_run(3.0).is_err());
}

#[test]
fn a_record_that_holds_the_key_is_not_written() {
    let d = scratch();
    let fx = scratch();
    let mut rec = run_with(
        vec![s("--exit"), s("0")],
        "none",
        "0000aaaa-0000-4000-a000-000000000008",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert!(e2e::write_record(&d.path().join("out"), &rec, Some("sk-secret-value")).is_ok());
    rec.stderr_tail.push("leaked sk-secret-value here".into());
    let e = e2e::write_record(&d.path().join("out2"), &rec, Some("sk-secret-value")).unwrap_err();
    assert!(!e.contains("sk-secret-value"));
    assert_eq!(e2e::redact("a sk-x b", Some("sk-x")), "a [redacted] b");
}

// --- the schedule --------------------------------------------------------------------------------------

#[test]
fn the_seeded_schedule_is_stable_and_differs_by_seed() {
    let (t, k) = (tools(), tasks());
    let a = schedule::full("seed-1", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    let b = schedule::full("seed-1", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    let c = schedule::full("seed-2", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    assert_eq!(a.slots, b.slots);
    assert_ne!(a.slots, c.slots);
    // Pinned: a change to the generator changes every published order, so it must be deliberate.
    let first: Vec<String> = a
        .slots
        .iter()
        .take(3)
        .map(|s| format!("{}/{}/{}", s.arm, s.task, s.rep))
        .collect();
    assert_eq!(
        first,
        [
            "none/sphinx-doc__sphinx-7440/0",
            "headroom/sphinx-doc__sphinx-7440/0",
            "lumen-1.8.0/sphinx-doc__sphinx-7440/0"
        ]
    );
}

#[test]
fn the_schedule_is_balanced_and_the_shared_arms_run_once() {
    let (t, k) = (tools(), tasks());
    let both = schedule::full("s", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    let tasks_n = schedule::task_ids(&k).len();
    assert_eq!(tasks_n, 13);
    let arms: BTreeSet<&str> = both.slots.iter().map(|s| s.arm.as_str()).collect();
    assert_eq!(arms.len(), 9, "{arms:?}");
    assert_eq!(both.slots.len(), 9 * 13 * 3, "the 351 runs of the plan");
    let counts = both.cell_counts();
    assert_eq!(counts.len(), 9 * 13);
    assert!(counts.values().all(|&n| n == 3), "every cell exactly 3");
    // No run listed twice.
    let unique: BTreeSet<_> = both
        .slots
        .iter()
        .map(|s| (&s.arm, &s.task, s.rep))
        .collect();
    assert_eq!(unique.len(), both.slots.len());

    // Each comparison alone, and what they share.
    let comp = schedule::full("s", &t, &k, &[Suite::Competitors], 3).unwrap();
    let ver = schedule::full("s", &t, &k, &[Suite::Versions], 3).unwrap();
    assert_eq!(comp.slots.len(), 6 * 13 * 3);
    assert_eq!(ver.slots.len(), 5 * 13 * 3);
    let shared = 2 * 13 * 3;
    assert_eq!(
        both.slots.len(),
        comp.slots.len() + ver.slots.len() - shared
    );
    for id in ["none", "lumen-1.9.0"] {
        let n = both.slots.iter().filter(|s| s.arm == id).count();
        assert_eq!(n, 13 * 3, "{id} runs once, for both reports");
    }
    // 1.1.5 stands in for 1.0.0 in the version comparison only.
    assert!(ver.slots.iter().any(|s| s.arm == "lumen-1.1.5"));
    assert!(!comp.slots.iter().any(|s| s.arm == "lumen-1.1.5"));
}

#[test]
fn no_arm_always_goes_first_and_fewer_than_three_runs_is_refused() {
    let (t, k) = (tools(), tasks());
    let both = schedule::full("s", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    // Within a task and rep the arms are contiguous: the first arm of each such group.
    let mut firsts = BTreeSet::new();
    for chunk in both.slots.chunks(9) {
        firsts.insert(chunk[0].arm.clone());
    }
    assert!(firsts.len() >= 4, "{firsts:?}");
    assert!(schedule::full("s", &t, &k, &[Suite::Competitors], 2).is_err());
    assert!(schedule::build("s", &[], &[s("t")], 3).is_err());
}

#[test]
fn the_pilot_is_one_run_of_every_arm_on_the_first_task() {
    let (t, k) = (tools(), tasks());
    let p = schedule::pilot("s", &t, &k).unwrap();
    assert_eq!(p.slots.len(), 9);
    let first = &k.swe[0].id;
    assert!(p.slots.iter().all(|s| &s.task == first && s.rep == 0));
    assert_eq!(
        p.slots
            .iter()
            .map(|s| &s.arm)
            .collect::<BTreeSet<_>>()
            .len(),
        9
    );
}

#[test]
fn nonces_are_unique_deterministic_and_uuid_shaped() {
    let (t, k) = (tools(), tasks());
    let sch = schedule::full("s", &t, &k, &[Suite::Competitors, Suite::Versions], 3).unwrap();
    let all: BTreeSet<String> = sch
        .slots
        .iter()
        .map(|x| schedule::nonce("s", x, 0))
        .collect();
    assert_eq!(all.len(), sch.slots.len());
    let x = &sch.slots[0];
    assert_eq!(schedule::nonce("s", x, 0), schedule::nonce("s", x, 0));
    assert_ne!(
        schedule::nonce("s", x, 0),
        schedule::nonce("s", x, 1),
        "a retry gets a new nonce"
    );
    let n = schedule::nonce("s", x, 0);
    let parts: Vec<usize> = n.split('-').map(str::len).collect();
    assert_eq!(parts, [8, 4, 4, 4, 12], "{n}");
    assert!(n.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
}

// --- the arms ------------------------------------------------------------------------------------------

fn normalise(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn golden(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/setup")
        .join(name);
    normalise(&std::fs::read_to_string(p).unwrap())
}

fn install_lumen(id: &str, home: &Path, fx: &Path) -> e2e::arms::ArmPlan {
    let t = tools();
    let arm = arms::arm_for(&t, id).unwrap();
    let hooks = hook_scripts_dir(fx);
    arm.install(&InstallCtx {
        home,
        claude_home: "/H",
        fetcher: &FixtureFetcher {
            dir: fx.to_path_buf(),
        },
        hook_scripts: Some(&hooks),
    })
    .unwrap()
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn the_lumen_arm_writes_what_setup_writes() {
    // The golden is what setup.rs's own step_register_mcp_in and step_install_hooks_in produced
    // for a home of /H, a server at /H/.lumen-bench/bin/lumen-mcp and a ledger beside it.
    let home = scratch();
    let fx = scratch();
    install_lumen("lumen-1.9.0", home.path(), fx.path());

    let want_mcp: Value = serde_json::from_str(&golden("claude.json")).unwrap();
    let got_mcp = read_json(&home.path().join(".claude.json"));
    assert_eq!(got_mcp["mcpServers"], want_mcp["mcpServers"]);

    let want_hooks: Value = serde_json::from_str(&golden("settings.json")).unwrap();
    let got_hooks = read_json(&home.path().join(".claude/settings.json"));
    assert_eq!(got_hooks["hooks"], want_hooks["hooks"]);

    // The scripts: the same exec lines as the ones Setup wrote, and the sidecars are beside them.
    for (script, exec) in [
        (
            "lumen_meter.sh",
            "[ -n \"$bin\" ] && exec \"$bin\" hook meter --writer lumen_meter.sh",
        ),
        (
            "lumen_read_intercept.sh",
            "[ -n \"$bin\" ] && exec \"$bin\" hook intercept",
        ),
    ] {
        assert!(golden(script).lines().any(|l| l == exec), "golden {script}");
        let ours = normalise(
            &std::fs::read_to_string(home.path().join(".claude/lumen").join(script)).unwrap(),
        );
        assert!(ours.lines().any(|l| l == exec), "{script}:\n{ours}");
        for key in ["default_mcp=", "default_db=", "default_spool="] {
            let g = golden(script);
            let want = g.lines().find(|l| l.starts_with(key)).unwrap();
            assert!(ours.lines().any(|l| l == want), "{script}: {want}");
        }
    }
    for s in lumen_bench::assets::SIDECARS {
        assert!(home.path().join(".lumen-bench/bin").join(s).is_file());
    }
}

#[test]
fn setup_rs_still_says_what_the_golden_and_the_arm_assume() {
    // A drift guard: the golden above was made from setup.rs at one moment. If these lines of the
    // source change, the arm and the golden must be looked at again.
    let src = normalise(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lumenator/src-tauri/src/setup.rs"),
        )
        .unwrap(),
    );
    for line in [
        "const METER_MATCHERS: [&str; 2] = [\"Read\", \"Bash\"];",
        "const INTERCEPT_MATCHERS: [&str; 4] = [",
        "\"type\":    \"stdio\",",
        "\"args\":    [],",
        "\"LUMEN_DB\":  db",
        "root[\"mcpServers\"][\"lumen\"] = entry;",
        "[ -n \"$bin\" ] && exec \"$bin\" hook meter --writer __LUMEN_WRITER__",
        "[ -n \"$bin\" ] && exec \"$bin\" hook intercept",
        "default_mcp=__LUMEN_MCP__",
        "default_db=__LUMEN_DB__",
        "default_spool=__LUMEN_SPOOL__",
        "global_settings_path_in(home: &Path) -> PathBuf {\n    home.join(\".claude/settings.json\")",
        "claude_json_path_in(home: &Path) -> PathBuf {\n    home.join(\".claude.json\")",
    ] {
        assert!(src.contains(line), "setup.rs no longer contains {line:?}");
    }
    assert!(
        src.contains("merge_hook_entry(&mut root[\"hooks\"][\"PreToolUse\"], matcher, &intercept)")
    );
}

#[test]
fn older_lumen_versions_get_the_hooks_their_version_had() {
    // 1.8.0 has the identity matchers; 1.6.0 meters Bash but has no identity hook.
    let fx = scratch();
    for (id, pre, post) in [
        ("lumen-1.8.0", 4, vec!["Read", "Bash"]),
        ("lumen-1.6.0", 1, vec!["Read", "Bash"]),
        ("lumen-1.1.5", 1, vec!["Read"]),
    ] {
        let home = scratch();
        install_lumen(id, home.path(), fx.path());
        let h = read_json(&home.path().join(".claude/settings.json"));
        let matchers = |phase: &str| -> Vec<String> {
            h["hooks"][phase]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["matcher"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(matchers("PreToolUse").len(), pre, "{id}");
        assert_eq!(matchers("PostToolUse"), post, "{id}");
    }
    // 1.9.0 and 1.8.0 are the same shape.
    let (a, b) = (scratch(), scratch());
    install_lumen("lumen-1.9.0", a.path(), fx.path());
    install_lumen("lumen-1.8.0", b.path(), fx.path());
    assert_eq!(
        read_json(&a.path().join(".claude/settings.json")),
        read_json(&b.path().join(".claude/settings.json"))
    );
}

#[test]
fn the_script_hook_version_needs_the_tags_scripts_and_gets_lumen_tok() {
    let t = tools();
    let arm = arms::arm_for(&t, "lumen-1.1.5").unwrap();
    let home = scratch();
    let fx = scratch();
    let e = arm
        .install(&InstallCtx {
            home: home.path(),
            claude_home: "/H",
            fetcher: &FixtureFetcher {
                dir: fx.path().to_path_buf(),
            },
            hook_scripts: None,
        })
        .expect_err("no scripts, no install");
    assert!(e.contains("hook scripts"), "{e}");
    // Nothing was written before the refusal.
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());

    let home = scratch();
    let plan = install_lumen("lumen-1.1.5", home.path(), fx.path());
    let mcp = read_json(&home.path().join(".claude.json"));
    assert_eq!(
        mcp["mcpServers"]["lumen"]["env"]["LUMEN_TOK"],
        "/H/.lumen-bench/bin/lumen-tok"
    );
    assert_eq!(plan.env["LUMEN_TOK"], "/H/.lumen-bench/bin/lumen-tok");
    // The tag's own script, byte for byte, not the shim.
    let s = std::fs::read_to_string(home.path().join(".claude/lumen/lumen_meter.sh")).unwrap();
    assert_eq!(normalise(&s), "#!/usr/bin/env bash\nexit 0\n");
}

#[test]
fn every_arm_installs_and_is_described_with_its_pins_and_telemetry_switches() {
    let t = tools();
    let fx = scratch();
    let want: BTreeSet<&str> = [
        "none",
        "lumen-1.9.0",
        "lumen-1.1.5",
        "lumen-1.6.0",
        "lumen-1.8.0",
        "squeez",
        "rtk",
        "headroom",
        "caveman",
    ]
    .into();
    let all = arms::all_arms(&t).unwrap();
    assert_eq!(all.iter().map(|a| a.id()).collect::<BTreeSet<_>>(), want);
    for arm in &all {
        let d = arm.describe();
        assert_eq!(d.id, arm.id());
        let home = scratch();
        let plan = {
            let hooks = hook_scripts_dir(fx.path());
            arm.install(&InstallCtx {
                home: home.path(),
                claude_home: "/home/bench",
                fetcher: &FixtureFetcher {
                    dir: fx.path().to_path_buf(),
                },
                hook_scripts: Some(&hooks),
            })
            .unwrap()
        };
        // Claude Code pinned, the arm's telemetry off, in every plan.
        assert_eq!(plan.env["DISABLE_AUTOUPDATER"], "1", "{}", d.id);
        for (k, v) in &d.telemetry_env {
            assert_eq!(&plan.env[k], v, "{} {k}", d.id);
        }
        for f in plan.written.iter().filter(|f| !f.ends_with('/')) {
            assert!(
                home.path().join(f).exists(),
                "{} claims to have written {f}",
                d.id
            );
        }
        if d.kind == "competitor" {
            assert!(!plan.setup.is_empty(), "{}", d.id);
        }
    }
    let caveman = arms::arm_for(&t, "caveman").unwrap();
    assert_eq!(
        caveman.describe().extra_env["CAVEMAN_DEFAULT_MODE"],
        "caveman"
    );
    let stand_in = arms::arm_for(&t, "lumen-1.1.5").unwrap().describe();
    assert!(
        stand_in.label.contains("stands in for 1.0.0"),
        "{}",
        stand_in.label
    );
}

#[test]
fn a_tool_arm_installs_the_pinned_version_and_checks_the_script_it_runs() {
    let t = tools();
    let fx = scratch();
    let install = |id: &str| {
        let home = scratch();
        arms::arm_for(&t, id)
            .unwrap()
            .install(&InstallCtx {
                home: home.path(),
                claude_home: "/home/bench",
                fetcher: &FixtureFetcher {
                    dir: fx.path().to_path_buf(),
                },
                hook_scripts: None,
            })
            .unwrap()
    };
    let squeez = install("squeez");
    assert_eq!(
        squeez.setup,
        vec![vec![s("/home/bench/.claude/squeez/bin/squeez"), s("setup")]]
    );
    let rtk = install("rtk");
    assert_eq!(rtk.setup[0][1..], [s("init"), s("-g"), s("--auto-patch")]);
    assert_eq!(rtk.path_prepend, vec![s("/home/bench/.local/bin")]);
    let hr = install("headroom");
    assert_eq!(hr.claude_prefix, ["headroom", "wrap", "claude", "--"]);
    assert!(hr.setup[0].contains(&s("headroom-ai[all]==0.41.0")));
    assert!(hr.setup[0].contains(&s("2026-10-18T00:00:00Z")));
    // The weights revision is the one tools.toml pins for the arm, read from there so the two
    // cannot drift apart again.
    let tools_toml: toml::Value = toml::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../competitors/tools.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    let kompress = tools_toml["arm"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"].as_str() == Some("headroom"))
        .and_then(|a| a["model"]["commit"].as_str())
        .unwrap()
        .to_string();
    assert_eq!(kompress.len(), 40);
    assert!(hr.setup[1].contains(&kompress), "{:?}", hr.setup[1]);
    let cv = install("caveman");
    let script = &cv.setup[0][2];
    assert!(script.contains("c129b946a1f7f2e9eeeff5c1a747d7b43b2630c6b9e5fc7aa8297cece9cd3804"));
    assert!(script.contains("sha256sum -c -"));
    assert!(script.contains("/v4.0.1/install.sh"));
    assert!(script.contains("--non-interactive"));
    // The pinned script is checked before it is run.
    assert!(script.find("sha256sum").unwrap() < script.find("bash \"$f\"").unwrap());
}

// --- the scratch home ------------------------------------------------------------------------------------

#[test]
fn an_install_cannot_name_a_path_outside_its_home() {
    let home = scratch();
    for bad in ["../x", "/etc/x", "a/../../x", "", "C:/x"] {
        let r = arms::write_under(home.path(), bad, b"x", false);
        // `C:/x` is a relative path on Unix and absolute on Windows; either way it must stay in.
        if bad == "C:/x" {
            if r.is_ok() {
                assert!(home.path().join("C:/x").exists() || home.path().join("C:").exists());
            }
            continue;
        }
        assert!(r.is_err(), "{bad:?}");
    }
    assert!(arms::write_under(home.path(), ".claude/a/b.json", b"{}", false).is_ok());
    assert!(home.path().join(".claude/a/b.json").is_file());
}

/// Size, mtime and sha256 of the real config files an arm must never touch. `~/.claude.json` is
/// left out on purpose: a Claude Code that is running right now rewrites it on its own, so a
/// change there could not be pinned on this test; the other files are only ever edited by hand.
type FileSnap = (String, Option<(u64, std::time::SystemTime, String)>);

fn real_config_snapshot() -> Vec<FileSnap> {
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return Vec::new();
    };
    let home = PathBuf::from(home);
    let mut files = vec![
        home.join(".claude/settings.json"),
        home.join(".claude/settings.local.json"),
        home.join(".claude/CLAUDE.md"),
    ];
    if let Ok(rd) = std::fs::read_dir(home.join(".claude/lumen")) {
        files.extend(rd.flatten().map(|e| e.path()));
    }
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let snap = std::fs::metadata(&p).ok().map(|m| {
                let bytes = std::fs::read(&p).unwrap_or_default();
                (m.len(), m.modified().unwrap(), sha256_hex(&bytes))
            });
            (p.display().to_string(), snap)
        })
        .collect()
}

#[test]
fn every_arm_installs_and_runs_only_in_its_scratch_home() {
    let before = real_config_snapshot();
    let view_before = real_home().and_then(|h| claude_json_view(&h));
    let root = scratch();
    let fx = scratch();
    let stream = write_file(
        fx.path(),
        "stream.jsonl",
        &(msg("msg_A", MODEL, 1, 1, 0, 0, 0, 0) + "\n"),
    );
    let t = tools();
    for (n, arm) in arms::all_arms(&t).unwrap().iter().enumerate() {
        let rec_path = fx.path().join(format!("rec-{n}.json"));
        let run_id = format!("0000bbbb-0000-4000-a000-00000000000{n}");
        let rec = run_with(
            vec![
                s("--record"),
                rec_path.display().to_string(),
                s("--stream"),
                stream.display().to_string(),
            ],
            arm.id(),
            &run_id,
            3.0,
            None,
            root.path(),
            fx.path(),
        );
        assert!(!rec.failure, "{}: {:?}", arm.id(), rec.failure_reason);
        let got = read_json(&rec_path);
        let home = PathBuf::from(got["home"].as_str().unwrap());
        assert!(
            home.starts_with(root.path()),
            "{}: HOME {}",
            arm.id(),
            home.display()
        );
        assert!(home.join(".claude.json").is_file(), "{}", arm.id());
        // The run left nothing outside its own directory under root.
        let entries: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().all(|e| e.starts_with("0000bbbb-")),
            "{entries:?}"
        );
    }
    assert_eq!(
        before,
        real_config_snapshot(),
        "a real Claude Code config file changed"
    );
    // ~/.claude.json is rewritten by any Claude Code running now, so its counters are not
    // compared; the keys an arm could change are.
    assert_eq!(
        view_before,
        real_home().and_then(|h| claude_json_view(&h)),
        "mcpServers, hooks, env, permissions or model of the real ~/.claude.json changed"
    );
}

// --- the Docker adapter's command lines (not run here) ---------------------------------------------------

#[test]
fn the_docker_command_lines_have_no_mount_no_key_value_and_the_pinned_claude() {
    let t = tools();
    let fx = scratch();
    let root = scratch();
    let sandbox = Sandbox::create(&root.path().join("run")).unwrap();
    let arm = arms::arm_for(&t, "headroom").unwrap();
    let plan = arm
        .install(&InstallCtx {
            home: &sandbox.home,
            claude_home: e2e::CONTAINER_HOME,
            fetcher: &FixtureFetcher {
                dir: fx.path().to_path_buf(),
            },
            hook_scripts: None,
        })
        .unwrap();
    let sp = spec("headroom", "0000cccc-0000-4000-a000-000000000001", 3.0);
    let ctx = RunCtx {
        spec: &sp,
        plan: &plan,
        sandbox: &sandbox,
        api_key: Some("sk-secret-value"),
    };
    let d = DockerExecutor {
        docker: "docker".into(),
        workdir: "/testbed".into(),
    };
    let argv = d.create_argv(&ctx, "@anthropic-ai/claude-code@2.1.292");
    let joined = argv.join("\u{1}");
    assert_eq!(&argv[..2], ["docker", "create"]);
    assert!(
        argv.windows(2).any(|w| w == ["--platform", "linux/amd64"]),
        "{argv:?}"
    );
    assert!(
        !joined.contains("sk-secret-value"),
        "the key's value is on a command line"
    );
    assert!(argv.windows(2).any(|w| w == ["-e", "ANTHROPIC_API_KEY"]));
    assert!(
        !argv
            .iter()
            .any(|a| a == "-v" || a == "--volume" || a.starts_with("--mount"))
    );
    assert!(argv.contains(&"HOME=/home/bench".to_string()));
    assert!(argv.contains(&"DISABLE_AUTOUPDATER=1".to_string()));
    assert!(argv.contains(&"HEADROOM_BEACON=off".to_string()));
    assert!(argv.contains(&sp.image));
    assert!(argv.contains(&"lumen-e2e-0000cccc-0000-4000-a000-000000000001".to_string()));
    let script = argv.last().unwrap();
    assert!(script.contains("npm install -g '@anthropic-ai/claude-code@2.1.292'"));
    assert!(script.contains("exec 'headroom' 'wrap' 'claude' '--' claude '-p'"));
    assert!(script.contains("'--append-system-prompt' 'run 0000cccc-0000-4000-a000-000000000001'"));
    assert!(script.contains("'--model' 'claude-sonnet-5-5'"));
    assert!(script.find("uv").unwrap() < script.find("exec").unwrap());
    // With no key, the variable is not passed at all.
    let ctx = RunCtx {
        api_key: None,
        ..ctx
    };
    assert!(
        !d.create_argv(&ctx, "p")
            .contains(&"ANTHROPIC_API_KEY".to_string())
    );
}

#[test]
fn the_help_and_unknown_commands_do_not_run_anything() {
    assert!(e2e::entry(&[s("--help")]).is_ok());
    assert!(e2e::entry(&[s("frobnicate")]).is_err());
    assert!(e2e::entry(&[s("schedule"), s("--seed"), s("x"), s("--reps"), s("3")]).is_ok());
    assert!(e2e::entry(&[s("schedule"), s("--reps"), s("2")]).is_err());
    assert!(e2e::entry(&[s("arms")]).is_ok());
}

// --- images ----------------------------------------------------------------------------------------------

#[test]
fn an_image_without_a_sha256_digest_is_rejected() {
    let hex = "a".repeat(64);
    assert!(
        e2e::validate_image(&format!(
            "swebench/sweb.eval.x86_64.django:latest@sha256:{hex}"
        ))
        .is_ok()
    );
    for bad in [
        "swebench/sweb.eval.x86_64.django:latest".to_string(),
        "repo".to_string(),
        "repo@sha256:abc".to_string(),
        format!("repo@sha256:{}", "g".repeat(64)),
        format!("repo@sha256:{}", "A".repeat(64)),
        format!("@sha256:{hex}"),
        format!("repo@sha512:{hex}"),
    ] {
        assert!(e2e::validate_image(&bad).is_err(), "{bad}");
    }
}

#[test]
fn the_docker_executor_refuses_an_unpinned_image_before_it_runs_docker() {
    use e2e::Executor;
    let t = tools();
    let fx = scratch();
    let root = scratch();
    let sandbox = Sandbox::create(&root.path().join("run")).unwrap();
    let plan = arms::arm_for(&t, "none")
        .unwrap()
        .install(&InstallCtx {
            home: &sandbox.home,
            claude_home: e2e::CONTAINER_HOME,
            fetcher: &FixtureFetcher {
                dir: fx.path().to_path_buf(),
            },
            hook_scripts: None,
        })
        .unwrap();
    let sp = spec("none", "0000cccc-0000-4000-a000-000000000002", 3.0);
    let ctx = RunCtx {
        spec: &sp,
        plan: &plan,
        sandbox: &sandbox,
        api_key: None,
    };
    // `docker` is a program that does not exist: reaching it would be a different error.
    let d = DockerExecutor {
        docker: "docker-that-does-not-exist".into(),
        workdir: "/testbed".into(),
    };
    let e = d.prepare(&ctx).unwrap_err();
    assert!(e.contains("not pinned"), "{e}");
}

// --- the budget, through the command line ------------------------------------------------------------------

struct Cmd {
    dir: tempfile::TempDir,
}

impl Cmd {
    fn new() -> Cmd {
        let c = Cmd { dir: scratch() };
        std::fs::write(c.dir.path().join("prompt.txt"), "A problem.").unwrap();
        // One message of 100,000 Sonnet 5.5 output tokens: exactly $1.00.
        std::fs::write(
            c.dir.path().join("stream.jsonl"),
            msg("msg_one", MODEL, 0, 100000, 0, 0, 0, 0) + "\n",
        )
        .unwrap();
        for t in ["t1", "t2"] {
            std::fs::create_dir_all(c.dir.path().join("tasks").join(t)).unwrap();
            std::fs::write(
                c.dir.path().join("tasks").join(t).join("prompt.txt"),
                "A problem.",
            )
            .unwrap();
        }
        c
    }
    fn out(&self) -> PathBuf {
        self.dir.path().join("out")
    }
    fn go(&self, sub: &str, budget: &str, extra: &[&str], lead: &[&str]) -> std::process::Output {
        let mut c = Command::new(BENCH);
        c.args(["e2e", sub, "--i-have-a-budget", budget]);
        c.args([
            "--cache",
            &self.dir.path().join("cache").display().to_string(),
        ]);
        c.args(["--out", &self.out().display().to_string()]);
        c.args(["--poll-ms", "50", "--min-run-usd", "1.0", "--cap", "3"]);
        c.args(["--claude-program", STUB]);
        for l in lead {
            c.args(["--claude-lead-arg", l]);
        }
        c.args(extra);
        c.env("ANTHROPIC_API_KEY", "sk-test-key");
        c.output().unwrap()
    }
    fn schedule(&self, budget: &str) -> std::process::Output {
        let stream = self.dir.path().join("stream.jsonl").display().to_string();
        self.go(
            "run-schedule",
            budget,
            &[
                "--arms",
                "none",
                "--tasks",
                "t1,t2",
                "--reps",
                "2",
                "--seed",
                "s",
                "--task-dir",
                &self.dir.path().join("tasks").display().to_string(),
            ],
            &["--stream", &stream],
        )
    }
    fn records(&self) -> Vec<Value> {
        let mut v: Vec<(String, Value)> = std::fs::read_dir(self.out())
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_name() != e2e::LEDGER_FILE)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            read_json(&e.path()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| {
            a.1["cap_usd"]
                .as_f64()
                .partial_cmp(&b.1["cap_usd"].as_f64())
                .unwrap()
                .reverse()
        });
        v.into_iter().map(|(_, r)| r).collect()
    }
    fn ledger(&self) -> Value {
        read_json(&self.out().join(e2e::LEDGER_FILE))
    }
}

fn text(o: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[test]
fn a_subagent_that_spends_is_killed_at_the_cap_while_the_main_thread_is_silent() {
    let d = scratch();
    let fx = scratch();
    let rec = run_with(
        vec![s("--spend-subagent"), s("100000")],
        "none",
        "0000aaaa-0000-4000-a000-000000000009",
        3.0,
        None,
        d.path(),
        fx.path(),
    );
    assert_eq!(
        rec.outcome,
        Outcome::CapExceeded { cap_usd: 3.0 },
        "{:?}",
        rec.flags
    );
    assert!(rec.failure && rec.live_usd >= 3.0, "{}", rec.live_usd);
    assert_eq!(rec.live_rounds, rec.live_rounds.max(3));
}

#[test]
fn a_single_run_is_bounded_by_the_smaller_of_the_cap_and_what_is_left() {
    let c = Cmd::new();
    let prompt = c.dir.path().join("prompt.txt").display().to_string();
    // The per-run cap is $3 but the whole budget is $2: the run is capped at $2.
    let o = c.go(
        "run",
        "2",
        &["--arm", "none", "--task", "t1", "--prompt-file", &prompt],
        &["--spend-forever", "100000"],
    );
    assert!(o.status.success(), "{}", text(&o));
    let recs = c.records();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0]["cap_usd"], 2.0);
    assert_eq!(recs[0]["outcome"]["kind"], "CapExceeded");
    let spent = c.ledger()["spent_usd"].as_f64().unwrap();
    assert!((2.0..=4.0).contains(&spent), "{spent}");
    assert!(!text(&o).contains("sk-test-key"));
    // And the budget is now spent: a second run does not start.
    let o = c.go(
        "run",
        "2",
        &["--arm", "none", "--task", "t2", "--prompt-file", &prompt],
        &["--spend-forever", "100000"],
    );
    assert!(!o.status.success());
    assert_eq!(c.records().len(), 1);
}

#[test]
fn a_schedule_stops_when_the_budget_cannot_cover_a_run_and_a_restart_does_not_respend_it() {
    let c = Cmd::new();
    // Four runs of $1.00; $2.50 left cannot cover a run of at least $1.00 after two.
    let o = c.schedule("2.5");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("stopping"), "{}", text(&o));
    let recs = c.records();
    assert_eq!(recs.len(), 2, "{}", text(&o));
    assert_eq!(
        recs[0]["cap_usd"], 2.5,
        "the first run is capped by the budget"
    );
    assert_eq!(recs[1]["cap_usd"], 1.5, "the second by what the first left");
    let l = c.ledger();
    assert!((l["spent_usd"].as_f64().unwrap() - 2.0).abs() < 1e-9, "{l}");
    assert_eq!(l["runs"].as_array().unwrap().len(), 2);
    assert!(l["pending"].is_null());

    // A restart with the same budget: nothing runs, the spend is not reset.
    let o = c.schedule("2.5");
    assert!(o.status.success(), "{}", text(&o));
    assert_eq!(c.records().len(), 2);
    assert!((c.ledger()["spent_usd"].as_f64().unwrap() - 2.0).abs() < 1e-9);

    // More budget: it carries on from the third run and does not redo the first two.
    let o = c.schedule("3.5");
    assert!(o.status.success(), "{}", text(&o));
    assert!(text(&o).contains("2 already done"), "{}", text(&o));
    let recs = c.records();
    assert_eq!(recs.len(), 3);
    assert!(
        recs.iter()
            .any(|r| r["cap_usd"] == 1.5 && r["run_id"] != recs[0]["run_id"])
    );
    assert!((c.ledger()["spent_usd"].as_f64().unwrap() - 3.0).abs() < 1e-9);
}

#[test]
fn a_run_is_reserved_at_its_cap_in_the_ledger_on_disk_while_it_is_still_running() {
    let c = Cmd::new();
    let stream = c.dir.path().join("stream.jsonl").display().to_string();
    let gate = c.dir.path().join("gate");
    let prompt = c.dir.path().join("prompt.txt").display().to_string();
    let mut child = Command::new(BENCH)
        .args(["e2e", "run", "--i-have-a-budget", "5"])
        .args(["--cache", &c.dir.path().join("cache").display().to_string()])
        .args(["--out", &c.out().display().to_string()])
        .args(["--poll-ms", "50", "--min-run-usd", "1.0", "--cap", "3"])
        .args(["--claude-program", STUB])
        .args(["--arm", "none", "--task", "t1", "--prompt-file", &prompt])
        .args(["--claude-lead-arg", "--stream"])
        .args(["--claude-lead-arg", &stream])
        .args(["--claude-lead-arg", "--wait-for"])
        .args(["--claude-lead-arg", &gate.display().to_string()])
        .env("ANTHROPIC_API_KEY", "sk-test-key")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The stub blocks on the gate, so the run is in flight while the ledger is read.
    let ledger_path = c.out().join(e2e::LEDGER_FILE);
    let started = std::time::Instant::now();
    let pending = loop {
        let p = std::fs::read_to_string(&ledger_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .filter(|l| !l["pending"].is_null());
        if let Some(l) = p {
            break l;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            panic!("no reservation reached the ledger on disk while the run was in flight");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(pending["pending"]["usd"], 3.0, "{pending}");
    assert_eq!(pending["spent_usd"], 0.0, "{pending}");
    assert!(pending["runs"].as_array().unwrap().is_empty(), "{pending}");
    assert!(c.records().is_empty(), "the run was not over yet");

    std::fs::write(&gate, "go").unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    // Settled: the reservation is replaced by the real spend ($1.00, not the $3 cap).
    let l = c.ledger();
    assert!(l["pending"].is_null(), "{l}");
    assert!((l["spent_usd"].as_f64().unwrap() - 1.0).abs() < 1e-9, "{l}");
    assert_eq!(l["runs"].as_array().unwrap().len(), 1);
    assert_eq!(c.records().len(), 1);
}

#[test]
fn a_run_that_died_mid_run_is_charged_at_its_cap_on_restart() {
    let c = Cmd::new();
    std::fs::create_dir_all(c.out()).unwrap();
    std::fs::write(
        c.out().join(e2e::LEDGER_FILE),
        r#"{"spent_usd":1.0,"runs":[],"pending":{"run_id":"x","usd":1.5}}"#,
    )
    .unwrap();
    let l = e2e::Ledger::load(&c.out()).unwrap();
    assert_eq!(l.spent_usd, 2.5);
    assert!(l.pending.is_none());
    // And the file was rewritten, so a second restart does not charge it again.
    assert_eq!(e2e::Ledger::load(&c.out()).unwrap().spent_usd, 2.5);
}

#[test]
fn an_unpinned_image_stops_a_real_run_before_anything_starts() {
    let c = Cmd::new();
    let prompt = c.dir.path().join("prompt.txt").display().to_string();
    let mut cmd = Command::new(BENCH);
    cmd.args([
        "e2e",
        "run",
        "--i-have-a-budget",
        "5",
        "--arm",
        "none",
        "--task",
        "t1",
    ])
    .args(["--prompt-file", &prompt, "--image", "repo:latest"])
    .args(["--cache", &c.dir.path().join("cache").display().to_string()])
    .args(["--out", &c.out().display().to_string()])
    .env("ANTHROPIC_API_KEY", "sk-test-key");
    let o = cmd.output().unwrap();
    assert!(!o.status.success());
    assert!(text(&o).contains("not pinned"), "{}", text(&o));
    assert!(
        !c.out().join(e2e::LEDGER_FILE).exists(),
        "nothing was reserved"
    );
}
