//! What Lumen actually saves, measured on a real corpus.
//!
//! `cargo test --release -p lumen-mcp --test efficiency -- --nocapture` prints the tables;
//! the assertions run either way. It lives in lumen-mcp because `format_outline` does —
//! measuring anything else would measure a reimplementation of the product.
//!
//! The corpus is this repository's own source: every Rust, Python and TypeScript file
//! tree-sitter can parse, at or above the interception threshold. Real files, the real
//! tokenizer, no hand-picked examples — a benchmark built from favourable inputs measures
//! the person who picked them.
//!
//! Three rules, because the product's changelog commits to them and a benchmark that
//! flatters is worth less than none:
//!
//!   1. **Counted, not estimated.** Every figure comes from the same tiktoken path the
//!      product bills with. A bytes/4 approximation would make the claim unfalsifiable.
//!   2. **Gross is not net.** An intercepted read is a *blocked* read: the model spends an
//!      extra round instead. A ratio cannot come out negative, so a ratio alone always
//!      flatters. `Econ` prices the round back in.
//!   3. **The unflattering number renders.** Interception does not pay on every file. Those
//!      files are named here rather than dropped, so a change that makes it worse fails.

use lumen_core::coverage;
use lumen_core::econ::Econ;
use lumen_core::ranked::TagLang;
use lumen_core::structure::{CodeItem, detect_lang, outline};
use lumen_core::tokenizer::count_tokens;
use lumen_mcp::{NOTE_ALLOWANCE, Outcome, Payload, format_outline};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Below this, a read is not intercepted, so its cost says nothing about what Lumen saves.
const THRESHOLD_LINES: usize = 300;

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    walk_where(dir, out, &|p| {
        TagLang::detect(&p.to_string_lossy()).is_some()
    });
}

fn walk_where(dir: &Path, out: &mut Vec<PathBuf>, keep: &dyn Fn(&Path) -> bool) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // Generated trees would measure our dependencies' formatting, not this codebase.
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules" | "dist") {
            continue;
        }
        if p.is_dir() {
            walk_where(&p, out, keep);
        } else if keep(&p) {
            out.push(p);
        }
    }
}

struct Measured {
    name: String,
    lines: usize,
    full: usize,
    /// What `smart_read` returns.
    outline: usize,
    /// What one follow-up `recall_file` returns: the median item plus 3 lines either side.
    /// Median rather than smallest, which would flatter it.
    recall: usize,
}

impl Measured {
    /// Tokens a smart_read + one recall avoids versus reading the file whole.
    fn avoided(&self) -> usize {
        self.full.saturating_sub(self.outline + self.recall)
    }
}

fn measure() -> Vec<Measured> {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();

    let mut out = Vec::new();
    for path in files {
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines = src.lines().count();
        if lines < THRESHOLD_LINES {
            continue;
        }
        let items = outline(&src, detect_lang(&path.to_string_lossy()));
        // `is_empty` is not enough. `outline` does not fail on a language it cannot parse — it
        // returns one synthetic whole-file item — so a file it never looked inside would enter
        // the corpus with a ~40-token "outline" and flatter every published ratio.
        if items.is_empty() || items.iter().all(|i| i.kind == "file") {
            continue;
        }
        let full = count_tokens(&src);
        let rendered = format_outline(&path.to_string_lossy(), lines, full, &items);

        let mid = &items[items.len() / 2];
        let span = mid.end_line.saturating_sub(mid.start_line) + 1;
        let body: String = src
            .lines()
            .skip(mid.start_line.saturating_sub(1))
            .take(span + 6)
            .collect::<Vec<_>>()
            .join("\n");

        out.push(Measured {
            name: path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned(),
            lines,
            full,
            outline: count_tokens(&rendered),
            recall: count_tokens(&body),
        });
    }
    out
}

#[test]
fn an_outline_costs_a_fraction_of_the_file_it_describes() {
    let files = measure();
    assert!(
        files.len() >= 20,
        "corpus too small to conclude anything: {}",
        files.len()
    );

    let full: usize = files.iter().map(|f| f.full).sum();
    let outlines: usize = files.iter().map(|f| f.outline).sum();
    let pairs: usize = files.iter().map(|f| f.outline + f.recall).sum();

    let mut ratios: Vec<f64> = files
        .iter()
        .map(|f| f.outline as f64 / f.full as f64)
        .collect();
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = ratios[ratios.len() / 2];
    let worst = *ratios.last().unwrap();

    println!("\n== Outline cost vs a full read ==================================");
    println!(
        "  corpus: {} files at or above {THRESHOLD_LINES} lines",
        files.len()
    );
    println!("  read in full:              {full:>9} tokens");
    println!(
        "  smart_read only:           {outlines:>9} tokens   {:>5.1}% of full",
        100.0 * outlines as f64 / full as f64
    );
    println!(
        "  smart_read + one recall:   {pairs:>9} tokens   {:>5.1}% of full",
        100.0 * pairs as f64 / full as f64
    );
    println!(
        "  per-file outline: median {:.1}%, worst {:.1}%",
        median * 100.0,
        worst * 100.0
    );

    println!(
        "\n  {:<46} {:>6} {:>8} {:>8} {:>7}",
        "largest files", "lines", "full", "outline", "ratio"
    );
    println!("  {}", "-".repeat(80));
    let mut by_size: Vec<&Measured> = files.iter().collect();
    by_size.sort_by_key(|f| std::cmp::Reverse(f.full));
    for f in by_size.iter().take(10) {
        let n = if f.name.len() > 46 {
            &f.name[f.name.len() - 46..]
        } else {
            &f.name
        };
        println!(
            "  {n:<46} {:>6} {:>8} {:>8} {:>6.1}%",
            f.lines,
            f.full,
            f.outline,
            100.0 * f.outline as f64 / f.full as f64
        );
    }

    // No figure is claimed for this any more, to the model or in the README: the copy that
    // said "5-10%" is gone. The ceiling stays. Outlines costing a fifth of the files they
    // describe would make interception a poor trade on this corpus, and that should fail
    // here before a release ships it.
    let aggregate = outlines as f64 / full as f64;
    assert!(
        aggregate < 0.20,
        "outlines cost {:.1}% of a full read, over the 20% ceiling",
        aggregate * 100.0
    );
    // Even the worst file must beat reading it whole, or interception made that read worse.
    assert!(
        worst < 1.0,
        "a file's outline costs more than reading it: {:.1}%",
        worst * 100.0
    );
}

/// Interception blocks a read, so the model spends another round. A percentage cannot
/// express that, which is why the product's headline is dollars.
#[test]
fn the_net_value_prices_in_the_round_interception_costs() {
    let files = measure();
    let econ = Econ::default();
    let per_read = econ.round_cost();
    let per_token = econ.value_per_token();
    let break_even = econ
        .s_min()
        .expect("a positive value per token yields a break-even");

    let avoided: usize = files.iter().map(|f| f.avoided()).sum();
    let gross = avoided as f64 * per_token;
    let cost = files.len() as f64 * per_read;
    let net = gross - cost;

    println!("\n== Net value, with the extra round priced in ====================");
    println!("  tokens avoided:          {avoided:>10}");
    println!("  value per saved token:   ${per_token:>12.9}");
    println!("  gross value:             ${gross:>10.2}");
    println!("  cost per intercept:      ${per_read:>10.4}");
    println!("  intercepts:              {:>10}", files.len());
    println!("  cost of those rounds:    ${cost:>10.2}");
    println!("  NET:                     ${net:>10.2}");
    println!("  break-even per read:     {break_even:>10.0} tokens avoided");
    if net <= 0.0 {
        println!("  ^ negative, and printed anyway: whatever the number is, it renders.");
    }

    // Deliberately NOT asserted positive. Pretending it must be is the exact failure the
    // 1.4.0 headline change was made to correct. What is asserted: the arithmetic is
    // consistent, and the round is actually subtracted — a regression that silently drops
    // the cost term fails here.
    assert!(gross > 0.0, "a corpus this size must avoid some tokens");
    assert!(
        cost > 0.0,
        "interception must be priced, not treated as free"
    );
    assert!(
        (net - (gross - cost)).abs() < 1e-9,
        "net must be gross minus cost, nothing omitted"
    );
    assert!(
        break_even > 0.0,
        "a break-even of zero would authorise any outline"
    );
}

/// Where interception does not pay, and by how much. Named rather than dropped.
#[test]
fn the_files_where_interception_does_not_pay_are_named() {
    let files = measure();
    let econ = Econ::default();
    let break_even = econ.s_min().expect("break-even");

    let mut losers: Vec<&Measured> = files
        .iter()
        .filter(|f| (f.avoided() as f64) < break_even)
        .collect();
    losers.sort_by_key(|f| f.avoided());

    println!("\n== Files an intercept costs more than it saves ==================");
    println!("  break-even is {break_even:.0} avoided tokens per read");
    println!("  {} of {} files fall short", losers.len(), files.len());
    for f in losers.iter().take(8) {
        println!(
            "  {:>7} avoided, {:>5} lines   {}",
            f.avoided(),
            f.lines,
            f.name
        );
    }
    if losers.is_empty() {
        println!("  none — every file in this corpus clears it");
    }

    // If nothing fell short the model would be wrong rather than the tool perfect: a file
    // one line over the threshold cannot avoid much.
    assert!(
        losers.len() < files.len(),
        "every file falls short — the threshold is wrong"
    );
}

/// The recorded ledger, priced — and how much of the headline rests on a real count.
///
/// The corpus tests above measure what interception *would* save on this repository. This
/// one prices what it *did* save on this machine, through the same `Econ` the UI headline
/// uses, so the report and the product cannot disagree.
///
/// The provenance half matters more than the dollars. `full_tokens` is the denominator of
/// every claim here, and it comes either from the tokenizer or from a bytes/4 guess.
/// `token_source` records which. A headline built mostly on guesses is not a measurement,
/// and until this test existed nothing distinguished the two.
#[test]
fn the_recorded_savings_are_priced_and_their_provenance_is_known() {
    let Some(db) = lumen_core::meter::db_path() else {
        return;
    };
    // Read-only: this is the installed app's ledger, and its schema is not this build's to
    // migrate.
    let Ok(conn) = lumen_core::meter::open_read_only(&db) else {
        return;
    };

    let Ok((opt_calls, saved)) = conn.query_row(
        "SELECT count(*), COALESCE(sum(saved_tokens), 0) FROM read_events \
         WHERE routed_via <> 'builtin_read'",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    ) else {
        return;
    };
    if opt_calls == 0 {
        return;
    }

    let econ = Econ::default();
    let per_token = econ.value_per_token();
    let per_read = econ.round_cost();
    let gross = saved as f64 * per_token;
    let cost = opt_calls as f64 * per_read;

    println!("\n== The recorded ledger, priced ==================================");
    println!("  intercepted reads:       {opt_calls:>10}");
    println!("  tokens saved:            {saved:>10}");
    println!("  gross value:             ${gross:>10.2}");
    println!("  cost of those rounds:    ${cost:>10.2}");
    println!("  NET:                     ${:>10.2}", gross - cost);

    // Provenance. Measured means lumen-tok counted the file; anything else is a guess or a
    // language the tokenizer would not take.
    let measured_saved: i64 = conn
        .query_row(
            "SELECT COALESCE(sum(saved_tokens), 0) FROM read_events \
             WHERE routed_via <> 'builtin_read' AND token_source = 'measured'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let share = 100.0 * measured_saved as f64 / saved as f64;
    println!("\n  of that saving, {share:.1}% rests on a tokenizer count rather than bytes/4");

    // The threshold is deliberately high. Below it the headline is an estimate wearing a
    // measurement's clothes, and the right response is to fix metering, not to soften the
    // wording. A regression that stops recording token_source fails here.
    assert!(
        share > 80.0,
        "only {share:.1}% of the reported saving comes from a measured count; \
         the headline is mostly estimate"
    );

    // Priced consistently with the corpus tests: the round is subtracted, not assumed free.
    assert!(per_token > 0.0, "a zero value per token prices nothing");
    assert!(
        cost > 0.0,
        "intercepted reads must be charged for the round they cost"
    );
}

/// The invariants a ledger must satisfy, asserted on whatever connection it is handed.
///
/// Separate from where the ledger comes from. The test used to open this machine's own
/// ledger, so its verdict depended on how the maintainer had used Claude Code: 565 of 2,547
/// eligible reads bypassed the optimizer there, through the fail-open guards, and a clean
/// `main` failed. A test that is red on a clean checkout teaches everyone to ignore red. The
/// invariants are properties of the rules, so they are checked on a ledger built in this file
/// (`fixture`), and a real ledger is audited only on request.
///
/// Panics on the first violated invariant. An empty ledger is itself a failure: a test that
/// passes on nothing is vacuous.
fn assert_ledger_agrees(conn: &rusqlite::Connection) {
    let mut stmt = conn
        .prepare(
            "SELECT routed_via, count(*), COALESCE(sum(full_tokens),0), \
             COALESCE(sum(tokens_returned),0), COALESCE(sum(saved_tokens),0) \
             FROM read_events GROUP BY routed_via ORDER BY 5 DESC",
        )
        .expect("the ledger has the read_events columns the invariants read");
    let rows: Vec<(String, i64, i64, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .expect("query")
        .flatten()
        .collect();
    assert!(
        !rows.is_empty(),
        "an empty ledger agrees with itself vacuously; refusing to pass on nothing"
    );

    println!("\n== The ledger under test ========================================");
    println!(
        "  {:<14} {:>7} {:>12} {:>12} {:>12} {:>7}",
        "routed_via", "calls", "full", "returned", "saved", "pct"
    );
    for (route, calls, full, returned, saved) in &rows {
        let pct = if *full > 0 {
            100.0 * *saved as f64 / *full as f64
        } else {
            0.0
        };
        println!("  {route:<14} {calls:>7} {full:>12} {returned:>12} {saved:>12} {pct:>6.1}%");
        // A row claiming a saving larger than the file is a metering bug, and it would
        // inflate every headline built on this table.
        assert!(
            *saved <= *full,
            "{route} claims {saved} saved of {full} total — impossible"
        );
    }

    // Per row, not per sum. `saved` is clamped at zero, so a call that returned MORE than
    // the file contributes 0 to the total while contributing a negative to
    // `sum(full) - sum(returned)`. Comparing the two sums therefore fails on correct data —
    // which is exactly what the first version of this test did, and it read like a metering
    // bug rather than an arithmetic mistake in the test.
    // Two eras, and the invariant differs between them. Until 737eb71 (2026-07-29) the writer
    // clamped a loss to zero; since then it records the signed difference, so a call that cost
    // more than the file finally shows as negative. Asserting `max(0, …)` across the whole table
    // fails on correct new data, and asserting signed equality fails on correct old data.
    //
    // The last clamped row is 2026-07-28 and the first signed row 2026-07-31, so the boundary is
    // real rather than chosen.
    const SIGNED_SINCE: &str = "2026-07-29";
    let violations: i64 = conn
        .query_row(
            "SELECT count(*) FROM read_events \
             WHERE (ts >= ?1 AND saved_tokens <> full_tokens - tokens_returned) \
                OR (ts <  ?1 AND saved_tokens <> MAX(0, full_tokens - tokens_returned))",
            [SIGNED_SINCE],
            |r| r.get(0),
        )
        .expect("the era query runs against the ledger schema");
    assert_eq!(
        violations, 0,
        "{violations} rows disagree with the saved_tokens rule for their era"
    );

    // What the clamp conceals: calls whose reply was larger than reading the file whole.
    // They cost more than they saved, and `saved_tokens = 0` cannot express that, so the
    // headline can only ever be flattered by them. Reported as a share of the headline so
    // the distortion is quantified rather than asserted away.
    let (loss_calls, lost): (i64, i64) = conn
        .query_row(
            "SELECT count(*), COALESCE(sum(tokens_returned - full_tokens), 0) \
             FROM read_events WHERE tokens_returned > full_tokens",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("the loss query runs against the ledger schema");
    let headline: i64 = rows
        .iter()
        .filter(|r| r.0 != "builtin_read")
        .map(|r| r.4)
        .sum();
    if headline > 0 {
        let pct = 100.0 * lost as f64 / headline as f64;
        println!(
            "\n  {loss_calls} calls returned MORE than the file: {lost} tokens spent above a plain read"
        );
        println!(
            "  the headline cannot show these — saved_tokens clamps at zero — and they are {pct:.2}% of it"
        );
        // Small today. If a change makes these material the headline becomes misleading,
        // and that should fail here rather than be discovered in a release note.
        assert!(
            pct < 5.0,
            "calls that cost more than they saved are {pct:.1}% of the reported saving; \
             the headline no longer describes what happened"
        );
    }

    // ── Leak, versus scope ───────────────────────────────────────────────────────────────
    //
    // What this replaced: `builtin_read` rows divided by all rows, published as
    // "64.9% of reads never reached a Lumen tool". Every builtin row was in the denominator
    // whether or not interception was ever going to fire, so the number measured the file mix of
    // one machine — mostly how many screenshots its author had looked at — and read as a routing
    // failure. On the honest denominator the leak rate is zero.
    //
    // A leak is the only bucket that can indict the router, and the only one asserted.
    let mut rows = conn
        .prepare("SELECT routed_via, path, lines, full_tokens, ts FROM read_events")
        .expect("prepare");
    let all: Vec<(String, String, Option<i64>, i64, String)> = rows
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .expect("query")
        .flatten()
        .collect();

    // Both ~/.claude/settings.json and the repo's .claude/settings.json register a meter on
    // Read, and on a development machine both fire — 13 builtin reads already carry two rows at
    // the same (path, ts) under different writer_hook values, and only 239 of 2,778 rows carry a
    // writer_hook at all, so the true inflation is unknown and at least 13. Deduplicated on the
    // identity of the read rather than on the row.
    // Keyed on (path, ts) — the identity of one *read*. Keyed on (path, lines, full_tokens) it
    // would be the identity of the *file*, and collapse 25 genuine reads of README.md into one:
    // that over-deduplicated by 1,288 rows on first attempt.
    let mut seen_reads: std::collections::HashSet<(String, String)> = Default::default();
    let dupes = {
        let mut d = 0i64;
        for (route, path, _, _, ts) in &all {
            if route == "builtin_read" && !seen_reads.insert((path.clone(), ts.clone())) {
                d += 1;
            }
        }
        d
    };
    seen_reads.clear();

    let mut leaked = 0i64;
    let mut below = 0i64;
    let mut unmeasurable = (0i64, 0i64);
    let mut uncovered: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
    let mut optimized = 0i64;
    for (route, path, lines, full, ts) in &all {
        if route != "builtin_read" {
            optimized += 1;
            continue;
        }
        if !seen_reads.insert((path.clone(), ts.clone())) {
            continue;
        }
        match coverage::classify(path, *lines) {
            coverage::Scope::Optimizable => leaked += 1,
            coverage::Scope::BelowThreshold => below += 1,
            coverage::Scope::Unmeasurable => {
                unmeasurable.0 += 1;
                unmeasurable.1 += full;
            }
            coverage::Scope::UncoveredKind => {
                let e = uncovered.entry(coverage::ext_of(path)).or_default();
                e.0 += 1;
                e.1 += full;
            }
        }
    }

    println!("\n== Coverage: what leaked, versus what was out of scope ==========");
    if dupes > 0 {
        println!("  ({dupes} duplicate builtin rows collapsed — two meter hooks are registered)");
    }
    println!("  optimized                {optimized:>7} calls");
    println!(
        "  not intercepted          {leaked:>7} calls   <- eligible; expected to be the fail-open guards{}",
        if leaked == 0 { " (none)" } else { "" }
    );
    println!("  out of scope:");
    println!("    below 300 lines        {below:>7} calls");
    println!(
        "    unmeasurable           {:>7} calls   {:>10} tokens excluded from every baseline",
        unmeasurable.0, unmeasurable.1
    );
    let uncovered_calls: i64 = uncovered.values().map(|v| v.0).sum();
    let uncovered_tokens: i64 = uncovered.values().map(|v| v.1).sum();
    println!(
        "    kinds not handled      {uncovered_calls:>7} calls   {uncovered_tokens:>10} tokens — the coverage backlog"
    );
    let mut backlog: Vec<_> = uncovered.iter().collect();
    backlog.sort_by_key(|(_, v)| -v.1);
    for (ext, (n, tok)) in backlog.iter().take(6) {
        println!("      .{ext:<8} {n:>5} calls {tok:>9} tokens");
    }

    // NOT asserted to be zero, and the reason matters.
    //
    // The intercept has two deliberate fail-open guards. `retry_escape_valve` lets a second read
    // of the same path through in the same session, so a model that genuinely cannot reach the
    // MCP tools is never deadlocked; `lumen_mcp_missing` does the same when the server is absent.
    // Both are recorded in the fault spool as `hook_fail_open`, and both produce exactly this
    // shape: a built-in read of a file the hook claims to cover.
    //
    // So "eligible and not intercepted" is a mixture of two different things — a routing failure,
    // and a guard doing its job. This test cannot tell them apart, because the spool is drained by
    // `lumen report` and is not a durable record. Asserting zero therefore fails on correct
    // behaviour: it passed when written and broke within the hour, once a subagent re-read a file
    // this session had already redirected once.
    //
    // What is asserted instead: the share stays small. A genuine routing regression — the hook not
    // firing at all — would push this far past the bound, because every eligible read would land
    // here. The paths are printed so a real leak is identifiable rather than merely counted.
    let eligible = leaked + optimized;
    if leaked > 0 {
        println!("  the {leaked} above are eligible reads that were not intercepted:");
        for (route, path, lines, _, _) in &all {
            if route == "builtin_read"
                && coverage::classify(path, *lines) == coverage::Scope::Optimizable
            {
                println!("    {:>5} lines  {path}", lines.unwrap_or(0));
            }
        }
        println!(
            "  expected causes: the retry_escape_valve and lumen_mcp_missing fail-open guards,"
        );
        println!("  which are recorded in the fault spool as hook_fail_open.");
    }
    let leak_share = 100.0 * leaked as f64 / eligible.max(1) as f64;
    assert!(
        leak_share < 5.0,
        "{leaked} of {eligible} eligible reads bypassed the optimizer ({leak_share:.1}%). \
         The fail-open guards account for a few; this many means the hook is not firing."
    );

    // ── The baseline, with binaries excluded ─────────────────────────────────────────────
    //
    // lumen-stats has excluded these from `missed_calls` since 1.2.1; this test never did, which
    // is why docs/efficiency.md published a 6.88M-token "missed" figure whose PNG half is a
    // bytes/4 guess the codebase itself documents as ~40x too high.
    let missed_tokens: i64 = all
        .iter()
        .filter(|(route, path, lines, _, _)| {
            route == "builtin_read"
                && coverage::classify(path, *lines) != coverage::Scope::Unmeasurable
        })
        .map(|(_, _, _, full, _)| full)
        .sum();
    let raw_missed: i64 = all
        .iter()
        .filter(|(route, ..)| route == "builtin_read")
        .map(|(_, _, _, full, _)| full)
        .sum();
    println!("\n  missed-optimization tokens, binaries excluded: {missed_tokens}");
    println!(
        "  (raw would be {raw_missed} — {:.0}% of it is unmeasurable files)",
        100.0 * (raw_missed - missed_tokens) as f64 / raw_missed.max(1) as f64
    );
    assert!(
        missed_tokens <= raw_missed,
        "excluding rows cannot increase the total"
    );
}

/// A ledger built here, on the real schema, so the invariants run on controlled data.
///
/// `lumen_core::meter::connect_db` creates the file in a temp dir with the production DDL and
/// migrations, so a column added or dropped by a migration reaches this test instead of being
/// hidden by a hand-written table. `LUMEN_DB` is never read or set: the ledger is passed by
/// path, which also keeps these tests safe to run in parallel.
mod fixture {
    use rusqlite::{Connection, params};

    /// Before and after the writer started recording a signed saving (see `SIGNED_SINCE`).
    pub const OLD: &str = "2026-07-20T10:00:00Z";
    pub const NEW: &str = "2026-08-15T10:00:00Z";

    #[allow(clippy::too_many_arguments)]
    pub fn row(
        conn: &Connection,
        ts: &str,
        route: &str,
        path: &str,
        lines: Option<i64>,
        full: i64,
        returned: i64,
        saved: i64,
        writer: &str,
    ) {
        conn.execute(
            "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,\
             saved_tokens,routed_via,channel,writer_hook,token_source) \
             VALUES(?1,'Read',?2,?3,?4,?5,?6,?7,'cli',?8,'measured')",
            params![ts, path, lines, returned, full, saved, route, writer],
        )
        .expect("insert fixture row");
    }

    /// A ledger that satisfies every invariant, then `tweak` applied to it. Returned
    /// read-only, as the real ledger is opened, with the temp dir held alive alongside it.
    pub fn ledger(
        healthy: bool,
        tweak: impl FnOnce(&Connection),
    ) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("lumen.db");
        {
            let conn = lumen_core::meter::connect_db(&db).expect("create ledger with real schema");
            if healthy {
                // Optimized reads, saving = full - returned.
                for i in 0..30 {
                    row(
                        &conn,
                        &format!("2026-08-15T10:00:{i:02}Z"),
                        "smart_read",
                        &format!("/w/src/f{i}.rs"),
                        Some(400 + i),
                        10_000,
                        800,
                        9_200,
                        "lumen-mcp",
                    );
                }
                // A call dearer than the file shows as a negative, in the signed era...
                row(
                    &conn,
                    NEW,
                    "recall_file",
                    "/w/a.py",
                    Some(350),
                    200,
                    260,
                    -60,
                    "lumen-mcp",
                );
                // ...and as a clamped zero before it. Both are correct for their era.
                row(
                    &conn,
                    OLD,
                    "smart_read",
                    "/w/b.py",
                    Some(320),
                    100,
                    150,
                    0,
                    "lumen-mcp",
                );

                // Built-in reads, which save nothing: returned == full.
                // One eligible read that slipped past (a fail-open guard), recorded by BOTH
                // registered meter hooks at the same (path, ts). Counted once it is 1 of 31
                // eligible reads (3.2%); counted twice it would be 2 of 32 (6.3%) and fail,
                // so this row also proves the de-duplication.
                for hook in ["hook-user", "hook-repo"] {
                    row(
                        &conn,
                        NEW,
                        "builtin_read",
                        "/w/src/leak.rs",
                        Some(450),
                        5_000,
                        5_000,
                        0,
                        hook,
                    );
                }
                // Out of scope, each for a different reason.
                row(
                    &conn,
                    NEW,
                    "builtin_read",
                    "/w/small.rs",
                    Some(40),
                    300,
                    300,
                    0,
                    "hook-user",
                );
                row(
                    &conn,
                    NEW,
                    "builtin_read",
                    "/w/shot.png",
                    None,
                    90_000,
                    90_000,
                    0,
                    "hook-user",
                );
                row(
                    &conn,
                    NEW,
                    "builtin_read",
                    "/w/notes.md",
                    Some(500),
                    7_000,
                    7_000,
                    0,
                    "hook-user",
                );
            }
            tweak(&conn);
        }
        let conn = lumen_core::meter::open_read_only(&db).expect("reopen read-only");
        (dir, conn)
    }
}

/// The invariants hold on a ledger that honours the rules, with every kind of row in it.
#[test]
fn the_recorded_ledger_agrees_with_itself() {
    let (_dir, conn) = fixture::ledger(true, |_| {});
    assert_ledger_agrees(&conn);
}

// Each test below corrupts the healthy ledger in exactly one way and expects exactly the
// assertion that guards against it. They exist so the test above cannot go green by asserting
// nothing: if a guard were deleted or loosened, the matching test here would stop panicking.

#[test]
#[should_panic(expected = "impossible")]
fn a_saving_larger_than_the_file_is_caught() {
    let (_dir, conn) = fixture::ledger(true, |c| {
        fixture::row(
            c,
            fixture::NEW,
            "compress_logs",
            "/w/x.log",
            Some(900),
            100,
            0,
            150,
            "lumen-mcp",
        );
    });
    assert_ledger_agrees(&conn);
}

#[test]
#[should_panic(expected = "disagree with the saved_tokens rule")]
fn a_clamped_saving_in_the_signed_era_is_caught() {
    let (_dir, conn) = fixture::ledger(true, |c| {
        // Returned more than the file but recorded as 0 after the writer went signed.
        fixture::row(
            c,
            fixture::NEW,
            "smart_read",
            "/w/c.py",
            Some(310),
            100,
            150,
            0,
            "lumen-mcp",
        );
    });
    assert_ledger_agrees(&conn);
}

#[test]
#[should_panic(expected = "disagree with the saved_tokens rule")]
fn a_negative_saving_in_the_clamped_era_is_caught() {
    let (_dir, conn) = fixture::ledger(true, |c| {
        fixture::row(
            c,
            fixture::OLD,
            "smart_read",
            "/w/c.py",
            Some(310),
            100,
            150,
            -50,
            "lumen-mcp",
        );
    });
    assert_ledger_agrees(&conn);
}

#[test]
#[should_panic(expected = "no longer describes what happened")]
fn losses_that_swamp_the_headline_are_caught() {
    let (_dir, conn) = fixture::ledger(true, |c| {
        fixture::row(
            c,
            fixture::NEW,
            "smart_read",
            "/w/d.py",
            Some(310),
            100,
            30_100,
            -30_000,
            "lumen-mcp",
        );
    });
    assert_ledger_agrees(&conn);
}

#[test]
#[should_panic(expected = "bypassed the optimizer")]
fn a_hook_that_stopped_firing_is_caught() {
    let (_dir, conn) = fixture::ledger(true, |c| {
        for i in 0..12 {
            fixture::row(
                c,
                &format!("2026-08-15T11:00:{i:02}Z"),
                "builtin_read",
                &format!("/w/src/missed{i}.rs"),
                Some(600),
                4_000,
                4_000,
                0,
                "hook-user",
            );
        }
    });
    assert_ledger_agrees(&conn);
}

#[test]
#[should_panic(expected = "vacuously")]
fn an_empty_ledger_does_not_pass() {
    let (_dir, conn) = fixture::ledger(false, |_| {});
    assert_ledger_agrees(&conn);
}

/// This machine's own ledger, audited on request. Opt-in because its verdict is a fact about
/// one person's usage — fail-open guards legitimately let eligible reads through — not about
/// the code, so it cannot gate anyone else's `cargo test`.
/// `LUMEN_TEST_REAL_LEDGER=1 cargo test -p lumen-mcp --test efficiency -- --nocapture`
#[test]
fn the_real_ledger_agrees_with_itself_when_asked() {
    if std::env::var_os("LUMEN_TEST_REAL_LEDGER").is_none() {
        println!(
            "skipped: set LUMEN_TEST_REAL_LEDGER=1 to audit this machine's own ledger; \
             the invariants are always checked on a fixture ledger by \
             the_recorded_ledger_agrees_with_itself"
        );
        return;
    }
    let db = lumen_core::meter::db_path()
        .expect("LUMEN_TEST_REAL_LEDGER is set but no ledger path resolves");
    // Read-only: this is the installed app's ledger, and its schema is not this build's to
    // migrate.
    let conn = lumen_core::meter::open_read_only(&db)
        .unwrap_or_else(|e| panic!("cannot open the ledger at {}: {e}", db.display()));
    assert_ledger_agrees(&conn);
}

// ── A reply costs no more than the file it was asked about, and its label ────────────────────
//
// The claim is in the README and docs/efficiency.md, so what stands behind it is written out
// here and nowhere wider. Checked:
//
//   * every readable file in this repository at or above the interception threshold, code or
//     not, because a caller may name any file;
//   * on each, every request shape: smart_read (outline, and mode="full"); recall_file by name
//     (exact, a substring, no match, too broad, every name), by line range (small, half, nearly
//     all, all, past the end, one line) and with no selector; compress_logs, by path and as
//     inline text;
//   * the reply as a client receives it, counted again here with the tokenizer. The call's own
//     `_meta` and its ledger row are what is compared against that count; they are not a source
//     of truth about themselves.
//
// What a reply may cost above the file is what labels it, per `Bound`: the note a fallback adds
// (the inflation note within `NOTE_ALLOWANCE`, the no-structure note counted from its own text,
// which carries the path) and the header of `smart_read(mode="full")`. A file or log with nothing
// visible in it is the one case with no bound at all.
//
// Not checked, and not claimed: error replies (they carry no file content), the replies a repeat
// read is answered with (returned only when cheaper than the full answer, and tested in
// repeat_reads.rs), and the JSON-RPC envelope and `_meta` block, which no one counts. Outside the
// corpus too: a file that is not UTF-8, a file above `MAX_DOC_TOKENS`, and a `recall_file`
// request whose start line is after its end line or far beyond the end of the file, none of
// which is generated.

/// What one reply may cost relative to the file it describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bound {
    /// No more than the file, and no note: `compress_logs` hands back the log itself when it
    /// cannot beat it.
    Strict,
    /// Cheaper than the file, or exactly the file behind the one line that says why.
    Noted,
    /// `smart_read(mode="full")`: the file behind the header that labels it.
    Full,
    /// A file with nothing visible in it. It cannot be answered with itself, because an empty text
    /// block is one a client may refuse, so the ranked arm's decline keeps the outline it would
    /// have sent. The one reply that is not bounded by the file, and the only one without a bound.
    Kept,
}

/// BPE can pair characters differently across the join between a note or header and the text
/// after it, so a bound built from the two parts counted separately is good to a token or two.
const SEAM: i64 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    SmartRead,
    Recall,
    Compress,
}

fn call(tool: Tool, args: &Value) -> Outcome {
    match tool {
        Tool::SmartRead => lumen_mcp::tool_smart_read(args),
        Tool::Recall => lumen_mcp::tool_recall_file(args),
        Tool::Compress => lumen_mcp::tool_compress_logs(args),
    }
}

fn tool_name(tool: Tool) -> &'static str {
    match tool {
        Tool::SmartRead => "smart_read",
        Tool::Recall => "recall_file",
        Tool::Compress => "compress_logs",
    }
}

/// One reply as a client receives it, with what the call recorded about it.
struct Reply {
    text: String,
    /// `_meta`: full, returned, saved.
    meta: (i64, i64, i64),
    /// The ledger row: route, tokens_returned, full_tokens, saved_tokens.
    ledger: (String, i64, i64, i64),
}

/// `result` is the JSON-RPC `result` object, whether it came from `Outcome` or off the wire.
fn parse_reply(result: &Value, ledger: (String, i64, i64, i64)) -> Result<Reply, String> {
    let blocks = result["content"].as_array().ok_or("no content array")?;
    if blocks.len() != 1 || blocks[0]["type"] != "text" {
        return Err(format!(
            "expected exactly one text block, got {} block(s)",
            blocks.len()
        ));
    }
    if result["isError"] != json!(false) {
        return Err("isError is not false".to_string());
    }
    let text = blocks[0]["text"]
        .as_str()
        .ok_or("the text block carries no string")?
        .to_string();
    let n = |k: &str| {
        result["_meta"][k]
            .as_i64()
            .ok_or(format!("_meta.{k} is missing"))
    };
    Ok(Reply {
        text,
        meta: (n("full_tokens")?, n("returned_tokens")?, n("saved_tokens")?),
        ledger,
    })
}

fn reply_of(out: &Outcome) -> Result<Reply, String> {
    let Payload::Ok(result) = &out.payload else {
        return Err(format!("the call failed: {:?}", out.error_message()));
    };
    let m = out.meter.as_ref().ok_or("the call wrote no ledger row")?;
    parse_reply(
        result,
        (
            m.routed_via.clone(),
            m.returned_tokens,
            m.full_tokens,
            m.saved_tokens,
        ),
    )
}

/// The file a call was asked about, as the checks see it.
struct Subject<'a> {
    name: &'a str,
    path: &'a str,
    src: &'a str,
    full: i64,
}

/// What a reply is, from its first line. Used to prove the corpus reached every branch of the
/// tools, and to tell a documented fallback from a reply that is simply too large.
fn branch_of(text: &str) -> &'static str {
    let first = text.lines().next().unwrap_or("");
    if first.starts_with("# lumen: the requested view was") {
        "inflation fallback"
    } else if first.starts_with("# lumen: no structure could be extracted") {
        "no structure"
    } else if first.ends_with("— outline") {
        "outline"
    } else if first.ends_with("(full)") {
        "full"
    } else if first.starts_with("# recall_file: nothing matched") {
        "nothing matched"
    } else if first.starts_with("# recall_file: no selector") {
        "no selector"
    } else if first.starts_with("# recall_file: those names cover") {
        "covers most"
    } else if first.starts_with("# recall_file:") && first.contains("too many") {
        "too many"
    } else if first.contains("(+3 lines context)") {
        "range"
    } else if first.starts_with("# matched by substring") {
        "substring excerpt"
    } else if first.contains("item(s):") {
        "excerpt"
    } else {
        "other"
    }
}

fn agree(bad: &mut Vec<String>, who: &str, what: &str, got: i64, want: i64) {
    if got != want {
        bad.push(format!(
            "{who}: {what} is {got}, but the reply as sent costs {want}"
        ));
    }
}

/// Every statement the guarantee makes about one reply. Violations are pushed, not panicked on,
/// so a run over the whole corpus reports every file at once. Returns the branch the reply took
/// and how many tokens it cost above the file (negative when it saved).
fn check(
    s: &Subject,
    label: &str,
    bound: Bound,
    r: &Reply,
    bad: &mut Vec<String>,
) -> (&'static str, i64) {
    let who = format!("{label} on {}", s.name);
    let sent = count_tokens(&r.text) as i64;
    let (route, l_returned, l_full, l_saved) = (&r.ledger.0, r.ledger.1, r.ledger.2, r.ledger.3);

    // What a call reports is what it sent. The caller is billed for the text, so the text is what
    // is counted, here and independently of the tool.
    agree(bad, &who, "_meta.returned_tokens", r.meta.1, sent);
    agree(bad, &who, "ledger tokens_returned", l_returned, sent);
    agree(bad, &who, "_meta.saved_tokens", r.meta.2, s.full - sent);
    agree(bad, &who, "ledger saved_tokens", l_saved, s.full - sent);
    if r.meta.0 != s.full || l_full != s.full {
        bad.push(format!(
            "{who}: the file costs {} tokens, the call says {} (_meta) and {l_full} (ledger)",
            s.full, r.meta.0
        ));
    }
    // Not even a blank file is answered with a blank text block, which a client may refuse.
    if r.text.trim().is_empty() {
        bad.push(format!("{who}: a blank text block"));
    }

    let branch = match bound {
        Bound::Strict if r.text == s.src => "the log itself",
        Bound::Strict if s.src.trim().is_empty() => "blank log",
        Bound::Strict => "compressed",
        _ => branch_of(&r.text),
    };
    // The headline sums these routes. A reply that cost as much as the file, or more, must not be
    // in one: it saved nothing, and pooling it in is how a loss reaches the savings total.
    if lumen_stats::LUMEN_ROUTES.contains(&route.as_str()) && sent >= s.full {
        bad.push(format!(
            "{who}: routed as a saving ({route}), but the reply costs {sent} against {} for the file",
            s.full
        ));
    }

    match bound {
        Bound::Strict => {
            if s.src.trim().is_empty() {
                if route != "would_inflate" {
                    bad.push(format!("{who}: a blank log is routed {route}"));
                }
            } else if r.text == s.src {
                if route != "would_inflate" || sent != s.full {
                    bad.push(format!(
                        "{who}: the log itself is routed {route} at {sent} tokens (log: {})",
                        s.full
                    ));
                }
            } else {
                if sent > s.full {
                    bad.push(format!(
                        "{who}: the reply costs {sent} against {} for the log itself",
                        s.full
                    ));
                }
                if route != "compress_logs" || sent >= s.full {
                    bad.push(format!(
                        "{who}: a reply that is not the log must be cheaper than it and routed \
                         compress_logs, got {sent} against {} routed {route}",
                        s.full
                    ));
                }
            }
        }
        Bound::Noted if sent >= s.full => match branch {
            // A ranked call that declines hands back the file itself, with no note: it saved
            // nothing and its route says so.
            _ if r.text == s.src => {
                if !route.starts_with("ranked_") || sent != s.full {
                    bad.push(format!(
                        "{who}: the file itself, routed {route} at {sent} tokens (file: {})",
                        s.full
                    ));
                }
            }
            "inflation fallback" => {
                let body = r.text.split_once("\n\n").map(|(_, rest)| rest);
                if body != Some(s.src) {
                    bad.push(format!(
                        "{who}: the fallback is not the file behind one note"
                    ));
                }
                if route != "would_inflate" {
                    bad.push(format!("{who}: the fallback is routed {route}"));
                }
                if sent > s.full + NOTE_ALLOWANCE {
                    bad.push(format!(
                        "{who}: the fallback costs {sent} against {} for the file, over the \
                         {NOTE_ALLOWANCE}-token allowance for its note",
                        s.full
                    ));
                }
            }
            "no structure" => {
                let note = format!(
                    "# lumen: no structure could be extracted from {}, so the file is returned \
                     instead.\n\n",
                    s.path
                );
                if r.text != format!("{note}{}", s.src) {
                    bad.push(format!(
                        "{who}: the fallback is not the file behind its note"
                    ));
                }
                if route != "ranked_no_defs" {
                    bad.push(format!("{who}: the fallback is routed {route}"));
                }
                let allowed = s.full + count_tokens(&note) as i64 + SEAM;
                if sent > allowed {
                    bad.push(format!(
                        "{who}: the fallback costs {sent}, more than the file ({}) and its note \
                         together ({allowed})",
                        s.full
                    ));
                }
            }
            _ => bad.push(format!(
                "{who}: the reply costs {sent} against {} for the file, and it is not one of \
                 the documented fallbacks (first line: {:?})",
                s.full,
                r.text.lines().next().unwrap_or("")
            )),
        },
        Bound::Noted => {
            if branch == "inflation fallback" || branch == "no structure" {
                bad.push(format!("{who}: a fallback that costs less than the file"));
            }
        }
        Bound::Kept => {
            if branch != "outline" || !route.starts_with("ranked_") {
                bad.push(format!(
                    "{who}: a file with nothing in it should be answered with the outline of \
                     the ranked arm's decline, got {branch:?} routed {route}"
                ));
            }
        }
        Bound::Full => {
            let header = format!("# {} (full)\n\n", s.path);
            if r.text != format!("{header}{}", s.src) {
                bad.push(format!("{who}: not the file behind its one-line header"));
            }
            if route != "smart_read_full" {
                bad.push(format!("{who}: routed {route}"));
            }
            let allowed = s.full + count_tokens(&header) as i64 + SEAM;
            if sent > allowed {
                bad.push(format!(
                    "{who}: costs {sent}, more than the file ({}) and its header together ({allowed})",
                    s.full
                ));
            }
        }
    }
    (branch, sent - s.full)
}

/// A file in the corpus.
struct Doc {
    name: String,
    path: String,
    src: String,
    full: i64,
    items: Vec<CodeItem>,
    /// `outline` found structure in it. One it cannot read comes back as a single synthetic
    /// whole-file item, which is where the tools answer with the file itself.
    described: bool,
}

impl Doc {
    /// A file as the tools will meet it: read from disk, counted with the product's tokenizer.
    /// `None` for a file that is not UTF-8 or has fewer than `min_lines` lines.
    fn from_file(name: String, path: &Path, min_lines: usize) -> Option<Doc> {
        let src = std::fs::read_to_string(path).ok()?;
        if src.lines().count() < min_lines {
            return None;
        }
        let p = path.to_string_lossy().into_owned();
        let items = outline(&src, detect_lang(&p));
        Some(Doc {
            name,
            full: count_tokens(&src) as i64,
            described: !items.iter().all(|i| i.kind == "file"),
            items,
            path: p,
            src,
        })
    }

    fn lines(&self) -> usize {
        self.src.lines().count()
    }

    fn subject(&self) -> Subject<'_> {
        Subject {
            name: &self.name,
            path: &self.path,
            src: &self.src,
            full: self.full,
        }
    }
}

/// `f` over every item on as many threads as the machine has cores, results in input order.
/// Counting tokens in a debug build is the cost of this file, and it is the same work on every
/// core. Items are handed out in order, so a caller that puts the heaviest first finishes sooner.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                // The tools recurse over parse trees, and a debug build's frames are large.
                std::thread::Builder::new()
                    .stack_size(16 << 20)
                    .spawn_scoped(scope, || {
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(i) else { break };
                            mine.push((i, f(item)));
                        }
                        mine
                    })
                    .expect("spawn a worker")
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a worker panicked"))
            .collect()
    });
    done.sort_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// Above this a file is a lockfile or a measurement table, not something a model is sent to read,
/// and every call counts the whole file at least twice. Three such files cost this test more than
/// all the sources together; none of the tools' branches depends on size.
const MAX_DOC_TOKENS: i64 = 50_000;

/// Every readable file in the repository at or above the interception threshold, whether or not
/// Lumen can read inside it: a caller may name any file. Binary files fail UTF-8 and are not
/// files a text tool is asked about. Returns the corpus, heaviest first, and the files left out
/// for being above `MAX_DOC_TOKENS`.
fn documents() -> (Vec<Doc>, Vec<String>) {
    let root = repo_root();
    let mut files = Vec::new();
    walk_where(&root, &mut files, &|_| true);
    files.sort();
    let mut sized: Vec<(u64, PathBuf)> = files
        .into_iter()
        .map(|p| (std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), p))
        .collect();
    sized.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let built = par_map(&sized, |(_, path)| {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        Doc::from_file(name, path, THRESHOLD_LINES)
    });
    let (mut docs, mut skipped) = (Vec::new(), Vec::new());
    for d in built.into_iter().flatten() {
        if d.full > MAX_DOC_TOKENS {
            skipped.push(d.name);
        } else {
            docs.push(d);
        }
    }
    docs.sort_by(|a, b| b.full.cmp(&a.full).then_with(|| a.name.cmp(&b.name)));
    (docs, skipped)
}

struct Shape {
    label: &'static str,
    tool: Tool,
    args: Value,
    bound: Bound,
}

fn shape(label: &'static str, tool: Tool, args: Value, bound: Bound) -> Shape {
    Shape {
        label,
        tool,
        args,
        bound,
    }
}

/// A query that is part of an item's name and no item's whole name, and that matches few enough
/// items to reach `recall_file`'s labelled substring fallback rather than its "too broad" outline.
fn substring_query(named: &[&CodeItem]) -> Option<String> {
    for item in named {
        let lower = item.name.as_deref()?.to_lowercase();
        let n = lower.chars().count();
        if n < 5 {
            continue;
        }
        let q: String = lower.chars().take(n - 1).collect();
        let q = q.trim().to_string();
        let exact = named
            .iter()
            .filter(|i| i.name.as_deref().map(str::to_lowercase).as_deref() == Some(q.as_str()))
            .count();
        let subs = named
            .iter()
            .filter(|i| {
                i.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(q.as_str()))
            })
            .count();
        if !q.is_empty() && exact == 0 && (1..=5).contains(&subs) {
            return Some(q);
        }
    }
    None
}

fn range_shape(path: &str, label: &'static str, from: usize, to: usize) -> Shape {
    shape(
        label,
        Tool::Recall,
        json!({ "path": path, "start_line": from, "end_line": to }),
        Bound::Noted,
    )
}

/// Every request shape for one file.
///
/// A `recall_file` request whose start line is after its end line, or far beyond the end of the
/// file, is not a shape here: this test measures the replies of the requests it generates.
fn shapes(d: &Doc) -> Vec<Shape> {
    let lines = d.lines();
    let p = d.path.as_str();
    let mut v = vec![
        shape(
            "smart_read outline",
            Tool::SmartRead,
            json!({ "path": p }),
            Bound::Noted,
        ),
        shape(
            "smart_read full",
            Tool::SmartRead,
            json!({ "path": p, "mode": "full" }),
            Bound::Full,
        ),
        shape(
            "recall no selector",
            Tool::Recall,
            json!({ "path": p }),
            Bound::Noted,
        ),
        shape(
            "recall names no match",
            Tool::Recall,
            json!({ "path": p, "names": ["no_such_item_anywhere_zz"] }),
            Bound::Noted,
        ),
        // One letter matches nearly every name: the query behind most of the overage the 1.7.0
        // metering found, and the one a "too broad" outline exists to answer.
        shape(
            "recall names too broad",
            Tool::Recall,
            json!({ "path": p, "names": ["e"] }),
            Bound::Noted,
        ),
        shape(
            "compress_logs path",
            Tool::Compress,
            json!({ "path": p }),
            Bound::Strict,
        ),
        shape(
            "compress_logs text",
            Tool::Compress,
            json!({ "text": d.src }),
            Bound::Strict,
        ),
    ];

    if lines > 0 {
        let mid = (lines / 2).max(1);
        let third = (lines / 3).max(1);
        v.push(range_shape(p, "recall range small", mid, mid + 20));
        v.push(range_shape(p, "recall range half", 1, mid));
        if lines >= 3 {
            v.push(range_shape(p, "recall range nearly all", 2, lines - 1));
        }
        v.push(range_shape(p, "recall range all", 1, 999_999));
        v.push(range_shape(
            p,
            "recall range past the end",
            lines.saturating_sub(5).max(1),
            lines + 500,
        ));
        v.push(range_shape(p, "recall range one line", third, third));
    }

    let named: Vec<&CodeItem> = d.items.iter().filter(|i| i.name.is_some()).collect();
    if !named.is_empty() {
        let span = |i: &CodeItem| i.end_line - i.start_line + 1;
        let mut by_span = named.clone();
        by_span.sort_by_key(|i| span(i));
        let name_of = |i: &CodeItem| i.name.clone().unwrap_or_default();
        v.push(shape(
            "recall names exact (median item)",
            Tool::Recall,
            json!({ "path": p, "names": [name_of(by_span[by_span.len() / 2])] }),
            Bound::Noted,
        ));
        // The largest item short of the 60% the tool turns into an outline: the excerpt closest
        // to the whole file that an exact name can produce, and so the likeliest to overshoot.
        if let Some(big) = by_span.iter().rev().find(|i| span(i) * 100 / lines < 55) {
            v.push(shape(
                "recall names exact (largest item)",
                Tool::Recall,
                json!({ "path": p, "names": [name_of(big)] }),
                Bound::Noted,
            ));
        }
        v.push(shape(
            "recall names every item",
            Tool::Recall,
            json!({ "path": p, "names": named.iter().map(|i| name_of(i)).collect::<Vec<_>>() }),
            Bound::Noted,
        ));
        if let Some(q) = substring_query(&named) {
            v.push(shape(
                "recall names substring",
                Tool::Recall,
                json!({ "path": p, "names": [q] }),
                Bound::Noted,
            ));
        }
    }
    v
}

#[derive(Default)]
struct Tally {
    bad: Vec<String>,
    /// (shape, branch) -> files that took it.
    hits: BTreeMap<(&'static str, &'static str), usize>,
    /// shape -> (the largest overage above the file in tokens, where).
    worst: BTreeMap<&'static str, (i64, String)>,
    calls: usize,
}

impl Tally {
    fn merge(&mut self, other: Tally) {
        self.bad.extend(other.bad);
        self.calls += other.calls;
        for (k, n) in other.hits {
            *self.hits.entry(k).or_default() += n;
        }
        for (k, w) in other.worst {
            let mine = self.worst.entry(k).or_insert((i64::MIN, String::new()));
            if w.0 > mine.0 {
                *mine = w;
            }
        }
    }

    fn note(&mut self, label: &'static str, branch: &'static str, over: i64, name: &str) {
        *self.hits.entry((label, branch)).or_default() += 1;
        let w = self.worst.entry(label).or_insert((i64::MIN, String::new()));
        if over > w.0 {
            *w = (over, name.to_string());
        }
    }

    fn reached(&self, label: &str, branch: &str) -> usize {
        self.hits.get(&(label, branch)).copied().unwrap_or_default()
    }

    /// The run took this branch on at least this many files, or the test is not testing it.
    fn expect(&self, label: &'static str, branch: &'static str, at_least: usize) {
        let n = self.reached(label, branch);
        assert!(
            n >= at_least,
            "{label} reached the {branch:?} branch on {n} file(s), expected at least {at_least}: \
             the run no longer exercises it"
        );
    }

    fn print(&self, title: &str, files: usize) {
        println!("\n== {title} ==============================");
        println!("  {} calls over {files} files", self.calls);
        println!(
            "  {:<36} {:>8}  (tokens above the file; negative = cheaper)",
            "shape", "worst"
        );
        for (label, (over, name)) in &self.worst {
            println!("  {label:<36} {over:>8}  {name}");
        }
        println!("\n  {:<36} {:<20} {:>6}", "shape", "reply was", "files");
        for ((label, branch), n) in &self.hits {
            println!("  {label:<36} {branch:<20} {n:>6}");
        }
    }
}

fn run_doc(d: &Doc) -> Tally {
    let mut t = Tally::default();
    for sh in shapes(d) {
        let out = call(sh.tool, &sh.args);
        t.calls += 1;
        match reply_of(&out) {
            Err(e) => t.bad.push(format!("{} on {}: {e}", sh.label, d.name)),
            Ok(r) => {
                let (branch, over) = check(&d.subject(), sh.label, sh.bound, &r, &mut t.bad);
                t.note(sh.label, branch, over, &d.name);
            }
        }
    }
    t
}

fn run_docs(docs: &[Doc]) -> Tally {
    let mut all = Tally::default();
    for t in par_map(docs, run_doc) {
        all.merge(t);
    }
    all
}

/// Fails with every violation at once, up to a screenful.
fn assert_no_violations(what: &str, bad: &[String]) {
    assert!(
        bad.is_empty(),
        "{what}: {} violation(s)\n  {}{}",
        bad.len(),
        bad.iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
        if bad.len() > 25 { "\n  ..." } else { "" }
    );
}

/// No reply costs more than the file it was asked about and the line or header that labels it, on
/// any real file, in any shape. `Bound` says what each shape may add.
#[test]
fn no_reply_costs_more_than_the_file_and_its_label_on_any_real_file() {
    let (docs, skipped) = documents();
    let described = docs.iter().filter(|d| d.described).count();
    let n = docs.len();
    // A corpus that shrank, or a walker that stopped finding files, must not pass quietly.
    assert!(
        n >= 40 && described >= 25 && n - described >= 3 && skipped.len() <= 8,
        "corpus too small to conclude anything: {n} files, {described} with structure, \
         {} over {MAX_DOC_TOKENS} tokens left out",
        skipped.len()
    );

    let t = run_docs(&docs);
    t.print("No reply costs more than the file and its label", n);
    println!(
        "\n  {described} files with structure, {} without; left out above {MAX_DOC_TOKENS} \
         tokens: {}",
        n - described,
        skipped.join(", ")
    );
    assert_no_violations("the corpus", &t.bad);

    // Every branch the guarantee is about was reached on real files, not merely allowed to be.
    // Floors, not exact counts: the corpus grows and shrinks with the repository.
    let most = n * 9 / 10;
    t.expect("smart_read outline", "outline", described / 2);
    t.expect("smart_read outline", "no structure", 3);
    t.expect("smart_read full", "full", n);
    t.expect("recall no selector", "no selector", most);
    t.expect("recall names no match", "nothing matched", most);
    t.expect("recall names too broad", "too many", n / 3);
    t.expect("recall names exact (median item)", "excerpt", described / 2);
    t.expect(
        "recall names exact (largest item)",
        "excerpt",
        described / 2,
    );
    t.expect("recall names every item", "covers most", described / 2);
    t.expect("recall names substring", "substring excerpt", described / 2);
    for label in [
        "recall range small",
        "recall range half",
        "recall range past the end",
        "recall range one line",
    ] {
        t.expect(label, "range", most);
    }
    // A range that asks for the whole file costs more than the file, on every file of 300 lines
    // or more: the guard is what answers it.
    t.expect("recall range all", "inflation fallback", most);
    t.expect("recall range nearly all", "inflation fallback", most);
    // A source file rarely compresses by more than the header costs, so it comes back as it is.
    t.expect("compress_logs path", "the log itself", n / 2);
    t.expect("compress_logs text", "the log itself", n / 2);
}

/// Files below the interception threshold, which no file in the corpus above is. At this size an
/// outline can cost more than the file it describes, so this is where the guard has the most to
/// do.
fn small_docs(dir: &Path) -> Vec<Doc> {
    let padded: String = (0..6)
        .map(|i| {
            format!(
                "/// Step {i}.\npub fn step_number_{i}(a: u32) -> u32 {{\n    let x = a.wrapping_mul({i});\n    \
                 let y = x.wrapping_add(7);\n    y\n}}\n\n"
            )
        })
        .collect();
    let files: [(&str, &str); 8] = [
        (
            "tiny.rs",
            "pub fn a() -> u8 { 1 }\npub fn b() -> u8 { 2 }\npub fn c() -> u8 { 3 }\n",
        ),
        (
            "short.py",
            "def first():\n    return 1\n\n\ndef second():\n    return 2\n\n\nclass Third:\n    def method(self):\n        return 3\n",
        ),
        (
            "short.ts",
            "export function one(): number { return 1; }\nexport function two(): number { return 2; }\nexport class Three {\n  value = 3;\n}\n",
        ),
        ("padded.rs", padded.as_str()),
        ("one_line.rs", "fn only() {}"),
        (
            "notes.txt",
            "plain prose with nothing to outline\nsecond line of the same\nthird line\n",
        ),
        ("empty.rs", ""),
        ("blank.txt", "\n\n   \n"),
    ];
    files
        .iter()
        .map(|(name, body)| {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            Doc::from_file(name.to_string(), &p, 0).expect("a fixture is readable")
        })
        .collect()
}

#[test]
fn no_reply_costs_more_than_a_small_file_and_its_label() {
    let dir = tempfile::tempdir().unwrap();
    let docs = small_docs(dir.path());

    let t = run_docs(&docs);
    t.print(
        "No reply costs more than a small file and its label",
        docs.len(),
    );
    assert_no_violations("the small files", &t.bad);

    // The branch this exists for: a reply that would have cost more than the file, answered with
    // the file. And one where the outline did pay, so the test is not only about fallbacks.
    t.expect("smart_read outline", "inflation fallback", 2);
    t.expect("smart_read outline", "outline", 1);
    t.expect("smart_read outline", "no structure", 1);
    t.expect("recall no selector", "inflation fallback", 2);
    t.expect("recall range all", "inflation fallback", 1);
}

// ── compress_logs on logs written for the purpose ────────────────────────────────────────────
//
// A source file is not a log, and nearly always comes back as itself. These sit on both sides of
// the point where compressing stops paying for the header that announces it, so that every
// branch of `compress_logs` has a case, in both ways of sending the log.

const LOG_LINE: &str = "worker 7 failed to flush the write-ahead log to disk\n";

fn synthetic_logs() -> Vec<(String, String)> {
    let mut trace =
        String::from("Exception in thread \"main\" java.lang.IllegalStateException: boom\n");
    for i in 0..30 {
        trace.push_str(&format!(
            "\tat com.example.pkg{i}.Service{i}.run(Service{i}.java:{})\n",
            40 + i
        ));
    }
    trace.push_str("Caused by: java.io.IOException: disk full\n");

    let mut logs: Vec<(String, String)> = [
        ("empty", String::new()),
        ("one line", "hello\n".to_string()),
        ("one line, no newline", "hello".to_string()),
        ("tiny", "one\ntwo\nthree\n".to_string()),
        (
            "incompressible",
            (0..40)
                .map(|i| format!("request {i} served in {} ms\n", 13 + i * 7))
                .collect(),
        ),
        ("one long line", format!("{}\n", "x".repeat(5_000))),
        // Compresses, but by less than the header costs: the band this is about.
        ("band", LOG_LINE.repeat(3)),
        ("repetitive", LOG_LINE.repeat(400)),
        (
            "repetitive, no final newline",
            LOG_LINE.repeat(400).trim_end().to_string(),
        ),
        ("blank runs", "a\n\n\n\n\n\n\nb\n".to_string()),
        (
            "blank runs, long",
            format!("start\n{}end\n", "\n".repeat(300)),
        ),
        ("crlf", "boom\r\n".repeat(50)),
        ("whitespace only", "   \n\t\n  \n".to_string()),
        ("one newline", "\n".to_string()),
        ("stack trace", trace),
        ("multi-byte", "写入日志失败：磁盘已满 🚨\n".repeat(100)),
    ]
    .into_iter()
    .map(|(n, l)| (n.to_string(), l))
    .collect();
    // From a log the header outweighs to one it does not, a line at a time.
    for n in 1..=40 {
        logs.push((format!("{n} identical line(s)"), LOG_LINE.repeat(n)));
    }
    logs
}

fn log_docs(dir: &Path) -> Vec<Doc> {
    synthetic_logs()
        .into_iter()
        .enumerate()
        .map(|(i, (name, log))| {
            let p = dir.join(format!("log{i:02}.log"));
            std::fs::write(&p, &log).unwrap();
            Doc::from_file(name, &p, 0).expect("a fixture is readable")
        })
        .collect()
}

/// The reply `compress_logs` gave before its header was counted: the header and the body,
/// whatever they cost against the log.
fn pre_fix_reply(label: &str, log: &str) -> String {
    let r = lumen_core::compress::compress_logs(log);
    format!(
        "# {label} — compressed\n\
         # {} lines → {} lines | {} tokens → {} tokens | saved {}\n\n{}",
        r.original_lines,
        r.compressed_lines,
        r.original_tokens,
        r.compressed_tokens,
        r.original_tokens as i64 - r.compressed_tokens as i64,
        r.text
    )
}

#[derive(Default)]
struct LogTally {
    bad: Vec<String>,
    /// branch -> replies that took it.
    branches: BTreeMap<&'static str, usize>,
    /// Replies on which the one before the fix cost more than the log.
    costlier_before: usize,
    /// Of those, the ones whose own header claimed the log had been made smaller.
    claimed_a_saving: usize,
}

fn check_log(t: &mut LogTally, d: &Doc, by_path: bool) {
    let (form, args, label) = if by_path {
        ("path", json!({ "path": d.path }), d.path.as_str())
    } else {
        ("text", json!({ "text": d.src }), "(inline text)")
    };
    let who = format!("{} ({form})", d.name);
    let r = match reply_of(&call(Tool::Compress, &args)) {
        Ok(r) => r,
        Err(e) => {
            t.bad.push(format!("{who}: {e}"));
            return;
        }
    };
    let s = Subject {
        name: &who,
        path: &d.path,
        src: &d.src,
        full: d.full,
    };
    let (branch, _) = check(&s, "compress_logs", Bound::Strict, &r, &mut t.bad);
    *t.branches.entry(branch).or_default() += 1;

    // Better or equal in delivered tokens, for every input: never above what was sent before.
    let before = pre_fix_reply(label, &d.src);
    let before_tokens = count_tokens(&before) as i64;
    let sent = count_tokens(&r.text) as i64;
    if sent > before_tokens {
        t.bad.push(format!(
            "{who}: the reply costs {sent} tokens, and the one before the fix cost {before_tokens}"
        ));
    }
    // Where compressing wins, and where nothing is visible to hand back, nothing changed.
    if matches!(branch, "compressed" | "blank log") && r.text != before {
        t.bad.push(format!(
            "{who}: the reply differs from the one before the fix"
        ));
    }
    if before_tokens > d.full {
        t.costlier_before += 1;
        let c = lumen_core::compress::compress_logs(&d.src);
        if c.original_tokens > c.compressed_tokens {
            t.claimed_a_saving += 1;
        }
    }
}

#[test]
fn compress_logs_costs_no_more_than_the_log_unless_the_log_is_blank() {
    let dir = tempfile::tempdir().unwrap();
    let docs = log_docs(dir.path());
    let mut t = LogTally::default();
    for d in &docs {
        check_log(&mut t, d, false);
        check_log(&mut t, d, true);
    }
    println!(
        "\n== compress_logs on {} logs, both forms ==============================",
        docs.len()
    );
    for (branch, n) in &t.branches {
        println!("  {branch:<16} {n:>4} replies");
    }
    println!(
        "  before the fix, {} replies cost more than the log, and {} of those said they saved tokens",
        t.costlier_before, t.claimed_a_saving
    );
    assert_no_violations("the synthetic logs", &t.bad);

    let got = |b: &str| t.branches.get(b).copied().unwrap_or_default();
    // Every branch was reached, or the test says nothing about it. A log is the reply when
    // compressing does not pay, and the compressed form when it does.
    assert!(got("the log itself") >= 20, "{:?}", t.branches);
    assert!(got("compressed") >= 20, "{:?}", t.branches);
    assert!(got("blank log") >= 6, "{:?}", t.branches);
    // The fix is for a reply that cost more than the log: the premise holds on these inputs, and
    // on some of them the old header said the log had shrunk.
    assert!(t.costlier_before >= 20, "premise: {}", t.costlier_before);
    assert!(t.claimed_a_saving >= 6, "premise: {}", t.claimed_a_saving);
}

// ── through the binary ───────────────────────────────────────────────────────────────────────

const INIT_ID: i64 = 999_999;

/// `calls`, made one after another on one `lumen-mcp` process the way a client makes them, with
/// the ledger that process wrote. Result `i` is call `i`: the frame the client received, and the
/// ledger row the call left behind.
fn through_the_binary(env: &[(&str, &str)], calls: &[(Tool, Value)]) -> Vec<Result<Reply, String>> {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("ledger.db");
    assert!(
        db.starts_with(std::env::temp_dir()),
        "the test ledger must live under the temp dir"
    );

    let mut input = String::new();
    let mut send = |v: Value| {
        input.push_str(&serde_json::to_string(&v).unwrap());
        input.push('\n');
    };
    send(json!({"jsonrpc":"2.0","id":INIT_ID,"method":"initialize",
        "params":{"protocolVersion":"2024-11-05","capabilities":{},
                  "clientInfo":{"name":"t","version":"0"}}}));
    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    for (i, (tool, args)) in calls.iter().enumerate() {
        send(json!({"jsonrpc":"2.0","id":i + 1,"method":"tools/call",
            "params":{"name":tool_name(*tool),"arguments":args}}));
    }

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lumen-mcp"));
    cmd.env("LUMEN_DB", &db)
        // A deadline is a property of the machine, not of the code under test.
        .env("LUMEN_RANKED_TIME_BUDGET_MS", "60000")
        .env_remove("LUMEN_RANKED_OUTLINE")
        .env_remove("LUMEN_REPEAT_READS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn lumen-mcp");
    let mut stdin = child.stdin.take().unwrap();
    // Replies can fill the pipe before the requests are all written, so the two ends of it are
    // not driven from one thread.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().unwrap();
    let written = writer.join().unwrap();
    assert!(out.status.success(), "lumen-mcp exited with {}", out.status);
    written.expect("write the requests");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let frames: BTreeMap<i64, Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| Some((v["id"].as_i64()?, v)))
        .collect();

    let conn = rusqlite::Connection::open(&db).expect("the ledger must exist");
    let mut stmt = conn
        .prepare(
            "SELECT routed_via, tokens_returned, full_tokens, saved_tokens \
             FROM read_events ORDER BY rowid",
        )
        .unwrap();
    let rows: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(rows.len(), calls.len(), "one ledger row per call");

    (0..calls.len())
        .map(|i| {
            let frame = frames
                .get(&(i as i64 + 1))
                .ok_or_else(|| "no reply frame".to_string())?;
            let result = frame
                .get("result")
                .ok_or_else(|| format!("no result in {frame}"))?;
            parse_reply(result, rows[i].clone())
        })
        .collect()
}

/// The smallest file and a middling one, of each kind the tools meet: with structure and without.
fn sample(docs: &[Doc]) -> Vec<&Doc> {
    let mut picked = Vec::new();
    for described in [true, false] {
        let kind: Vec<&Doc> = docs.iter().filter(|d| d.described == described).collect();
        assert!(
            !kind.is_empty(),
            "the corpus has no file with described={described}"
        );
        // Heaviest first, so the last is the smallest.
        picked.push(kind[kind.len() - 1]);
        picked.push(kind[kind.len() / 2]);
    }
    picked
}

/// What a client receives and what lands in the ledger is what the function returned, for the
/// same shapes as above. The checks on the function cannot see the framing, or the row the
/// binary writes after it has replied.
#[test]
fn the_binary_sends_and_records_what_the_function_returns() {
    let (docs, _) = documents();
    let dir = tempfile::tempdir().unwrap();
    let small = small_docs(dir.path());
    let logs = log_docs(dir.path());

    let mut cases: Vec<(&Doc, Shape)> = Vec::new();
    for d in sample(&docs).into_iter().chain(small.iter()) {
        cases.extend(shapes(d).into_iter().map(|sh| (d, sh)));
    }
    for d in &logs {
        cases.extend(
            shapes(d)
                .into_iter()
                .filter(|sh| sh.tool == Tool::Compress)
                .map(|sh| (d, sh)),
        );
    }
    let calls: Vec<(Tool, Value)> = cases
        .iter()
        .map(|(_, sh)| (sh.tool, sh.args.clone()))
        .collect();
    let wire = through_the_binary(&[], &calls);
    assert_eq!(wire.len(), cases.len());

    let mut bad = Vec::new();
    for ((d, sh), over_the_wire) in cases.iter().zip(wire) {
        let here = reply_of(&call(sh.tool, &sh.args));
        let (here, there) = match (here, over_the_wire) {
            (Ok(h), Ok(w)) => (h, w),
            (h, w) => {
                bad.push(format!(
                    "{} on {}: in process [{}], over the wire [{}]",
                    sh.label,
                    d.name,
                    h.err().unwrap_or_default(),
                    w.err().unwrap_or_default()
                ));
                continue;
            }
        };
        check(&d.subject(), sh.label, sh.bound, &there, &mut bad);
        if there.text != here.text || there.meta != here.meta || there.ledger != here.ledger {
            bad.push(format!(
                "{} on {}: the binary answered differently from the function (route {} against {})",
                sh.label, d.name, there.ledger.0, here.ledger.0
            ));
        }
    }
    println!("\n  {} calls through the binary", cases.len());
    assert_no_violations("through the binary", &bad);
}

/// The ranked arm is off by default and has its own way of answering a file it declines, so it
/// is driven the way it is turned on.
#[test]
fn a_ranked_smart_read_costs_no_more_than_the_file_and_its_note() {
    let (docs, _) = documents();
    let dir = tempfile::tempdir().unwrap();
    let small = small_docs(dir.path());
    // Large enough that the arm goes ahead and ranks, whatever the sample of real files does.
    let big_path = dir.path().join("big.rs");
    std::fs::write(&big_path, marked_source(MARK_A, 450)).unwrap();
    let big = Doc::from_file("big.rs".to_string(), &big_path, 0).unwrap();
    let subjects: Vec<&Doc> = small
        .iter()
        .chain(sample(&docs))
        .chain(std::iter::once(&big))
        .collect();
    let calls: Vec<(Tool, Value)> = subjects
        .iter()
        .map(|d| (Tool::SmartRead, json!({ "path": d.path })))
        .collect();

    let wire = through_the_binary(&[("LUMEN_RANKED_OUTLINE", "on")], &calls);
    let mut bad = Vec::new();
    let mut routes: BTreeMap<String, usize> = BTreeMap::new();
    for (d, w) in subjects.iter().zip(wire) {
        match w {
            Err(e) => bad.push(format!("smart_read (ranked) on {}: {e}", d.name)),
            Ok(r) => {
                *routes.entry(r.ledger.0.clone()).or_default() += 1;
                let bound = if d.src.trim().is_empty() {
                    Bound::Kept
                } else {
                    Bound::Noted
                };
                check(&d.subject(), "smart_read ranked", bound, &r, &mut bad);
            }
        }
    }
    println!("\n  ranked routes: {routes:?}");
    assert_no_violations("the ranked arm", &bad);
    // Both ways out of the arm were taken: an outline chosen by rank, and a decline that answers
    // with the file.
    assert!(
        routes.get("ranked_outline").copied().unwrap_or(0) >= 1,
        "{routes:?}"
    );
    assert!(
        routes.get("ranked_not_worth_it").copied().unwrap_or(0) >= 1,
        "{routes:?}"
    );
}

// ── a tool asked about one file never answers with another ──────────────────────────────────

/// A source file of `items` functions, each name and comment carrying `marker`.
fn marked_source(marker: &str, items: usize) -> String {
    let mut s = format!("//! Fixture for {marker}.\n\n");
    for i in 0..items {
        s.push_str(&format!(
            "/// The {marker} item number {i:04}.\npub fn {marker}_item_{i:04}(a: u32) -> u32 {{\n    let x = a.wrapping_mul({i});\n    x.wrapping_add({i})\n}}\n\n"
        ));
    }
    s
}

/// One comment line, at the end of the first file and the start of the second.
const PAD: &str = "// the same size, laid out differently\n";

/// Two files with the same size to the byte and, set here, the same modification time: everything
/// a cache keyed on a file's identity short of its path would have to go on. They differ in the
/// marker, and in the second everything sits one line and `PAD.len()` bytes lower, so a parse of
/// one that was applied to the other would point at the wrong places instead of reading the same.
fn marked_pair(dir: &Path, items: usize) -> (Doc, Doc) {
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let make = |name: &str, body: String| {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(when)
            .unwrap();
        Doc::from_file(name.to_string(), &p, 0).unwrap()
    };
    let a = make("a.rs", format!("{}{PAD}", marked_source(MARK_A, items)));
    let b = make("b.rs", format!("{PAD}{}", marked_source(MARK_B, items)));
    assert_eq!(
        a.src.len(),
        b.src.len(),
        "premise: the same size to the byte"
    );
    assert_ne!(a.src, b.src);
    (a, b)
}

const MARK_A: &str = "xalphax";
const MARK_B: &str = "xbravox";

#[test]
fn a_tool_asked_about_one_file_never_returns_the_content_of_another() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = marked_pair(dir.path(), 60);
    assert!(
        a.lines() >= THRESHOLD_LINES,
        "premise: files of a size the tools intercept"
    );

    let mut bad = Vec::new();
    let mut answered = 0;
    // Twice over, alternating between the files on every call: a memo that is wrong shows up on
    // the second visit to a file, and on the first call after the other one.
    for round in 0..2 {
        let (sa, sb) = (shapes(&a), shapes(&b));
        assert_eq!(
            sa.iter().map(|s| s.label).collect::<Vec<_>>(),
            sb.iter().map(|s| s.label).collect::<Vec<_>>(),
            "premise: the same requests are made of both files"
        );
        for (x, y) in sa.iter().zip(&sb) {
            for (d, own, other, sh) in [(&a, MARK_A, MARK_B, x), (&b, MARK_B, MARK_A, y)] {
                let reply = match reply_of(&call(sh.tool, &sh.args)) {
                    Ok(r) => r,
                    Err(e) => {
                        bad.push(format!("{} on {}: {e}", sh.label, d.name));
                        continue;
                    }
                };
                answered += 1;
                if reply.text.contains(other) {
                    bad.push(format!(
                        "round {round}: {} on {} returned content of the other file ({other})",
                        sh.label, d.name
                    ));
                }
                // The reply is about the file that was asked about, so the test is not passing
                // on replies that say nothing at all.
                if !reply.text.contains(own) && !sh.label.ends_with("no match") {
                    bad.push(format!(
                        "round {round}: {} on {} does not mention {own}",
                        sh.label, d.name
                    ));
                }
            }
        }
    }
    println!("\n  {answered} replies read for the other file's marker");
    assert_no_violations("the file pair", &bad);
    assert!(
        answered >= 2 * 2 * 15,
        "only {answered} replies were examined"
    );
}

/// The ranked arm keeps a parse of each file between calls, which is where one file's answer could
/// become another's. Driven through the binary, since the arm is turned on by the environment.
///
/// The marker alone would not show it: the outline takes its text from the file being read and
/// only its positions from the parse, so a parse of the wrong file reads as the right file at the
/// wrong places. What shows it is that a file is answered the same whatever was read before it.
#[test]
fn a_ranked_smart_read_never_returns_the_content_of_another_file() {
    let dir = tempfile::tempdir().unwrap();
    // Large enough to clear the size below which the arm declines: the cache is only reached by
    // a call that goes ahead.
    let (a, b) = marked_pair(dir.path(), 450);
    let env = [("LUMEN_RANKED_OUTLINE", "on")];
    let ask = |ds: &[&Doc]| -> Vec<(Tool, Value)> {
        ds.iter()
            .map(|d| (Tool::SmartRead, json!({ "path": d.path })))
            .collect()
    };
    let alone = |d: &Doc| -> Reply {
        through_the_binary(&env, &ask(&[d]))
            .remove(0)
            .unwrap_or_else(|e| panic!("{} read on its own: {e}", d.name))
    };
    let (alone_a, alone_b) = (alone(&a), alone(&b));
    assert_eq!(
        (alone_a.ledger.0.as_str(), alone_b.ledger.0.as_str()),
        ("ranked_outline", "ranked_outline"),
        "premise: the ranked arm answers both files"
    );
    assert_ne!(
        alone_a.text, alone_b.text,
        "premise: the two files read differently"
    );

    let order = [&a, &b, &a, &b, &b, &a];
    let wire = through_the_binary(&env, &ask(&order));
    let mut bad = Vec::new();
    for (i, (d, w)) in order.iter().zip(wire).enumerate() {
        let (own, other, expected) = if d.name == a.name {
            (MARK_A, MARK_B, &alone_a)
        } else {
            (MARK_B, MARK_A, &alone_b)
        };
        match w {
            Err(e) => bad.push(format!("call {i} on {}: {e}", d.name)),
            Ok(r) => {
                if r.ledger.0 != "ranked_outline" {
                    bad.push(format!(
                        "call {i} on {} was routed {}, not through the ranked arm",
                        d.name, r.ledger.0
                    ));
                }
                if r.text.contains(other) {
                    bad.push(format!(
                        "call {i} on {} returned content of the other file",
                        d.name
                    ));
                }
                if !r.text.contains(own) {
                    bad.push(format!("call {i} on {} does not mention {own}", d.name));
                }
                if r.text != expected.text {
                    bad.push(format!(
                        "call {i} on {} was answered differently from the same call made on its \
                         own, so something kept from an earlier call changed the answer",
                        d.name
                    ));
                }
            }
        }
    }
    assert_no_violations("the ranked file pair", &bad);
}
