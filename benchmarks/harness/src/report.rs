//! The result document and its human-readable summary.
//!
//! `Results` is serialised with serde from plain structs and `BTreeMap`s, so the key order is the
//! field order below on every run. Nothing is built from ad-hoc JSON values.

use crate::builds::Build;
use crate::corpus::{BUCKETS, Corpus, bucket_of};
use crate::env::Environment;
use crate::latency::{BuildLatency, Series, Subject};
use crate::stats::Dist;
use crate::tokens::{Aggregates, GroupStat, ModeStat, Named};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write;

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Results {
    pub schema: u32,
    pub label: String,
    /// A label that is not `MAJOR.MINOR.PATCH` is a pre-release measurement of some commit.
    pub pre_release: bool,
    pub tokenizer: TokenizerInfo,
    pub corpus: Vec<CorpusInfo>,
    pub builds: Vec<BuildInfo>,
    pub latency: Option<LatencySection>,
    pub model: Model,
    pub environment: Environment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizerInfo {
    pub name: String,
    pub implementation: String,
    pub is_proxy_for_claude: bool,
    pub caveat: String,
    pub counted: String,
    pub line_endings: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusInfo {
    pub name: String,
    pub url: String,
    pub license: String,
    pub sha: String,
    pub note: String,
    /// SHA-256 over `path TAB blob-id NL` of every candidate file.
    pub fingerprint: String,
    pub candidate_files: usize,
    pub measured_files: usize,
    pub skipped_files: usize,
    pub files_by_bucket: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokensSection {
    /// SHA-256 of this build's per-file token rows with the build column held at a fixed value
    /// (so it does not depend on the label). Equal digests mean every token figure is equal,
    /// across builds, labels, machines and operating systems.
    pub digest: String,
    pub files: usize,
    pub aggregates: Aggregates,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInfo {
    pub label: String,
    pub role: String,
    pub sha: String,
    pub dirty: bool,
    pub profile: String,
    pub profile_note: String,
    pub source: String,
    /// How the build learns whose call it is serving: `env` (a session id read when the server
    /// starts) or `hook` (a record per call, written by the PreToolUse hook). Found by running the
    /// build, not by reading its usage text.
    pub identity: String,
    pub identity_note: String,
    pub reported_version: String,
    pub tokens: Option<TokensSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencySection {
    /// Recorded runs per series, pooled over all passes.
    pub runs: usize,
    pub passes: usize,
    pub runs_per_pass: usize,
    /// Discarded warm-up runs per series per pass.
    pub warmup_runs: usize,
    pub minimum_runs: usize,
    pub clock: String,
    pub warmup_note: String,
    /// How p90 and p99 are computed.
    pub percentile_method: String,
    /// What is kept of the individual runs.
    pub samples_note: String,
    pub subjects: Vec<Subject>,
    pub builds: Vec<BuildLatency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    /// Every dollar figure in this document is this, never a measurement.
    pub status: String,
    pub source: String,
    pub rates_usd_per_million_tokens: Rates,
    pub rates_note: String,
    pub context_tokens: f64,
    pub output_tokens: f64,
    pub rounds_remaining: f64,
    pub inputs_source: String,
    pub formula: String,
    pub value_per_token_usd: f64,
    pub extra_round_cost_usd: f64,
    pub s_min_tokens: Option<f64>,
}

fn round9(x: f64) -> f64 {
    (x * 1e9).round() / 1e9
}

/// The economics model, as the code in `lumen-core` defines it.
pub fn model() -> Model {
    let e = lumen_core::econ::Econ::default();
    use lumen_core::rates;
    Model {
        status: "modelled".to_string(),
        source: "crates/lumen-core/src/econ.rs (Econ::default) and crates/lumen-core/src/rates.rs"
            .to_string(),
        rates_usd_per_million_tokens: Rates {
            input: round9(rates::INPUT * 1e6),
            output: round9(rates::OUTPUT * 1e6),
            cache_read: round9(rates::CACHE_READ * 1e6),
            cache_write: round9(rates::CACHE_WRITE * 1e6),
        },
        rates_note:
            "rates.rs labels these Sonnet 3.5 / 3.7 / 4 pricing. The harness does not check \
                     them against a current price list; they are the model's inputs, not a quote."
                .to_string(),
        context_tokens: e.context_tokens,
        output_tokens: e.output_tokens,
        rounds_remaining: e.rounds_remaining,
        inputs_source: format!(
            "{}: means over 19,161 main-agent turns of the Lumen author's own sessions (econ.rs); \
             another user's profile differs",
            e.source.as_str()
        ),
        formula: "value of one saved token = (cache_write + cache_read x rounds_remaining) / 1e6 \
                  dollars; one intercepted Read costs one extra round = (context x cache_read + \
                  output x output_rate) / 1e6 dollars; a dollar figure below is the median saved \
                  tokens of a call x the value of one token"
            .to_string(),
        value_per_token_usd: round9(e.value_per_token()),
        extra_round_cost_usd: round9(e.round_cost()),
        s_min_tokens: e.s_min().map(|s| s.round()),
    }
}

pub fn tokenizer_info() -> TokenizerInfo {
    TokenizerInfo {
        name: "cl100k_base".to_string(),
        implementation: "lumen_core::tokenizer::count_tokens, the function the meter uses"
            .to_string(),
        is_proxy_for_claude: true,
        caveat: "cl100k_base is not Claude's tokenizer. Counts compare modes with one another; \
                 they are not a bill and not the number of tokens Claude would charge."
            .to_string(),
        counted: "the reply text of a tool call. The JSON-RPC envelope and `_meta` are not \
                  counted, exactly as the meter does not count them."
            .to_string(),
        line_endings: "files are read byte for byte from the git object database; no line-ending \
                       conversion happens anywhere"
            .to_string(),
    }
}

pub fn corpus_info(c: &Corpus) -> CorpusInfo {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for b in &BUCKETS {
        by.insert(b.name.to_string(), 0);
    }
    for f in &c.files {
        if let Some(n) = by.get_mut(BUCKETS[bucket_of(f.lines)].name) {
            *n += 1;
        }
    }
    CorpusInfo {
        name: c.name.clone(),
        url: c.url.clone(),
        license: c.license.clone(),
        sha: c.sha.clone(),
        note: c.note.clone(),
        fingerprint: c.fingerprint.clone(),
        candidate_files: c.candidates,
        measured_files: c.files.len(),
        skipped_files: c.skipped.len(),
        files_by_bucket: by,
    }
}

pub fn build_info(b: &Build, version: &str, tokens: Option<TokensSection>) -> BuildInfo {
    BuildInfo {
        label: b.label.clone(),
        role: b.role.to_string(),
        sha: b.sha.clone(),
        dirty: b.dirty,
        profile: b.profile.to_string(),
        profile_note: b.profile_note.clone(),
        source: b.source.clone(),
        identity: b.identity.label().to_string(),
        identity_note: b.identity.describe().to_string(),
        reported_version: version.to_string(),
        tokens,
    }
}

/// `1.8.0` is a release label; anything else is a measurement of some commit before release.
pub fn is_release_label(label: &str) -> bool {
    let parts: Vec<&str> = label.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

// ── the summary ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Mode {
    Outline,
    Recall,
    Stub,
    Delta,
}

const MODES: [Mode; 4] = [Mode::Outline, Mode::Recall, Mode::Stub, Mode::Delta];

impl Mode {
    fn title(self) -> &'static str {
        match self {
            Mode::Outline => "outline: `smart_read(path)`, the first read",
            Mode::Recall => "recall: `recall_file(path, [one representative item])`",
            Mode::Stub => "repeat-read stub: the second identical `smart_read`, repeat reads on",
            Mode::Delta => {
                "repeat-read delta: `smart_read` after one small edit to the file, repeat reads on"
            }
        }
    }
    fn short(self) -> &'static str {
        match self {
            Mode::Outline => "outline",
            Mode::Recall => "recall",
            Mode::Stub => "stub",
            Mode::Delta => "delta",
        }
    }
    fn of(self, g: &GroupStat) -> &ModeStat {
        match self {
            Mode::Outline => &g.outline,
            Mode::Recall => &g.recall,
            Mode::Stub => &g.stub,
            Mode::Delta => &g.delta,
        }
    }
}

fn dist_cell(d: Option<&Dist>) -> (String, String) {
    d.map_or(("-".to_string(), "-".to_string()), |d| {
        (d.median.to_string(), d.p90.to_string())
    })
}

fn usd_cell(v: Option<f64>) -> String {
    v.map_or("-".to_string(), |v| format!("${v:.4}"))
}

fn routes_cell(m: &BTreeMap<String, usize>) -> String {
    if m.is_empty() {
        return "-".to_string();
    }
    m.iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A duration in nanoseconds, in the unit a human reads it in.
pub fn fmt_ns(ns: i64) -> String {
    if ns < 1_000_000 {
        format!("{:.0} us", ns as f64 / 1e3)
    } else {
        format!("{:.2} ms", ns as f64 / 1e6)
    }
}

fn find_group<'a>(
    list: &'a [Named],
    bucket: Option<&str>,
    lang: Option<&str>,
) -> Option<&'a GroupStat> {
    list.iter()
        .find(|n| n.bucket.as_deref() == bucket && n.language.as_deref() == lang)
        .map(|n| &n.stat)
}

/// One table: a row per group, `full` once, then median / p90 / modelled dollars per build.
fn token_table(r: &Results, mode: Mode, by_language: bool) -> String {
    let tokens: Vec<(&BuildInfo, &Aggregates)> = r
        .builds
        .iter()
        .filter_map(|b| b.tokens.as_ref().map(|t| (b, &t.aggregates)))
        .collect();
    let Some((_, first)) = tokens.first() else {
        return String::new();
    };
    let axis = if by_language {
        "language"
    } else {
        "bucket (lines)"
    };
    let mut s = String::new();
    let _ = write!(s, "| {axis} | n | full med | full p90 |");
    for (b, _) in &tokens {
        let _ = write!(s, " {0} med | {0} p90 | {0} modelled $/call |", b.label);
    }
    s.push('\n');
    s.push_str("|---|---:|---:|---:|");
    for _ in &tokens {
        s.push_str("---:|---:|---:|");
    }
    s.push('\n');
    let rows: Vec<(String, Option<&str>, Option<&str>)> = if by_language {
        first
            .by_language
            .iter()
            .filter_map(|n| {
                n.language
                    .as_deref()
                    .map(|l| (l.to_string(), None, Some(l)))
            })
            .collect()
    } else {
        first
            .by_bucket
            .iter()
            .filter_map(|n| n.bucket.as_deref().map(|b| (b.to_string(), Some(b), None)))
            .collect()
    };
    for (name, b, l) in rows {
        let Some(g0) = find_group(
            if by_language {
                &first.by_language
            } else {
                &first.by_bucket
            },
            b,
            l,
        ) else {
            continue;
        };
        let (fm, fp) = dist_cell(g0.full.as_ref());
        let n = mode
            .of(g0)
            .returned
            .as_ref()
            .map_or("-".to_string(), |d| d.n.to_string());
        let _ = write!(s, "| {name} | {n} | {fm} | {fp} |");
        for (_, agg) in &tokens {
            let list = if by_language {
                &agg.by_language
            } else {
                &agg.by_bucket
            };
            match find_group(list, b, l) {
                Some(g) => {
                    let m = mode.of(g);
                    let (med, p90) = dist_cell(m.returned.as_ref());
                    let _ = write!(s, " {med} | {p90} | {} |", usd_cell(m.modelled_usd_median));
                }
                None => s.push_str(" - | - | - |"),
            }
        }
        s.push('\n');
    }
    s
}

fn route_table(r: &Results) -> String {
    let tokens: Vec<(&BuildInfo, &Aggregates)> = r
        .builds
        .iter()
        .filter_map(|b| b.tokens.as_ref().map(|t| (b, &t.aggregates)))
        .collect();
    let Some((_, first)) = tokens.first() else {
        return String::new();
    };
    let mut s = String::from("| bucket | mode |");
    for (b, _) in &tokens {
        let _ = write!(s, " {} routes |", b.label);
    }
    s.push_str("\n|---|---|");
    for _ in &tokens {
        s.push_str("---|");
    }
    s.push('\n');
    for n in &first.by_bucket {
        let Some(bucket) = n.bucket.as_deref() else {
            continue;
        };
        for mode in MODES {
            let _ = write!(s, "| {bucket} | {} |", mode.short());
            for (_, agg) in &tokens {
                let cell = find_group(&agg.by_bucket, Some(bucket), None)
                    .map_or("-".to_string(), |g| routes_cell(&mode.of(g).routes));
                let _ = write!(s, " {cell} |");
            }
            s.push('\n');
        }
    }
    s
}

fn latency_table(sec: &LatencySection, prefixes: &[&str]) -> String {
    let builds = &sec.builds;
    if builds.is_empty() {
        return String::new();
    }
    let mut s = String::from("| series | subject | n |");
    for b in builds {
        let _ = write!(s, " {0} median | {0} p90 | {0} p99 |", b.label);
    }
    s.push_str("\n|---|---|---:|");
    for _ in builds {
        s.push_str("---:|---:|---:|");
    }
    s.push('\n');
    // Prefix order first; within a prefix the candidate's series, then any only the baseline has.
    let mut ids: Vec<&str> = Vec::new();
    for prefix in prefixes {
        for b in builds.iter().rev() {
            for x in &b.series {
                if x.id.starts_with(prefix) && !ids.contains(&x.id.as_str()) {
                    ids.push(x.id.as_str());
                }
            }
        }
    }
    for id in ids {
        let subject = builds
            .iter()
            .find_map(|b| {
                b.series
                    .iter()
                    .find(|x| x.id == id)
                    .and_then(|x| x.subject.as_deref())
            })
            .map_or("-".to_string(), |s| s.to_string());
        let n = builds
            .iter()
            .find_map(|b| {
                b.series
                    .iter()
                    .find(|x| x.id == id)
                    .and_then(|x| x.lat.as_ref())
                    .map(|l| l.n)
            })
            .map_or("-".to_string(), |n| n.to_string());
        let _ = write!(s, "| `{id}` | {subject} | {n} |");
        for b in builds {
            match b.series.iter().find(|x| x.id == id) {
                Some(Series { lat: Some(l), .. }) => {
                    let _ = write!(
                        s,
                        " {} | {} | {} |",
                        fmt_ns(l.median_ns),
                        fmt_ns(l.p90_ns),
                        fmt_ns(l.p99_ns)
                    );
                }
                Some(_) => s.push_str(" n/a (see notes) | | |"),
                None => s.push_str(" - | | |"),
            }
        }
        s.push('\n');
    }
    s
}

fn unusual_statuses(sec: &LatencySection) -> String {
    let mut s = String::new();
    for b in &sec.builds {
        for x in &b.series {
            if x.status != "measured" {
                let routes = if x.routes.is_empty() {
                    String::new()
                } else {
                    format!(" (routes: {})", routes_cell(&x.routes))
                };
                let _ = writeln!(s, "- `{}` on {}: {}{}", x.id, b.label, x.status, routes);
            }
        }
    }
    s
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

fn opt<T: std::fmt::Display>(v: &Option<T>) -> String {
    v.as_ref().map_or("unknown".to_string(), |v| v.to_string())
}

/// How p90 and p99 are computed, with what that means for `n` recorded runs.
pub fn percentile_note(n: usize) -> String {
    let rank = |p: usize| (p * n).div_ceil(100).max(1);
    format!(
        "nearest-rank: the percentile p of n runs is the run at rank ceil(p x n / 100) of the \
         sorted runs, fastest first, with no interpolation. With n = {n} the median is run {} \
         (of the two middle runs of an even count, the faster), p90 is run {}, and p99 is run {}, \
         which leaves the slowest {} runs above it.",
        rank(50),
        rank(90),
        rank(99),
        n.saturating_sub(rank(99))
    )
}

/// What is kept of the individual runs: their pass medians, not the runs.
pub const SAMPLES_NOTE: &str = "not written. Each series carries `pass_medians_ns`, the median \
    of each pass on its own, in pass order: how far they sit from one another is how much the \
    machine moved while the run was going on.";

/// Of the series with a figure, how many had the median of every pass within 20% of the pooled
/// median. `(within, total)`.
fn pass_stability(sec: &LatencySection) -> (usize, usize) {
    let (mut within, mut total) = (0, 0);
    for b in &sec.builds {
        for x in &b.series {
            let Some(l) = &x.lat else { continue };
            if x.pass_medians_ns.is_empty() {
                continue;
            }
            total += 1;
            let m = l.median_ns as f64;
            if x.pass_medians_ns
                .iter()
                .all(|&p| (0.8..=1.2).contains(&(p as f64 / m)))
            {
                within += 1;
            }
        }
    }
    (within, total)
}

/// A hook that got slower between the baseline and the candidate, when it did, with the cause the
/// code gives. Built from this run's own medians, so it says nothing when the run shows nothing.
fn intercept_block_note(r: &Results, sec: &LatencySection) -> Option<String> {
    const ID: &str = "hook.intercept_block.spawn";
    let role_of = |label: &str| {
        r.builds
            .iter()
            .find(|b| b.label == label)
            .map(|b| b.role.as_str())
    };
    let med = |b: &BuildLatency, id: &str| {
        b.series
            .iter()
            .find(|x| x.id == id)
            .and_then(|x| x.lat.as_ref())
            .map(|l| l.median_ns)
    };
    let base = sec
        .builds
        .iter()
        .find(|b| role_of(&b.label) == Some("baseline"))?;
    let cand = sec
        .builds
        .iter()
        .find(|b| role_of(&b.label) == Some("candidate"))?;
    let (b, c) = (med(base, ID)?, med(cand, ID)?);
    if c - b < 500_000 || (c as f64) < b as f64 * 1.1 {
        return None;
    }
    let pass = match (
        med(base, "hook.intercept_pass.spawn"),
        med(cand, "hook.intercept_pass.spawn"),
    ) {
        (Some(pb), Some(pc)) => format!(
            " The hook that lets a Read through (`hook.intercept_pass.spawn`) is {} on {} and {} \
             on {}, so the extra time is in the blocking path, not in starting the process.",
            fmt_ns(pc),
            cand.label,
            fmt_ns(pb),
            base.label
        ),
        _ => String::new(),
    };
    Some(format!(
        "`{ID}` is {} slower on {} than on {} ({} against {}, medians as measured on this \
         machine in this run). Reading the code, the cause is the lookup #42 (the `recall_file` \
         redirect) added: on every blocked first Read of a file the hook now asks the ledger, \
         with a read-only query (`meter::lumen_answered`), whether Lumen already answered for \
         that file, before it decides what to tell the model; {} makes no such lookup. It is a \
         cost of that redirect, not noise.{pass}",
        fmt_ns(c - b),
        cand.label,
        base.label,
        fmt_ns(c),
        fmt_ns(b),
        base.label
    ))
}

pub fn summary(r: &Results) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Lumen benchmarks: {}\n", r.label);
    if r.pre_release {
        let cand = r.builds.iter().find(|b| b.role == "candidate");
        let at = cand.map_or("unknown".to_string(), |b| b.sha.chars().take(12).collect());
        let _ = writeln!(
            s,
            "> **Pre-release measurement.** This is the harness run on commit `{at}`, before the \
             1.8.0 release. It is not a 1.8.0 result and must not be quoted as one.\n"
        );
    }
    s.push_str(
        "Per-call figures, per file-size bucket and per language. There is no overall \
         \"percent saved\" here on purpose: how much a person saves overall depends on their \
         usage mix (how many reads are large, how many repeat).\n\n",
    );
    let _ = writeln!(
        s,
        "- **MEASURED**: token counts (real `{}` tokenizer) and latencies (monotonic clock).\n\
         - **MODELLED**: every dollar figure. `{}`. Not measured, not a bill.\n\
         - Tokenizer: `{}` is a proxy for Claude's tokenizer. {}\n",
        r.tokenizer.name, r.model.source, r.tokenizer.name, r.tokenizer.caveat
    );

    s.push_str("## Builds\n\n| label | role | commit | profile | per-call identity | reports version |\n|---|---|---|---|---|---|\n");
    for b in &r.builds {
        let _ = writeln!(
            s,
            "| {} | {} | `{}`{} | {} | {} | {} |",
            b.label,
            b.role,
            b.sha,
            if b.dirty { " (dirty tree)" } else { "" },
            b.profile_note,
            b.identity_note,
            b.reported_version
        );
    }

    let e = &r.environment;
    let _ = write!(
        s,
        "\n## Machine\n\n- OS: {} ({} {})\n- CPU: {}, {} logical cores{}{}\n- RAM: {}\n{}- Toolchain: {} / {}\n- Started: {}\n- Load at start: {}; at end: {}\n",
        opt(&e.os_version),
        e.os,
        e.arch,
        opt(&e.cpu_model),
        opt(&e.cpu_logical_cores),
        e.cpu_physical_cores
            .map_or(String::new(), |p| format!(", {p} physical")),
        e.cpu_core_layout
            .as_ref()
            .map_or(String::new(), |l| format!(" ({l})")),
        e.ram_bytes.map_or("unknown".to_string(), gib),
        e.power
            .as_ref()
            .map_or(String::new(), |p| format!("- Power: {p}\n")),
        opt(&e.rustc),
        opt(&e.cargo),
        e.started_utc,
        opt(&e.load_at_start),
        opt(&e.load_at_end),
    );
    if let Some(f) = &e.rustflags {
        let _ = writeln!(s, "- RUSTFLAGS: `{f}`");
    }
    for p in &e.probe_errors {
        let _ = writeln!(s, "- probe failed: {p}");
    }

    s.push_str("\n## Corpus\n\n| repository | license | commit | measured files | by bucket |\n|---|---|---|---:|---|\n");
    for c in &r.corpus {
        let by = BUCKETS
            .iter()
            .map(|b| {
                let n = c
                    .files_by_bucket
                    .get(b.name)
                    .map_or("?".to_string(), |n| n.to_string());
                format!("{} {n}", b.name)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            s,
            "| [{}]({}) | {} | `{}` | {} | {} |",
            c.name, c.url, c.license, c.sha, c.measured_files, by
        );
    }

    if r.builds.iter().any(|b| b.tokens.is_some()) {
        s.push_str(
            "\n## Tokens per call (MEASURED)\n\nMedian and p90 over files, of the tokens the call \
             returned. `full` is the file's own tokens (what a plain Read puts in context), over \
             every file of the group; `n` is the files the mode applies to (a file with no named \
             item has no recall). The dollar column is MODELLED: the median tokens saved by the \
             call, times the model's value of one saved token. It does not include the one extra \
             round an intercepted Read costs (below).\n",
        );
        for by_language in [false, true] {
            let _ = writeln!(
                s,
                "\n### By {}\n",
                if by_language {
                    "language"
                } else {
                    "file-size bucket"
                }
            );
            for mode in MODES {
                let _ = writeln!(
                    s,
                    "**{}**\n\n{}",
                    mode.title(),
                    token_table(r, mode, by_language)
                );
            }
        }
        s.push_str("### Routes\n\nHow the server answered, from its own ledger. A route other than the mode's own is a fall-back the benchmark reports rather than hides: `would_inflate` is the guard returning the file because an outline would not be smaller; `unchanged_not_smaller`, `delta_too_big` and `no_base` are the repeat-read gate declining.\n\n");
        s.push_str(&route_table(r));
        let digests: Vec<String> = r
            .builds
            .iter()
            .filter_map(|b| {
                b.tokens
                    .as_ref()
                    .map(|t| format!("{} `{}`", b.label, t.digest))
            })
            .collect();
        let _ = writeln!(
            s,
            "\nToken table digests (SHA-256 of the per-file rows of `*-tokens.tsv` with the build column blanked, so a digest does not depend on a label; equal digests mean equal token figures): {}",
            digests.join("; ")
        );
    }

    if let Some(l) = &r.latency {
        let (stable, of) = pass_stability(l);
        let _ = write!(
            s,
            "\n## Latency (MEASURED)\n\n{} recorded runs per series: {} passes of {}, pooled. In each pass the builds take turns (the order flips every pass), and each series first has {} warm-up runs that are discarded ({}). Clock: {}. \
             Release builds, built here. Medians vary with the machine and its load, and this document makes no claim about how closely another run will land. What this run shows about itself: in {stable} of {of} series, the median of every one of the {} passes is within 20% of the series' pooled median (per-pass medians are in the JSON). `benchmarks/run.sh compare <this file>` re-measures on another machine and prints, per series, the ratio to this run, the same ratio after dividing by the process floor, how many series are within 20%, and whether that machine looks like this one.\n\n\
             Percentiles are {} Raw samples: {}\n\n",
            l.runs,
            l.passes,
            l.runs_per_pass,
            l.warmup_runs,
            l.warmup_note,
            l.clock,
            l.passes,
            l.percentile_method,
            l.samples_note
        );
        s.push_str("Subjects (corpus files the series run on):\n\n| bucket | file | lines | bytes | recall item |\n|---|---|---:|---:|---|\n");
        for x in &l.subjects {
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | `{}` |",
                x.bucket, x.rel, x.lines, x.bytes, x.recall_item
            );
        }
        s.push_str("\n### Hooks, as the harness sees them: process spawn to exit\n\nThe whole hook as Claude Code runs it. `hook.floor` is a hook invocation that does no work, so it is the cost of the process itself; the work is what each row adds to it. `hook.per_call_identity` is the PreToolUse hook on a call of one of Lumen's own MCP tools, which records who is calling; a build either has it (the series is timed) or does not (the slot says `n/a`, and why). Which one is decided by running the build, not by reading its usage text.\n\n");
        s.push_str(&latency_table(
            l,
            &[
                "hook.floor",
                "hook.intercept_pass.spawn",
                "hook.intercept_block.spawn",
                "hook.meter_read.spawn",
                "hook.meter_bash.spawn",
                "hook.per_call_identity",
            ],
        ));
        if let Some(note) = intercept_block_note(r, l) {
            let _ = write!(s, "\nNote: {note}\n");
        }
        s.push_str("\n### Hooks, work only: `hook::run_with` called in-process, warm (only for a build made from this checkout)\n\nNo process start, a warm tokenizer. A fresh hook process pays tokenizer start-up that these calls do not, which is part of the spawn figures above.\n\n");
        s.push_str(&latency_table(
            l,
            &[
                "hook.intercept_pass.inproc",
                "hook.intercept_block.inproc",
                "hook.meter_read.inproc",
                "hook.meter_bash.inproc",
            ],
        ));
        s.push_str("\n### Derived: hook work above the process floor\n\n`median(spawn) - median(floor)`, in each build. A derived figure, not a series of its own.\n\n| hook |");
        for b in &l.builds {
            let _ = write!(s, " {} |", b.label);
        }
        s.push_str("\n|---|");
        for _ in &l.builds {
            s.push_str("---:|");
        }
        s.push('\n');
        if let Some(first) = l.builds.first() {
            for d in &first.derived {
                let _ = write!(s, "| `{}` |", d.id);
                for b in &l.builds {
                    let cell = b
                        .derived
                        .iter()
                        .find(|x| x.id == d.id)
                        .and_then(|x| x.median_ns)
                        .map_or("n/a".to_string(), fmt_ns);
                    let _ = write!(s, " {cell} |");
                }
                s.push('\n');
            }
        }
        s.push_str("\n### Server calls, round trip as a client sees it\n\nFrom just before the request is written to the arrival of the reply line. `server.ping` is the round trip with no work; the ledger row is written after the reply and is not in the interval. For a build with per-call identity every call in these series also carries the hook's record and `_meta` id, so the server's claim of it is inside the interval; `server.identity.placed` against `server.identity.unplaced` (the same call with no record and no id) isolates that claim. Only a build with per-call identity has those two series.\n\n");
        s.push_str(&latency_table(l, &["server."]));
        let u = unusual_statuses(l);
        if !u.is_empty() {
            let _ = write!(
                s,
                "\nNotes on series that are not plain measurements:\n\n{u}"
            );
        }
        s.push_str(
            "\nLoad on the machine around the whole latency run (every build, every pass):\n\n",
        );
        if let Some(b) = l.builds.first() {
            let _ = writeln!(s, "- before: {}; after: {}", b.load_before, b.load_after);
        }
    }

    let m = &r.model;
    let _ = write!(
        s,
        "\n## Dollars (MODELLED)\n\nEvery dollar figure above is `{}`: a model's arithmetic on measured token counts, not a measured cost.\n\n\
         - Source: {}\n- Rates, USD per million tokens: input {}, output {}, cache read {}, cache write {}. {}\n\
         - Inputs: context {} tokens, output {} tokens, {} rounds remaining. {}\n\
         - Value of one saved token: ${} (modelled). One extra round for an intercepted Read: ${} (modelled), which {} saved tokens pay for.\n\
         - Formula: {}\n",
        m.status,
        m.source,
        m.rates_usd_per_million_tokens.input,
        m.rates_usd_per_million_tokens.output,
        m.rates_usd_per_million_tokens.cache_read,
        m.rates_usd_per_million_tokens.cache_write,
        m.rates_note,
        m.context_tokens,
        m.output_tokens,
        m.rounds_remaining,
        m.inputs_source,
        m.value_per_token_usd,
        m.extra_round_cost_usd,
        m.s_min_tokens
            .map_or("an unknown number of".to_string(), |t| format!("{t:.0}")),
        m.formula,
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_three_part_number_is_a_release_label() {
        assert!(is_release_label("1.8.0"));
        assert!(is_release_label("10.0.12"));
        assert!(!is_release_label("current-main"));
        assert!(!is_release_label("1.8"));
        assert!(!is_release_label("1.8.0-rc1"));
        assert!(!is_release_label("1..0"));
    }

    #[test]
    fn durations_read_in_a_human_unit() {
        assert_eq!(fmt_ns(850), "1 us");
        assert_eq!(fmt_ns(123_000), "123 us");
        assert_eq!(fmt_ns(2_500_000), "2.50 ms");
    }

    #[test]
    fn the_model_says_what_it_is() {
        let m = model();
        assert_eq!(m.status, "modelled");
        assert_eq!(m.rates_usd_per_million_tokens.cache_read, 0.5);
        assert_eq!(m.rates_usd_per_million_tokens.cache_write, 6.25);
        assert_eq!(m.value_per_token_usd, 0.00003875);
        assert!(
            m.s_min_tokens
                .is_some_and(|s| (5_000.0..6_000.0).contains(&s))
        );
    }

    #[test]
    fn percentiles_say_they_are_nearest_rank_and_what_that_means_for_n() {
        let t = percentile_note(200);
        assert!(t.starts_with("nearest-rank"), "{t}");
        assert!(t.contains("median is run 100"), "{t}");
        assert!(t.contains("p90 is run 180"), "{t}");
        assert!(t.contains("p99 is run 198"), "{t}");
        assert!(t.contains("slowest 2 runs"), "{t}");
        // A small n does not make a rank of zero.
        assert!(percentile_note(1).contains("median is run 1"));
    }

    fn lat_series(id: &str, ns: i64, passes: &[i64]) -> Series {
        Series {
            id: id.to_string(),
            what: String::new(),
            subject: None,
            status: "measured".to_string(),
            lat: crate::stats::lat(&[ns]),
            routes: BTreeMap::new(),
            pass_medians_ns: passes.to_vec(),
            raw: vec![],
        }
    }

    fn two_builds(base_block: i64, cand_block: i64) -> Results {
        let info = |label: &str, role: &str| BuildInfo {
            label: label.to_string(),
            role: role.to_string(),
            sha: "0".repeat(40),
            dirty: false,
            profile: "release".to_string(),
            profile_note: String::new(),
            source: String::new(),
            identity: "env".to_string(),
            identity_note: String::new(),
            reported_version: String::new(),
            tokens: None,
        };
        let lb = |label: &str, block: i64| BuildLatency {
            label: label.to_string(),
            reported_version: String::new(),
            load_before: String::new(),
            load_after: String::new(),
            series: vec![
                lat_series("hook.intercept_pass.spawn", 3_000_000, &[3_000_000]),
                lat_series("hook.intercept_block.spawn", block, &[block, block * 2]),
            ],
            derived: vec![],
        };
        Results {
            schema: SCHEMA,
            label: "x".to_string(),
            pre_release: true,
            tokenizer: tokenizer_info(),
            corpus: vec![],
            builds: vec![info("v1.7.0", "baseline"), info("pre", "candidate")],
            latency: Some(LatencySection {
                runs: 1,
                passes: 2,
                runs_per_pass: 1,
                warmup_runs: 0,
                minimum_runs: 1,
                clock: String::new(),
                warmup_note: String::new(),
                percentile_method: String::new(),
                samples_note: String::new(),
                subjects: vec![],
                builds: vec![lb("v1.7.0", base_block), lb("pre", cand_block)],
            }),
            model: model(),
            environment: crate::env::probe(),
        }
    }

    #[test]
    fn a_hook_that_got_slower_is_called_out_from_the_run_s_own_medians() {
        let r = two_builds(3_400_000, 5_500_000);
        let sec = r.latency.as_ref().unwrap();
        let note = intercept_block_note(&r, sec).expect("2.1 ms slower is a finding");
        assert!(note.contains("2.10 ms slower on pre than on v1.7.0"), "{note}");
        assert!(note.contains("5.50 ms against 3.40 ms"), "{note}");
        assert!(note.contains("#42"), "{note}");
        assert!(note.contains("lumen_answered"), "{note}");
        // Same speed, or faster: nothing to say.
        let same = two_builds(3_400_000, 3_450_000);
        assert!(intercept_block_note(&same, same.latency.as_ref().unwrap()).is_none());
        let faster = two_builds(5_500_000, 3_400_000);
        assert!(intercept_block_note(&faster, faster.latency.as_ref().unwrap()).is_none());
    }

    #[test]
    fn the_pass_spread_is_counted_not_promised() {
        // The block series has a pass at twice the pooled median: outside 20%. The pass series
        // has one pass at its median.
        let r = two_builds(3_400_000, 5_500_000);
        let (within, total) = pass_stability(r.latency.as_ref().unwrap());
        assert_eq!((within, total), (2, 4));
        // The old sentence is gone from the generated summary.
        assert!(!summary(&r).contains("expected to land within"));
    }
}
