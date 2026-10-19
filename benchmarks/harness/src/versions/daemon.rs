//! Daemon metrics for the version-over-version benchmark (V4): ingest throughput, cold start,
//! and idle CPU and memory, for one version's `lumen-daemon` (and its `lumen-mcp`).
//!
//! Everything here runs through [`Sandbox`], inside [`RealLedgerGuard`], and never spawns a
//! binary any other way. [`run`] is what the report (V5) calls.
//!
//! # The synthetic transcript set
//!
//! Ingest throughput needs transcripts, and real ones hold a person's work. [`generate`] writes a
//! seeded set of Claude Code-shaped JSONL files (about 40 MB by default) from a word list: no real
//! transcript, path or name goes in. Its digest is committed (`benchmarks/versions/transcripts.sha256`)
//! and a test compares it, so the input of every run is provably the same bytes. Session ids start
//! with `lumen-bench-`, so a row that reached a real ledger trips the guard.
//!
//! # What is measured, and what is not
//!
//! - Ingest: the daemon starts on an empty projects directory and is ready (port accepting and the
//!   `turns` table present); the set is moved in with renames; `COUNT(*) FROM turns` is polled every
//!   50 ms, read-only, until it equals the set's turn count, or, for a version that drops or adds
//!   turns, until it has not changed for 10 s. The time is from the move to the last change of the
//!   count, so no waiting is in it. `bytes_per_s` is the set's size over that time.
//! - Cold start: spawn to ready (daemon), spawn to the `initialize` reply (`lumen-mcp`), spawn to exit
//!   of the first hook. On Linux the page cache is dropped before each run when `sudo -n` allows it;
//!   otherwise, and on macOS and Windows, the figure is labelled "warm file cache".
//! - Idle: a 30 s settle, then one sample a second for 120 s, and on until 60 samples are in. CPU is seconds per minute of wall time
//!   between the first and last sample; RSS is the median and the peak of the samples.
//!
//! Not measured here: script hooks (the script-hook series does), and 1.0.0's daemon on Windows,
//! which is `n/a`: its home lookup is the known-folder API, which ignores the sandbox's
//! `USERPROFILE`, so it would read the real projects directory.

use crate::manifest::VersionEntry;
use crate::sandbox::{OLD_DAEMON_PORT, RealLedgerGuard, Sandbox};
use crate::util::{Ctx, Res};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

// --- the synthetic transcript set -------------------------------------------------------------

/// The seed of the committed set. Changing it, or the generator, changes the digest.
pub const SEED: u64 = 0x4C55_4D45_4E56_3400;
/// About 40 MB.
pub const TARGET_BYTES: u64 = 40 * 1024 * 1024;
pub const FILES: usize = 40;
/// The committed digest and counts of the standard set (`Spec::standard`).
pub const COMMITTED: &str = include_str!("../../../versions/transcripts.sha256");

/// 2026-01-01T00:00:00Z in milliseconds: the first timestamp of the set.
const EPOCH_MS: i64 = 1_767_225_600_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub seed: u64,
    pub target_bytes: u64,
    pub files: usize,
}

impl Spec {
    pub fn standard() -> Spec {
        Spec {
            seed: SEED,
            target_bytes: TARGET_BYTES,
            files: FILES,
        }
    }
}

/// What a generation produced. `turns` counts distinct assistant message ids with usage: the rows
/// a daemon that ingests every one of them ends with (a message id repeats across the lines of one
/// message, as in a real transcript, and a ledger keeps it once).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub files: usize,
    pub bytes: u64,
    pub lines: u64,
    pub turns: u64,
    pub digest: String,
}

struct Rng(u64);

impl Rng {
    /// splitmix64: small, and the same on every platform.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
    fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len() as u64) as usize]
    }
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "cache", "delta", "engine", "field", "graph", "handle", "index", "joint",
    "kernel", "ledger", "module", "node", "offset", "parse", "queue", "route", "state", "token",
    "unit", "value", "window", "yield", "zone", "batch", "block", "chunk", "count", "debug",
    "event", "frame", "guard", "hash", "input", "label", "merge", "order", "patch", "range",
    "scope", "table", "trace", "update", "vector", "write", "buffer", "config", "driver", "filter",
    "header", "lookup", "mapper", "output", "reader", "sender", "worker", "result", "option",
    "length", "source", "target",
];

fn words(rng: &mut Rng, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 12);
    while s.len() < bytes {
        s.push_str(rng.word());
        s.push(' ');
    }
    s.truncate(bytes);
    s
}

/// Numbered source-looking lines, as a Read result looks.
fn code_text(rng: &mut Rng, bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 80);
    let mut n = 1;
    while s.len() < bytes {
        let indent = " ".repeat(rng.below(4) as usize * 4);
        let (a, b, c) = (rng.word(), rng.word(), rng.word());
        s.push_str(&format!(
            "{n:>6}\t{indent}let {a}_{b} = {c}({a}, {});\n",
            rng.below(1000)
        ));
        n += 1;
    }
    s
}

/// ISO-8601 UTC with milliseconds, from milliseconds since the epoch (no calendar crate).
pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let s = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        s / 3600,
        (s % 3600) / 60,
        s % 60
    )
}

const TOOLS: &[&str] = &["Read", "Bash", "Grep", "Edit"];

fn push_line(v: &Value, buf: &mut Vec<u8>, lines: &mut u64) {
    buf.extend_from_slice(v.to_string().as_bytes());
    buf.push(b'\n');
    *lines += 1;
}

/// One transcript: lines until `target` bytes (at least one turn). Returns the bytes and counts.
fn one_file(
    rng: &mut Rng,
    session: &str,
    project: usize,
    subagent: bool,
    target: usize,
    clock: &mut i64,
    counter: &mut u64,
) -> (Vec<u8>, u64, u64) {
    let cwd = format!("/bench/proj-{project}");
    let mut buf: Vec<u8> = Vec::with_capacity(target + 16 * 1024);
    let (mut lines, mut turns) = (0u64, 0u64);
    let mut parent = Value::Null;
    let mut cache_read = rng.range(18_000, 40_000);
    loop {
        // The person's line.
        *counter += 1;
        *clock += rng.range(2_000, 60_000) as i64;
        let uuid = format!("{:016x}{:08x}", rng.next(), *counter);
        let n = rng.range(60, 400) as usize;
        let prompt = words(rng, n);
        push_line(
            &json!({"parentUuid": parent, "isSidechain": subagent, "userType": "external",
                    "cwd": cwd, "sessionId": session, "version": "2.1.0", "gitBranch": "main",
                    "type": "user", "uuid": uuid, "timestamp": iso(EPOCH_MS + *clock),
                    "message": {"role": "user", "content": prompt}}),
            &mut buf,
            &mut lines,
        );
        parent = json!(uuid);
        // A few model rounds per prompt, each one a message id, each with a tool call.
        for _ in 0..rng.range(1, 3) {
            *counter += 1;
            *clock += rng.range(1_000, 9_000) as i64;
            let mid = format!("msg_{:016x}{:08x}", rng.next(), *counter);
            let auuid = format!("{:016x}{:08x}", rng.next(), *counter);
            cache_read += rng.range(500, 6_000);
            let usage = json!({
                "input_tokens": rng.range(1, 60), "output_tokens": rng.range(20, 2_000),
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": rng.range(0, 5_000),
                "cache_creation": {"ephemeral_5m_input_tokens": rng.range(0, 5_000),
                                   "ephemeral_1h_input_tokens": 0},
                "service_tier": "standard", "speed": "standard",
                "inference_geo": "not_available"
            });
            let n = rng.range(80, 700) as usize;
            let text = words(rng, n);
            let tool = TOOLS[rng.below(TOOLS.len() as u64) as usize];
            let tid = format!("toolu_{:016x}", rng.next());
            let input = match tool {
                "Read" => json!({"file_path": format!("{cwd}/src/m{}.rs", rng.below(200))}),
                "Bash" => json!({"command": format!("cargo test -p m{}", rng.below(50)),
                                 "description": "run the tests"}),
                "Grep" => json!({"pattern": rng.word()}),
                _ => json!({"file_path": format!("{cwd}/src/m{}.rs", rng.below(200)),
                            "old_string": "alpha", "new_string": "bravo"}),
            };
            let ts = iso(EPOCH_MS + *clock);
            let assistant = |content: Value| {
                json!({"parentUuid": parent, "isSidechain": subagent, "userType": "external",
                       "cwd": cwd, "sessionId": session, "version": "2.1.0", "gitBranch": "main",
                       "type": "assistant", "uuid": auuid, "timestamp": ts,
                       "message": {"id": mid, "type": "message", "role": "assistant",
                                   "model": "claude-sonnet-5-5", "content": content,
                                   "stop_reason": null, "usage": usage}})
            };
            push_line(
                &assistant(json!([{"type": "text", "text": text}])),
                &mut buf,
                &mut lines,
            );
            // The same message again, carrying the tool call: one turn, two lines.
            push_line(
                &assistant(json!([{"type": "tool_use", "id": tid, "name": tool, "input": input}])),
                &mut buf,
                &mut lines,
            );
            turns += 1;
            *clock += rng.range(50, 4_000) as i64;
            let size = rng.range(600, 6_000) as usize;
            let out = if tool == "Read" {
                code_text(rng, size)
            } else {
                words(rng, size)
            };
            *counter += 1;
            let ruuid = format!("{:016x}{:08x}", rng.next(), *counter);
            push_line(
                &json!({"parentUuid": auuid, "isSidechain": subagent, "userType": "external",
                        "cwd": cwd, "sessionId": session, "version": "2.1.0", "gitBranch": "main",
                        "type": "user", "uuid": ruuid, "timestamp": iso(EPOCH_MS + *clock),
                        "message": {"role": "user", "content": [
                            {"tool_use_id": tid, "type": "tool_result", "content": out}]}}),
                &mut buf,
                &mut lines,
            );
            parent = json!(ruuid);
        }
        if buf.len() >= target {
            return (buf, lines, turns);
        }
    }
}

/// Generate the set, handing each file to `sink` as `(relative path with '/', bytes)`. Nothing is
/// kept in memory beyond one file. The digest covers every path and every byte in generation order.
pub fn generate(spec: &Spec, sink: &mut dyn FnMut(&str, &[u8]) -> Res<()>) -> Res<Summary> {
    if spec.files == 0 {
        return Err("a transcript set needs at least one file".into());
    }
    let per_file = (spec.target_bytes / spec.files as u64) as usize;
    let mut rng = Rng(spec.seed);
    let mut hasher = Sha256::new();
    let mut s = Summary {
        files: 0,
        bytes: 0,
        lines: 0,
        turns: 0,
        digest: String::new(),
    };
    let (mut clock, mut counter) = (0i64, 0u64);
    for i in 0..spec.files {
        let project = i % 8;
        // Every eighth file is a subagent's transcript, in its parent session's own directory.
        let subagent = i % 8 == 7;
        let parent_idx = if subagent { i - 1 } else { i };
        let session = format!("lumen-bench-{parent_idx:04}");
        let rel = if subagent {
            format!("-bench-proj-{project}/{session}/subagents/agent-a{i:03}.jsonl")
        } else {
            format!("-bench-proj-{project}/{session}.jsonl")
        };
        let share = if subagent { per_file / 2 } else { per_file };
        let (bytes, lines, turns) = one_file(
            &mut rng,
            &session,
            project,
            subagent,
            share,
            &mut clock,
            &mut counter,
        );
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
        sink(&rel, &bytes)?;
        s.files += 1;
        s.bytes += bytes.len() as u64;
        s.lines += lines;
        s.turns += turns;
    }
    s.digest = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(s)
}

/// Write the set under `dir`.
pub fn write_set(spec: &Spec, dir: &Path) -> Res<Summary> {
    generate(spec, &mut |rel, bytes| {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
        }
        std::fs::write(&p, bytes).ctx(&format!("write {rel}"))
    })
}

/// The digest and counts committed for the standard set: `key=value` lines, `#` comments.
pub fn committed() -> Res<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for l in COMMITTED.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (k, v) = l
            .split_once('=')
            .ok_or_else(|| format!("transcripts.sha256: not key=value: {l}"))?;
        m.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(m)
}

// --- statistics -------------------------------------------------------------------------------

/// Median (nearest rank, so an observed value) and min-max of a set of runs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Series {
    pub n: usize,
    pub median: f64,
    pub min: f64,
    pub max: f64,
}

pub fn series(values: &[f64]) -> Option<Series> {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let rank = (50 * v.len()).div_ceil(100).clamp(1, v.len());
    Some(Series {
        n: v.len(),
        median: v[rank - 1],
        min: v[0],
        max: v[v.len() - 1],
    })
}

/// Rate over a time, refusing a time that is not positive.
pub fn throughput(amount: f64, elapsed: Duration) -> Res<f64> {
    let s = elapsed.as_secs_f64();
    if s <= 0.0 {
        return Err("a throughput needs a positive elapsed time".into());
    }
    Ok(amount / s)
}

// --- process sampling -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct ProcSample {
    /// User plus system CPU time of the process so far.
    pub cpu_secs: f64,
    pub rss_bytes: u64,
}

/// CPU time and resident set of a process, by pid, from the OS: `proc_pidinfo` on macOS, `/proc` on
/// Linux, `GetProcessTimes` and `K32GetProcessMemoryInfo` on Windows.
pub fn sample(pid: u32) -> Res<ProcSample> {
    sample_os(pid)
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn sample_os(pid: u32) -> Res<ProcSample> {
    // SAFETY: `ti` is a plain C struct the call fills; the size passed is its size.
    let (ti, got, size) = unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let got = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKINFO,
            0,
            &mut ti as *mut libc::proc_taskinfo as *mut libc::c_void,
            size,
        );
        (ti, got, size)
    };
    if got != size {
        return Err(format!("proc_pidinfo for pid {pid} returned {got}"));
    }
    // The CPU times are in mach absolute time units: nanoseconds on Intel, 125/3 ns on Apple
    // silicon. The timebase says which.
    let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: `tb` is a plain C struct the call fills.
    let rc = unsafe { libc::mach_timebase_info(&mut tb) };
    if rc != 0 || tb.denom == 0 {
        return Err("mach_timebase_info failed".into());
    }
    let ticks = ti.pti_total_user as f64 + ti.pti_total_system as f64;
    Ok(ProcSample {
        cpu_secs: ticks * f64::from(tb.numer) / f64::from(tb.denom) / 1e9,
        rss_bytes: ti.pti_resident_size,
    })
}

#[cfg(target_os = "linux")]
fn sample_os(pid: u32) -> Res<ProcSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ctx("read /proc stat")?;
    // After the last ')' (the name may hold spaces): state is field 0, utime 11, stime 12.
    let rest = stat
        .rsplit_once(')')
        .map(|(_, r)| r)
        .ok_or("unreadable /proc stat")?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| -> Res<f64> {
        f.get(i)
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| "unreadable CPU time in /proc stat".to_string())
    };
    // SAFETY: sysconf with a valid name has no memory effects.
    let (hz, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    if hz <= 0 || page <= 0 {
        return Err("sysconf failed".into());
    }
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ctx("read /proc statm")?;
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("unreadable /proc statm")?;
    Ok(ProcSample {
        cpu_secs: (tick(11)? + tick(12)?) / hz as f64,
        rss_bytes: pages * page as u64,
    })
}

#[cfg(windows)]
fn sample_os(pid: u32) -> Res<ProcSample> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let ft =
        |f: FILETIME| ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64;
    // SAFETY: a handle opened here is closed here; the structs are plain C data the calls fill.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return Err(format!("OpenProcess({pid}) failed"));
        }
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
        let times = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mem = K32GetProcessMemoryInfo(h, &mut pmc, pmc.cb);
        CloseHandle(h);
        if times == 0 || mem == 0 {
            return Err(format!("could not read the times or memory of pid {pid}"));
        }
        // FILETIME units are 100 ns.
        Ok(ProcSample {
            cpu_secs: (ft(k) + ft(u)) / 1e7,
            rss_bytes: pmc.WorkingSetSize as u64,
        })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn sample_os(_pid: u32) -> Res<ProcSample> {
    Err("process sampling is not implemented on this OS".into())
}

/// Idle CPU and memory of one process.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Idle {
    pub samples: usize,
    pub cpu_secs_per_min: f64,
    pub rss_median_bytes: u64,
    pub rss_peak_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct IdleCfg {
    pub settle: Duration,
    pub window: Duration,
    pub interval: Duration,
    /// The window runs on until this many samples are in. On a loaded machine a sleep overshoots
    /// its interval, and a window of time alone gave fewer samples than it was meant to.
    pub min_samples: usize,
}

impl IdleCfg {
    /// 30 s to settle, then once a second for 120 s, and at least 60 samples.
    pub fn standard() -> IdleCfg {
        IdleCfg {
            settle: Duration::from_secs(30),
            window: Duration::from_secs(120),
            interval: Duration::from_secs(1),
            min_samples: 60,
        }
    }
}

/// Settle, then sample `pid` every `interval` for `window`, and until `min_samples` are in. A
/// sample that fails (the process died) is an error: an idle figure of a process that was not
/// there would be a made-up number.
pub fn measure_idle(pid: u32, cfg: &IdleCfg) -> Res<Idle> {
    std::thread::sleep(cfg.settle);
    let first = sample(pid)?;
    let t0 = Instant::now();
    let mut last = (first, t0);
    let mut rss = vec![first.rss_bytes as i64];
    while t0.elapsed() < cfg.window || rss.len() < cfg.min_samples {
        std::thread::sleep(cfg.interval);
        let s = sample(pid).ctx("sample during the idle window")?;
        rss.push(s.rss_bytes as i64);
        last = (s, Instant::now());
    }
    let wall = last.1.duration_since(t0).as_secs_f64();
    if wall <= 0.0 {
        return Err("the idle window was empty".into());
    }
    rss.sort_unstable();
    let pick = |p: usize| rss[((p * rss.len()).div_ceil(100)).clamp(1, rss.len()) - 1];
    Ok(Idle {
        samples: rss.len(),
        cpu_secs_per_min: (last.0.cpu_secs - first.cpu_secs).max(0.0) / wall * 60.0,
        rss_median_bytes: pick(50) as u64,
        rss_peak_bytes: rss[rss.len() - 1] as u64,
    })
}

// --- inputs and outputs -----------------------------------------------------------------------

fn na<T>(why: &str) -> Cell<T> {
    Cell::Na(why.to_string())
}

/// A cell of the result: a value, or the reason there is none. Never a zero in place of a reason.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cell<T> {
    Value(T),
    Na(String),
}

pub const WARM: &str = "warm file cache";
pub const COLD: &str = "cold file cache (page cache dropped with sudo -n)";

/// What to measure, and how hard.
#[derive(Clone, Debug)]
pub struct Plan {
    pub spec: Spec,
    pub ingest_runs: usize,
    pub cold_runs: usize,
    pub poll: Duration,
    /// How long a count that is not the set's turn count must stay put before ingest counts as
    /// done (and incomplete). A count that reaches the set's turn count is done at once.
    pub stable: Duration,
    /// Give up on ingest after this long without finishing.
    pub ingest_timeout: Duration,
    pub ready_timeout: Duration,
    pub idle: IdleCfg,
}

impl Plan {
    pub fn standard() -> Plan {
        Plan {
            spec: Spec::standard(),
            ingest_runs: 5,
            cold_runs: 30,
            poll: Duration::from_millis(50),
            stable: Duration::from_secs(10),
            ingest_timeout: Duration::from_secs(600),
            ready_timeout: Duration::from_secs(60),
            idle: IdleCfg::standard(),
        }
    }
}

/// The version under test: its binaries and its capability entry from the manifest.
pub struct Target<'a> {
    pub version: &'a str,
    pub entry: &'a VersionEntry,
    pub daemon: &'a Path,
    /// `None`: no `lumen-mcp` to measure (its cells say so).
    pub mcp: Option<&'a Path>,
    /// Arguments after the daemon's path. Empty for a real build; a test's stub needs some.
    pub daemon_args: Vec<String>,
    /// A scratch directory under which every sandbox of this run is made. Never inside the repo.
    pub scratch: &'a Path,
    /// `macos`, `linux` or `windows`: the OS the binaries are for (`std::env::consts::OS`).
    pub os: &'a str,
}

/// What [`run`] reaches outside itself for, so a test can see that it did.
pub struct Deps<'a> {
    /// Called with the port before a daemon that binds a fixed one starts. The real one is
    /// [`crate::sandbox::ensure_port_free`].
    pub port_check: &'a dyn Fn(u16) -> Res<()>,
    pub guard: &'a RealLedgerGuard,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct IngestRun {
    pub bytes: u64,
    pub elapsed_s: f64,
    pub bytes_per_s: f64,
    pub turns_per_s: f64,
    pub turns: i64,
    pub turns_expected: u64,
    /// Every row of the expected turns arrived. False is published as such, not hidden.
    pub complete: bool,
    pub rows: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ingest {
    pub runs: Vec<IngestRun>,
    pub bytes_per_s: Series,
    pub turns_per_s: Series,
    /// Rows per table, median and range over the runs.
    pub rows: BTreeMap<String, Series>,
    pub set_bytes: u64,
    pub set_digest: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub version: String,
    pub os: String,
    /// "warm file cache" or the cold label, on every cold-start figure.
    pub cache: String,
    pub ingest: Cell<Ingest>,
    pub cold_daemon_ms: Cell<Series>,
    pub cold_mcp_ms: Cell<Series>,
    pub cold_hook_ms: Cell<Series>,
    pub idle_daemon_empty: Cell<Idle>,
    pub idle_daemon_after_ingest: Cell<Idle>,
    pub idle_mcp: Cell<Idle>,
}

/// Why this version's daemon cannot be measured on `os`, if it cannot.
///
/// 1.0.0 ignores `LUMEN_PROJECTS_DIR` and reads `dirs::home_dir()/.claude/projects`. On Windows
/// that is the known-folder API, which ignores `USERPROFILE`, so the sandbox cannot redirect it:
/// the daemon would read the real projects directory.
pub fn daemon_applicable(entry: &VersionEntry, os: &str) -> Result<(), String> {
    if entry.daemon_reads_home && os == "windows" {
        return Err(
            "would read the real projects dir: its home lookup ignores the sandbox there".into(),
        );
    }
    Ok(())
}

/// Where the transcripts go for this version: `LUMEN_PROJECTS_DIR` where the daemon reads it,
/// else the scratch home's `.claude/projects` (1.0.0).
pub fn projects_dir_for(entry: &VersionEntry, sb: &Sandbox) -> PathBuf {
    if entry.daemon_reads_home {
        sb.home.join(".claude").join("projects")
    } else {
        sb.projects.clone()
    }
}

// --- running a daemon -------------------------------------------------------------------------

struct Running {
    child: Child,
    sb: Sandbox,
    projects: PathBuf,
    ready: Duration,
}

impl Running {
    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn schema_has_turns(db: &Path) -> bool {
    if !db.is_file() {
        return false;
    }
    let Ok(conn) = lumen_core::meter::open_read_only(db) else {
        return false;
    };
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'turns'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Rows of every table of the ledger, read-only.
pub fn table_rows(db: &Path) -> Res<BTreeMap<String, i64>> {
    let conn = lumen_core::meter::open_read_only(db).ctx("open the ledger read-only")?;
    let names: Vec<String> = {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .ctx("list the tables")?;
        st.query_map([], |r| r.get::<_, String>(0))
            .ctx("list the tables")?
            .collect::<Result<_, _>>()
            .ctx("list the tables")?
    };
    let mut out = BTreeMap::new();
    for n in names {
        let c: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{}\"", n.replace('"', "\"\"")),
                [],
                |r| r.get(0),
            )
            .ctx(&format!("count {n}"))?;
        out.insert(n, c);
    }
    Ok(out)
}

fn start_daemon(t: &Target, deps: &Deps, root: &Path, plan: &Plan) -> Res<Running> {
    daemon_applicable(t.entry, t.os)?;
    if t.entry.daemon_fixed_port {
        // 1.0.0 binds 127.0.0.1:9999 whatever the environment says: with the app running, the
        // app's window could talk to this daemon.
        (deps.port_check)(OLD_DAEMON_PORT)?;
    }
    let sb = Sandbox::create(root)?;
    let projects = projects_dir_for(t.entry, &sb);
    std::fs::create_dir_all(&projects).ctx("create the projects dir")?;
    let addr: SocketAddr = if t.entry.daemon_fixed_port {
        SocketAddr::from(([127, 0, 0, 1], OLD_DAEMON_PORT))
    } else {
        sb.ws_addr.parse().ctx("the sandbox's address")?
    };
    let mut cmd = Command::new(t.daemon);
    sb.apply(&mut cmd);
    // No `current_dir` here: the sandbox's scratch cwd stays (1.0.0 writes `lumen.db` into it).
    cmd.args(&t.daemon_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let t0 = Instant::now();
    let child = cmd
        .spawn()
        .ctx(&format!("spawn the daemon of {}", t.version))?;
    let mut run = Running {
        child,
        sb,
        projects,
        ready: Duration::ZERO,
    };
    // Ready: the port accepts and the schema exists.
    let (mut port_up, mut schema_up) = (false, false);
    loop {
        if !port_up {
            port_up = TcpStream::connect_timeout(&addr, Duration::from_millis(20)).is_ok();
        }
        if !schema_up {
            schema_up = schema_has_turns(&run.sb.db);
        }
        if port_up && schema_up {
            run.ready = t0.elapsed();
            return Ok(run);
        }
        if let Some(status) = run.child.try_wait().ctx("poll the daemon")? {
            return Err(format!(
                "the daemon of {} exited before it was ready: {status}",
                t.version
            ));
        }
        if t0.elapsed() > plan.ready_timeout {
            return Err(format!(
                "the daemon of {} was not ready after {} s (port {port_up}, schema {schema_up})",
                t.version,
                plan.ready_timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Decides when ingest is over. Fed `(count, time)`; done as soon as the count is `expected` (the
/// set's turn count), or once it has been above zero and has not changed for `stable` (a version
/// that drops or adds turns). The time of the last change is the end of ingest.
///
/// Waiting for a pause alone ended a run that was only slow: on a loaded runner a commit can stall
/// for longer than the pause, and the run was cut short and reported incomplete.
pub struct Settle {
    stable: Duration,
    expected: i64,
    count: i64,
    changed_at: Option<Instant>,
}

impl Settle {
    pub fn new(stable: Duration, expected: i64) -> Settle {
        Settle {
            stable,
            expected,
            count: 0,
            changed_at: None,
        }
    }
    /// `Some(last_change)` once settled.
    pub fn feed(&mut self, count: i64, now: Instant) -> Option<Instant> {
        if count != self.count {
            self.count = count;
            self.changed_at = Some(now);
        }
        match self.changed_at {
            Some(at) if self.count > 0 && self.count == self.expected => Some(at),
            Some(at) if self.count > 0 && now.duration_since(at) >= self.stable => Some(at),
            _ => None,
        }
    }
    pub fn count(&self) -> i64 {
        self.count
    }
}

/// One fresh ingest run. With `idle`, the daemon is then sampled idle before it is stopped.
fn ingest_once(
    t: &Target,
    deps: &Deps,
    plan: &Plan,
    root: &Path,
    idle: Option<&IdleCfg>,
) -> Res<(IngestRun, Summary, Option<Idle>)> {
    // The set is written before the daemon exists, next to the projects dir, so the move is a
    // rename on one filesystem and whole files appear at once.
    let stage = root.join("stage");
    let summary = write_set(&plan.spec, &stage)?;
    let run = start_daemon(t, deps, root, plan)?;
    std::thread::sleep(Duration::from_millis(300));
    let expected = i64::try_from(summary.turns).ctx("the set's turn count")?;
    let mut settle = Settle::new(plan.stable, expected);
    let mut conn = None;
    let t_move = Instant::now();
    for e in std::fs::read_dir(&stage).ctx("list the staged set")? {
        let e = e.ctx("list the staged set")?;
        std::fs::rename(e.path(), run.projects.join(e.file_name())).ctx("move the set in")?;
    }
    let end = loop {
        let now = Instant::now();
        if conn.is_none() {
            conn = lumen_core::meter::open_read_only(&run.sb.db).ok();
        }
        // A busy or failing read is no sample: the next poll tries again.
        if let Some(c) = &conn
            && let Ok(n) = c.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get::<_, i64>(0))
            && let Some(at) = settle.feed(n, now)
        {
            break at;
        }
        if now.duration_since(t_move) > plan.ingest_timeout {
            return Err(format!(
                "ingest of {} did not settle in {} s (turns so far: {})",
                t.version,
                plan.ingest_timeout.as_secs(),
                settle.count()
            ));
        }
        std::thread::sleep(plan.poll);
    };
    let elapsed = end.duration_since(t_move).max(Duration::from_millis(1));
    let turns = settle.count();
    let rows = table_rows(&run.sb.db)?;
    let after = match idle {
        Some(cfg) => Some(measure_idle(run.pid(), cfg)?),
        None => None,
    };
    Ok((
        IngestRun {
            bytes: summary.bytes,
            elapsed_s: elapsed.as_secs_f64(),
            bytes_per_s: throughput(summary.bytes as f64, elapsed)?,
            turns_per_s: throughput(turns as f64, elapsed)?,
            turns,
            turns_expected: summary.turns,
            complete: turns as u64 == summary.turns,
            rows,
        },
        summary,
        after,
    ))
}

fn ingest_series(t: &Target, deps: &Deps, plan: &Plan) -> Res<(Ingest, Option<Idle>)> {
    let mut runs = Vec::new();
    let mut set = None;
    let mut after = None;
    for i in 0..plan.ingest_runs {
        let root = t.scratch.join(format!("ingest-{}-{i}", t.version));
        let last = i + 1 == plan.ingest_runs;
        let (r, s, idle) = ingest_once(t, deps, plan, &root, last.then_some(&plan.idle))?;
        let _ = std::fs::remove_dir_all(&root);
        set = Some(s);
        after = idle.or(after);
        runs.push(r);
    }
    let s = set.ok_or("no ingest runs were asked for")?;
    let col = |f: &dyn Fn(&IngestRun) -> f64| series(&runs.iter().map(f).collect::<Vec<_>>());
    let mut rows = BTreeMap::new();
    let tables: Vec<String> = runs.iter().flat_map(|r| r.rows.keys().cloned()).collect();
    for tbl in tables {
        let vals: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.rows.get(&tbl).map(|n| *n as f64))
            .collect();
        if let Some(sr) = series(&vals) {
            rows.insert(tbl, sr);
        }
    }
    Ok((
        Ingest {
            bytes_per_s: col(&|r| r.bytes_per_s).ok_or("no throughput")?,
            turns_per_s: col(&|r| r.turns_per_s).ok_or("no throughput")?,
            rows,
            set_bytes: s.bytes,
            set_digest: s.digest,
            runs,
        },
        after,
    ))
}

// --- cold start -------------------------------------------------------------------------------

/// Drop the Linux page cache. `false` when it cannot be done without a prompt.
fn drop_page_cache() -> bool {
    cfg!(target_os = "linux")
        && Command::new("sudo")
            .args(["-n", "sh", "-c", "sync; echo 3 > /proc/sys/vm/drop_caches"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn cold_daemon(t: &Target, deps: &Deps, plan: &Plan, cold: bool) -> Res<Series> {
    let mut v = Vec::new();
    for i in 0..plan.cold_runs {
        if cold {
            drop_page_cache();
        }
        let root = t.scratch.join(format!("cold-d-{}-{i}", t.version));
        let r = start_daemon(t, deps, &root, plan)?;
        v.push(ms(r.ready));
        drop(r);
        let _ = std::fs::remove_dir_all(&root);
    }
    series(&v).ok_or_else(|| "no cold-start runs were asked for".to_string())
}

/// A running `lumen-mcp` on stdio.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<std::io::Result<String>>,
}

impl Mcp {
    fn spawn(bin: &Path, sb: &Sandbox) -> Res<Mcp> {
        let mut cmd = Command::new(bin);
        sb.apply(&mut cmd);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().ctx(&format!("spawn {}", bin.display()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("lumen-mcp has no stdout")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines() {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Ok(Mcp {
            child,
            stdin,
            lines,
        })
    }

    /// Send `initialize` and wait for the reply to it.
    fn initialize(&mut self) -> Res<()> {
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "lumen-bench", "version": "0"}}});
        let stdin = self.stdin.as_mut().ok_or("stdin is closed")?;
        stdin
            .write_all(format!("{req}\n").as_bytes())
            .ctx("write initialize")?;
        stdin.flush().ctx("flush initialize")?;
        match self.lines.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(line)) => {
                let v: Value = serde_json::from_str(&line).ctx("the initialize reply")?;
                if v["id"] != json!(1) || v.get("result").is_none() {
                    return Err(format!("initialize got no result: {line}"));
                }
                Ok(())
            }
            Ok(Err(e)) => Err(format!("reading the initialize reply: {e}")),
            Err(RecvTimeoutError::Timeout) => {
                Err("lumen-mcp did not answer initialize in 30 s".into())
            }
            Err(RecvTimeoutError::Disconnected) => Err("lumen-mcp closed its stdout".into()),
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn cold_mcp(t: &Target, bin: &Path, plan: &Plan, cold: bool) -> Res<Series> {
    let mut v = Vec::new();
    for i in 0..plan.cold_runs {
        if cold {
            drop_page_cache();
        }
        let root = t.scratch.join(format!("cold-m-{}-{i}", t.version));
        let sb = Sandbox::create(&root)?;
        let t0 = Instant::now();
        let mut m = Mcp::spawn(bin, &sb)?;
        m.initialize()?;
        v.push(ms(t0.elapsed()));
        drop(m);
        let _ = std::fs::remove_dir_all(&root);
    }
    series(&v).ok_or_else(|| "no cold-start runs were asked for".to_string())
}

fn cold_hook(t: &Target, bin: &Path, plan: &Plan, cold: bool) -> Res<Series> {
    let mut v = Vec::new();
    for i in 0..plan.cold_runs {
        if cold {
            drop_page_cache();
        }
        let root = t.scratch.join(format!("cold-h-{}-{i}", t.version));
        let sb = Sandbox::create(&root)?;
        let file = sb.cwd.join("small.rs");
        std::fs::write(&file, "fn main() {}\n").ctx("write the file the hook reads")?;
        let payload = json!({
            "session_id": "lumen-bench-hook", "transcript_path": "/dev/null",
            "cwd": sb.cwd.to_string_lossy(), "permission_mode": "default",
            "hook_event_name": "PreToolUse", "tool_name": "Read",
            "tool_input": {"file_path": file.to_string_lossy()}, "tool_use_id": "toolu_bench"
        })
        .to_string();
        let mut cmd = Command::new(bin);
        sb.apply(&mut cmd);
        cmd.args(["hook", "intercept"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let t0 = Instant::now();
        let mut child = cmd.spawn().ctx("spawn the hook")?;
        {
            let mut stdin = child.stdin.take().ok_or("the hook has no stdin")?;
            stdin
                .write_all(payload.as_bytes())
                .ctx("write the hook payload")?;
        }
        let status = child.wait().ctx("wait for the hook")?;
        v.push(ms(t0.elapsed()));
        if status.code().is_none() {
            return Err(format!("the hook did not exit normally: {status}"));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    series(&v).ok_or_else(|| "no cold-start runs were asked for".to_string())
}

fn idle_mcp(t: &Target, bin: &Path, plan: &Plan) -> Res<Idle> {
    let root = t.scratch.join(format!("idle-m-{}", t.version));
    let sb = Sandbox::create(&root)?;
    let mut m = Mcp::spawn(bin, &sb)?;
    m.initialize()?;
    let r = measure_idle(m.child.id(), &plan.idle);
    drop(m);
    let _ = std::fs::remove_dir_all(&root);
    r
}

fn idle_daemon_empty(t: &Target, deps: &Deps, plan: &Plan) -> Res<Idle> {
    let root = t.scratch.join(format!("idle-d-{}", t.version));
    let r = start_daemon(t, deps, &root, plan)?;
    let out = measure_idle(r.pid(), &plan.idle);
    drop(r);
    let _ = std::fs::remove_dir_all(&root);
    out
}

// --- the entry point --------------------------------------------------------------------------

/// Measure one version. The real ledgers are checked before and after (also when this panics: see
/// [`RealLedgerGuard::begin`]). A daemon that cannot be measured here gives `n/a` cells with the
/// reason; a measurement that fails stops the run with the step's name.
pub fn run(t: &Target, plan: &Plan, deps: &Deps) -> Res<Report> {
    let scope = deps.guard.begin()?;
    let out = run_inner(t, plan, deps);
    let after = scope.finish();
    let report = out?;
    after?;
    Ok(report)
}

fn run_inner(t: &Target, plan: &Plan, deps: &Deps) -> Res<Report> {
    std::fs::create_dir_all(t.scratch).ctx("create the scratch dir")?;
    let cold = drop_page_cache();
    let cache = if cold { COLD } else { WARM }.to_string();

    let (ingest, cold_daemon_ms, idle_empty, idle_after) = match daemon_applicable(t.entry, t.os) {
        Err(why) => (na(&why), na(&why), na(&why), na(&why)),
        Ok(()) => {
            let (ing, after) = ingest_series(t, deps, plan)?;
            let cd = cold_daemon(t, deps, plan, cold)?;
            let ie = idle_daemon_empty(t, deps, plan)?;
            (
                Cell::Value(ing),
                Cell::Value(cd),
                Cell::Value(ie),
                after.map_or_else(|| na("no ingest run to sample after"), Cell::Value),
            )
        }
    };
    let (cold_mcp_ms, cold_hook_ms, idle_mcp) = match t.mcp {
        None => {
            let why = "no lumen-mcp binary was given";
            (na(why), na(why), na(why))
        }
        Some(bin) => {
            let m = cold_mcp(t, bin, plan, cold)?;
            let h = if t.entry.hook_cli {
                Cell::Value(cold_hook(t, bin, plan, cold)?)
            } else {
                na(
                    "this version's hooks are scripts, timed by the script-hook series; \
                    `lumen-mcp hook` does not exist before 1.6.0 and is never spawned",
                )
            };
            (Cell::Value(m), h, Cell::Value(idle_mcp(t, bin, plan)?))
        }
    };
    Ok(Report {
        version: t.version.to_string(),
        os: t.os.to_string(),
        cache,
        ingest,
        cold_daemon_ms,
        cold_mcp_ms,
        cold_hook_ms,
        idle_daemon_empty: idle_empty,
        idle_daemon_after_ingest: idle_after,
        idle_mcp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::VersionsManifest;
    use std::cell::RefCell;

    fn entry(v: &str) -> VersionEntry {
        VersionsManifest::load_committed()
            .unwrap()
            .get(v)
            .unwrap()
            .clone()
    }

    fn small() -> Spec {
        Spec {
            seed: 7,
            target_bytes: 600 * 1024,
            files: 8,
        }
    }

    fn digest_of(spec: &Spec) -> Summary {
        generate(spec, &mut |_, _| Ok(())).unwrap()
    }

    // --- the generator ---

    /// Prints the committed file's content. Run by hand after a deliberate change to the
    /// generator: `cargo test -p lumen-bench print_committed_digest -- --ignored --nocapture`.
    #[test]
    #[ignore = "prints the digest to commit"]
    fn print_committed_digest() {
        let s = digest_of(&Spec::standard());
        println!(
            "digest={}\nfiles={}\nbytes={}\nlines={}\nturns={}",
            s.digest, s.files, s.bytes, s.lines, s.turns
        );
    }

    #[test]
    fn the_generator_digest_is_stable_and_matches_the_committed_one() {
        let a = digest_of(&Spec::standard());
        let b = digest_of(&Spec::standard());
        assert_eq!(a, b, "the same seed must give the same set");
        let c = committed().unwrap();
        assert_eq!(c["digest"], a.digest, "the committed digest is out of date");
        assert_eq!(c["files"], a.files.to_string());
        assert_eq!(c["bytes"], a.bytes.to_string());
        assert_eq!(c["lines"], a.lines.to_string());
        assert_eq!(c["turns"], a.turns.to_string());
        assert!(a.bytes > 36_000_000 && a.bytes < 48_000_000, "{}", a.bytes);
        let other = Spec {
            seed: SEED + 1,
            ..Spec::standard()
        };
        assert_ne!(digest_of(&other).digest, a.digest);
    }

    #[test]
    fn the_set_is_claude_code_shaped_and_holds_nothing_real() {
        let mut first: Option<String> = None;
        let mut all_ids = std::collections::BTreeSet::new();
        let mut paths = Vec::new();
        let s = generate(&small(), &mut |rel, bytes| {
            paths.push(rel.to_string());
            let text = std::str::from_utf8(bytes).unwrap();
            for l in text.lines() {
                let v: Value = serde_json::from_str(l).unwrap();
                assert!(
                    v["sessionId"].as_str().unwrap().starts_with("lumen-bench-"),
                    "a leaked row must trip the guard"
                );
                for needle in ["/Users/", "/home/", "C:\\", "@"] {
                    assert!(!l.contains(needle), "{needle} in a generated line");
                }
                if v["type"] == "assistant" {
                    if first.is_none() {
                        first = Some(l.to_string());
                    }
                    all_ids.insert(v["message"]["id"].as_str().unwrap().to_string());
                }
            }
            Ok(())
        })
        .unwrap();
        // The daemon's own parser reads it.
        let rec: lumen_core::record::Record = serde_json::from_str(&first.unwrap()).unwrap();
        assert!(rec.is_billable());
        assert_eq!(
            all_ids.len() as u64,
            s.turns,
            "turns = distinct message ids"
        );
        assert!(
            paths.iter().any(|p| p.contains("/subagents/agent-")),
            "some transcripts are subagents"
        );
        assert!(
            paths
                .iter()
                .all(|p| p.ends_with(".jsonl") && !p.contains(".."))
        );
    }

    #[test]
    fn timestamps_are_iso_utc() {
        assert_eq!(iso(EPOCH_MS), "2026-01-01T00:00:00.000Z");
        assert_eq!(
            iso(EPOCH_MS + 86_400_000 * 59 + 3_723_004),
            "2026-03-01T01:02:03.004Z"
        );
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
    }

    // --- arithmetic ---

    #[test]
    fn throughput_and_series_are_computed_from_the_runs() {
        assert_eq!(
            throughput(1000.0, Duration::from_millis(500)).unwrap(),
            2000.0
        );
        assert!(throughput(1.0, Duration::ZERO).is_err());
        let s = series(&[5.0, 1.0, 3.0, 9.0, 7.0]).unwrap();
        assert_eq!((s.n, s.median, s.min, s.max), (5, 5.0, 1.0, 9.0));
        assert!(series(&[]).is_none());
    }

    #[test]
    fn ingest_ends_at_the_last_change_not_at_the_end_of_the_wait() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // 20 of 25 turns: a version that drops some, so only the pause ends it.
        let mut s = Settle::new(Duration::from_secs(2), 25);
        assert!(s.feed(0, at(0)).is_none(), "an empty ledger is not settled");
        assert!(s.feed(0, at(5_000)).is_none(), "zero rows is never done");
        assert!(s.feed(10, at(5_050)).is_none());
        assert!(s.feed(20, at(5_100)).is_none());
        assert!(s.feed(20, at(6_000)).is_none());
        assert!(s.feed(20, at(7_099)).is_none(), "one ms short of 2 s");
        assert_eq!(s.feed(20, at(7_100)), Some(at(5_100)));
    }

    #[test]
    fn ingest_ends_when_every_turn_is_in_without_waiting_for_a_pause() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut s = Settle::new(Duration::from_secs(10), 111);
        assert!(s.feed(23, at(380)).is_none());
        // The CI failure: a commit stalled past the pause, at 23 of 111 turns. Still not done.
        assert!(
            s.feed(23, at(800)).is_none(),
            "a slow run is not a finished one"
        );
        assert!(s.feed(23, at(9_000)).is_none());
        assert!(s.feed(110, at(9_100)).is_none());
        assert_eq!(
            s.feed(111, at(9_150)),
            Some(at(9_150)),
            "done at the last turn"
        );
    }

    #[test]
    fn a_count_past_the_set_waits_for_the_pause() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut s = Settle::new(Duration::from_secs(2), 10);
        assert!(s.feed(10, at(100)).is_some(), "exactly the set: done");
        let mut s = Settle::new(Duration::from_secs(2), 10);
        assert!(
            s.feed(12, at(100)).is_none(),
            "a version that adds turns may still be adding"
        );
        assert_eq!(s.feed(12, at(2_100)), Some(at(100)));
    }

    // --- sampling ---

    #[test]
    fn self_rss_is_positive_and_cpu_time_moves_with_work() {
        let a = sample(std::process::id()).unwrap();
        assert!(a.rss_bytes > 0, "self RSS must be positive");
        // Spin for about 300 ms of CPU: the unit of the CPU time is checked against the clock. The
        // whole process is sampled and other tests run beside this one, so only a unit that is
        // wrong by orders of magnitude (either way) fails.
        let t = Instant::now();
        let mut x = 1u64;
        while t.elapsed() < Duration::from_millis(300) {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        let b = sample(std::process::id()).unwrap();
        let d = b.cpu_secs - a.cpu_secs;
        assert!(d > 0.2 && d < 8.0, "CPU delta {d} s for 0.3 s of spinning");
    }

    #[test]
    fn idle_sampling_reports_rss_and_cpu_for_self() {
        let cfg = IdleCfg {
            settle: Duration::from_millis(10),
            window: Duration::from_millis(300),
            interval: Duration::from_millis(50),
            min_samples: 4,
        };
        let r = measure_idle(std::process::id(), &cfg).unwrap();
        assert!(r.samples >= 4);
        assert!(r.rss_median_bytes > 0 && r.rss_peak_bytes >= r.rss_median_bytes);
        assert!(r.cpu_secs_per_min >= 0.0);
        assert!(
            measure_idle(u32::MAX - 1, &cfg).is_err(),
            "a missing process is an error"
        );
    }

    #[test]
    fn idle_sampling_takes_its_minimum_however_slow_the_clock() {
        // A window already over when sampling starts: what a loaded runner did to a 300 ms one.
        let cfg = IdleCfg {
            settle: Duration::ZERO,
            window: Duration::ZERO,
            interval: Duration::from_millis(1),
            min_samples: 5,
        };
        let r = measure_idle(std::process::id(), &cfg).unwrap();
        assert_eq!(r.samples, 5);
    }

    // --- the rules ---

    #[test]
    fn a_daemon_that_reads_the_home_is_na_on_windows_only() {
        let old = entry("1.0.0");
        let why = daemon_applicable(&old, "windows").unwrap_err();
        assert!(why.contains("would read the real projects dir"), "{why}");
        assert!(daemon_applicable(&old, "macos").is_ok());
        assert!(daemon_applicable(&old, "linux").is_ok());
        assert!(daemon_applicable(&entry("1.9.0"), "windows").is_ok());
    }

    #[test]
    fn the_projects_dir_follows_what_the_daemon_reads() {
        let root = tempfile::tempdir().unwrap();
        let sb = Sandbox::create(root.path()).unwrap();
        assert_eq!(projects_dir_for(&entry("1.9.0"), &sb), sb.projects);
        assert_eq!(
            projects_dir_for(&entry("1.0.0"), &sb),
            sb.home.join(".claude").join("projects")
        );
    }

    fn stub_target<'a>(
        e: &'a VersionEntry,
        scratch: &'a Path,
        exe: &'a Path,
        os: &'a str,
    ) -> Target<'a> {
        Target {
            version: "stub",
            entry: e,
            daemon: exe,
            mcp: None,
            daemon_args: [
                "--ignored",
                "--exact",
                "versions::daemon::tests::stub_daemon",
                "--nocapture",
                "--test-threads=1",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            scratch,
            os,
        }
    }

    fn fast_plan() -> Plan {
        Plan {
            spec: small(),
            ingest_runs: 2,
            cold_runs: 2,
            poll: Duration::from_millis(10),
            // Only a run short of the set waits this long; the stub's runs end at its last turn.
            stable: Duration::from_secs(5),
            ingest_timeout: Duration::from_secs(60),
            ready_timeout: Duration::from_secs(30),
            idle: IdleCfg {
                settle: Duration::from_millis(50),
                window: Duration::from_millis(300),
                interval: Duration::from_millis(50),
                min_samples: 4,
            },
        }
    }

    fn ok_port(_: u16) -> Res<()> {
        Ok(())
    }

    /// The stub daemon: this test binary, started by the harness as a daemon. It takes the same
    /// environment a real one does, creates a ledger with lumen's schema, accepts on its port,
    /// and ingests every assistant line with usage from the projects directory into `turns`.
    /// `#[ignore]`d so that a normal `cargo test` never runs it.
    #[test]
    #[ignore = "a stub daemon, started by the tests below"]
    fn stub_daemon() {
        let db = PathBuf::from(std::env::var_os("LUMEN_DB").expect("LUMEN_DB"));
        let addr = std::env::var("LUMEN_WS_ADDR").expect("LUMEN_WS_ADDR");
        let projects = std::env::var_os("LUMEN_PROJECTS_DIR")
            .map(PathBuf::from)
            .expect("LUMEN_PROJECTS_DIR");
        std::fs::write(
            db.parent().unwrap().join("stub-cwd.txt"),
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        )
        .unwrap();
        let conn = lumen_core::meter::connect_db(&db).unwrap();
        // A busy ledger waits rather than panic: a dead stub reads as an incomplete ingest.
        conn.busy_timeout(Duration::from_secs(30)).unwrap();
        let listener = std::net::TcpListener::bind(&addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut offsets: BTreeMap<PathBuf, usize> = BTreeMap::new();
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else if p.extension().is_some_and(|x| x == "jsonl") {
                        out.push(p);
                    }
                }
            }
        }
        loop {
            let _ = listener.accept();
            let mut files = Vec::new();
            walk(&projects, &mut files);
            for f in files {
                let Ok(text) = std::fs::read_to_string(&f) else {
                    continue;
                };
                let from = *offsets.get(&f).unwrap_or(&0);
                let Some(end) = text.rfind('\n').map(|i| i + 1) else {
                    continue;
                };
                if end <= from {
                    continue;
                }
                conn.execute_batch("BEGIN").unwrap();
                for l in text[from..end].lines() {
                    let Ok(v) = serde_json::from_str::<Value>(l) else {
                        continue;
                    };
                    let (m, u) = (&v["message"], &v["message"]["usage"]);
                    if m["role"] != "assistant" || u.is_null() {
                        continue;
                    }
                    conn.execute(
                        "INSERT OR IGNORE INTO turns (message_id, session_id, ts, model, \
                         input_tokens, output_tokens, cache_read_input_tokens, \
                         cache_creation_input_tokens, source_file, is_subagent) \
                         VALUES (?,?,?,?,?,?,?,?,?,0)",
                        stub_params(&v, m, u, &f),
                    )
                    .unwrap();
                }
                conn.execute_batch("COMMIT").unwrap();
                offsets.insert(f, end);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    type StubRow = (String, String, String, String, i64, i64, i64, i64, String);

    fn stub_params(v: &Value, m: &Value, u: &Value, f: &Path) -> StubRow {
        let s = |x: &Value| x.as_str().unwrap_or_default().to_string();
        let n = |x: &Value| x.as_i64().unwrap_or(0);
        (
            s(&m["id"]),
            s(&v["sessionId"]),
            s(&v["timestamp"]),
            s(&m["model"]),
            n(&u["input_tokens"]),
            n(&u["output_tokens"]),
            n(&u["cache_read_input_tokens"]),
            n(&u["cache_creation_input_tokens"]),
            f.to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn a_stub_daemon_gives_a_computed_throughput() {
        let scratch = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let e = entry("1.9.0");
        let t = stub_target(&e, scratch.path(), &exe, std::env::consts::OS);
        let guard = RealLedgerGuard::with_paths(vec![]);
        let deps = Deps {
            port_check: &ok_port,
            guard: &guard,
        };
        let plan = fast_plan();
        let (ing, after) = ingest_series(&t, &deps, &plan).unwrap();
        let expect = digest_of(&plan.spec);
        assert_eq!(ing.runs.len(), 2);
        for r in &ing.runs {
            assert!(r.complete, "the stub must ingest every turn: {r:?}");
            assert_eq!(r.turns as u64, expect.turns);
            assert_eq!(r.bytes, expect.bytes);
            // Computed, not copied: bytes over the measured time.
            assert!((r.bytes_per_s - r.bytes as f64 / r.elapsed_s).abs() < 1e-6 * r.bytes_per_s);
            assert!((r.turns_per_s - r.turns as f64 / r.elapsed_s).abs() < 1e-6 * r.turns_per_s);
            assert!(r.bytes_per_s > 0.0 && r.elapsed_s > 0.0);
            assert_eq!(r.rows["turns"], r.turns);
        }
        assert_eq!(ing.set_digest, expect.digest);
        assert_eq!(ing.rows["turns"].n, 2);
        assert!(
            ing.bytes_per_s.min <= ing.bytes_per_s.median
                && ing.bytes_per_s.median <= ing.bytes_per_s.max
        );
        let idle = after.expect("the last run is sampled idle");
        assert!(idle.rss_peak_bytes > 0);
        // The scratch is cleaned up.
        assert!(std::fs::read_dir(scratch.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_daemon_spawn_keeps_the_sandbox_scratch_cwd() {
        let scratch = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let e = entry("1.9.0");
        let t = stub_target(&e, scratch.path(), &exe, std::env::consts::OS);
        let guard = RealLedgerGuard::with_paths(vec![]);
        let deps = Deps {
            port_check: &ok_port,
            guard: &guard,
        };
        let root = scratch.path().join("cwd-run");
        let r = start_daemon(&t, &deps, &root, &fast_plan()).unwrap();
        let seen = std::fs::read_to_string(r.sb.dir().join("stub-cwd.txt")).unwrap();
        assert_eq!(
            std::fs::canonicalize(&seen).unwrap(),
            std::fs::canonicalize(&r.sb.cwd).unwrap(),
            "the daemon ran in {seen}, not the scratch cwd"
        );
        assert!(r.ready > Duration::ZERO);
    }

    #[test]
    fn the_port_check_runs_before_a_fixed_port_daemon_and_refuses() {
        let scratch = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let calls = RefCell::new(Vec::new());
        let check = |p: u16| -> Res<()> {
            calls.borrow_mut().push(p);
            Err("127.0.0.1:9999 is in use: quit Lumen first".to_string())
        };
        let guard = RealLedgerGuard::with_paths(vec![]);
        let deps = Deps {
            port_check: &check,
            guard: &guard,
        };
        // 1.0.0: the check is called with 9999, and its refusal stops the run before a spawn.
        let old = entry("1.0.0");
        let t = stub_target(&old, scratch.path(), &exe, "macos");
        let err = run(&t, &fast_plan(), &deps).unwrap_err();
        assert!(err.contains("quit Lumen first"), "{err}");
        assert_eq!(*calls.borrow(), vec![OLD_DAEMON_PORT]);
        // A version with a movable port never asks.
        calls.borrow_mut().clear();
        let new = entry("1.9.0");
        let t = stub_target(&new, scratch.path(), &exe, std::env::consts::OS);
        let _ = start_daemon(&t, &deps, &scratch.path().join("free"), &fast_plan());
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn windows_1_0_0_is_na_and_never_spawned() {
        let scratch = tempfile::tempdir().unwrap();
        let old = entry("1.0.0");
        let missing = scratch.path().join("no-such-daemon");
        let t = stub_target(&old, scratch.path(), &missing, "windows");
        let calls = RefCell::new(0);
        let check = |_p: u16| -> Res<()> {
            *calls.borrow_mut() += 1;
            Ok(())
        };
        let guard = RealLedgerGuard::with_paths(vec![]);
        let deps = Deps {
            port_check: &check,
            guard: &guard,
        };
        let r = run(&t, &fast_plan(), &deps).unwrap();
        match (
            &r.ingest,
            &r.cold_daemon_ms,
            &r.idle_daemon_empty,
            &r.idle_daemon_after_ingest,
        ) {
            (Cell::Na(a), Cell::Na(b), Cell::Na(c), Cell::Na(d)) => {
                for x in [a, b, c, d] {
                    assert!(x.contains("would read the real projects dir"), "{x}");
                }
            }
            other => panic!("expected n/a cells, got {other:?}"),
        }
        assert_eq!(*calls.borrow(), 0, "no port check, no spawn");
        assert!(matches!(r.cold_mcp_ms, Cell::Na(_)), "no mcp was given");
    }

    /// One real run. `LUMEN_BENCH_DAEMON_BIN` (and optionally `LUMEN_BENCH_MCP_BIN`,
    /// `LUMEN_BENCH_VERSION`) name binaries extracted from a release asset; `LUMEN_BENCH_SCRATCH`
    /// is a scratch directory. The JSON report goes to stdout. By hand:
    /// `cargo test -p lumen-bench manual_real_run -- --ignored --nocapture`.
    #[test]
    #[ignore = "a real run against a release asset, by hand"]
    fn manual_real_run() {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let bin = var("LUMEN_BENCH_DAEMON_BIN").expect("LUMEN_BENCH_DAEMON_BIN");
        let mcp = var("LUMEN_BENCH_MCP_BIN");
        let scratch = var("LUMEN_BENCH_SCRATCH").expect("LUMEN_BENCH_SCRATCH");
        let version = std::env::var("LUMEN_BENCH_VERSION").unwrap_or_else(|_| "1.9.0".into());
        let e = entry(&version);
        let t = Target {
            version: &version,
            entry: &e,
            daemon: &bin,
            mcp: mcp.as_deref(),
            daemon_args: vec![],
            scratch: &scratch,
            os: std::env::consts::OS,
        };
        let mut plan = Plan::standard();
        if let Ok(s) = std::env::var("LUMEN_BENCH_COLD_RUNS") {
            plan.cold_runs = s.parse().unwrap();
        }
        if let Ok(s) = std::env::var("LUMEN_BENCH_INGEST_RUNS") {
            plan.ingest_runs = s.parse().unwrap();
        }
        let guard = RealLedgerGuard::new().unwrap();
        let deps = Deps {
            port_check: &crate::sandbox::ensure_port_free,
            guard: &guard,
        };
        let r = run(&t, &plan, &deps).unwrap();
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
    }
}
