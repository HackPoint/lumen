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
use lumen_core::structure::{detect_lang, outline};
use lumen_core::tokenizer::count_tokens;
use lumen_mcp::format_outline;

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

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
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
            walk(&p, out);
        } else if TagLang::detect(&p.to_string_lossy()).is_some() {
            out.push(p);
        }
    }
}

struct Measured {
    name: String,
    /// The absolute path. `name` is repo-relative for readable tables, but the tools need
    /// something they can open.
    path: String,
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
            path: path.to_string_lossy().into_owned(),
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

    // The documented claim is 5-10% of a full read. Asserted as a ceiling: if an outline
    // ever costs more than a fifth of its file, the README is no longer true and this must
    // fail before a release repeats it.
    let aggregate = outlines as f64 / full as f64;
    assert!(
        aggregate < 0.20,
        "outlines cost {:.1}% of a full read; the claim is 5-10%",
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
    let Ok(conn) = lumen_core::meter::connect_db(&db) else {
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

/// The recorded ledger's own arithmetic. Skips cleanly where there is no database.
#[test]
fn the_recorded_ledger_agrees_with_itself() {
    let Some(db) = lumen_core::meter::db_path() else {
        return;
    };
    let Ok(conn) = lumen_core::meter::connect_db(&db) else {
        return;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT routed_via, count(*), COALESCE(sum(full_tokens),0), \
         COALESCE(sum(tokens_returned),0), COALESCE(sum(saved_tokens),0) \
         FROM read_events GROUP BY routed_via ORDER BY 5 DESC",
    ) else {
        return;
    };
    let Ok(iter) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
        ))
    }) else {
        return;
    };
    let rows: Vec<(String, i64, i64, i64, i64)> = iter.flatten().collect();
    if rows.is_empty() {
        return;
    }

    println!("\n== This machine's recorded ledger ==============================");
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
        .unwrap_or(0);
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
        .unwrap_or((0, 0));
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
        "  LEAKED                   {leaked:>7} calls   <- eligible but not intercepted{}",
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

    // A leak means the hook did not fire on a file it claims to cover. There is no acceptable
    // non-zero value, so this is an equality rather than a threshold.
    assert_eq!(
        leaked, 0,
        "{leaked} reads were eligible for interception and were not intercepted"
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

/// No Lumen tool may return more than the file it was asked about.
///
/// The corpus form of the guarantee: not "this did not happen recently" but "this cannot happen",
/// checked against every real file in the repository at or above the interception threshold and
/// for every shape a caller can ask for.
#[test]
fn no_tool_returns_more_than_the_file_on_any_real_file() {
    let files = measure();
    assert!(!files.is_empty(), "corpus must not be empty");

    let mut worst: Option<(String, i64, i64, String)> = None;
    for f in &files {
        let calls = [
            ("smart_read outline", serde_json::json!({ "path": &f.path })),
            (
                "recall_file no selector",
                serde_json::json!({ "path": &f.path }),
            ),
            (
                "recall_file whole range",
                serde_json::json!({ "path": &f.path, "start_line": 1, "end_line": 999_999 }),
            ),
        ];
        for (label, args) in calls {
            let out = if label.starts_with("smart_read") {
                lumen_mcp::tool_smart_read(&args)
            } else {
                lumen_mcp::tool_recall_file(&args)
            };
            let Some(m) = out.meter else { continue };
            let over = m.returned_tokens - m.full_tokens;
            if over > 0 && worst.as_ref().map(|w| over > w.1).unwrap_or(true) {
                worst = Some((f.name.clone(), over, m.full_tokens, label.to_string()));
            }
            assert!(
                m.returned_tokens <= m.full_tokens + lumen_mcp::NOTE_ALLOWANCE,
                "{label} on {} returned {} against {} for the file (over by {over})",
                f.name,
                m.returned_tokens,
                m.full_tokens,
            );
        }
    }

    println!("\n== Worst overage across the corpus ==============================");
    match worst {
        Some((name, over, full, label)) => println!(
            "  {over} tokens ({:.2}% of the file) — {label} on {name}",
            100.0 * over as f64 / full.max(1) as f64
        ),
        None => println!("  none: every reply cost less than reading the file"),
    }
    println!(
        "  allowance is {} tokens (one explanatory line)",
        lumen_mcp::NOTE_ALLOWANCE
    );
}
