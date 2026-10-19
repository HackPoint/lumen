//! Latency: how long the hooks and the server's tools take, from the caller's side.
//!
//! Every figure is a monotonic-clock (`std::time::Instant`) interval, summarised as median, p90
//! and p99 over at least [`MIN_RUNS`] recorded runs, after `warmup` runs that are executed the
//! same way and discarded. Each series says exactly what its interval contains. The percentiles
//! are nearest-rank (`stats::percentile`): the sample at rank `ceil(p * n / 100)` of the sorted
//! samples, no interpolation. With the default 200 runs the p99 is therefore the third slowest
//! sample and the median is the 100th, the lower of the two middle values.
//!
//! The run is [`PASSES`] passes over every series of every build, the builds taking turns, and the
//! samples of the passes are pooled: a burst of other work on a machine that is not idle is then
//! spread over every series of both builds instead of sitting on one.
//!
//! Hooks are measured twice, so that the cost of starting a process is separate from the cost of
//! the work:
//!
//! * **spawn**: the whole hook as Claude Code runs it, from just before the process is created to
//!   its exit: process start, reading the payload from stdin, the work, exit. The payload is
//!   written inside the interval.
//! * **floor**: the same, for a hook invocation that does no work (an unknown subcommand, which
//!   prints the usage line and exits 1). Process start and stdin only. `spawn - floor` is the
//!   work plus what a cold process pays for it (the tokenizer is built on every spawn).
//! * **in-process**: `lumen_mcp::hook::run_with` called directly, warm. Only for a build made from
//!   this checkout: the harness links the checkout's library, not another build's.
//! * **identity**: `hook.per_call_identity`, the hook that records who is calling a Lumen tool,
//!   for a build that does (found by behaviour, see `driver::probe_identity`), and the server's
//!   claim of that record (`server.identity.*`). A build that does not has an `n/a` slot.
//!
//! Server calls are timed as a client sees them: from just before the request is written to the
//! pipe to the arrival of the reply line. The server answers first and writes its ledger row
//! after, so the row write is not inside the interval; an untimed `tools/list` after every call
//! waits for it, so one call's row write never sits in front of the next call's timing.
//! `server.ping` is the round trip with no work, the floor under every server series.
//!
//! What is not modelled: a cold cache, a busy machine, and a real ledger with months of rows (the
//! ledger here holds a few hundred). The README says so.

use crate::builds::Build;
use crate::corpus::{BUCKETS, Lang, bucket_of};
use crate::driver::{
    Identity, Server, ledger_rows, ledger_tool_count, lumen_call_payload, records_for,
};
use crate::sandbox::Sandbox;
use crate::stats::{Lat, lat};
use crate::tokens::Prepared;
use crate::util::{Ctx, Res};
use crate::versions::caps::Caps;
use lumen_mcp::hook::{Env, run_with};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The fewest recorded runs a series may have.
pub const MIN_RUNS: usize = 100;

#[derive(Debug, Clone, Copy)]
pub struct LatencyConfig {
    pub runs: usize,
    pub warmup: usize,
}

/// One measured interval, or the reason there is none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub what: String,
    pub subject: Option<String>,
    /// `measured`, `measured, mixed routes`, or `n/a: <reason>`.
    pub status: String,
    pub lat: Option<Lat>,
    /// Server series: how the server routed each call, from its own ledger.
    pub routes: BTreeMap<String, usize>,
    /// The median of each pass on its own, in pass order: how much the machine moved during the
    /// run. The raw samples are not written (a few hundred KB of numbers); these are.
    #[serde(default)]
    pub pass_medians_ns: Vec<i64>,
    /// The recorded intervals, nanoseconds, kept to pool the passes. Not written to the results.
    #[serde(skip)]
    pub raw: Vec<i64>,
}

impl Series {
    pub(crate) fn measured(
        id: &str,
        what: &str,
        subject: Option<String>,
        ns: &[i64],
    ) -> Res<Series> {
        Ok(Series {
            id: id.to_string(),
            what: what.to_string(),
            subject,
            status: "measured".to_string(),
            lat: Some(lat(ns).ok_or_else(|| format!("{id}: no samples"))?),
            routes: BTreeMap::new(),
            pass_medians_ns: Vec::new(),
            raw: ns.to_vec(),
        })
    }

    pub fn na(id: &str, what: &str, reason: &str) -> Series {
        Series::with_status(id, what, format!("n/a: {reason}"))
    }

    /// A cell the capability map says cannot exist for this build: `n/a — <reason>`.
    pub fn na_because(id: &str, what: &str, reason: &str) -> Series {
        Series::with_status(id, what, format!("n/a — {reason}"))
    }

    fn with_status(id: &str, what: &str, status: String) -> Series {
        Series {
            id: id.to_string(),
            what: what.to_string(),
            subject: None,
            status,
            lat: None,
            routes: BTreeMap::new(),
            pass_medians_ns: Vec::new(),
            raw: Vec::new(),
        }
    }
}

/// `median(spawn) - median(floor)` for one hook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Derived {
    pub id: String,
    pub what: String,
    pub median_ns: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildLatency {
    pub label: String,
    pub reported_version: String,
    /// The machine's load when the latency run (every build, every pass) began and ended, or why
    /// it is unknown. The same for every build: they are measured interleaved.
    pub load_before: String,
    pub load_after: String,
    pub series: Vec<Series>,
    pub derived: Vec<Derived>,
}

/// A corpus file the latency series run on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subject {
    pub bucket: String,
    pub rel: String,
    pub lines: usize,
    pub bytes: usize,
    pub recall_item: String,
    #[serde(skip)]
    pub abs: PathBuf,
    #[serde(skip)]
    pub src: String,
    #[serde(skip)]
    pub alt: String,
}

impl Subject {
    pub(crate) fn label(&self) -> String {
        format!(
            "{} ({} lines, bucket {})",
            self.rel, self.lines, self.bucket
        )
    }
}

/// One subject per size bucket below 3000 lines: the lower-median file by line count among the
/// corpus's Rust files in that bucket that have a representative item and a delta edit. Rust,
/// because the hooks only ever see source files and the repo itself is Rust; a bucket with no such
/// file has no subject and its series say so.
pub fn pick_subjects(corpus_root: &Path, prep: &[Prepared]) -> Vec<Subject> {
    let mut out = Vec::new();
    for (bi, b) in BUCKETS.iter().enumerate().take(3) {
        let mut c: Vec<(usize, &str, &Prepared, &str, &str)> = prep
            .iter()
            .filter(|p| p.file.lang == Lang::Rust && bucket_of(p.file.lines) == bi)
            .filter_map(|p| {
                let rep = p.rep.as_ref()?;
                let edit = p.edit.as_ref()?;
                Some((
                    p.file.lines,
                    p.file.rel.as_str(),
                    p,
                    rep.name.as_str(),
                    edit.new_src.as_str(),
                ))
            })
            .collect();
        c.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        if c.is_empty() {
            continue;
        }
        let (lines, rel, p, name, alt) = c[(c.len() - 1) / 2];
        out.push(Subject {
            bucket: b.name.to_string(),
            rel: rel.to_string(),
            lines,
            bytes: p.src.len(),
            recall_item: name.to_string(),
            abs: corpus_root.join(rel),
            src: p.src.clone(),
            alt: alt.to_string(),
        });
    }
    out
}

// ── payloads ─────────────────────────────────────────────────────────────────

pub(crate) fn pre_read(session: &str, abs: &Path) -> Vec<u8> {
    let p = abs.to_string_lossy();
    json!({
        "session_id": session, "transcript_path": "/dev/null",
        "cwd": abs.parent().map(|d| d.to_string_lossy().into_owned()),
        "permission_mode": "default", "hook_event_name": "PreToolUse",
        "tool_name": "Read", "tool_input": {"file_path": p},
        "tool_use_id": "toolu_bench"
    })
    .to_string()
    .into_bytes()
}

pub(crate) fn post_read(session: &str, s: &Subject) -> Vec<u8> {
    let p = s.abs.to_string_lossy();
    json!({
        "session_id": session, "transcript_path": "/dev/null",
        "cwd": s.abs.parent().map(|d| d.to_string_lossy().into_owned()),
        "permission_mode": "default", "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_input": {"file_path": p},
        "tool_response": {"type": "text", "file": {
            "filePath": p, "content": s.src, "numLines": s.lines + 1,
            "startLine": 1, "totalLines": s.lines + 1}},
        "tool_use_id": "toolu_bench", "duration_ms": 12
    })
    .to_string()
    .into_bytes()
}

/// A Bash result: the first 40 lines of the subject, as command output.
fn post_bash(session: &str, s: &Subject) -> Vec<u8> {
    let out: String = s.src.split_inclusive('\n').take(40).collect();
    json!({
        "session_id": session, "transcript_path": "/dev/null", "cwd": "/",
        "permission_mode": "default", "hook_event_name": "PostToolUse",
        "tool_name": "Bash", "tool_input": {"command": "cargo test", "description": "bench"},
        "tool_response": {"stdout": out, "stderr": "", "interrupted": false,
                          "isImage": false, "noOutputExpected": false},
        "tool_use_id": "toolu_bench", "duration_ms": 1278
    })
    .to_string()
    .into_bytes()
}

// ── timing ───────────────────────────────────────────────────────────────────

/// `warmup` discarded runs, then `runs` recorded ones; `one(i)` returns the interval to record.
pub(crate) fn sample(
    cfg: &LatencyConfig,
    mut one: impl FnMut(usize) -> Res<Duration>,
) -> Res<Vec<i64>> {
    let mut v = Vec::with_capacity(cfg.runs);
    for i in 0..cfg.warmup + cfg.runs {
        let d = one(i)?;
        if i >= cfg.warmup {
            v.push(d.as_nanos() as i64);
        }
    }
    Ok(v)
}

fn hook_cmd(bin: &Path, iso: &Sandbox, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    iso.apply(&mut c);
    c.arg("hook")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Spawn, write the payload, wait. The interval opens before the process exists and closes when
/// it has exited.
pub(crate) fn time_spawn(cmd: &mut Command, payload: &[u8]) -> Res<(i32, Duration)> {
    let t0 = Instant::now();
    let mut child = cmd.spawn().ctx("spawn the hook")?;
    {
        let mut stdin = child.stdin.take().ok_or("the hook has no stdin")?;
        stdin.write_all(payload).ctx("write the hook payload")?;
    }
    let status = child.wait().ctx("wait for the hook")?;
    let d = t0.elapsed();
    let code = status
        .code()
        .ok_or_else(|| format!("the hook did not exit normally: {status}"))?;
    Ok((code, d))
}

struct SpawnSeries<'a> {
    build: &'a Build,
    iso: &'a Sandbox,
    cfg: &'a LatencyConfig,
}

impl SpawnSeries<'_> {
    fn run(
        &self,
        id: &str,
        what: &str,
        subject: Option<String>,
        args: &[&str],
        expect_exit: i32,
        payload: &dyn Fn(usize) -> Vec<u8>,
    ) -> Res<Series> {
        let mut cmd = hook_cmd(&self.build.bin, self.iso, args);
        let ns = sample(self.cfg, |i| {
            let p = payload(i);
            let (code, d) = time_spawn(&mut cmd, &p)?;
            if code != expect_exit {
                return Err(format!(
                    "{id}: the hook exited {code}, expected {expect_exit} (run {i}); \
                     this series would not be measuring the path it names"
                ));
            }
            Ok(d)
        })?;
        Series::measured(id, what, subject, &ns)
    }
}

fn run_in_process(
    iso: &Sandbox,
    cfg: &LatencyConfig,
    id: &str,
    args: &[&str],
    expect_exit: i32,
    payload: &dyn Fn(usize) -> Vec<u8>,
) -> Res<Vec<i64>> {
    let db = iso.db.to_string_lossy().into_owned();
    let var = move |k: &str| -> Option<String> { (k == "LUMEN_DB").then(|| db.clone()) };
    let env = Env {
        var: &var,
        state_dir: &iso.tmp,
    };
    let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    sample(cfg, |i| {
        let p = payload(i);
        let t0 = Instant::now();
        let out = run_with(&argv, &p, &env);
        let d = t0.elapsed();
        if out.exit != expect_exit || (expect_exit == 0 && !out.stderr.is_empty()) {
            return Err(format!(
                "{id}: in-process exit {} with stderr {:?}, expected exit {expect_exit} and no \
                 stderr (run {i})",
                out.exit, out.stderr
            ));
        }
        Ok(d)
    })
}

// ── the hook series ──────────────────────────────────────────────────────────

const FLOOR_ID: &str = "hook.floor";

#[derive(Clone, Copy)]
enum Kind {
    PreRead,
    PostRead,
    PostBash,
}

/// One in-process series: a hook, its payload shape, and the exit code it must return.
struct InProc<'a> {
    id: &'static str,
    what: &'static str,
    subject: Option<&'a Subject>,
    args: &'static [&'static str],
    exit: i32,
    kind: Kind,
}

const IDENTITY_ID: &str = "hook.per_call_identity";
const IDENTITY_WHAT: &str = "per-call identity hook: PreToolUse `hook intercept` on a call of a Lumen MCP tool, which writes the record the server claims; spawn to exit";

/// The slot for the per-call identity hook, by what the build does ([`Build::identity`], found by
/// `driver::probe_identity`): a build that records identity when `hook intercept` is given a
/// PreToolUse payload for one of Lumen's tools gets a timed series, any other says so.
///
/// Each timed run is one fresh `tool_use_id` and session; afterwards (untimed) the record must be
/// there, and it is removed, which is what the server does when it claims it: the directory holds
/// at most the one record, so the hook's prune pass reads the same few entries it would in a
/// session. An exit other than 0 or a missing record stops the run: the series would not be
/// measuring the path it names.
fn identity_series(build: &Build, iso: &Sandbox, cfg: &LatencyConfig) -> Res<Series> {
    if build.identity != Identity::Hook {
        return Ok(Series::with_status(
            IDENTITY_ID,
            IDENTITY_WHAT,
            format!(
                "n/a — this build records no per-call identity (a PreToolUse payload for \
                 mcp__lumen__smart_read, sent to `hook intercept`, left no record beside the \
                 ledger; commit {})",
                &build.sha[..12]
            ),
        ));
    }
    let mut cmd = hook_cmd(&build.bin, iso, &["intercept"]);
    let ns = sample(cfg, |i| {
        let id = format!("toolu_bench_identity_{i}");
        let payload = lumen_call_payload(&format!("bench-{}-identity-{i}", build.label), &id);
        let (code, d) = time_spawn(&mut cmd, &payload)?;
        if code != 0 {
            return Err(format!(
                "{IDENTITY_ID}: the hook exited {code}, expected 0 (run {i})"
            ));
        }
        let found = records_for(iso, &id);
        if found.is_empty() {
            return Err(format!(
                "{IDENTITY_ID}: the hook left no record for {id} (run {i}); this series would \
                 not be measuring the path it names"
            ));
        }
        for f in found {
            std::fs::remove_file(&f).ctx(&format!("remove the record {}", f.display()))?;
        }
        Ok(d)
    })?;
    Series::measured(IDENTITY_ID, IDENTITY_WHAT, None, &ns)
}

/// Every `hook.*` series of a build whose hooks are not `lumen-mcp hook ...`, in the order
/// [`hooks`] writes them, each `n/a — <reason>`. `why` is the reason for the spawn and in-process
/// series; the Bash meter and identity have their own where the map gives one.
fn na_hooks(caps: &Caps, why: &str, out: &mut Vec<Series>) {
    let bash = caps.na_bash_meter().filter(|_| !caps.no_lumen);
    let identity = caps.na_identity().filter(|_| !caps.no_lumen);
    let rows: [(&str, &str, Option<&str>); 9] = [
        (FLOOR_ID, "process floor", None),
        (
            "hook.intercept_pass.spawn",
            "PreToolUse Read under the threshold",
            None,
        ),
        (
            "hook.intercept_block.spawn",
            "PreToolUse Read over the threshold",
            None,
        ),
        ("hook.meter_read.spawn", "PostToolUse Read", None),
        ("hook.meter_bash.spawn", "PostToolUse Bash", bash.as_deref()),
        (
            "hook.intercept_pass.inproc",
            "run_with, warm: PreToolUse Read under the threshold",
            Some(INPROC_NA),
        ),
        (
            "hook.intercept_block.inproc",
            "run_with, warm: PreToolUse Read over the threshold",
            Some(INPROC_NA),
        ),
        (
            "hook.meter_read.inproc",
            "run_with, warm: PostToolUse Read",
            Some(INPROC_NA),
        ),
        (
            "hook.meter_bash.inproc",
            "run_with, warm: PostToolUse Bash",
            Some(INPROC_NA),
        ),
    ];
    for (id, what, own) in rows {
        out.push(Series::na_because(id, what, own.unwrap_or(why)));
    }
    out.push(Series::na_because(
        IDENTITY_ID,
        IDENTITY_WHAT,
        identity.as_deref().unwrap_or(why),
    ));
}

/// Why no prebuilt build has an in-process series.
const INPROC_NA: &str = "in-process hooks need the hook library linked into the harness, which only a build from this checkout has; no release asset does";

fn hooks(
    build: &Build,
    subjects: &[Subject],
    cfg: &LatencyConfig,
    work: &Path,
    in_process: bool,
    out: &mut Vec<Series>,
) -> Res<()> {
    // Before anything is created or spawned: a version the map says has no `hook` subcommand is
    // never asked for one, because `lumen-mcp hook ...` on those binaries starts an MCP server.
    if let Some(caps) = &build.caps
        && !caps.may_spawn_hook()
    {
        let why = caps
            .na_hook_subcommand()
            .unwrap_or_else(|| "no Lumen: there are no hooks".to_string());
        na_hooks(caps, &why, out);
        // A version before 1.6.0 has script hooks: time the tag's own scripts (V3), which
        // replace the five spawn rows just written. No Lumen has no hooks at all.
        if !caps.no_lumen {
            crate::versions::script_hooks::overlay(build, subjects, cfg, work, out)?;
        }
        return Ok(());
    }
    let spawn_iso = Sandbox::create(&work.join("hooks-spawn"))?;
    let proc_iso = Sandbox::create(&work.join("hooks-inproc"))?;
    let tag = build.label.as_str();
    let small = subjects.iter().find(|s| s.bucket == BUCKETS[0].name);
    let medium = subjects.iter().find(|s| s.bucket == BUCKETS[1].name);
    let sp = SpawnSeries {
        build,
        iso: &spawn_iso,
        cfg,
    };

    // The floor: the process and nothing else. The payload is the medium subject's, so the stdin
    // write costs what it costs in the series it is subtracted from.
    let floor_payload = medium.map_or_else(
        || pre_read("bench-floor", Path::new("/")),
        |m| post_read("bench-floor", m),
    );
    out.push(sp.run(
        FLOOR_ID,
        "process floor: spawn, stdin, exit, for a hook subcommand that does no work",
        None,
        &["no-such-subcommand"],
        1,
        &|_| floor_payload.clone(),
    )?);

    // The ledger must exist, with this build's own schema, before the intercept reads it. One
    // meter run creates it the way a real session does.
    if let Some(m) = medium {
        let mut cmd = hook_cmd(&build.bin, &spawn_iso, &["meter"]);
        let (code, _) = time_spawn(&mut cmd, &post_read("bench-create", m))?;
        if code != 0 || !spawn_iso.db.is_file() {
            return Err(format!(
                "the meter hook exited {code} and left no ledger at {}",
                spawn_iso.db.display()
            ));
        }
    }
    let created_reads = i64::from(medium.is_some());

    match small {
        Some(s) => out.push(sp.run(
            "hook.intercept_pass.spawn",
            "PreToolUse Read of a file under the 300-line threshold: not intercepted (exit 0)",
            Some(s.label()),
            &["intercept"],
            0,
            &|i| pre_read(&format!("bench-{tag}-pass-{i}"), &s.abs),
        )?),
        None => out.push(Series::na(
            "hook.intercept_pass.spawn",
            "PreToolUse Read under the threshold",
            "no file under 300 lines in the corpus",
        )),
    }
    match medium {
        Some(m) => {
            out.push(sp.run(
                "hook.intercept_block.spawn",
                "PreToolUse Read of a file over the threshold, first Read of the session: blocked (exit 2)",
                Some(m.label()),
                &["intercept"],
                2,
                &|i| pre_read(&format!("bench-{tag}-block-{i}"), &m.abs),
            )?);
            out.push(sp.run(
                "hook.meter_read.spawn",
                "PostToolUse Read: count the file's tokens, write one ledger row (exit 0)",
                Some(m.label()),
                &["meter"],
                0,
                &|i| post_read(&format!("bench-{tag}-mr-{i}"), m),
            )?);
            out.push(sp.run(
                "hook.meter_bash.spawn",
                "PostToolUse Bash: count the output's tokens, write one ledger row (exit 0); output is the first 40 lines of the subject",
                Some(m.label()),
                &["meter"],
                0,
                &|i| post_bash(&format!("bench-{tag}-mb-{i}"), m),
            )?);
            let want_reads = created_reads + (cfg.warmup + cfg.runs) as i64;
            let got_reads = ledger_tool_count(&spawn_iso.db, "Read")?;
            let got_bash = ledger_tool_count(&spawn_iso.db, "Bash")?;
            if got_reads != want_reads || got_bash != (cfg.warmup + cfg.runs) as i64 {
                return Err(format!(
                    "the meter hook wrote {got_reads} Read and {got_bash} Bash rows, expected \
                     {want_reads} and {}: a series did not measure a metered call",
                    cfg.warmup + cfg.runs
                ));
            }
        }
        None => {
            for (id, what) in [
                (
                    "hook.intercept_block.spawn",
                    "PreToolUse Read over the threshold",
                ),
                ("hook.meter_read.spawn", "PostToolUse Read"),
                ("hook.meter_bash.spawn", "PostToolUse Bash"),
            ] {
                out.push(Series::na(
                    id,
                    what,
                    "no file of 300-999 lines in the corpus",
                ));
            }
        }
    }

    // In-process: the same four hooks, called as functions.
    let jobs = [
        InProc {
            id: "hook.intercept_pass.inproc",
            what: "run_with, warm: PreToolUse Read under the threshold",
            subject: small,
            args: &["intercept"],
            exit: 0,
            kind: Kind::PreRead,
        },
        InProc {
            id: "hook.intercept_block.inproc",
            what: "run_with, warm: PreToolUse Read over the threshold, blocked",
            subject: medium,
            args: &["intercept"],
            exit: 2,
            kind: Kind::PreRead,
        },
        InProc {
            id: "hook.meter_read.inproc",
            what: "run_with, warm: PostToolUse Read, one ledger row",
            subject: medium,
            args: &["meter"],
            exit: 0,
            kind: Kind::PostRead,
        },
        InProc {
            id: "hook.meter_bash.inproc",
            what: "run_with, warm: PostToolUse Bash, one ledger row",
            subject: medium,
            args: &["meter"],
            exit: 0,
            kind: Kind::PostBash,
        },
    ];
    if !in_process {
        for j in &jobs {
            out.push(Series::na(
                j.id,
                j.what,
                "the harness links this checkout's hook library, so it can time in-process only a binary built from the checkout; the comparable figure for any other build is spawn minus floor",
            ));
        }
    } else {
        if let Some(m) = medium {
            // The ledger, so the intercept's lookup and the meter's insert hit a real file.
            let (code, _) = time_spawn(
                &mut hook_cmd(&build.bin, &proc_iso, &["meter"]),
                &post_read("bench-create", m),
            )?;
            if code != 0 {
                return Err(format!(
                    "the meter hook exited {code} creating the in-process ledger"
                ));
            }
        }
        for (n, j) in jobs.iter().enumerate() {
            let Some(s) = j.subject else {
                out.push(Series::na(
                    j.id,
                    j.what,
                    "no subject file in the corpus for this bucket",
                ));
                continue;
            };
            let payload = |i: usize| -> Vec<u8> {
                let session = format!("bench-{tag}-ip{n}-{i}");
                match j.kind {
                    Kind::PreRead => pre_read(&session, &s.abs),
                    Kind::PostRead => post_read(&session, s),
                    Kind::PostBash => post_bash(&session, s),
                }
            };
            let ns = run_in_process(&proc_iso, cfg, j.id, j.args, j.exit, &payload)?;
            out.push(Series::measured(j.id, j.what, Some(s.label()), &ns)?);
        }
    }

    out.push(identity_series(build, &spawn_iso, cfg)?);
    Ok(())
}

// ── the server series ────────────────────────────────────────────────────────

struct Pending {
    id: Option<String>,
    what: String,
    subject: Option<String>,
    ns: Vec<i64>,
    calls: usize,
    expect: &'static str,
}

fn put_files(root: &Path, subjects: &[Subject]) -> Res<()> {
    for s in subjects {
        let dest = root.join(&s.rel);
        let parent = dest.parent().ok_or("a scratch file with no parent")?;
        std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
        std::fs::write(&dest, s.src.as_bytes()).ctx(&format!("write {}", dest.display()))?;
    }
    Ok(())
}

/// One timed call: the round trip, then an untimed wait for the ledger row.
fn timed_call(s: &mut Server, f: impl FnOnce(&mut Server) -> Res<()>) -> Res<Duration> {
    f(s)?;
    let d = s.last_rtt;
    s.sync()?;
    Ok(d)
}

fn settle(parts: Vec<Pending>, db: &Path, out: &mut Vec<Series>) -> Res<()> {
    let rows = ledger_rows(db)?;
    let want: usize = parts.iter().map(|p| p.calls).sum();
    if rows.len() != want {
        return Err(format!(
            "{want} server calls but {} ledger rows: a call went unmetered",
            rows.len()
        ));
    }
    let mut at = 0usize;
    for p in parts {
        let mut routes = BTreeMap::new();
        for r in &rows[at..at + p.calls] {
            *routes.entry(r.routed_via.clone()).or_insert(0usize) += 1;
        }
        at += p.calls;
        let Some(id) = p.id else { continue };
        // A ledger with no `routed_via` column cannot say how a call was routed: that is recorded
        // as such and is not "mixed".
        let all_expected = routes.len() == 1
            && (routes.contains_key(p.expect) || routes.contains_key(crate::driver::NO_ROUTE));
        let mut s = Series::measured(&id, &p.what, p.subject, &p.ns)?;
        if !all_expected {
            s.status = format!(
                "measured, mixed routes: expected every call to be {}, see routes",
                p.expect
            );
        }
        s.routes = routes;
        out.push(s);
    }
    Ok(())
}

fn servers(
    build: &Build,
    subjects: &[Subject],
    cfg: &LatencyConfig,
    work: &Path,
    out: &mut Vec<Series>,
) -> Res<String> {
    // Spawn plus `initialize`, to the reply.
    let start_iso = Sandbox::create(&work.join("srv-start"))?;
    let scratch = work.join("srv-files");
    put_files(&scratch, subjects)?;
    let ns = sample(cfg, |i| {
        let t0 = Instant::now();
        let mut s = Server::start(
            &build.bin,
            &scratch,
            &start_iso,
            &format!("bench-start-{i}"),
            false,
            build.identity,
        )?;
        let d = t0.elapsed();
        s.set_clamped(build.clamped());
        s.finish()?;
        Ok(d)
    })?;
    out.push(Series::measured(
        "server.start",
        "spawn lumen-mcp and answer `initialize`",
        None,
        &ns,
    )?);

    let n = cfg.warmup + cfg.runs;

    // Repeat reads off, asked for (`Server::start` sets the variable): on is the default from 1.8.0.
    let iso = Sandbox::create(&work.join("srv-plain"))?;
    let mut s = Server::start(
        &build.bin,
        &scratch,
        &iso,
        "bench-lat-plain",
        false,
        build.identity,
    )?;
    s.set_clamped(build.clamped());
    let version = s.version.clone();
    let mut parts = Vec::new();
    let ns = sample(cfg, |_| {
        s.ping()?;
        Ok(s.last_rtt)
    })?;
    out.push(Series::measured(
        "server.ping",
        "JSON-RPC round trip with no work: the floor under every server series",
        None,
        &ns,
    )?);
    for sub in subjects {
        let rel = sub.rel.as_str();
        let ns = sample(cfg, |_| {
            timed_call(&mut s, |s| s.smart_read(rel).map(|_| ()))
        })?;
        parts.push(Pending {
            id: Some(format!("server.smart_read.{}", sub.bucket)),
            what: "smart_read, repeat reads off: the outline".to_string(),
            subject: Some(sub.label()),
            ns,
            calls: n,
            expect: "smart_read",
        });
        let item = sub.recall_item.as_str();
        let ns = sample(cfg, |_| {
            timed_call(&mut s, |s| s.recall(rel, &[item]).map(|_| ()))
        })?;
        parts.push(Pending {
            id: Some(format!("server.recall_file.{}", sub.bucket)),
            what: format!("recall_file for one item (`{item}`)"),
            subject: Some(sub.label()),
            ns,
            calls: n,
            expect: "recall_file",
        });
    }
    s.finish()?;
    // Series in the order the report lists them: smart_read and recall per bucket.
    let mut plain = Vec::new();
    settle(parts, &iso.db, &mut plain)?;
    out.append(&mut plain);

    // Repeat reads on: the gate, answering "unchanged" and "changed". A version without repeat
    // reads has no gate to time: its cells say so.
    if let Some(why) = build.caps.as_ref().and_then(Caps::na_repeat) {
        for sub in subjects {
            for (id, what) in [
                (
                    "server.repeat_unchanged",
                    "smart_read, repeat reads on, file unchanged: the stub",
                ),
                (
                    "server.repeat_changed",
                    "smart_read, repeat reads on, one item changed: the delta",
                ),
            ] {
                let mut na = Series::na_because(&format!("{id}.{}", sub.bucket), what, &why);
                na.subject = Some(sub.label());
                out.push(na);
            }
        }
    } else {
        let scratch2 = work.join("srv-files-repeat");
        put_files(&scratch2, subjects)?;
        let iso2 = Sandbox::create(&work.join("srv-repeat"))?;
        let mut s = Server::start(
            &build.bin,
            &scratch2,
            &iso2,
            "bench-lat-repeat",
            true,
            build.identity,
        )?;
        s.set_clamped(build.clamped());
        let mut parts = Vec::new();
        for sub in subjects {
            let rel = sub.rel.as_str();
            let file = scratch2.join(rel);
            // The base: the first read of the file in this session. Not timed, not a series.
            let ns = sample(&LatencyConfig { runs: 1, warmup: 0 }, |_| {
                timed_call(&mut s, |s| s.smart_read(rel).map(|_| ()))
            })?;
            parts.push(Pending {
                id: None,
                what: String::new(),
                subject: None,
                ns,
                calls: 1,
                expect: "smart_read",
            });
            // Unchanged: the stub does not replace the base, so every call is the same question.
            let ns = sample(cfg, |_| {
                timed_call(&mut s, |s| s.smart_read(rel).map(|_| ()))
            })?;
            parts.push(Pending {
                id: Some(format!("server.repeat_unchanged.{}", sub.bucket)),
                what: "smart_read, repeat reads on, file unchanged since the last read: the stub"
                    .to_string(),
                subject: Some(sub.label()),
                ns,
                calls: n,
                expect: "smart_read_unchanged",
            });
            // Changed: the file alternates between two contents that differ in one item, so every
            // call finds it changed since the last and answers a delta. The write is not timed.
            let ns = sample(cfg, |i| {
                let content = if i % 2 == 0 { &sub.alt } else { &sub.src };
                std::fs::write(&file, content.as_bytes())
                    .ctx(&format!("write {}", file.display()))?;
                timed_call(&mut s, |s| s.smart_read(rel).map(|_| ()))
            })?;
            parts.push(Pending {
                id: Some(format!("server.repeat_changed.{}", sub.bucket)),
                what:
                    "smart_read, repeat reads on, one item changed since the last read: the delta"
                        .to_string(),
                subject: Some(sub.label()),
                ns,
                calls: n,
                expect: "smart_read_delta",
            });
        }
        s.finish()?;
        settle(parts, &iso2.db, out)?;
    }

    claim_series(build, subjects, cfg, work, &scratch, out)?;
    Ok(version)
}

pub const CLAIM_PLACED_ID: &str = "server.identity.placed";
pub const CLAIM_UNPLACED_ID: &str = "server.identity.unplaced";

/// The server's side of live identity, for a build that has it: `smart_read` of the same file on
/// the same server, once with the hook's record in place and the `_meta` id that names it (the
/// server finds the record, reads it and removes it), once with neither (the server finds nothing
/// to claim). The calls alternate, and which goes first alternates too, so a drift in the machine
/// lands on both. Repeat reads are off, so both answer the same outline and the difference between
/// the two medians is the claim. Not emitted for a build without live identity.
fn claim_series(
    build: &Build,
    subjects: &[Subject],
    cfg: &LatencyConfig,
    work: &Path,
    scratch: &Path,
    out: &mut Vec<Series>,
) -> Res<()> {
    if build.identity != Identity::Hook {
        // The two-build mode has always left these out. A version run says why they are absent.
        if let Some(why) = build.caps.as_ref().and_then(Caps::na_identity) {
            for (id, what) in [
                (
                    CLAIM_PLACED_ID,
                    "smart_read with the hook's record in place: the server claims it",
                ),
                (
                    CLAIM_UNPLACED_ID,
                    "smart_read with no record and no _meta id",
                ),
            ] {
                out.push(Series::na_because(id, what, &why));
            }
        }
        return Ok(());
    }
    let Some(m) = subjects.iter().find(|s| s.bucket == BUCKETS[1].name) else {
        return Ok(());
    };
    let iso = Sandbox::create(&work.join("srv-identity"))?;
    let mut s = Server::start(
        &build.bin,
        scratch,
        &iso,
        "bench-lat-identity",
        false,
        build.identity,
    )?;
    s.set_clamped(build.clamped());
    let rel = m.rel.as_str();
    let mut placed = Vec::with_capacity(cfg.runs);
    let mut unplaced = Vec::with_capacity(cfg.runs);
    for i in 0..cfg.warmup + cfg.runs {
        for step in 0..2 {
            let is_placed = (i + step) % 2 == 0;
            let d = timed_call(&mut s, |s| {
                (if is_placed {
                    s.smart_read(rel)
                } else {
                    s.smart_read_unplaced(rel)
                })
                .map(|_| ())
            })?;
            if i >= cfg.warmup {
                let ns = d.as_nanos() as i64;
                if is_placed {
                    placed.push(ns);
                } else {
                    unplaced.push(ns);
                }
            }
        }
    }
    s.finish()?;
    let rows = ledger_rows(&iso.db)?;
    let n = cfg.warmup + cfg.runs;
    if rows.len() != 2 * n {
        return Err(format!(
            "{} server calls but {} ledger rows: a call went unmetered",
            2 * n,
            rows.len()
        ));
    }
    let mut routes = BTreeMap::new();
    for r in &rows {
        *routes.entry(r.routed_via.clone()).or_insert(0usize) += 1;
    }
    let all_expected = routes.len() == 1 && routes.contains_key("smart_read");
    for (id, what, ns, calls) in [
        (
            CLAIM_PLACED_ID,
            "smart_read, repeat reads off, with the hook's record in place and the _meta id that names it: the server claims it",
            placed,
            n,
        ),
        (
            CLAIM_UNPLACED_ID,
            "smart_read, repeat reads off, with no record and no _meta id: nothing to claim",
            unplaced,
            n,
        ),
    ] {
        let mut series = Series::measured(id, what, Some(m.label()), &ns)?;
        if all_expected {
            series.routes.insert("smart_read".to_string(), calls);
        } else {
            series.status = "measured, mixed routes: expected every call to be smart_read, see routes (both series together)".to_string();
            series.routes = routes.clone();
        }
        out.push(series);
    }
    Ok(())
}

fn derive(series: &[Series]) -> Vec<Derived> {
    let med = |id: &str| {
        series
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.lat.as_ref())
            .map(|l| l.median_ns)
    };
    let floor = med(FLOOR_ID);
    let mut out: Vec<Derived> = [
        "intercept_pass",
        "intercept_block",
        "meter_read",
        "meter_bash",
    ]
    .iter()
    .map(|h| {
        let id = format!("hook.{h}.spawn");
        Derived {
            median_ns: med(&id).zip(floor).map(|(a, f)| a - f),
            id: format!("hook.{h}.spawn_minus_floor"),
            what: format!("median({id}) - median({FLOOR_ID})"),
        }
    })
    .collect();
    // Live identity, when the build has it (otherwise there is nothing to subtract). The floor's
    // payload is the medium subject's, a few KB; the identity hook's is a few hundred bytes, so
    // the first figure slightly understates what the hook adds over a bare process.
    out.push(Derived {
        id: format!("{IDENTITY_ID}.spawn_minus_floor"),
        what: format!("median({IDENTITY_ID}) - median({FLOOR_ID})"),
        median_ns: med(IDENTITY_ID).zip(floor).map(|(a, f)| a - f),
    });
    out.push(Derived {
        id: format!("{IDENTITY_ID}.server_claim"),
        what: format!("median({CLAIM_PLACED_ID}) - median({CLAIM_UNPLACED_ID})"),
        median_ns: med(CLAIM_PLACED_ID)
            .zip(med(CLAIM_UNPLACED_ID))
            .map(|(a, b)| a - b),
    });
    out
}

/// The load, as text; a probe that fails says so instead of leaving a blank.
fn load_text() -> String {
    match crate::env::load_now(std::env::consts::OS) {
        Ok(t) => t,
        Err(e) => format!("unknown ({e})"),
    }
}

/// How many passes a latency run is spread over. Every series is measured once per pass, with the
/// builds taking turns, and the samples of all passes are pooled. A burst of other work on the
/// machine (this is routinely a shared laptop) then lands in one pass of every series of both
/// builds instead of wrecking one series of one build.
pub const PASSES: usize = 5;

/// `(passes, recorded runs per series per pass)` for a configuration. The recorded total is
/// `passes * per_pass`, which is `cfg.runs` rounded up to a whole number of passes.
pub fn plan_passes(cfg: &LatencyConfig) -> (usize, usize) {
    let passes = PASSES.min(cfg.runs.max(1));
    (passes, cfg.runs.div_ceil(passes))
}

/// The median of one pass of one series, when it has samples.
fn pass_median(raw: &[i64]) -> Option<i64> {
    lat(raw).map(|l| l.median_ns)
}

/// Pool one build's passes into one set of series, in the first pass's order. The median of each
/// pass is kept beside the pooled figures: how far one pass sits from another says how much the
/// machine moved while the run was going on.
fn merge_passes(passes: Vec<Vec<Series>>) -> Res<Vec<Series>> {
    let mut it = passes.into_iter();
    let mut out = it.next().ok_or("no latency passes")?;
    for s in &mut out {
        s.pass_medians_ns = pass_median(&s.raw).into_iter().collect();
    }
    for pass in it {
        if pass.len() != out.len() {
            return Err(format!(
                "a latency pass has {} series and the first has {}",
                pass.len(),
                out.len()
            ));
        }
        for (o, s) in out.iter_mut().zip(pass) {
            if o.id != s.id {
                return Err(format!(
                    "latency series out of order: {} and {}",
                    o.id, s.id
                ));
            }
            o.pass_medians_ns.extend(pass_median(&s.raw));
            o.raw.extend_from_slice(&s.raw);
            for (route, n) in s.routes {
                *o.routes.entry(route).or_insert(0) += n;
            }
            if o.status == "measured" && s.status != "measured" {
                o.status = s.status;
            }
        }
    }
    for s in &mut out {
        if s.lat.is_some() {
            s.lat = Some(lat(&s.raw).ok_or_else(|| format!("{}: no samples", s.id))?);
        }
    }
    Ok(out)
}

/// The order the builds are measured in, in pass `pass`: every build takes a turn at being
/// first. Pass 0 is the order given; each later pass starts one build further along. For two
/// builds that is the alternate-and-flip the harness has always done; for N it is a rotation, so
/// no build is first (or always last) in every pass whenever there are at least two passes.
pub fn pass_order(builds: usize, pass: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..builds).collect();
    if builds > 0 {
        order.rotate_left(pass % builds);
    }
    order
}

/// A build with nothing to time (no Lumen): every series is `n/a — <reason>`.
fn no_lumen_latency(build: &Build, subjects: &[Subject], load: &str) -> BuildLatency {
    let why = "no Lumen: there is no hook, no server and no daemon to time";
    let mut series = Vec::new();
    if let Some(caps) = &build.caps {
        na_hooks(caps, why, &mut series);
    }
    for (id, what) in [
        ("server.start", "spawn lumen-mcp and answer `initialize`"),
        ("server.ping", "JSON-RPC round trip with no work"),
    ] {
        series.push(Series::na_because(id, what, why));
    }
    for sub in subjects {
        for (prefix, what) in [
            ("server.smart_read", "smart_read: the outline"),
            ("server.recall_file", "recall_file for one item"),
            (
                "server.repeat_unchanged",
                "smart_read, repeat reads on: the stub",
            ),
            (
                "server.repeat_changed",
                "smart_read, repeat reads on: the delta",
            ),
        ] {
            let mut na = Series::na_because(&format!("{prefix}.{}", sub.bucket), what, why);
            na.subject = Some(sub.label());
            series.push(na);
        }
    }
    for (id, what) in [
        (
            CLAIM_PLACED_ID,
            "smart_read with the hook's record in place",
        ),
        (
            CLAIM_UNPLACED_ID,
            "smart_read with no record and no _meta id",
        ),
    ] {
        series.push(Series::na_because(id, what, why));
    }
    let derived = derive(&series);
    BuildLatency {
        label: build.label.clone(),
        reported_version: "n/a — no Lumen".to_string(),
        load_before: load.to_string(),
        load_after: load.to_string(),
        series,
        derived,
    }
}

/// All latency series for every build, pooled over [`PASSES`] passes in which the builds take
/// turns ([`pass_order`]: the first build moves along every pass), so a drift in the machine's
/// state falls on every build alike. `cfg.warmup` is per series per pass. A build's hooks are
/// run in-process only when it was built from this checkout ([`Build::inproc`]): the harness
/// links the checkout's library. A "no Lumen" build is not run: its series say so.
pub fn measure_builds(
    builds: &[Build],
    subjects: &[Subject],
    cfg: &LatencyConfig,
    work: &Path,
) -> Res<Vec<BuildLatency>> {
    let (passes, per_pass) = plan_passes(cfg);
    let pass_cfg = LatencyConfig {
        runs: per_pass,
        warmup: cfg.warmup,
    };
    let load_before = load_text();
    // Only builds that can be run take part in the passes (and so in the rotation).
    let active: Vec<usize> = (0..builds.len())
        .filter(|i| !builds[*i].is_no_lumen())
        .collect();
    let mut pooled: Vec<Vec<Vec<Series>>> = builds.iter().map(|_| Vec::new()).collect();
    let mut versions: Vec<String> = builds.iter().map(|_| String::new()).collect();
    for pass in 0..passes {
        for slot in pass_order(active.len(), pass) {
            let bi = active[slot];
            let build = &builds[bi];
            eprintln!(
                "lumen-bench: latency pass {}/{passes}, {} ({per_pass} runs)",
                pass + 1,
                build.label
            );
            let w = work.join(format!("lat-{}-pass{pass}", build.label));
            let mut series = Vec::new();
            hooks(build, subjects, &pass_cfg, &w, build.inproc, &mut series)?;
            let version = servers(build, subjects, &pass_cfg, &w, &mut series)?;
            if !versions[bi].is_empty() && versions[bi] != version {
                return Err(format!(
                    "{} reported version {} and then {version} within one run",
                    build.label, versions[bi]
                ));
            }
            versions[bi] = version;
            pooled[bi].push(series);
        }
    }
    let load_after = load_text();
    let mut out = Vec::with_capacity(builds.len());
    for ((build, p), version) in builds.iter().zip(pooled).zip(versions) {
        if build.is_no_lumen() {
            out.push(no_lumen_latency(build, subjects, &load_before));
            continue;
        }
        let series = merge_passes(p)?;
        let derived = derive(&series);
        out.push(BuildLatency {
            label: build.label.clone(),
            reported_version: version,
            load_before: load_before.clone(),
            load_after: load_after.clone(),
            series,
            derived,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(id: &str, ns: i64) -> Series {
        Series {
            id: id.to_string(),
            what: String::new(),
            subject: None,
            status: "measured".to_string(),
            lat: lat(&[ns]),
            routes: BTreeMap::new(),
            pass_medians_ns: vec![],
            raw: vec![ns],
        }
    }

    #[test]
    fn the_derived_figure_is_spawn_minus_floor() {
        let mk = series;
        let d = derive(&[mk(FLOOR_ID, 1_000), mk("hook.meter_read.spawn", 3_500)]);
        let m = d
            .iter()
            .find(|d| d.id == "hook.meter_read.spawn_minus_floor")
            .unwrap();
        assert_eq!(m.median_ns, Some(2_500));
        let none = d
            .iter()
            .find(|d| d.id == "hook.meter_bash.spawn_minus_floor")
            .unwrap();
        assert_eq!(none.median_ns, None);
    }

    #[test]
    fn the_identity_figures_exist_for_every_build_and_are_empty_without_the_hook() {
        let find = |d: &[Derived], id: &str| d.iter().find(|d| d.id == id).unwrap().median_ns;
        // A build with no identity hook: the entries are there, without a figure.
        let d = derive(&[series(FLOOR_ID, 1_000)]);
        assert_eq!(find(&d, "hook.per_call_identity.spawn_minus_floor"), None);
        assert_eq!(find(&d, "hook.per_call_identity.server_claim"), None);
        // A build with it: the hook minus the floor, and the claimed call minus the unclaimed one.
        let d = derive(&[
            series(FLOOR_ID, 1_000),
            series(IDENTITY_ID, 4_000),
            series(CLAIM_PLACED_ID, 700),
            series(CLAIM_UNPLACED_ID, 500),
        ]);
        assert_eq!(
            find(&d, "hook.per_call_identity.spawn_minus_floor"),
            Some(3_000)
        );
        assert_eq!(find(&d, "hook.per_call_identity.server_claim"), Some(200));
    }

    #[test]
    fn a_build_that_records_no_identity_gets_an_n_a_slot_that_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let iso = Sandbox::create(tmp.path()).unwrap();
        let cfg = LatencyConfig { runs: 1, warmup: 0 };
        let build = Build {
            label: "old".to_string(),
            role: "baseline",
            sha: "5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94".to_string(),
            dirty: false,
            bin: PathBuf::from("does-not-exist"),
            profile: "release",
            profile_note: String::new(),
            source: String::new(),
            identity: Identity::Env,
            inproc: false,
            caps: None,
        };
        let s = identity_series(&build, &iso, &cfg).unwrap();
        assert!(s.lat.is_none());
        assert!(
            s.status
                .starts_with("n/a — this build records no per-call identity"),
            "{}",
            s.status
        );
        assert!(s.status.contains("5d1ec0408ad2"), "{}", s.status);
    }

    #[test]
    fn a_run_is_split_into_passes_that_add_up_to_at_least_the_runs_asked_for() {
        let cfg = |runs| LatencyConfig { runs, warmup: 1 };
        assert_eq!(plan_passes(&cfg(200)), (5, 40));
        assert_eq!(plan_passes(&cfg(100)), (5, 20));
        assert_eq!(plan_passes(&cfg(101)), (5, 21));
        assert_eq!(plan_passes(&cfg(3)), (3, 1));
        assert_eq!(plan_passes(&cfg(1)), (1, 1));
    }

    #[test]
    fn pooled_passes_have_the_samples_of_all_of_them() {
        let pass = |ns: &[i64], status: &str, route_n: usize| {
            let mut s = Series::measured("server.ping", "w", None, ns).unwrap();
            status.clone_into(&mut s.status);
            s.routes.insert("smart_read".to_string(), route_n);
            vec![s, Series::na("hook.per_call_identity", "w", "none")]
        };
        let merged = merge_passes(vec![
            pass(&[10, 20, 30], "measured", 3),
            pass(&[40, 50, 60], "measured, mixed routes: x", 2),
        ])
        .unwrap();
        let ping = &merged[0];
        let l = ping.lat.as_ref().unwrap();
        assert_eq!(l.n, 6);
        assert_eq!(l.min_ns, 10);
        assert_eq!(l.max_ns, 60);
        assert_eq!(ping.status, "measured, mixed routes: x");
        assert_eq!(ping.routes["smart_read"], 5);
        // The median of each pass is kept in pass order, beside the pooled figures.
        assert_eq!(ping.pass_medians_ns, vec![20, 50]);
        // A series with no figure stays without one.
        assert!(merged[1].lat.is_none());
        assert!(merged[1].pass_medians_ns.is_empty());
        assert!(merged[1].status.starts_with("n/a"));
        // Passes that do not line up are an error, not a quiet truncation.
        let ragged = vec![pass(&[1], "measured", 1), vec![]];
        assert!(merge_passes(ragged).is_err());
    }

    #[test]
    fn two_builds_alternate_exactly_as_they_always_did() {
        for pass in 0..PASSES {
            let mut old: Vec<usize> = (0..2).collect();
            if pass % 2 == 1 {
                old.reverse();
            }
            assert_eq!(pass_order(2, pass), old, "pass {pass}");
        }
    }

    #[test]
    fn the_pass_order_rotates_so_no_build_is_first_in_every_pass() {
        for n in 2..=11usize {
            let first: Vec<usize> = (0..PASSES).map(|p| pass_order(n, p)[0]).collect();
            for b in 0..n {
                assert!(
                    first.iter().any(|f| *f != b),
                    "{n} builds: build {b} is first in every pass: {first:?}"
                );
            }
            // Every pass is a permutation of the builds.
            for p in 0..PASSES {
                let mut o = pass_order(n, p);
                o.sort_unstable();
                assert_eq!(o, (0..n).collect::<Vec<_>>());
            }
            // And no build is always last either.
            let last: Vec<usize> = (0..PASSES)
                .map(|p| *pass_order(n, p).last().unwrap())
                .collect();
            for b in 0..n {
                assert!(last.iter().any(|l| *l != b), "{n} builds: {b} always last");
            }
        }
        assert_eq!(pass_order(4, 0), vec![0, 1, 2, 3]);
        assert_eq!(pass_order(4, 1), vec![1, 2, 3, 0]);
        assert_eq!(pass_order(1, 3), vec![0]);
        assert!(pass_order(0, 3).is_empty());
    }

    fn fake_subject(bucket: &str) -> Subject {
        Subject {
            bucket: bucket.to_string(),
            rel: "a.rs".to_string(),
            lines: 10,
            bytes: 100,
            recall_item: "f".to_string(),
            abs: PathBuf::from("a.rs"),
            src: String::new(),
            alt: String::new(),
        }
    }

    #[test]
    fn no_lumen_has_no_hook_server_or_daemon_series_and_every_one_says_so() {
        let b = Build::no_lumen();
        let l = no_lumen_latency(&b, &[fake_subject("<300")], "load");
        assert!(l.reported_version.starts_with("n/a"));
        assert!(!l.series.is_empty());
        for s in &l.series {
            assert!(s.lat.is_none() && s.raw.is_empty(), "{} has a figure", s.id);
            assert!(s.status.starts_with("n/a — "), "{}: {}", s.id, s.status);
        }
        assert!(l.series.iter().any(|s| s.id == "hook.per_call_identity"));
        assert!(
            l.series
                .iter()
                .any(|s| s.id == "server.repeat_unchanged.<300")
        );
        assert!(l.derived.iter().all(|d| d.median_ns.is_none()));
    }

    #[test]
    fn a_version_without_a_hook_subcommand_gets_na_hook_rows_and_spawns_nothing() {
        // `bin` does not exist: any spawn would be an error. The hook series are n/a, in the
        // order `hooks` writes them, each with a reason of its own kind.
        let m = crate::manifest::VersionsManifest::load_committed().unwrap();
        let caps = Caps::from_entry("1.0.0", m.get("1.0.0").unwrap()).unwrap();
        let build = Build {
            label: "1.0.0".to_string(),
            role: "version",
            sha: "f80a0eb47fdb9b486f174f444ad61f45cce42d54".to_string(),
            dirty: false,
            bin: PathBuf::from("/does/not/exist/lumen-mcp"),
            profile: "release",
            profile_note: String::new(),
            source: String::new(),
            identity: Identity::Env,
            inproc: false,
            caps: Some(caps),
        };
        let tmp = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        hooks(
            &build,
            &[],
            &LatencyConfig { runs: 1, warmup: 0 },
            tmp.path(),
            false,
            &mut out,
        )
        .unwrap();
        let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "hook.floor",
                "hook.intercept_pass.spawn",
                "hook.intercept_block.spawn",
                "hook.meter_read.spawn",
                "hook.meter_bash.spawn",
                "hook.intercept_pass.inproc",
                "hook.intercept_block.inproc",
                "hook.meter_read.inproc",
                "hook.meter_bash.inproc",
                "hook.per_call_identity",
            ]
        );
        for s in &out {
            assert!(
                s.lat.is_none() && s.status.starts_with("n/a — "),
                "{}: {}",
                s.id,
                s.status
            );
        }
        let by = |id: &str| out.iter().find(|s| s.id == id).unwrap().status.clone();
        assert!(by("hook.meter_bash.spawn").contains("no Bash meter"));
        assert!(by("hook.meter_read.spawn").contains("no `lumen-mcp hook` subcommand"));
        assert!(by("hook.meter_read.inproc").contains("no release asset"));
        assert!(by("hook.per_call_identity").contains("no per-call identity"));
    }
}
