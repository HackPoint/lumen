//! The version x metric report (V5): a golden output from a fixture covering every case, and the
//! rules it must keep. No network and no spawned binary: the input is data.
//!
//! The fixture is `tests/fixtures/versions_report.json`, the golden `versions_report.golden.md`.
//! Both are written by `regen` (`cargo test -p lumen-bench --test versions_report regen --
//! --ignored`) from the builder below, so the JSON a reviewer reads is the one the test renders.

use lumen_bench::versions::daemon;
use lumen_bench::versions::report::{
    Cell, Daemon, HookKind, IdleStat, IngestStat, LatStat, LatencyEntry, Measurements, Stat,
    TokenData, VersionData, render,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn norm(s: &str) -> String {
    s.replace("\r\n", "\n")
}

// --- the fixture -----------------------------------------------------------------------------

const BUCKETS: [&str; 4] = ["<300", "300-999", "1000-2999", "3000+"];
/// Two files per bucket: lines and full tokens.
const FILES: [(u32, i64); 8] = [
    (120, 1500),
    (250, 3000),
    (500, 6000),
    (800, 10000),
    (1500, 20000),
    (2500, 30000),
    (3500, 45000),
    (5000, 60000),
];

fn tokens(outline_pct: i64, repeat: bool) -> Cell<TokenData> {
    let files = FILES
        .iter()
        .enumerate()
        .map(
            |(i, (lines, full))| lumen_bench::versions::report::FileTokens {
                path: format!("fixture/src/file_{i}.rs"),
                bucket: BUCKETS[i / 2].to_string(),
                lines: *lines,
                full: *full,
                outline: full * outline_pct / 100,
                recall: Some(400 + i as i64),
                stub: repeat.then_some(40),
                delta: repeat.then_some(150 + i as i64),
            },
        )
        .collect();
    Cell::Value(TokenData {
        files,
        repeat_na: (!repeat)
            .then(|| "this version has no repeat reads (they arrived in 1.7.0)".to_string()),
    })
}

fn no_lumen_tokens() -> Cell<TokenData> {
    let Cell::Value(mut td) = tokens(100, false) else {
        unreachable!()
    };
    for f in &mut td.files {
        f.outline = f.full.min(25_000);
        f.recall = Some(f.outline);
    }
    td.repeat_na =
        Some("no Lumen: a file is read whole each time, there is no repeat-read gate".to_string());
    Cell::Value(td)
}

fn lat(id: &str, p50_us: i64, passes_us: [i64; 3]) -> LatencyEntry {
    LatencyEntry {
        id: id.to_string(),
        cell: Cell::Value(LatStat {
            p50_ns: p50_us * 1000,
            p90_ns: p50_us * 1000 * 12 / 10,
            p99_ns: p50_us * 1000 * 15 / 10,
            pass_medians_ns: passes_us.iter().map(|p| p * 1000).collect(),
        }),
    }
}

fn lat_na(id: &str, why: &str) -> LatencyEntry {
    LatencyEntry {
        id: id.to_string(),
        cell: Cell::Na(why.to_string()),
    }
}

fn stat(median: f64, spread: f64) -> Stat {
    Stat {
        n: 5,
        median,
        min: median - spread,
        max: median + spread,
    }
}

fn idle(cpu: f64, rss_mib: u64) -> Cell<IdleStat> {
    Cell::Value(IdleStat {
        cpu_secs_per_min: cpu,
        rss_median_bytes: rss_mib * 1024 * 1024,
        rss_peak_bytes: (rss_mib + 2) * 1024 * 1024,
    })
}

fn daemon_data(
    ingest: Cell<IngestStat>,
    cold_ms: f64,
    hook: Cell<Stat>,
    cpu: f64,
    rss: u64,
) -> Daemon {
    Daemon {
        cache: "warm file cache".to_string(),
        ingest,
        cold_daemon_ms: Cell::Value(stat(cold_ms, 4.0)),
        cold_mcp_ms: Cell::Value(stat(cold_ms / 4.0, 1.0)),
        cold_hook_ms: hook,
        idle_daemon_empty: idle(cpu, rss),
        idle_daemon_after_ingest: idle(cpu * 2.0, rss * 2),
        idle_mcp: idle(cpu / 10.0, rss / 4),
    }
}

fn ingest(mb_s: f64, turns_s: f64) -> Cell<IngestStat> {
    Cell::Value(IngestStat {
        bytes_per_s: stat(mb_s * 1e6, mb_s * 1e5),
        turns_per_s: stat(turns_s, turns_s / 20.0),
    })
}

struct Spec {
    version: &'static str,
    kind: HookKind,
    outline_pct: i64,
    repeat: bool,
    floor_us: i64,
    block_us: i64,
    block_passes: [i64; 3],
    ping_us: i64,
    ping_passes: [i64; 3],
    ingest: Cell<IngestStat>,
    cold_ms: f64,
    cpu: f64,
    rss: u64,
}

fn version(s: Spec) -> VersionData {
    let script = s.kind == HookKind::Script;
    let mut latency = vec![
        lat("hook.floor", s.floor_us, [s.floor_us; 3]),
        lat("hook.intercept_block.spawn", s.block_us, s.block_passes),
        if script {
            lat_na("hook.meter_bash.spawn", "the Bash meter arrived in 1.6.0")
        } else {
            lat(
                "hook.meter_bash.spawn",
                s.block_us + 500,
                [s.block_us + 500; 3],
            )
        },
        lat("server.ping", s.ping_us, s.ping_passes),
    ];
    latency.push(lat(
        "server.smart_read.300-999",
        9_000,
        [8_900, 9_000, 9_100],
    ));
    VersionData {
        version: s.version.to_string(),
        no_lumen: false,
        provenance: format!("Lumen_{}_amd64.deb (sha256 {})", s.version, "ab".repeat(32)),
        labels: if script {
            vec!["hooks are script hook (bash + python3), interpreted from the tag".to_string()]
        } else {
            vec![]
        },
        hook_kind: s.kind,
        tokens: tokens(s.outline_pct, s.repeat),
        latency,
        daemon: daemon_data(
            s.ingest,
            s.cold_ms,
            if script {
                Cell::Na(
                    "this version's hooks are scripts, timed by the script-hook series".to_string(),
                )
            } else {
                Cell::Value(stat(s.floor_us as f64 / 1000.0, 0.2))
            },
            s.cpu,
            s.rss,
        ),
    }
}

pub fn fixture() -> Measurements {
    let nl_why = "no Lumen: there is no hook, no server and no daemon to time";
    let no_lumen = VersionData {
        version: "no Lumen".to_string(),
        no_lumen: true,
        provenance: "no Lumen: a file is read whole, with the Read cap".to_string(),
        labels: vec![],
        hook_kind: HookKind::None,
        tokens: no_lumen_tokens(),
        latency: vec![
            lat_na("hook.floor", nl_why),
            lat_na("hook.intercept_block.spawn", nl_why),
            lat_na("server.ping", nl_why),
        ],
        daemon: Daemon::all_na(nl_why),
    };
    let no_asset = "1.0.0 shipped no Linux app asset";
    let v100 = VersionData::unavailable("1.0.0", HookKind::Script, no_asset);
    let v115 = version(Spec {
        version: "1.1.5",
        kind: HookKind::Script,
        outline_pct: 30,
        repeat: false,
        floor_us: 3_000,
        block_us: 45_000,
        block_passes: [44_000, 45_000, 46_000],
        ping_us: 400,
        ping_passes: [390, 400, 410],
        ingest: ingest(20.0, 800.0),
        cold_ms: 60.0,
        cpu: 0.30,
        rss: 40,
    });
    // The ingest of 1.5.1 stalled: a failed cell, not a throughput.
    let v151 = version(Spec {
        version: "1.5.1",
        kind: HookKind::Script,
        outline_pct: 25,
        repeat: false,
        floor_us: 3_000,
        block_us: 42_000,
        block_passes: [41_000, 42_000, 43_000],
        ping_us: 380,
        ping_passes: [370, 380, 390],
        ingest: Cell::Failed("incomplete: 12 of 40 turns".to_string()),
        cold_ms: 58.0,
        cpu: 0.28,
        rss: 41,
    });
    // Binary hooks are slower than the scripts here on purpose: not compared, not a regression.
    let v160 = version(Spec {
        version: "1.6.0",
        kind: HookKind::Binary,
        outline_pct: 20,
        repeat: false,
        floor_us: 5_000,
        block_us: 6_000,
        block_passes: [5_900, 6_000, 6_100],
        ping_us: 350,
        ping_passes: [340, 350, 360],
        ingest: ingest(24.0, 960.0),
        cold_ms: 50.0,
        cpu: 0.20,
        rss: 38,
    });
    // 1.7.0: the outline got bigger (tokens and dollars worse), the block hook's runs separate
    // from 1.6.0's (worse), and the ping's passes overlap 1.6.0's (equal, never flagged).
    let v170 = version(Spec {
        version: "1.7.0",
        kind: HookKind::Binary,
        outline_pct: 24,
        repeat: true,
        floor_us: 5_000,
        block_us: 9_000,
        block_passes: [8_800, 9_000, 9_200],
        ping_us: 365,
        ping_passes: [355, 365, 375],
        ingest: ingest(26.0, 1040.0),
        cold_ms: 51.0,
        cpu: 0.21,
        rss: 39,
    });
    let v180 = version(Spec {
        version: "1.8.0",
        kind: HookKind::Binary,
        outline_pct: 20,
        repeat: true,
        floor_us: 5_000,
        block_us: 6_000,
        block_passes: [5_900, 6_000, 6_100],
        ping_us: 350,
        ping_passes: [340, 350, 360],
        ingest: ingest(28.0, 1120.0),
        cold_ms: 49.0,
        cpu: 0.20,
        rss: 38,
    });
    Measurements {
        schema: 1,
        os: "linux".to_string(),
        platform: "linux-x86_64".to_string(),
        measured_r: None,
        notes: vec!["a fixture: the figures are made up to exercise every case".to_string()],
        versions: vec![no_lumen, v100, v115, v151, v160, v170, v180],
    }
}

fn load() -> Measurements {
    let text = std::fs::read_to_string(fixtures().join("versions_report.json")).unwrap();
    Measurements::parse(&norm(&text)).unwrap()
}

#[test]
#[ignore = "writes the fixture and the golden"]
fn regen() {
    let m = fixture();
    std::fs::write(
        fixtures().join("versions_report.json"),
        m.to_json().unwrap(),
    )
    .unwrap();
    let r = render(&m).unwrap();
    std::fs::write(fixtures().join("versions_report.golden.md"), r.markdown).unwrap();
}

// --- the tests -------------------------------------------------------------------------------

#[test]
fn the_fixture_json_is_the_builder_output() {
    // Compared by what they render: serde_json reads a float back within one ulp of what the
    // builder made, which no fixed-precision cell can show.
    let (a, b) = (load(), fixture());
    assert_eq!(a.versions.len(), b.versions.len());
    assert_eq!(render(&a).unwrap().markdown, render(&b).unwrap().markdown);
}

#[test]
fn the_golden_output_matches() {
    let golden =
        norm(&std::fs::read_to_string(fixtures().join("versions_report.golden.md")).unwrap());
    let got = render(&load()).unwrap().markdown;
    assert_eq!(
        got, golden,
        "the report changed; if on purpose, run the `regen` test"
    );
}

#[test]
fn the_same_input_gives_the_same_bytes() {
    let a = render(&load()).unwrap();
    let b = render(&load()).unwrap();
    assert_eq!(a.markdown, b.markdown);
    // And through a JSON round trip of the input.
    let m = load();
    let again = Measurements::parse(&m.to_json().unwrap()).unwrap();
    assert_eq!(render(&again).unwrap().markdown, a.markdown);
}

fn row<'a>(md: &'a str, label: &str) -> Vec<&'a str> {
    let line = md
        .lines()
        .find(|l| l.starts_with(&format!("| {label}")))
        .unwrap_or_else(|| panic!("no row {label:?}"));
    line.trim_matches('|').split(" | ").map(str::trim).collect()
}

#[test]
fn na_is_a_reason_never_a_zero_and_never_in_a_delta() {
    let md = render(&load()).unwrap().markdown;
    // 1.0.0 has no asset here: every one of its cells says why.
    let r = row(&md, "smart_read outline, <300 lines");
    // columns: metric, no Lumen, 1.0.0, 1.1.5, ...
    assert!(
        r[2].starts_with("n/a — 1.0.0 shipped no Linux app asset"),
        "{}",
        r[2]
    );
    assert!(
        !r[2].contains("vs previous") && !r[2].contains("vs no Lumen"),
        "{}",
        r[2]
    );
    // 1.1.5's previous version has no figure: no delta, not a delta against zero.
    assert!(r[3].contains("no delta"), "{}", r[3]);
    // The stub of a version with no repeat reads, and its successor's delta.
    let stub = row(&md, "repeat-read stub, all sizes");
    assert!(
        stub[5].starts_with("n/a — this version has no repeat reads"),
        "{}",
        stub[5]
    );
    assert!(stub[6].contains("vs previous: no delta"), "{}", stub[6]);
    // No Lumen's hooks and daemon do not exist.
    let hook = row(&md, "hook.intercept_block.spawn p50");
    assert!(hook[1].starts_with("n/a — no Lumen"), "{}", hook[1]);
    // Dollars: 1.0.0 is n/a too, and 1.1.5's change against no Lumen is a real difference.
    let d = row(
        &md,
        "smart_read outline, 1 call, all sizes (USD net, median, R=0",
    );
    assert!(d[2].starts_with("n/a —"), "{}", d[2]);
    assert!(d[1].contains("$0.0000 (baseline)"), "{}", d[1]);
    // No table cell is a bare zero for a missing figure.
    for line in md.lines().filter(|l| l.starts_with("| ")) {
        for cell in line.trim_matches('|').split(" | ").skip(1) {
            assert_ne!(cell.trim(), "0", "{line}");
        }
    }
}

#[test]
fn script_hooks_are_labelled_and_never_compared_with_binary_hooks() {
    let r = render(&load()).unwrap();
    let md = &r.markdown;
    let floor = row(md, "hook.floor p50");
    // 1.1.5 and 1.5.1 are scripts; 1.6.0 is binary and slower than the script, which is not a
    // regression and not a delta.
    assert!(floor[3].contains("(script)"), "{}", floor[3]);
    assert!(floor[4].contains("(script)"), "{}", floor[4]);
    assert!(!floor[5].contains("(script)"), "{}", floor[5]);
    assert!(
        floor[5].contains("vs previous: no delta (script hooks and binary hooks are not compared)"),
        "{}",
        floor[5]
    );
    assert!(
        !r.regressions
            .iter()
            .any(|g| g.metric.starts_with("hook.floor") && g.newer == "1.6.0"),
        "{:?}",
        r.regressions
    );
    // Two script versions are compared with each other.
    assert!(floor[4].contains("vs previous: "), "{}", floor[4]);
    assert!(!floor[4].contains("vs previous: no delta"), "{}", floor[4]);
}

#[test]
fn an_incomplete_ingest_is_a_failed_cell_and_never_a_throughput() {
    let md = render(&load()).unwrap().markdown;
    let r = row(&md, "ingest throughput (MB/s");
    assert_eq!(r[4], "**incomplete: 12 of 40 turns**");
    let t = row(&md, "ingest throughput (turns per s");
    assert_eq!(t[4], "**incomplete: 12 of 40 turns**");
    // 1.6.0's previous version failed: no delta against it.
    assert!(r[5].contains("vs previous: no delta"), "{}", r[5]);
}

fn run_of(turns: i64, expected: u64, complete: bool) -> daemon::IngestRun {
    daemon::IngestRun {
        bytes: 40_000_000,
        elapsed_s: 2.0,
        bytes_per_s: 20_000_000.0,
        turns_per_s: turns as f64 / 2.0,
        turns,
        turns_expected: expected,
        complete,
        rows: BTreeMap::new(),
    }
}

fn series(v: f64) -> daemon::Series {
    daemon::Series {
        n: 5,
        median: v,
        min: v,
        max: v,
    }
}

fn report_with(runs: Vec<daemon::IngestRun>) -> daemon::Report {
    let ing = daemon::Ingest {
        runs,
        bytes_per_s: series(2e7),
        turns_per_s: series(100.0),
        rows: BTreeMap::new(),
        set_bytes: 1,
        set_digest: String::new(),
    };
    fn na<T>() -> daemon::Cell<T> {
        daemon::Cell::Na("x".to_string())
    }
    daemon::Report {
        version: "1.0.0".to_string(),
        os: "linux".to_string(),
        cache: "warm file cache".to_string(),
        ingest: daemon::Cell::Value(ing),
        cold_daemon_ms: daemon::Cell::Value(series(50.0)),
        cold_mcp_ms: na(),
        cold_hook_ms: na(),
        idle_daemon_empty: na(),
        idle_daemon_after_ingest: na(),
        idle_mcp: na(),
    }
}

#[test]
fn the_daemon_report_converts_an_incomplete_ingest_to_a_failure() {
    use lumen_bench::versions::report::daemon_data;
    let d = daemon_data(&report_with(vec![
        run_of(40, 40, true),
        run_of(12, 40, false),
        run_of(30, 40, false),
    ]));
    // The run with the fewest turns names the failure.
    assert_eq!(
        d.ingest,
        Cell::Failed("incomplete: 12 of 40 turns".to_string())
    );
    let ok = daemon_data(&report_with(vec![
        run_of(40, 40, true),
        run_of(40, 40, true),
    ]));
    assert!(matches!(ok.ingest, Cell::Value(_)));
    assert!(matches!(ok.cold_mcp_ms, Cell::Na(_)));
}

#[test]
fn a_worse_newer_version_is_flagged_with_its_issue_text() {
    let r = render(&load()).unwrap();
    // The block hook's runs separate between 1.6.0 and 1.7.0.
    let hook = r
        .regressions
        .iter()
        .find(|g| g.metric.starts_with("hook.intercept_block.spawn p50"))
        .expect("the separated latency is a regression");
    assert_eq!(
        (hook.older.as_str(), hook.newer.as_str()),
        ("1.6.0", "1.7.0")
    );
    assert!(
        hook.evidence.contains("the runs separate"),
        "{}",
        hook.evidence
    );
    // A higher token median is one, too.
    let tok = r
        .regressions
        .iter()
        .find(|g| {
            g.metric
                .starts_with("smart_read outline, all sizes (tokens")
        })
        .expect("a higher median is a regression");
    assert_eq!((tok.older.as_str(), tok.newer.as_str()), ("1.6.0", "1.7.0"));
    // And the dollars follow the tokens.
    assert!(
        r.regressions
            .iter()
            .any(|g| g.group.contains("Net dollars"))
    );
    // The issue carries the metric, both values, both versions and a place to look.
    let i = &r.issues[r.regressions.iter().position(|g| g == hook).unwrap()];
    assert!(i.title.contains("hook.intercept_block.spawn p50") && i.title.contains("1.7.0"));
    assert!(i.body.contains("1.6.0") && i.body.contains("1.7.0"));
    assert!(i.body.contains(&hook.old_value) && i.body.contains(&hook.new_value));
    assert!(i.body.contains("Likely cause, to confirm"));
    assert!(i.body.contains("git log --oneline v1.6.0..v1.7.0"));
    assert!(
        i.command
            .starts_with("gh issue create -R HackPoint/lumen --title '")
    );
    assert!(
        r.markdown.contains(&i.command),
        "the command is printed in the report"
    );
    // Nothing older than 1.6.0 -> 1.7.0 or newer than it is worse than its predecessor, except
    // what this fixture put there on purpose.
    assert!(
        r.regressions.iter().all(|g| g.newer == "1.7.0"),
        "{:?}",
        r.regressions
    );
}

#[test]
fn noise_is_equal_and_never_flagged() {
    let r = render(&load()).unwrap();
    // The ping moved from 350 to 365 us, but the passes overlap: equal.
    assert!(
        !r.regressions
            .iter()
            .any(|g| g.metric.starts_with("server.ping p50")),
        "{:?}",
        r.regressions
    );
    let ping = row(&r.markdown, "server.ping p50");
    assert!(
        ping[6].contains("vs previous: +4.3% (equal: ranges overlap)"),
        "{}",
        ping[6]
    );
    // A latency with no pass medians cannot separate, however far apart the medians are.
    let mut m = load();
    for v in &mut m.versions {
        for e in &mut v.latency {
            if let Cell::Value(l) = &mut e.cell {
                l.pass_medians_ns.clear();
            }
        }
    }
    let r = render(&m).unwrap();
    assert!(
        !r.regressions.iter().any(|g| g.group.contains("latency")),
        "{:?}",
        r.regressions
    );
}

#[test]
fn a_regression_is_judged_against_the_previous_version_that_is_comparable() {
    // A newer version better than the older is never listed.
    let r = render(&load()).unwrap();
    assert!(!r.regressions.iter().any(|g| g.newer == "1.8.0"));
    assert!(
        r.markdown.contains("## Regressions"),
        "the section is always there"
    );
}

#[test]
fn each_figure_shows_its_change_against_the_previous_version_and_against_no_lumen() {
    let md = render(&load()).unwrap().markdown;
    let r = row(&md, "smart_read outline, all sizes (tokens");
    // 1.7.0: 24% of the file against 20% in 1.6.0, and against no Lumen (the whole file).
    assert!(r[6].contains("vs previous: +"), "{}", r[6]);
    assert!(r[6].contains("**worse**"), "{}", r[6]);
    assert!(r[6].contains("vs no Lumen: -"), "{}", r[6]);
    // No Lumen's own column has no deltas.
    assert!(!r[1].contains("vs "), "{}", r[1]);
}

#[test]
fn the_dollar_rows_say_modelled_and_that_the_measured_r_is_not_available_yet() {
    let md = render(&load()).unwrap().markdown;
    assert!(md.contains("MODELLED"));
    assert!(md.contains("R from the no-Lumen E2E arm: not available yet"));
    assert!(md.contains("R=0 (floor)"));
    assert!(!md.contains("no-Lumen E2E median)"));
    let mut m = load();
    m.measured_r = Some(7);
    let md = render(&m).unwrap().markdown;
    assert!(md.contains("R=7 (no-Lumen E2E median)"));
    assert!(md.contains("R=0 (floor)"));
    assert!(!md.contains("not available yet"));
}

#[test]
fn a_dollar_figure_is_the_engines_and_a_loss_stays_negative() {
    // At R=0 an outline's net is negative on Sonnet 5.5 (score.rs): the table shows the minus.
    let md = render(&load()).unwrap().markdown;
    let d = row(
        &md,
        "smart_read outline, 1 call, all sizes (USD net, median, R=0",
    );
    assert!(d[4].starts_with("-$0."), "{}", d[4]);
}

#[test]
fn the_report_never_calls_gh_and_has_no_run_path() {
    // `gh` is named only inside the text of a printed command, never as a process to spawn.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/versions");
    let spawn = ["Command::new(", "\"gh\")"].concat();
    for f in ["report.rs", "collect.rs"] {
        let text = std::fs::read_to_string(src.join(f)).unwrap();
        assert!(!text.contains(&spawn), "{f} spawns gh");
    }
}

#[test]
fn a_bad_input_is_refused() {
    let mut m = load();
    m.schema = 99;
    assert!(Measurements::parse(&m.to_json().unwrap()).is_err());
    let mut m = load();
    m.versions.push(m.versions[2].clone());
    assert!(render(&m).is_err(), "a version listed twice");
}
