//! Token-efficiency benchmarks for every MCP tool, compared against a committed baseline.
//!
//! ## Why this exists separately from `efficiency.rs`
//!
//! `efficiency.rs` measures *this repository* — whatever source files happen to be present. That is
//! the right shape for "what does the optimizer save on real code", and the wrong shape for showing
//! progress: its numbers move when the corpus moves. The break-even count went from 17 to 16 within
//! an hour of being published, because a commit added a file. A benchmark whose numbers drift for
//! reasons unrelated to the code cannot attribute a change to a fix.
//!
//! So this file measures **frozen, generated inputs**. The generators below are the specification:
//! same bytes on every machine and every commit, so a delta means the code changed and nothing else
//! could have caused it.
//!
//! ## Coverage
//!
//! Every tool the server advertises must have at least one scenario, and
//! `every_advertised_tool_is_benchmarked` reads `tools/list` to enforce it — so adding a fifth tool
//! fails this suite until it is measured, rather than silently going unmeasured. Scenarios are
//! driven through `handle_tools_call`, the same dispatch the stdio server uses, so the argument
//! parsing and error paths are exercised rather than bypassed.
//!
//! ## Reading the output
//!
//! ```text
//! cargo test --release -p lumen-mcp --test optimizer_bench -- --nocapture
//! ```
//!
//! Each row shows the committed baseline, the current measurement and the delta. A negative delta
//! on `returned` is an improvement: fewer tokens for the same request.
//!
//! ## Updating the baseline
//!
//! ```text
//! LUMEN_BENCH_BLESS=1 cargo test --release -p lumen-mcp --test optimizer_bench
//! ```
//!
//! Commit the regenerated JSON **with** the fix that changed it. That is what makes the git history
//! of one file the record of the optimizer's progress, fix by fix.

use lumen_mcp::handle_tools_call;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ── Frozen fixtures ──────────────────────────────────────────────────────────
//
// Generated rather than committed as files: the generator is visible in the diff, where a blob of
// fixture text would not be, and there is no way for an editor or a formatter to quietly change it.
// Every shape here corresponds to a case the tools behave differently on.

/// Few items, substantial bodies. The shape an outline is meant to win on.
fn few_big_items(items: usize, body: usize) -> String {
    let mut s = String::from("use std::collections::HashMap;\n\n");
    for i in 0..items {
        s.push_str(&format!(
            "/// Operation {i}.\npub fn operation_{i}(input: &str) -> String {{\n"
        ));
        for j in 0..body {
            s.push_str(&format!(
                "    let step_{j} = input.len().wrapping_add({j});\n"
            ));
        }
        s.push_str("    format!(\"{input}\")\n}\n\n");
    }
    s
}

/// Many declarations, almost no bodies. An outline of this costs about as much as the file, which
/// is what the inflation guard exists for.
fn many_tiny_items(n: usize) -> String {
    (0..n).map(|i| format!("pub fn f{i}() {{}}\n")).collect()
}

/// Below any useful threshold. Any reply carries a header the file cannot amortise.
fn tiny_file() -> String {
    "fn main() {\n    println!(\"hi\");\n}\n".to_string()
}

/// A language with no tree-sitter grammar here. `outline` returns one synthetic whole-file item,
/// which must not be reported as a saving.
fn unsupported_language(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("SET var_{i} TO {i}; -- prose that no grammar here parses\n"))
        .collect()
}

/// TypeScript with a realistic class-plus-methods shape.
fn typescript_service(methods: usize, body: usize) -> String {
    let mut s = String::from(
        "import { Injectable } from '@angular/core';\n\n@Injectable()\nexport class Service {\n",
    );
    for i in 0..methods {
        s.push_str(&format!("  method_{i}(arg: string): number {{\n"));
        for j in 0..body {
            s.push_str(&format!("    const v{j} = arg.length + {j};\n"));
        }
        s.push_str("    return 0;\n  }\n\n");
    }
    s.push_str("}\n");
    s
}

/// Python with module-level functions and a class.
fn python_module(funcs: usize, body: usize) -> String {
    let mut s = String::from("import os\nimport sys\n\n\n");
    for i in 0..funcs {
        s.push_str(&format!("def handler_{i}(payload):\n"));
        for j in 0..body {
            s.push_str(&format!("    local_{j} = len(payload) + {j}\n"));
        }
        s.push_str("    return payload\n\n\n");
    }
    s.push_str("class Coordinator:\n    def run(self):\n        return 1\n");
    s
}

/// A log dominated by one repeated line — the case `compress_logs` is for.
fn repetitive_log(unique: usize, repeats: usize) -> String {
    let mut s = String::new();
    for i in 0..unique {
        s.push_str(&format!(
            "2026-07-31T12:00:{:02}Z INFO starting phase {i}\n",
            i % 60
        ));
        for _ in 0..repeats {
            s.push_str("2026-07-31T12:00:00Z WARN retrying connection to 127.0.0.1:9999\n");
        }
    }
    s
}

/// A log with a deep stack trace, repeated.
fn stack_trace_log(traces: usize, depth: usize) -> String {
    let mut s = String::new();
    for t in 0..traces {
        s.push_str(&format!("thread 'main' panicked at src/lib.rs:{t}:\n"));
        for f in 0..depth {
            s.push_str(&format!("   {f}: lumen_core::module::function_{f}\n"));
            s.push_str(&format!(
                "             at ./crates/lumen-core/src/file_{f}.rs:{f}\n"
            ));
        }
    }
    s
}

/// A log with nothing to collapse. Compression must not make it bigger.
fn incompressible_log(lines: usize) -> String {
    (0..lines)
        .map(|i| {
            format!(
                "2026-07-31T12:00:00Z INFO unique event {i} value={}\n",
                i * 7919
            )
        })
        .collect()
}

// ── Scenarios ────────────────────────────────────────────────────────────────

struct Scenario {
    /// Stable key. Never rename without also renaming it in the baseline, or the row reads as a
    /// deletion plus an addition and its history is lost.
    id: &'static str,
    tool: &'static str,
    /// What this case is for, printed so the table explains itself.
    intent: &'static str,
    /// Fixture filename (extension drives language detection) and its content.
    fixture: Option<(&'static str, String)>,
    /// Arguments, given the fixture's forward-slash relative path.
    args: fn(&str) -> Value,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        // ── lumen_ping: no tokens, but the dispatch and reply shape are still worth pinning.
        Scenario {
            id: "ping/basic",
            tool: "lumen_ping",
            intent: "liveness; carries no token metrics",
            fixture: None,
            args: |_| json!({ "message": "bench" }),
        },
        // ── smart_read
        Scenario {
            id: "smart_read/outline_few_big_items",
            tool: "smart_read",
            intent: "the win case: few items, real bodies",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/outline_many_tiny_items",
            tool: "smart_read",
            intent: "outline can exceed the file; must fall back rather than claim a saving",
            fixture: Some(("decls.rs", many_tiny_items(120))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/outline_tiny_file",
            tool: "smart_read",
            intent: "too small to amortise any header",
            fixture: Some(("tiny.rs", tiny_file())),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/outline_unsupported_language",
            tool: "smart_read",
            intent: "no grammar: must return the file, not a one-item pseudo-outline",
            fixture: Some(("notes.xyz", unsupported_language(400))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/outline_typescript",
            tool: "smart_read",
            intent: "TS class with methods",
            fixture: Some(("service.ts", typescript_service(14, 20))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/outline_python",
            tool: "smart_read",
            intent: "Python functions plus a class",
            fixture: Some(("mod.py", python_module(14, 20))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "smart_read/full_mode",
            tool: "smart_read",
            intent: "delivery, not optimisation: expected to cost the file plus a header",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "mode": "full" }),
        },
        // ── recall_file
        Scenario {
            id: "recall_file/exact_one_name",
            tool: "recall_file",
            intent: "the intended use: one named item out of many",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "names": ["operation_6"] }),
        },
        Scenario {
            id: "recall_file/exact_three_names",
            tool: "recall_file",
            intent: "several named items, merged where they touch",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "names": ["operation_1", "operation_2", "operation_9"] }),
        },
        Scenario {
            id: "recall_file/substring_narrow",
            tool: "recall_file",
            intent: "inexact query, few matches: honoured but labelled",
            fixture: Some(("service.ts", typescript_service(14, 20))),
            args: |p| json!({ "path": p, "names": ["method_1"] }),
        },
        Scenario {
            id: "recall_file/substring_broad",
            tool: "recall_file",
            intent: "inexact query matching most of the file: must return the map, not the bodies",
            fixture: Some(("service.ts", typescript_service(14, 20))),
            args: |p| json!({ "path": p, "names": ["method"] }),
        },
        Scenario {
            id: "recall_file/no_match",
            tool: "recall_file",
            intent: "nothing matched: offer the outline so the caller can retry",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "names": ["nonexistent_symbol"] }),
        },
        Scenario {
            id: "recall_file/range_narrow",
            tool: "recall_file",
            intent: "an explicit small range out of a large file",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "start_line": 100, "end_line": 130 }),
        },
        Scenario {
            id: "recall_file/range_whole_file",
            tool: "recall_file",
            intent: "asking for everything must not cost more than reading everything",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p, "start_line": 1, "end_line": 999_999 }),
        },
        Scenario {
            id: "recall_file/no_selector",
            tool: "recall_file",
            intent: "no selector: outline, not the file plus a header",
            fixture: Some(("big.rs", few_big_items(12, 40))),
            args: |p| json!({ "path": p }),
        },
        // ── compress_logs
        Scenario {
            id: "compress_logs/repetitive_path",
            tool: "compress_logs",
            intent: "the win case: one line repeated",
            fixture: Some(("app.log", repetitive_log(20, 30))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "compress_logs/stack_traces",
            tool: "compress_logs",
            intent: "repeated deep stack frames",
            fixture: Some(("crash.log", stack_trace_log(12, 25))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "compress_logs/incompressible",
            tool: "compress_logs",
            intent: "nothing to collapse: must not inflate",
            fixture: Some(("unique.log", incompressible_log(400))),
            args: |p| json!({ "path": p }),
        },
        Scenario {
            id: "compress_logs/inline_text",
            tool: "compress_logs",
            intent: "inline text rather than a file, so there is no path to meter",
            fixture: None,
            args: |_| json!({ "text": repetitive_log(10, 20) }),
        },
    ]
}

// ── Measurement ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Measured {
    tool: String,
    /// Tokens the file would have cost to read whole. 0 where there is no file.
    full_tokens: i64,
    /// Tokens the reply cost.
    returned_tokens: i64,
    /// `full - returned`, signed: a loss must be representable.
    saved_tokens: i64,
    /// Which route the call was metered as. A change here is a behavioural change even when the
    /// token counts happen to match.
    route: String,
    /// Whether the call returned an error rather than content.
    is_error: bool,
}

/// Where fixtures live, as a **relative** path.
///
/// This is load-bearing, and a random tempdir was wrong twice over.
///
/// Every tool embeds the path it was given in its reply header, so the path's own token cost is part
/// of `returned_tokens`. With `TempDir::new()` the path contains a random component, and three
/// consecutive runs with no code change produced 351, 351 and 357 tokens for the same scenario — ±6
/// of pure noise, which is larger than most real improvements and would make a committed baseline
/// fail at random.
///
/// An absolute path is no better: `/Users/gshmunik/...` and `/home/runner/...` tokenize differently,
/// so a baseline blessed on a laptop could never match in CI.
///
/// A short relative path is identical on every machine. Cargo runs a test with the package root as
/// the working directory, so this resolves the same way everywhere, and the header contains exactly
/// these bytes.
const FIXTURE_DIR: &str = "target/bench-fixtures";

/// Create the fixture directory and return the path to `name` **with forward slashes**.
///
/// Returned as a `String` built with `/` rather than via `PathBuf::join`, because the separator ends
/// up in the reply header and therefore in `returned_tokens`. `join` yields
/// `target/bench-fixtures\\big.rs` on Windows, which tokenizes differently from the Unix form — the
/// Windows CI job failed by +1 to +3 tokens on nine scenarios for exactly that reason, with no
/// behavioural difference at all.
///
/// Windows accepts `/` in paths, so one string works everywhere and the header is identical on every
/// platform. This is the same defect as the random tempdir, one level down: anything that varies in
/// the path varies in the measurement.
fn fixture_rel(name: &str) -> String {
    std::fs::create_dir_all(FIXTURE_DIR).expect("create the fixture directory");
    format!("{FIXTURE_DIR}/{name}")
}

/// Run one scenario. Returns the measurement and the median wall-clock over a few passes.
fn measure(s: &Scenario) -> (Measured, u128) {
    let path = match &s.fixture {
        Some((name, body)) => {
            let p = fixture_rel(name);
            // Rewritten every time rather than only when absent: a stale file from an earlier
            // version of a generator would silently invalidate every number taken against it.
            std::fs::write(&p, body).expect("write fixture");
            p
        }
        None => fixture_rel("(none)"),
    };
    let args = (s.args)(&path);
    let params = json!({ "name": s.tool, "arguments": args });

    // Median of five: the token numbers are deterministic, but wall-clock is not, and a median is
    // less misleading than a single sample or a mean with an outlier in it.
    let mut times = Vec::new();
    let mut last = None;
    for _ in 0..5 {
        let t = Instant::now();
        let out = handle_tools_call(Some(params.clone()));
        times.push(t.elapsed().as_micros());
        last = Some(out);
    }
    times.sort_unstable();
    let out = last.expect("at least one pass");

    let meta = out
        .result()
        .and_then(|v| v.get("_meta").cloned())
        .unwrap_or_else(|| json!({}));
    let num = |k: &str| meta.get(k).and_then(Value::as_i64).unwrap_or(0);

    (
        Measured {
            tool: s.tool.to_string(),
            full_tokens: num("full_tokens"),
            returned_tokens: num("returned_tokens"),
            saved_tokens: num("saved_tokens"),
            route: out
                .meter
                .as_ref()
                .map(|m| m.routed_via.clone())
                .unwrap_or_else(|| "(unmetered)".to_string()),
            is_error: out.error_code().is_some(),
        },
        times[times.len() / 2],
    )
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/optimizer_baseline.json")
}

/// The committed baseline: measurements, plus the hash of every input they were taken against.
///
/// Both halves matter. Measurements alone would let an edited fixture masquerade as an optimizer
/// improvement — a smaller number for a smaller input.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Baseline {
    #[serde(default)]
    fixtures: BTreeMap<String, String>,
    #[serde(default)]
    scenarios: BTreeMap<String, Measured>,
}

fn load_full_baseline() -> Baseline {
    match std::fs::read_to_string(baseline_path()) {
        Ok(s) => serde_json::from_str(&s).expect("the baseline must be valid JSON"),
        Err(_) => Baseline::default(),
    }
}

fn load_baseline() -> BTreeMap<String, Measured> {
    load_full_baseline().scenarios
}

fn load_fixture_hashes() -> BTreeMap<String, String> {
    load_full_baseline().fixtures
}

fn pct(from: i64, to: i64) -> String {
    if from == 0 {
        return if to == 0 {
            "  0.0%".into()
        } else {
            "   new".into()
        };
    }
    format!(
        "{:+6.1}%",
        100.0 * (to - from) as f64 / from.unsigned_abs() as f64
    )
}

/// The benchmark: measure every scenario, compare against the committed baseline, and report.
///
/// Fails on a token regression. Never fails on wall-clock — a shared CI runner cannot support that
/// claim, and a flaky perf gate teaches people to re-run until it passes.
#[test]
fn every_mcp_tool_is_measured_against_the_committed_baseline() {
    let base = load_baseline();
    let bless = std::env::var("LUMEN_BENCH_BLESS").as_deref() == Ok("1");

    let mut current: BTreeMap<String, Measured> = BTreeMap::new();
    let mut timings: BTreeMap<String, u128> = BTreeMap::new();
    let scen = scenarios();
    for s in &scen {
        let (m, t) = measure(s);
        current.insert(s.id.to_string(), m);
        timings.insert(s.id.to_string(), t);
    }

    println!("\n== MCP optimizer benchmarks =====================================");
    println!("   frozen fixtures, so a delta means the code changed\n");
    println!(
        "  {:<38} {:>8} {:>8} {:>8} {:>7} {:>7}",
        "scenario", "full", "returned", "saved", "vs base", "µs"
    );
    println!("  {}", "-".repeat(80));

    let mut regressions: Vec<String> = Vec::new();
    let mut improvements: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    for s in &scen {
        let m = &current[s.id];
        let t = timings[s.id];
        match base.get(s.id) {
            None => {
                added.push(s.id.to_string());
                println!(
                    "  {:<38} {:>8} {:>8} {:>8} {:>7} {:>7}",
                    s.id, m.full_tokens, m.returned_tokens, m.saved_tokens, "NEW", t
                );
            }
            Some(b) => {
                let delta = m.returned_tokens - b.returned_tokens;
                println!(
                    "  {:<38} {:>8} {:>8} {:>8} {:>7} {:>7}",
                    s.id,
                    m.full_tokens,
                    m.returned_tokens,
                    m.saved_tokens,
                    pct(b.returned_tokens, m.returned_tokens),
                    t
                );
                if delta > 0 {
                    regressions.push(format!(
                        "{}: returned {} -> {} (+{delta})",
                        s.id, b.returned_tokens, m.returned_tokens
                    ));
                } else if delta < 0 {
                    improvements.push(format!(
                        "{}: returned {} -> {} ({delta})",
                        s.id, b.returned_tokens, m.returned_tokens
                    ));
                }
                if b.route != m.route {
                    regressions.push(format!(
                        "{}: route changed {} -> {} (behavioural, even if the tokens match)",
                        s.id, b.route, m.route
                    ));
                }
            }
        }
        println!("  {:<38} {}", "", s.intent);
    }

    // Totals over the scenarios that actually have a file behind them, so ping and inline text do
    // not drag the ratio around.
    let (f, r): (i64, i64) = current
        .values()
        .filter(|m| m.full_tokens > 0)
        .fold((0, 0), |(a, b), m| {
            (a + m.full_tokens, b + m.returned_tokens)
        });
    println!(
        "\n  across scenarios with a file: {r} returned vs {f} full = {:.1}%",
        100.0 * r as f64 / f.max(1) as f64
    );

    if !improvements.is_empty() {
        println!("\n  IMPROVED since the baseline:");
        for i in &improvements {
            println!("    {i}");
        }
    }
    if !added.is_empty() {
        println!("\n  NEW scenarios (no baseline yet): {}", added.join(", "));
    }

    if bless {
        let out = Baseline {
            fixtures: fixture_hashes(),
            scenarios: current.clone(),
        };
        std::fs::write(
            baseline_path(),
            format!(
                "{}\n",
                serde_json::to_string_pretty(&out).expect("serialise")
            ),
        )
        .expect("write baseline");
        println!("\n  baseline rewritten: {}", baseline_path().display());
        println!("  commit it WITH the change that moved it.");
        return;
    }

    // Missing baseline entries are a hard failure, not a warning: a scenario added without a
    // baseline has no history, which is the one thing this file exists to provide.
    assert!(
        added.is_empty() || base.is_empty(),
        "scenarios have no baseline: {}. Run with LUMEN_BENCH_BLESS=1 and commit the result.",
        added.join(", ")
    );
    assert!(
        regressions.is_empty(),
        "the optimizer got worse:\n  {}\n\nIf this is intended, re-bless the baseline in the same \
         commit so the change is visible in its history.",
        regressions.join("\n  ")
    );
    if base.is_empty() {
        println!("\n  no baseline committed yet — run with LUMEN_BENCH_BLESS=1");
    }
}

/// Every tool the server advertises must be benchmarked.
///
/// Read from `tools/list` rather than a hand-kept list, so a fifth tool fails this suite until
/// someone measures it. "Full coverage" is only a claim if something checks it.
#[test]
fn every_advertised_tool_is_benchmarked() {
    let advertised: Vec<String> = lumen_mcp::handle_tools_list()["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .map(|t| t["name"].as_str().expect("a tool name").to_string())
        .collect();
    assert!(!advertised.is_empty(), "the server advertises no tools");

    let benched: std::collections::BTreeSet<&str> = scenarios().iter().map(|s| s.tool).collect();
    let missing: Vec<&String> = advertised
        .iter()
        .filter(|t| !benched.contains(t.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these advertised tools have no benchmark scenario: {missing:?}"
    );

    // And the reverse: a scenario naming a tool that no longer exists would measure the not-found
    // path forever while appearing to pass.
    let unknown: Vec<&str> = benched
        .iter()
        .filter(|t| !advertised.iter().any(|a| a == *t))
        .copied()
        .collect();
    assert!(
        unknown.is_empty(),
        "these scenarios name tools the server does not advertise: {unknown:?}"
    );
}

/// No scenario may cost more than reading the file whole.
///
/// Separate from the baseline comparison on purpose: the baseline says "no worse than last time",
/// which would happily preserve a bad number forever. This says "never worse than not using the
/// tool at all", which is the claim the product makes.
#[test]
fn no_scenario_costs_more_than_reading_the_file() {
    // The one explanatory line a fallback adds, plus room for the path in a header.
    const ALLOWANCE: i64 = lumen_mcp::NOTE_ALLOWANCE;
    let mut worst: Option<(String, i64)> = None;

    for s in &scenarios() {
        let (m, _) = measure(s);
        // Scenarios with no file behind them have nothing to compare against.
        if m.full_tokens == 0 {
            continue;
        }
        let over = m.returned_tokens - m.full_tokens;
        if over > 0 && worst.as_ref().map(|w| over > w.1).unwrap_or(true) {
            worst = Some((s.id.to_string(), over));
        }
        assert!(
            m.returned_tokens <= m.full_tokens + ALLOWANCE,
            "{} returned {} tokens against {} for the file (over by {over}, allowance {ALLOWANCE})",
            s.id,
            m.returned_tokens,
            m.full_tokens
        );
    }

    match worst {
        Some((id, over)) => {
            println!("\n  worst overage: {over} tokens on {id} (allowance {ALLOWANCE})")
        }
        None => println!("\n  no scenario exceeded the file"),
    }
}

/// The fixtures must be byte-identical everywhere, or the baseline is meaningless.
///
/// This started as "no fixture may contain the current year", which fired immediately — the log
/// fixtures embed `2026-07-31T12:00:00Z`. Those are frozen literals and perfectly deterministic, so
/// the check was a false positive: it proxied "does this look like a timestamp" for "does this
/// change between runs".
///
/// The guarantee that actually matters is stronger and checkable: **a committed baseline is only
/// comparable if the inputs are unchanged.** So each fixture's content is hashed and the hash is
/// stored alongside the measurements. If a generator is edited, this fails and says the deltas are
/// no longer comparable — which is exactly the mistake that would otherwise present a changed
/// input as an optimizer improvement.
#[test]
fn the_fixtures_match_the_hashes_the_baseline_was_taken_against() {
    // Pure functions of their arguments: same bytes however many times they are called.
    for _ in 0..3 {
        assert_eq!(few_big_items(12, 40), few_big_items(12, 40));
        assert_eq!(typescript_service(14, 20), typescript_service(14, 20));
        assert_eq!(repetitive_log(20, 30), repetitive_log(20, 30));
    }

    let current = fixture_hashes();
    let bless = std::env::var("LUMEN_BENCH_BLESS").as_deref() == Ok("1");
    let recorded = load_fixture_hashes();

    if bless || recorded.is_empty() {
        // Written by the baseline test, which owns the file. Nothing to compare against yet.
        println!("\n  fixture hashes ({} fixtures):", current.len());
        for (k, v) in &current {
            println!("    {k:<24} {v}");
        }
        return;
    }

    let mut changed: Vec<String> = Vec::new();
    for (name, hash) in &current {
        match recorded.get(name) {
            Some(old) if old == hash => {}
            Some(old) => changed.push(format!("{name}: {old} -> {hash}")),
            None => changed.push(format!("{name}: new fixture ({hash})")),
        }
    }
    for name in recorded.keys() {
        if !current.contains_key(name) {
            changed.push(format!("{name}: removed"));
        }
    }
    assert!(
        changed.is_empty(),
        "the benchmark inputs changed, so the committed deltas are not comparable:\n  {}\n\n\
         Re-bless with LUMEN_BENCH_BLESS=1 and say in the commit message that the inputs moved, \
         so nobody reads the new numbers as an optimizer improvement.",
        changed.join("\n  ")
    );
}

/// FNV-1a over the fixture bytes.
///
/// Hand-rolled rather than `DefaultHasher`: that is SipHash with unspecified stability across Rust
/// releases, and a hash that changes with the toolchain would fail this test for a reason that has
/// nothing to do with the fixtures. Not a cryptographic requirement — only "did these bytes change".
fn fnv1a(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Every fixture in the scenario table, hashed by filename.
fn fixture_hashes() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for s in scenarios() {
        if let Some((name, body)) = &s.fixture {
            out.insert(name.to_string(), fnv1a(body.as_bytes()));
        }
    }
    // Inline-text scenarios have no file, but their input is just as much an input.
    out.insert(
        "(inline)compress_logs".to_string(),
        fnv1a(repetitive_log(10, 20).as_bytes()),
    );
    out
}
