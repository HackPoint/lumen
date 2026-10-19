//! The version x metric table, and the regressions it shows (V5).
//!
//! **One input, one output.** [`Measurements`] is the whole input: what the runners measured for
//! every version on this OS, normalised (a cell is a value, an `n/a - <reason>` or a failure).
//! [`render`] turns it into the Markdown table and the list of regressions, and nothing else
//! reaches the output: no clock, no path, no environment. The same input gives the same bytes.
//!
//! **Scored with today's engine.** Token cells are medians of what each version returned, per
//! tool and size bucket. Dollar cells are `score.rs` over the same per-file counts, for the
//! primary MODELLED reference session at R = 0 and, once the no-Lumen E2E arm has run, at the R
//! it measured. Nothing in a version's own arithmetic reaches a figure.
//!
//! **Cells.** A cell is a figure, `n/a - <reason>` (never 0, never inside a delta), or a failed
//! measurement (an ingest that did not finish is `incomplete: N of M turns`, never a throughput).
//! Every figure carries its change against the previous version and against no Lumen. Script
//! hooks (up to 1.5.1) are labelled and never compared with binary hooks.
//!
//! **Worse.** Defined per metric, in `benchmarks/versions/METHOD.md` ("What is worse"), and
//! applied here: tokens and dollars by the median; latency, ingest and cold start only when the
//! runs separate (every pass median of the newer build beyond every one of the older, or the
//! whole min-max beyond the other's), so noise is never called a regression. Each regression is
//! listed with the text of the issue it opens and the `gh issue create` command that files it.
//! The harness prints that command and never runs it.

use crate::corpus::BUCKETS;
use crate::score::{
    self, BLOCK_MIN_LINES, Counts, Flow, MODELLED, ReferenceSession, Rounds, Sizing, score_flow,
};
use crate::stats::percentile;
use crate::util::{Ctx, Res};
use crate::versions::daemon;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;

/// The schema of [`Measurements`]. Bumped when a field changes meaning.
pub const SCHEMA: u32 = 1;

/// Where the issues are filed.
pub const ISSUE_REPO: &str = "HackPoint/lumen";

/// Idle CPU is one window per process, so it has no run-to-run range. It is called worse only
/// when it rose by more than this fraction and more than [`IDLE_CPU_MIN_DELTA`] CPU seconds a
/// minute (METHOD.md, "What is worse").
pub const IDLE_CPU_REL: f64 = 0.25;
pub const IDLE_CPU_MIN_DELTA: f64 = 0.05;

// --- the input -------------------------------------------------------------------------------

/// A value, the reason there is none, or a measurement that failed. Never a zero in place of a
/// reason. The reason of `Na` is the text after `n/a - `.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cell<T> {
    Value(T),
    Na(String),
    Failed(String),
}

/// How a version's hooks run: scripts (up to 1.5.1), the `lumen-mcp hook` binary (1.6.0 on), or
/// not at all (no Lumen). Script and binary hook rows are never compared with one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    Script,
    Binary,
    None,
}

/// Median and range over runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    pub n: usize,
    pub median: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleStat {
    pub cpu_secs_per_min: f64,
    pub rss_median_bytes: u64,
    pub rss_peak_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestStat {
    pub bytes_per_s: Stat,
    pub turns_per_s: Stat,
}

/// The daemon's metrics for one version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Daemon {
    /// "warm file cache" or the cold label: it goes with every cold-start figure.
    pub cache: String,
    pub ingest: Cell<IngestStat>,
    pub cold_daemon_ms: Cell<Stat>,
    pub cold_mcp_ms: Cell<Stat>,
    pub cold_hook_ms: Cell<Stat>,
    pub idle_daemon_empty: Cell<IdleStat>,
    pub idle_daemon_after_ingest: Cell<IdleStat>,
    pub idle_mcp: Cell<IdleStat>,
}

impl Daemon {
    /// Every daemon cell is `n/a` for the same reason.
    pub fn all_na(why: &str) -> Daemon {
        fn na<T>(why: &str) -> Cell<T> {
            Cell::Na(why.to_string())
        }
        Daemon {
            cache: String::new(),
            ingest: na(why),
            cold_daemon_ms: na(why),
            cold_mcp_ms: na(why),
            cold_hook_ms: na(why),
            idle_daemon_empty: na(why),
            idle_daemon_after_ingest: na(why),
            idle_mcp: na(why),
        }
    }
}

/// One latency series: p50, p90, p99 of the pooled runs, and the median of each pass (the
/// evidence for "the runs separate").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatStat {
    pub p50_ns: i64,
    pub p90_ns: i64,
    pub p99_ns: i64,
    #[serde(default)]
    pub pass_medians_ns: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyEntry {
    pub id: String,
    pub cell: Cell<LatStat>,
}

/// One corpus file, one version: what each call returned, re-counted with today's cl100k.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileTokens {
    pub path: String,
    pub bucket: String,
    pub lines: u32,
    pub full: i64,
    pub outline: i64,
    pub recall: Option<i64>,
    pub stub: Option<i64>,
    pub delta: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenData {
    pub files: Vec<FileTokens>,
    /// Why the stub and delta figures cannot exist for this version (before 1.7.0, no Lumen).
    pub repeat_na: Option<String>,
}

/// Everything measured for one version (or for no Lumen).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionData {
    /// `1.9.0`, or `no Lumen`.
    pub version: String,
    pub no_lumen: bool,
    /// The asset's name and sha256 (never a path), or what no Lumen is.
    pub provenance: String,
    /// What the capability map says about this build that a reader must know.
    pub labels: Vec<String>,
    pub hook_kind: HookKind,
    pub tokens: Cell<TokenData>,
    pub latency: Vec<LatencyEntry>,
    pub daemon: Daemon,
}

impl VersionData {
    /// A version nothing can be measured for here (no asset on the OS): every cell says why.
    pub fn unavailable(version: &str, hook_kind: HookKind, why: &str) -> VersionData {
        VersionData {
            version: version.to_string(),
            no_lumen: false,
            provenance: format!("n/a — {why}"),
            labels: Vec::new(),
            hook_kind,
            tokens: Cell::Na(why.to_string()),
            latency: Vec::new(),
            daemon: Daemon::all_na(why),
        }
    }
}

/// The whole input of the report: the file `<os>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurements {
    pub schema: u32,
    /// `macos`, `linux` or `windows`.
    pub os: String,
    /// `macos-arm64`, ... : the manifest's column.
    pub platform: String,
    /// R from the no-Lumen E2E arm (its median), once that arm has run.
    pub measured_r: Option<u32>,
    /// How the run was configured, as plain sentences (run counts, reduced settings).
    pub notes: Vec<String>,
    /// Oldest first; no Lumen anywhere (it is found by its flag).
    pub versions: Vec<VersionData>,
}

impl Measurements {
    pub fn parse(text: &str) -> Res<Measurements> {
        let m: Measurements = serde_json::from_str(text).ctx("the measurements JSON")?;
        m.check()?;
        Ok(m)
    }

    pub fn to_json(&self) -> Res<String> {
        let mut s = serde_json::to_string_pretty(self).ctx("serialise the measurements")?;
        s.push('\n');
        Ok(s)
    }

    fn check(&self) -> Res<()> {
        if self.schema != SCHEMA {
            return Err(format!(
                "the measurements have schema {}, this report reads {SCHEMA}",
                self.schema
            ));
        }
        if self.versions.iter().filter(|v| v.no_lumen).count() > 1 {
            return Err("more than one no-Lumen entry".to_string());
        }
        let mut seen = std::collections::BTreeSet::new();
        for v in &self.versions {
            if !seen.insert(v.version.as_str()) {
                return Err(format!("{} appears twice", v.version));
            }
        }
        if let Some(r) = self.measured_r
            && r > score::MAX_R
        {
            return Err(format!(
                "measured_r {r} is over the supported {}",
                score::MAX_R
            ));
        }
        Ok(())
    }
}

// --- conversions from what the runners produce -------------------------------------------------

fn na_reason(status: &str) -> String {
    // `latency::Series` writes `n/a — <reason>` (a cell the map says cannot exist) or
    // `n/a: <reason>` (a measurement the machine could not make).
    let s = status
        .strip_prefix("n/a — ")
        .or_else(|| status.strip_prefix("n/a: "))
        .or_else(|| status.strip_prefix("n/a - "))
        .unwrap_or(status);
    s.to_string()
}

/// The latency series of one build as report entries. The in-process series are dropped: no
/// prebuilt version has them (METHOD.md, n/a rules), and a column of identical `n/a` says nothing.
pub fn latency_entries(b: &crate::latency::BuildLatency) -> Vec<LatencyEntry> {
    b.series
        .iter()
        .filter(|s| !s.id.ends_with(".inproc"))
        .map(|s| LatencyEntry {
            id: s.id.clone(),
            cell: match (&s.lat, s.status.starts_with("measured")) {
                (Some(l), true) => Cell::Value(LatStat {
                    p50_ns: l.median_ns,
                    p90_ns: l.p90_ns,
                    p99_ns: l.p99_ns,
                    pass_medians_ns: s.pass_medians_ns.clone(),
                }),
                _ => Cell::Na(na_reason(&s.status)),
            },
        })
        .collect()
}

fn stat_of(s: &daemon::Series) -> Stat {
    Stat {
        n: s.n,
        median: s.median,
        min: s.min,
        max: s.max,
    }
}

fn cell_of<T, U>(c: &daemon::Cell<T>, f: impl Fn(&T) -> U) -> Cell<U> {
    match c {
        daemon::Cell::Value(v) => Cell::Value(f(v)),
        daemon::Cell::Na(r) => Cell::Na(r.clone()),
    }
}

fn idle_of(i: &daemon::Idle) -> IdleStat {
    IdleStat {
        cpu_secs_per_min: i.cpu_secs_per_min,
        rss_median_bytes: i.rss_median_bytes,
        rss_peak_bytes: i.rss_peak_bytes,
    }
}

/// The daemon's report as report cells. An ingest in which any run did not finish is a failed
/// cell, `incomplete: N of M turns` (the run with the fewest turns), never a throughput: a daemon
/// that stalls part-way would otherwise look fast.
pub fn daemon_data(r: &daemon::Report) -> Daemon {
    let ingest = match &r.ingest {
        daemon::Cell::Na(why) => Cell::Na(why.clone()),
        daemon::Cell::Value(i) => {
            match i
                .runs
                .iter()
                .filter(|run| !run.complete)
                .min_by_key(|run| run.turns)
            {
                Some(run) => Cell::Failed(format!(
                    "incomplete: {} of {} turns",
                    run.turns, run.turns_expected
                )),
                None => Cell::Value(IngestStat {
                    bytes_per_s: stat_of(&i.bytes_per_s),
                    turns_per_s: stat_of(&i.turns_per_s),
                }),
            }
        }
    };
    Daemon {
        cache: r.cache.clone(),
        ingest,
        cold_daemon_ms: cell_of(&r.cold_daemon_ms, stat_of),
        cold_mcp_ms: cell_of(&r.cold_mcp_ms, stat_of),
        cold_hook_ms: cell_of(&r.cold_hook_ms, stat_of),
        idle_daemon_empty: cell_of(&r.idle_daemon_empty, idle_of),
        idle_daemon_after_ingest: cell_of(&r.idle_daemon_after_ingest, idle_of),
        idle_mcp: cell_of(&r.idle_mcp, idle_of),
    }
}

/// The token runner's per-file results as report data. A file's stub and delta exist only where
/// the version has repeat reads; otherwise the version carries one reason for all of them.
pub fn token_data(files: &[crate::tokens::FileResult]) -> TokenData {
    let repeat_na = files
        .iter()
        .find_map(|f| f.repeat_na.clone())
        .map(|s| na_reason(&s));
    TokenData {
        repeat_na: repeat_na.clone(),
        files: files
            .iter()
            .map(|f| FileTokens {
                path: format!("{}/{}", f.corpus, f.path),
                bucket: f.bucket.clone(),
                lines: u32::try_from(f.lines).unwrap_or(u32::MAX),
                full: f.full,
                outline: f.outline,
                recall: f.recall,
                stub: repeat_na.is_none().then_some(f.stub),
                delta: if repeat_na.is_none() { f.delta } else { None },
            })
            .collect(),
    }
}

// --- the table's model -------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    Tokens,
    Dollars,
    Latency,
    Ingest,
    Startup,
    Resources,
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::Tokens => "Tokens returned per call",
            Group::Dollars => "Net dollars per call (MODELLED)",
            Group::Latency => "Hook and tool latency",
            Group::Ingest => "Daemon ingest",
            Group::Startup => "Cold start",
            Group::Resources => "Idle CPU and memory",
        }
    }

    /// Where a reader should look for the cause of a regression in this group. A pointer, not a
    /// finding: the run times and counts, it does not trace.
    fn look_at(self) -> &'static str {
        match self {
            Group::Tokens => {
                "the outline, recall or repeat-read output changed between the tags: \
                 `crates/lumen-mcp` and `crates/lumen-core`"
            }
            Group::Dollars => {
                "follows the token figures of the same tool and bucket, or the way the engine \
                 prices the extra round: `crates/lumen-core/src/savings.rs`"
            }
            Group::Latency => {
                "the hook or the request path did more work, or started slower: \
                 `crates/lumen-mcp/src` (and `.claude/hooks` for script hooks)"
            }
            Group::Ingest => "the daemon's transcript parsing or its writes: `crates/lumen-daemon`",
            Group::Startup => "the daemon's, the server's or the hook's start-up work",
            Group::Resources => "what the process holds or polls while idle",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Tokens,
    Usd,
    Ms3,
    Ms1,
    MbPerS,
    PerS,
    CpuS,
    Mib,
}

impl Unit {
    fn suffix(self) -> &'static str {
        match self {
            Unit::Tokens => "tokens",
            Unit::Usd => "USD",
            Unit::Ms3 | Unit::Ms1 => "ms",
            Unit::MbPerS => "MB/s",
            Unit::PerS => "per s",
            Unit::CpuS => "CPU s/min",
            Unit::Mib => "MiB",
        }
    }
}

fn fmt_abs(unit: Unit, v: f64) -> String {
    match unit {
        Unit::Tokens => format!("{}", v.round() as i64),
        Unit::Usd => format!("${v:.4}"),
        Unit::Ms3 | Unit::CpuS => format!("{v:.3}"),
        Unit::Ms1 | Unit::Mib => format!("{v:.1}"),
        Unit::MbPerS => format!("{v:.2}"),
        Unit::PerS => format!("{v:.0}"),
    }
}

fn fmt_value(unit: Unit, v: f64) -> String {
    if v < 0.0 {
        format!("-{}", fmt_abs(unit, -v))
    } else {
        fmt_abs(unit, v)
    }
}

fn fmt_signed(unit: Unit, d: f64) -> String {
    format!(
        "{}{}",
        if d < 0.0 { "-" } else { "+" },
        fmt_abs(unit, d.abs())
    )
}

/// The change from `old` to `new`: dollars as an absolute difference (a saving can start from
/// zero), everything else as a percentage, or the absolute difference when `old` is zero.
fn fmt_delta(unit: Unit, old: f64, new: f64) -> String {
    if unit == Unit::Usd || old == 0.0 {
        return fmt_signed(unit, new - old);
    }
    format!("{:+.1}%", (new - old) / old * 100.0)
}

#[derive(Debug, Clone, PartialEq)]
struct Point {
    v: f64,
    /// The range the figure is judged by: pass medians, min-max, or median-to-peak.
    spread: Option<(f64, f64)>,
    /// Shown after the figure.
    note: Option<String>,
}

impl Point {
    fn exact(v: f64) -> Point {
        Point {
            v,
            spread: None,
            note: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Val(Point),
    Na(String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judge {
    /// Not judged: shown with its deltas only.
    None,
    /// The figures themselves (median tokens, dollars).
    Exact,
    /// The runs must separate (pass medians, min-max, median-to-peak).
    Spread,
    /// One window, no range: a fixed margin ([`IDLE_CPU_REL`], [`IDLE_CPU_MIN_DELTA`]).
    Margin,
}

struct Row {
    group: Group,
    label: String,
    unit: Unit,
    higher_is_worse: bool,
    judge: Judge,
    /// A hook figure: script and binary hooks are not compared.
    hook: bool,
    slots: Vec<Slot>,
}

#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    Worse(String),
    Better,
    Equal(&'static str),
}

fn na(why: &str) -> Slot {
    Slot::Na(why.to_string())
}

// --- tokens and dollars ------------------------------------------------------------------------

const TOOLS: [(&str, &str); 4] = [
    ("outline", "smart_read outline"),
    ("recall", "recall_file"),
    ("stub", "repeat-read stub"),
    ("delta", "edit delta"),
];

/// The tools of the dollar rows. A repeat read and a blocked Read are scored as flows (see
/// [`Scorer::net`]).
const DOLLAR_TOOLS: [(&str, &str); 5] = [
    ("outline", "smart_read outline, 1 call"),
    ("recall", "recall_file after an outline, 1 call"),
    ("stub", "outline then repeat-read stub, 2 calls"),
    ("delta", "outline then edit delta, 2 calls"),
    ("blocked", "blocked Read then outline, 1 call"),
];

fn bucket_names() -> Vec<String> {
    BUCKETS.iter().map(|b| b.name.to_string()).collect()
}

fn bucket_label(b: &str) -> String {
    if b == "all" {
        "all sizes".to_string()
    } else {
        format!("{b} lines")
    }
}

fn median(v: &mut [i64]) -> Option<f64> {
    v.sort_unstable();
    percentile(v, 50).map(|m| m as f64)
}

fn returned(f: &FileTokens, tool: &str) -> Option<i64> {
    match tool {
        "outline" => Some(f.outline),
        "recall" => f.recall,
        "stub" => f.stub,
        "delta" => f.delta,
        _ => None,
    }
}

fn in_bucket(f: &FileTokens, bucket: &str) -> bool {
    bucket == "all" || f.bucket == bucket
}

fn token_slot(v: &VersionData, tool: &str, bucket: &str) -> Slot {
    let td = match &v.tokens {
        Cell::Na(r) => return Slot::Na(r.clone()),
        Cell::Failed(r) => return Slot::Failed(r.clone()),
        Cell::Value(td) => td,
    };
    if matches!(tool, "stub" | "delta")
        && let Some(why) = &td.repeat_na
    {
        return Slot::Na(why.clone());
    }
    let mut vals: Vec<i64> = td
        .files
        .iter()
        .filter(|f| in_bucket(f, bucket))
        .filter_map(|f| returned(f, tool))
        .collect();
    match median(&mut vals) {
        Some(m) => Slot::Val(Point::exact(m)),
        None => na("no file of this size has a figure for this tool"),
    }
}

/// Scores one call of one file with the engine, caching what it has already scored: the same
/// counts score the same, and a corpus has many files of the same size.
struct Scorer {
    sizing: Sizing,
    model: String,
    cache: HashMap<(u8, i64, i64, i64, i64, u32), f64>,
}

impl Scorer {
    fn new() -> Res<Scorer> {
        let session = ReferenceSession::committed()?;
        Ok(Scorer {
            sizing: session.primary(),
            model: session.model.clone(),
            cache: HashMap::new(),
        })
    }

    /// The engine's net dollars (its floor) of the tool's call for `f` at `rounds`, or `None`
    /// where the file has no such call.
    fn net(&mut self, tool: &str, f: &FileTokens, rounds: Rounds) -> Res<Option<f64>> {
        let outline = Counts {
            full_tokens: f.full,
            returned_tokens: f.outline,
        };
        let second = |r: Option<i64>| {
            r.map(|t| Counts {
                full_tokens: f.full,
                returned_tokens: t,
            })
        };
        let (kind, flow, which) = match tool {
            "outline" => (
                0u8,
                Some(Flow::Outline {
                    path: f.path.clone(),
                    outline,
                }),
                0,
            ),
            "recall" => (
                1,
                second(f.recall).map(|recall| Flow::OutlineThenRecall {
                    path: f.path.clone(),
                    outline,
                    recall,
                }),
                1,
            ),
            "stub" => (
                2,
                second(f.stub).map(|stub| Flow::OutlineThenStub {
                    path: f.path.clone(),
                    outline,
                    stub,
                }),
                usize::MAX,
            ),
            "delta" => (
                3,
                // The edit changes the file by a few tokens; its full count is the file's.
                second(f.delta).map(|delta| Flow::EditThenDelta {
                    path: f.path.clone(),
                    outline,
                    delta,
                }),
                usize::MAX,
            ),
            "blocked" => (
                4,
                (f.lines >= BLOCK_MIN_LINES).then(|| Flow::BlockedThenOutline {
                    path: f.path.clone(),
                    file_lines: f.lines,
                    outline,
                }),
                usize::MAX,
            ),
            other => return Err(format!("no flow for the tool {other:?}")),
        };
        let Some(flow) = flow else {
            return Ok(None);
        };
        let second_ret = match tool {
            "recall" => f.recall,
            "stub" => f.stub,
            "delta" => f.delta,
            _ => None,
        }
        .unwrap_or(0);
        let key = (kind, f.full, f.outline, second_ret, 0, rounds.r());
        if let Some(v) = self.cache.get(&key) {
            return Ok(Some(*v));
        }
        let s = score_flow(&flow, &self.sizing, &self.model, rounds)?;
        // A stub or a delta is priced against what Lumen returned the first time, so the engine's
        // line for it moves with the outline. Those two are therefore scored as the flow (the
        // outline and the repeat read: both calls), and the blocked-Read flow the same way, so a
        // better outline is never shown as a worse stub.
        let money = if which == usize::MAX {
            &s.total
        } else {
            &s.calls
                .get(which)
                .ok_or_else(|| format!("{}: the engine scored no call {which}", f.path))?
                .money
        };
        let net = money
            .bold_net_usd()
            .ok_or_else(|| format!("{}: no price for {}", f.path, self.model))?;
        self.cache.insert(key, net);
        Ok(Some(net))
    }
}

fn dollar_slot(
    sc: &mut Scorer,
    v: &VersionData,
    tool: &str,
    bucket: &str,
    rounds: Rounds,
) -> Res<Slot> {
    if v.no_lumen {
        // The baseline: no Lumen saves nothing and costs nothing, by definition.
        return Ok(Slot::Val(Point {
            v: 0.0,
            spread: None,
            note: Some("baseline".to_string()),
        }));
    }
    let td = match &v.tokens {
        Cell::Na(r) => return Ok(Slot::Na(r.clone())),
        Cell::Failed(r) => return Ok(Slot::Failed(r.clone())),
        Cell::Value(td) => td,
    };
    if matches!(tool, "stub" | "delta")
        && let Some(why) = &td.repeat_na
    {
        return Ok(Slot::Na(why.clone()));
    }
    if tool == "blocked" && bucket == "<300" {
        return Ok(na("a Read under 300 lines is not blocked"));
    }
    let mut nets = Vec::new();
    for f in td.files.iter().filter(|f| in_bucket(f, bucket)) {
        if let Some(n) = sc.net(tool, f, rounds)? {
            // Rounded to the precision the table shows plus two digits, so float noise between
            // two equal scores is not a difference.
            nets.push((n * 1e6).round() as i64);
        }
    }
    Ok(match median(&mut nets) {
        Some(m) => Slot::Val(Point::exact(m / 1e6)),
        None => na("no file of this size has a figure for this tool"),
    })
}

// --- the rows ----------------------------------------------------------------------------------

fn token_rows(m: &Measurements) -> Vec<Row> {
    let mut rows = Vec::new();
    let buckets: Vec<String> = bucket_names()
        .into_iter()
        .chain(["all".to_string()])
        .collect();
    for (tool, name) in TOOLS {
        for b in &buckets {
            rows.push(Row {
                group: Group::Tokens,
                label: format!("{name}, {} (tokens, median)", bucket_label(b)),
                unit: Unit::Tokens,
                higher_is_worse: true,
                judge: Judge::Exact,
                hook: false,
                slots: m.versions.iter().map(|v| token_slot(v, tool, b)).collect(),
            });
        }
    }
    rows
}

fn dollar_rows(m: &Measurements) -> Res<Vec<Row>> {
    let mut sc = Scorer::new()?;
    let mut rows = Vec::new();
    let buckets: Vec<String> = bucket_names()
        .into_iter()
        .chain(["all".to_string()])
        .collect();
    let rounds = score::rounds_for_report(m.measured_r);
    for r in rounds {
        for (tool, name) in DOLLAR_TOOLS {
            for b in &buckets {
                let mut slots = Vec::new();
                for v in &m.versions {
                    slots.push(dollar_slot(&mut sc, v, tool, b, r)?);
                }
                rows.push(Row {
                    group: Group::Dollars,
                    label: format!(
                        "{name}, {} (USD net, median, {})",
                        bucket_label(b),
                        r.label()
                    ),
                    unit: Unit::Usd,
                    higher_is_worse: false,
                    judge: Judge::Exact,
                    hook: false,
                    slots,
                });
            }
        }
    }
    Ok(rows)
}

fn latency_slot(v: &VersionData, id: &str, which: usize) -> Slot {
    match v.latency.iter().find(|e| e.id == id) {
        None if v.no_lumen => na("no Lumen: there is no hook, no server and no daemon to time"),
        None => na("not measured for this version"),
        Some(e) => match &e.cell {
            Cell::Na(r) => Slot::Na(r.clone()),
            Cell::Failed(r) => Slot::Failed(r.clone()),
            Cell::Value(l) => {
                let ms = |ns: i64| ns as f64 / 1e6;
                let spread = (which == 0 && !l.pass_medians_ns.is_empty()).then(|| {
                    let lo = l.pass_medians_ns.iter().min().copied().unwrap_or(0);
                    let hi = l.pass_medians_ns.iter().max().copied().unwrap_or(0);
                    (ms(lo), ms(hi))
                });
                Slot::Val(Point {
                    v: ms([l.p50_ns, l.p90_ns, l.p99_ns][which]),
                    spread,
                    note: None,
                })
            }
        },
    }
}

fn latency_rows(m: &Measurements) -> Vec<Row> {
    // The ids, in the order they first appear.
    let mut ids: Vec<&str> = Vec::new();
    for v in &m.versions {
        for e in &v.latency {
            if !ids.contains(&e.id.as_str()) {
                ids.push(&e.id);
            }
        }
    }
    let mut rows = Vec::new();
    for id in ids {
        for (which, p) in ["p50", "p90", "p99"].into_iter().enumerate() {
            rows.push(Row {
                group: Group::Latency,
                label: format!("{id} {p} (ms)"),
                unit: Unit::Ms3,
                higher_is_worse: true,
                // Only the median has pass-level evidence.
                judge: if which == 0 {
                    Judge::Spread
                } else {
                    Judge::None
                },
                hook: id.starts_with("hook."),
                slots: m
                    .versions
                    .iter()
                    .map(|v| latency_slot(v, id, which))
                    .collect(),
            });
        }
    }
    rows
}

fn stat_slot(c: &Cell<Stat>, scale: f64) -> Slot {
    match c {
        Cell::Na(r) => Slot::Na(r.clone()),
        Cell::Failed(r) => Slot::Failed(r.clone()),
        Cell::Value(s) => Slot::Val(Point {
            v: s.median * scale,
            spread: Some((s.min * scale, s.max * scale)),
            note: None,
        }),
    }
}

fn idle_slot(c: &Cell<IdleStat>, rss: bool) -> Slot {
    match c {
        Cell::Na(r) => Slot::Na(r.clone()),
        Cell::Failed(r) => Slot::Failed(r.clone()),
        Cell::Value(i) if rss => {
            let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
            Slot::Val(Point {
                v: mib(i.rss_median_bytes),
                spread: Some((mib(i.rss_median_bytes), mib(i.rss_peak_bytes))),
                note: Some(format!(
                    "peak {}",
                    fmt_abs(Unit::Mib, mib(i.rss_peak_bytes))
                )),
            })
        }
        Cell::Value(i) => Slot::Val(Point::exact(i.cpu_secs_per_min)),
    }
}

fn daemon_rows(m: &Measurements) -> Vec<Row> {
    let ds = |f: &dyn Fn(&Daemon) -> Slot| -> Vec<Slot> {
        m.versions.iter().map(|v| f(&v.daemon)).collect()
    };
    let ingest = |bytes: bool| {
        ds(&|d| match &d.ingest {
            Cell::Na(r) => Slot::Na(r.clone()),
            Cell::Failed(r) => Slot::Failed(r.clone()),
            Cell::Value(i) => {
                let (s, scale) = if bytes {
                    (&i.bytes_per_s, 1e-6)
                } else {
                    (&i.turns_per_s, 1.0)
                };
                stat_slot(&Cell::Value(s.clone()), scale)
            }
        })
    };
    let row = |group, label: &str, unit, higher_is_worse, judge, slots| Row {
        group,
        label: label.to_string(),
        unit,
        higher_is_worse,
        judge,
        hook: false,
        slots,
    };
    let mut rows = vec![
        row(
            Group::Ingest,
            "ingest throughput (MB/s, median of runs)",
            Unit::MbPerS,
            false,
            Judge::Spread,
            ingest(true),
        ),
        row(
            Group::Ingest,
            "ingest throughput (turns per s, median of runs)",
            Unit::PerS,
            false,
            Judge::Spread,
            ingest(false),
        ),
    ];
    rows.push(row(
        Group::Startup,
        "cold start: daemon spawn to port and schema (ms, median of runs)",
        Unit::Ms1,
        true,
        Judge::Spread,
        ds(&|d| stat_slot(&d.cold_daemon_ms, 1.0)),
    ));
    rows.push(row(
        Group::Startup,
        "cold start: lumen-mcp spawn to initialize (ms, median of runs)",
        Unit::Ms1,
        true,
        Judge::Spread,
        ds(&|d| stat_slot(&d.cold_mcp_ms, 1.0)),
    ));
    let mut hook = row(
        Group::Startup,
        "cold start: first hook spawn (ms, median of runs)",
        Unit::Ms1,
        true,
        Judge::Spread,
        ds(&|d| stat_slot(&d.cold_hook_ms, 1.0)),
    );
    hook.hook = true;
    rows.push(hook);
    for (what, pick) in [
        (
            "daemon, empty",
            (|d: &Daemon| d.idle_daemon_empty.clone()) as fn(&Daemon) -> Cell<IdleStat>,
        ),
        ("daemon, after ingest", |d: &Daemon| {
            d.idle_daemon_after_ingest.clone()
        }),
        ("lumen-mcp", |d: &Daemon| d.idle_mcp.clone()),
    ] {
        rows.push(row(
            Group::Resources,
            &format!("idle CPU, {what} (CPU s/min)"),
            Unit::CpuS,
            true,
            Judge::Margin,
            ds(&|d| idle_slot(&pick(d), false)),
        ));
        rows.push(row(
            Group::Resources,
            &format!("idle RSS, {what} (MiB, median)"),
            Unit::Mib,
            true,
            Judge::Spread,
            ds(&|d| idle_slot(&pick(d), true)),
        ));
    }
    rows
}

fn rows_of(m: &Measurements) -> Res<Vec<Row>> {
    let mut rows = token_rows(m);
    rows.extend(dollar_rows(m)?);
    rows.extend(latency_rows(m));
    rows.extend(daemon_rows(m));
    Ok(rows)
}

// --- comparing ---------------------------------------------------------------------------------

fn judge(row: &Row, old: &Point, new: &Point) -> Option<Verdict> {
    let worse_when_higher = row.higher_is_worse;
    let side = |new_is_higher: bool| new_is_higher == worse_when_higher;
    match row.judge {
        Judge::None => None,
        Judge::Exact => {
            // Compared after rounding to a millionth, so float noise is not a difference.
            let q = |x: f64| (x * 1e6).round() as i64;
            let (o, n) = (q(old.v), q(new.v));
            Some(if n == o {
                Verdict::Equal("equal")
            } else if side(n > o) {
                Verdict::Worse(format!(
                    "the median went from {} to {}",
                    fmt_value(row.unit, old.v),
                    fmt_value(row.unit, new.v)
                ))
            } else {
                Verdict::Better
            })
        }
        Judge::Spread => Some(match (old.spread, new.spread) {
            (Some(o), Some(n)) => {
                // The newer build's whole range beyond the older one's, on the bad side.
                let beyond_bad = if worse_when_higher {
                    n.0 > o.1
                } else {
                    n.1 < o.0
                };
                let beyond_good = if worse_when_higher {
                    n.1 < o.0
                } else {
                    n.0 > o.1
                };
                if beyond_bad {
                    Verdict::Worse(format!(
                        "the runs separate: the newer range {} to {} lies entirely {} the older \
                         range {} to {}",
                        fmt_value(row.unit, n.0),
                        fmt_value(row.unit, n.1),
                        if worse_when_higher { "above" } else { "below" },
                        fmt_value(row.unit, o.0),
                        fmt_value(row.unit, o.1)
                    ))
                } else if beyond_good {
                    Verdict::Better
                } else {
                    Verdict::Equal("equal: ranges overlap")
                }
            }
            _ => Verdict::Equal("not judged: no run-to-run range"),
        }),
        Judge::Margin => {
            let up = |a: f64, b: f64| b > a * (1.0 + IDLE_CPU_REL) && b - a > IDLE_CPU_MIN_DELTA;
            let (o, n) = (old.v, new.v);
            Some(if side(true) && up(o, n) || side(false) && up(n, o) {
                Verdict::Worse(format!(
                    "it moved from {} to {}, past the margin of {:.0}% and {} {}",
                    fmt_value(row.unit, o),
                    fmt_value(row.unit, n),
                    IDLE_CPU_REL * 100.0,
                    IDLE_CPU_MIN_DELTA,
                    row.unit.suffix()
                ))
            } else if side(true) && up(n, o) || side(false) && up(o, n) {
                Verdict::Better
            } else {
                Verdict::Equal("equal: within the margin")
            })
        }
    }
}

/// The index of the version before `i`, skipping no Lumen.
fn previous(m: &Measurements, i: usize) -> Option<usize> {
    (0..i).rev().find(|j| !m.versions[*j].no_lumen)
}

fn no_lumen_index(m: &Measurements) -> Option<usize> {
    m.versions.iter().position(|v| v.no_lumen)
}

/// Why `a` and `b` cannot be compared on `row`, if they cannot.
fn not_comparable(row: &Row, m: &Measurements, a: usize, b: usize) -> Option<&'static str> {
    (row.hook && m.versions[a].hook_kind != m.versions[b].hook_kind)
        .then_some("script hooks and binary hooks are not compared")
}

// --- rendering ---------------------------------------------------------------------------------

fn md(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn show_slot(row: &Row, m: &Measurements, i: usize) -> String {
    let v = &m.versions[i];
    let p = match &row.slots[i] {
        Slot::Na(r) => return md(&format!("n/a — {r}")),
        Slot::Failed(r) => return format!("**{}**", md(r)),
        Slot::Val(p) => p,
    };
    let mut out = fmt_value(row.unit, p.v);
    if let Some(n) = &p.note {
        let _ = write!(out, " ({n})");
    }
    if row.hook && v.hook_kind == HookKind::Script {
        out.push_str(" (script)");
    }
    if v.no_lumen {
        return md(&out);
    }
    let line = |what: &str, other: Option<usize>, row: &Row, verdict: bool| -> String {
        let Some(j) = other else {
            return String::new();
        };
        match &row.slots[j] {
            Slot::Val(_) if not_comparable(row, m, i, j).is_some() => {
                let why = not_comparable(row, m, i, j).unwrap_or_default();
                format!("<br>{what}: no delta ({why})")
            }
            Slot::Val(o) => {
                let mut s = format!("<br>{what}: {}", fmt_delta(row.unit, o.v, p.v));
                // Worse and better are said of a version against the one before it. Against no
                // Lumen the change is shown, not judged.
                match judge(row, o, p).filter(|_| verdict) {
                    Some(Verdict::Worse(_)) => s.push_str(" **worse**"),
                    Some(Verdict::Better) => s.push_str(" better"),
                    Some(Verdict::Equal(e)) => {
                        let _ = write!(s, " ({e})");
                    }
                    None => {}
                }
                s
            }
            _ => format!("<br>{what}: no delta (no figure to compare with)"),
        }
    };
    out.push_str(&line("vs previous", previous(m, i), row, true));
    out.push_str(&line("vs no Lumen", no_lumen_index(m), row, false));
    md(&out)
}

/// A regression: a newer version worse than the one before it, on a judged metric.
#[derive(Debug, Clone, PartialEq)]
pub struct Regression {
    pub metric: String,
    pub group: &'static str,
    pub older: String,
    pub newer: String,
    pub old_value: String,
    pub new_value: String,
    pub evidence: String,
    pub look_at: &'static str,
}

/// The issue a regression opens: its title, its body and the command that files it.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub title: String,
    pub body: String,
    pub command: String,
}

fn find_regressions(m: &Measurements, rows: &[Row]) -> Vec<Regression> {
    let mut out = Vec::new();
    for row in rows {
        for i in 0..m.versions.len() {
            if m.versions[i].no_lumen {
                continue;
            }
            let Some(j) = previous(m, i) else { continue };
            if not_comparable(row, m, i, j).is_some() {
                continue;
            }
            // A cell with no figure, or a failed one, is never compared.
            let (Slot::Val(old), Slot::Val(new)) = (&row.slots[j], &row.slots[i]) else {
                continue;
            };
            if let Some(Verdict::Worse(why)) = judge(row, old, new) {
                let with_note = |p: &Point| {
                    let mut s = fmt_value(row.unit, p.v);
                    if let Some(n) = &p.note {
                        let _ = write!(s, " ({n})");
                    }
                    s
                };
                out.push(Regression {
                    metric: row.label.clone(),
                    group: row.group.title(),
                    older: m.versions[j].version.clone(),
                    newer: m.versions[i].version.clone(),
                    old_value: with_note(old),
                    new_value: with_note(new),
                    evidence: why,
                    look_at: row.group.look_at(),
                });
            }
        }
    }
    out
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The issue text for one regression, as the method asks: the metric, both values, both versions
/// and the likely cause (where to look: the run cannot trace a cause).
pub fn issue_for(r: &Regression, os: &str) -> Issue {
    let title = format!(
        "bench: {} is worse in {} than in {} ({os})",
        r.metric, r.newer, r.older
    );
    let modelled = if r.group.contains("MODELLED") {
        format!(
            "\nThe dollar figures are {MODELLED} (the primary reference session, scored with today's engine).\n"
        )
    } else {
        String::new()
    };
    let body = format!(
        "The version-over-version benchmark (`benchmarks/versions/run.sh`, {os}) found a newer \
         version worse than the one before it.\n\
         \n\
         - metric: {metric} ({group})\n\
         - older: {older}, {old}\n\
         - newer: {newer}, {new}\n\
         - evidence: {why}\n\
         {modelled}\n\
         Likely cause, to confirm: {look}. See `git log --oneline v{older}..v{newer}`.\n\
         \n\
         Published as worse in `benchmarks/versions/results/{os}.md`; the row is not left out.\n",
        metric = r.metric,
        group = r.group,
        older = r.older,
        old = r.old_value,
        newer = r.newer,
        new = r.new_value,
        why = r.evidence,
        look = r.look_at,
    );
    let command = format!(
        "gh issue create -R {ISSUE_REPO} --title {} --body {}",
        shell_quote(&title),
        shell_quote(&body)
    );
    Issue {
        title,
        body,
        command,
    }
}

/// What [`render`] produces.
pub struct Rendered {
    pub markdown: String,
    pub regressions: Vec<Regression>,
    pub issues: Vec<Issue>,
}

fn header(m: &Measurements) -> String {
    let mut s = String::from("| metric |");
    for v in &m.versions {
        let _ = write!(s, " {} |", md(&v.version));
    }
    s.push('\n');
    s.push_str("|---|");
    for _ in &m.versions {
        s.push_str("---|");
    }
    s.push('\n');
    s
}

/// The Markdown report and its regressions. Deterministic in `m`.
pub fn render(m: &Measurements) -> Res<Rendered> {
    m.check()?;
    let rows = rows_of(m)?;
    let regressions = find_regressions(m, &rows);
    let issues: Vec<Issue> = regressions.iter().map(|r| issue_for(r, &m.os)).collect();
    let session = ReferenceSession::committed()?;

    let mut s = String::new();
    let _ = writeln!(s, "# Lumen, version over version ({})\n", m.os);
    let _ = writeln!(
        s,
        "Platform {}. Every version is the file Lumen published for it, scored with today's \
         engine on the same corpus and the same inputs. `benchmarks/versions/METHOD.md` is the \
         method; `benchmarks/versions/run.sh` reproduces this file.\n",
        m.platform
    );
    s.push_str("## How to read the table\n\n");
    s.push_str(
        "- Each figure shows its change against the previous version (`vs previous`) and against \
         no Lumen (`vs no Lumen`). Dollars change by a difference, everything else by a \
         percentage.\n\
         - `n/a — <reason>` is a cell that cannot exist. It is never 0 and never enters a \
         change.\n\
         - A cell in bold is a measurement that failed. An ingest that did not finish is \
         `incomplete: N of M turns`, never a throughput.\n\
         - `(script)` marks a script hook (bash and python3, up to 1.5.1). Script hooks are never \
         compared with binary hooks.\n\
         - `worse` is defined per metric in METHOD.md (\"What is worse\"): tokens and dollars by \
         the median; latency, ingest, cold start and memory only when the runs separate. A \
         change inside the noise is `equal`, not worse.\n\
         - `compress_logs` is not measured by the token runner and has no row.\n\n",
    );
    let _ = writeln!(
        s,
        "{MODELLED}: dollars are scored against a reference session, not a real one: \
         `{}` at the prices of `crates/lumen-core/src/rates.rs` as of {}, the context and output \
         sizes lumen-core measured ({} prompt tokens, {} output tokens), the session closed more \
         than an hour before the clock. Net dollars are the engine's floor.",
        session.model,
        session.prices_as_of,
        session.primary().prompt_tokens,
        session.primary().output_tokens
    );
    match m.measured_r {
        Some(r) => {
            let _ = writeln!(
                s,
                "R, the rounds remaining that a saved token is read again, is shown at R=0 (the \
                 floor) and at R={r}, the median of the no-Lumen E2E arm.\n"
            );
        }
        None => s.push_str(
            "R from the no-Lumen E2E arm: not available yet. Only the R=0 floor is shown.\n\n",
        ),
    }
    if !m.notes.is_empty() {
        s.push_str("Run notes:\n\n");
        for n in &m.notes {
            let _ = writeln!(s, "- {}", md(n));
        }
        s.push('\n');
    }

    s.push_str("## Versions\n\n| version | asset | notes |\n|---|---|---|\n");
    for v in &m.versions {
        let mut notes = v.labels.clone();
        if !v.daemon.cache.is_empty() {
            notes.push(format!("cold start: {}", v.daemon.cache));
        }
        let _ = writeln!(
            s,
            "| {} | {} | {} |",
            md(&v.version),
            md(&v.provenance),
            md(&notes.join("; "))
        );
    }
    s.push('\n');

    let mut last: Option<Group> = None;
    for row in &rows {
        if last != Some(row.group) {
            if last.is_some() {
                s.push('\n');
            }
            let _ = writeln!(s, "## {}\n", row.group.title());
            s.push_str(&header(m));
            last = Some(row.group);
        }
        let _ = write!(s, "| {} |", md(&row.label));
        for i in 0..m.versions.len() {
            let _ = write!(s, " {} |", show_slot(row, m, i));
        }
        s.push('\n');
    }
    s.push('\n');

    s.push_str("## Regressions\n\n");
    if regressions.is_empty() {
        s.push_str("No newer version is worse than the one before it on any judged metric.\n");
    } else {
        let _ = writeln!(
            s,
            "{} regression(s). Each is published as worse and opens an issue in {ISSUE_REPO}. \
             The commands below are printed, not run.\n",
            regressions.len()
        );
        s.push_str("| metric | older | newer | evidence |\n|---|---|---|---|\n");
        for r in &regressions {
            let _ = writeln!(
                s,
                "| {} | {}: {} | {}: {} | {} |",
                md(&r.metric),
                md(&r.older),
                md(&r.old_value),
                md(&r.newer),
                md(&r.new_value),
                md(&r.evidence)
            );
        }
        s.push_str("\n## Issues to file\n\n```sh\n");
        for i in &issues {
            s.push_str(&i.command);
            s.push_str("\n\n");
        }
        s.push_str("```\n");
    }
    Ok(Rendered {
        markdown: s,
        regressions,
        issues,
    })
}
