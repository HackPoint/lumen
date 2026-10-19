//! No savings figure rests on a fixed number of rounds.
//!
//! Until 1.9.0 the optimizer's net value was priced with a fixed 194 rounds remaining and a fixed
//! 1.604 rounds per call (lumen-stats, and the app through it), and the benchmark harness priced
//! its "$/call" with the hook's fixed 65 rounds. Each was the author's own sessions multiplied out
//! and shown to everyone. `lumen_core::savings` now derives R and the round cost for each call
//! from the turns that followed it.
//!
//! This fails if the old figures come back anywhere in the workspace's code, and if anything but
//! the ranked arm's sizing reaches for `econ`'s fixed inputs: that sizing keeps them (see
//! `econ.rs` for the measurement that decided it), and no figure a person reads may use them.
//! A text scan of Rust and TypeScript sources, comments included: a comment that quotes 1.604 as
//! a fact is how the number comes back.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Every Rust and TypeScript source under `dir`, skipping build output and dependencies.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !matches!(
                name,
                "target" | "node_modules" | "dist" | ".angular" | "gen"
            ) {
                sources(&p, out);
            }
        } else if name.ends_with(".rs") || name.ends_with(".ts") {
            out.push(p);
        }
    }
}

fn workspace_sources() -> Vec<(String, String)> {
    let root = root();
    let mut files = Vec::new();
    for dir in [
        "crates",
        "lumenator/src",
        "lumenator/src-tauri/src",
        "benchmarks/harness",
    ] {
        sources(&root.join(dir), &mut files);
    }
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

const THIS_FILE: &str = "crates/lumen-core/tests/retired_constants.rs";

#[test]
fn the_fixed_rounds_of_the_old_net_value_are_gone_from_every_source() {
    // Built so this file does not match itself.
    let retired: Vec<String> = [
        ["VALUE", "_ROUNDS"].concat(),
        ["PAIR", "_MULTIPLIER"].concat(),
        ["value", "_rounds"].concat(),
        ["pair", "_multiplier"].concat(),
        ["value", "Rounds"].concat(),
        ["pair", "Multiplier"].concat(),
        ["net_value", "_usd"].concat(),
        ["net", "ValueUsd"].concat(),
        ["round_cost", "_usd"].concat(),
        ["round", "CostUsd"].concat(),
        ["1", ".604"].concat(),
        ["194", ".0"].concat(),
        ["194 ", "rounds"].concat(),
    ]
    .into_iter()
    .collect();
    let mut hits = Vec::new();
    for (rel, text) in workspace_sources() {
        if rel == THIS_FILE {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            for r in &retired {
                if line.contains(r.as_str()) {
                    hits.push(format!("{rel}:{}: {r}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the old fixed-round net value is back:\n{}",
        hits.join("\n")
    );
}

/// The files that may use `econ`'s fixed sizing inputs: econ itself, the ranked arm that sizes an
/// outline with them, the MCP server that records the inputs on each ranked row, and the version
/// benchmark's scoring, which uses the constants only to size its MODELLED reference session
/// (the prompt and output a round costs), never for a savings figure: every figure still comes
/// from `savings::summarize`.
const SIZING: &[&str] = &[
    "crates/lumen-core/src/econ.rs",
    "crates/lumen-core/src/ranked.rs",
    "crates/lumen-mcp/src/lib.rs",
    "benchmarks/harness/src/score.rs",
    "benchmarks/harness/tests/score.rs",
];

#[test]
fn only_the_ranked_arms_sizing_reaches_for_econs_fixed_inputs() {
    let fixed = [
        "DEFAULT_ROUNDS_REMAINING",
        "DEFAULT_CONTEXT_TOKENS",
        "DEFAULT_OUTPUT_TOKENS",
        "CACHE_READ_PER_M",
        "CACHE_WRITE_PER_M",
        "OUTPUT_PER_M",
        "INPUT_PER_M",
        "value_per_token",
        "round_cost(",
        "econ::",
        "Econ::",
    ];
    let mut hits = Vec::new();
    for (rel, text) in workspace_sources() {
        if rel == THIS_FILE || SIZING.contains(&rel.as_str()) {
            continue;
        }
        // The ranked arm's own tests exercise its sizing, and a test of the sizing is not a figure.
        let sizing_test = matches!(
            rel.as_str(),
            "crates/lumen-core/tests/ranked_coverage.rs"
                | "crates/lumen-core/tests/ranked_timing.rs"
                | "crates/lumen-core/tests/phase_a_effect.rs"
        ) || (rel.starts_with("crates/lumen-mcp/tests/")
            && text.contains("ranked"));
        if sizing_test {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            for f in fixed {
                if line.contains(f) {
                    hits.push(format!("{rel}:{}: {f}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "a figure outside the ranked arm's sizing uses econ's fixed inputs:\n{}",
        hits.join("\n")
    );
}

/// `number` in `line` as a number of its own: `362_965.0` does not hold `65.0`.
fn is_number_in(line: &str, number: &str) -> bool {
    line.match_indices(number).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + number.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit() || c == '_' || c == '.')
            && !after.is_some_and(|c| c.is_ascii_digit() || c == '_')
    })
}

/// The 65 rounds the ranked sizing assumes stays with the ranked sizing: the constant is read in
/// `econ.rs` alone, and the number appears nowhere else but in fixtures of the column a ranked
/// row records it in.
#[test]
fn sixty_five_rounds_is_the_ranked_sizings_alone() {
    let name = ["DEFAULT_ROUNDS", "_REMAINING"].concat();
    let number = ["65", ".0"].concat();
    // Where `65.0` may appear, and why.
    const FIXTURES: &[(&str, &str)] = &[(
        "crates/lumen-core/src/schema.rs",
        "a ranked row's recorded econ_rounds, in a fixture of the read_events columns",
    )];
    let mut hits = Vec::new();
    for (rel, text) in workspace_sources() {
        if rel == THIS_FILE || rel == "crates/lumen-core/src/econ.rs" {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            let fixture = FIXTURES.iter().any(|(f, _)| *f == rel);
            if line.contains(name.as_str()) || (is_number_in(line, &number) && !fixture) {
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "the ranked sizing's 65 rounds is read outside it:\n{}",
        hits.join("\n")
    );
    // And econ.rs itself says why it keeps it.
    let econ = std::fs::read_to_string(root().join("crates/lumen-core/src/econ.rs")).unwrap();
    assert!(econ.contains("Kept for the ranked arm's sizing only"));
    assert!(econ.contains("off by default"));
}

/// The scan reads what it says: the savings engine and every surface that shows a saving.
#[test]
fn the_scan_covers_every_surface_that_shows_a_saving() {
    let rels: Vec<String> = workspace_sources().into_iter().map(|(r, _)| r).collect();
    for want in [
        "crates/lumen-core/src/savings/mod.rs",
        "crates/lumen-stats/src/lib.rs",
        "crates/lumen-cli/src/savings.rs",
        "lumenator/src-tauri/src/lib.rs",
        "lumenator/src/app/session.service.ts",
        "benchmarks/harness/src/report.rs",
    ] {
        assert!(rels.iter().any(|r| r == want), "{want} is not scanned");
    }
}
