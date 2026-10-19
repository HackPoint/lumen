//! `lumen-bench versions all`: the one command that reproduces the version-over-version
//! benchmark on this OS (V5). `benchmarks/versions/run.sh` is a thin wrapper around it.
//!
//! It fetches every pinned asset (refused on a sha256 mismatch), then measures, one build after
//! another and all inside the sandbox under the real-ledger guard: the token figures, the latency
//! series (binary hooks, script hooks and the server), and the daemon metrics (ingest, cold start,
//! idle CPU and memory), for every version of the manifest that has an asset here, plus no Lumen.
//! It writes `<out>/<os>.json` (the [`Measurements`]) and `<out>/<os>.md` (the report rendered
//! from that very JSON), and prints the `gh issue create` command of every regression. It never
//! files one: that is the orchestrator's job, after review.
//!
//! What is measured is behind [`Measure`], so the orchestration is tested with stubs and `--dry-run`
//! prints the plan without a network, a spawn or a file.

use crate::assets;
use crate::builds::Build;
use crate::corpus::{self, Corpus};
use crate::latency::{self, BuildLatency, LatencyConfig, MIN_RUNS, Subject};
use crate::manifest::{VersionsManifest, host_platform};
use crate::run;
use crate::sandbox::{RealLedgerGuard, ensure_port_free};
use crate::tokens::{self, FileResult, Prepared};
use crate::util::{Ctx, Res};
use crate::versions::caps::Caps;
use crate::versions::daemon::{self, Deps, Target};
use crate::versions::report::{
    self, Daemon, HookKind, Measurements, VersionData, daemon_data, latency_entries, token_data,
};
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
usage: benchmarks/versions/run.sh [options]       (= benchmarks/run.sh versions all [options])
       benchmarks/run.sh versions report FILE     render the report of a results JSON again

  the one command that reproduces the version-over-version benchmark on this OS. It fetches the
  pinned release assets of every version of benchmarks/versions/manifest.toml (sha256 checked,
  never rebuilt), measures each of them and no Lumen in the sandbox under the real-ledger guard
  (tokens, latency of the hooks and the server, daemon ingest, cold start, idle CPU and memory),
  scores them with today's engine, and writes <out>/<os>.json and <out>/<os>.md. The gh issue
  command of every regression is printed, not run.

  --versions A,B,..  only these versions (default: every version of the manifest)
  --out DIR          where <os>.json and <os>.md go (default benchmarks/versions/results)
  --cache DIR        assets and corpora (default: a directory under the system temp dir; never
                     inside the repository)
  --measured-r N     R, the median rounds remaining of the no-Lumen E2E arm, once it has run
  --runs N           recorded runs per latency series (default 200, at least 100)
  --warmup N         discarded warm-up runs per series per pass (default 20)
  --ingest-runs N    fresh ingest runs per version (default 5)
  --cold-runs N      cold-start runs per series (default 30)
  --idle-settle S    seconds to settle before the idle samples (default 30)
  --idle-window S    seconds of idle samples, one a second (default 120)
  --dry-run          print what would be fetched and measured, and where the results would go;
                     fetch, start and write nothing

  Any setting other than the default is written into the results as a note, so a reduced run is
  never mistaken for the full one.
";

/// What the runners measure. [`Real`] is the real thing; tests give stubs.
pub trait Measure {
    /// The token figures of one build over every corpus.
    fn tokens(&self, build: &Build) -> Res<Vec<FileResult>>;
    /// The latency series of every build (interleaved passes, so one call for all).
    fn latency(&self, builds: &[Build]) -> Res<Vec<BuildLatency>>;
    /// The daemon metrics of one build.
    fn daemon(&self, build: &Build) -> Res<daemon::Report>;
}

/// What [`collect`] assembles the measurements from.
pub struct Job<'a> {
    pub manifest: &'a VersionsManifest,
    pub os: &'a str,
    pub platform: &'a str,
    /// The versions asked for, oldest first.
    pub versions: &'a [String],
    /// The builds of those with an asset here (and no Lumen, if asked), probed.
    pub builds: &'a [Build],
    /// The versions with no asset on this platform, and the manifest's reason.
    pub na: &'a [(String, String)],
    pub measured_r: Option<u32>,
    pub notes: Vec<String>,
}

fn hook_kind(caps: &Caps) -> HookKind {
    if caps.no_lumen {
        HookKind::None
    } else if caps.hook_cli {
        HookKind::Binary
    } else {
        HookKind::Script
    }
}

/// Measure everything, in a fixed order (every build's tokens, then the latency of all builds,
/// then every build's daemon) and assemble [`Measurements`]. A measurement that fails stops the
/// run: a table with a silent hole in it is not a result.
pub fn collect(job: &Job, measure: &dyn Measure) -> Res<Measurements> {
    let mut files: Vec<(String, Vec<FileResult>)> = Vec::new();
    for b in job.builds {
        eprintln!("lumen-bench: tokens, {}", b.label);
        files.push((
            b.label.clone(),
            measure
                .tokens(b)
                .map_err(|e| format!("tokens of {}: {e}", b.label))?,
        ));
    }
    eprintln!("lumen-bench: latency, {} builds", job.builds.len());
    let lat = measure
        .latency(job.builds)
        .map_err(|e| format!("latency: {e}"))?;
    let mut daemons: Vec<(String, Daemon)> = Vec::new();
    for b in job.builds {
        if b.is_no_lumen() {
            continue;
        }
        eprintln!("lumen-bench: daemon, {}", b.label);
        let r = measure
            .daemon(b)
            .map_err(|e| format!("daemon metrics of {}: {e}", b.label))?;
        daemons.push((b.label.clone(), daemon_data(&r)));
    }

    let mut out: Vec<VersionData> = Vec::new();
    // No Lumen first: the baseline column.
    for b in job.builds.iter().filter(|b| b.is_no_lumen()) {
        let l = lat.iter().find(|l| l.label == b.label);
        out.push(VersionData {
            version: "no Lumen".to_string(),
            no_lumen: true,
            provenance: b.source.clone(),
            labels: b.caps.as_ref().map(Caps::labels).unwrap_or_default(),
            hook_kind: HookKind::None,
            tokens: report::Cell::Value(token_data(&take(&mut files, &b.label)?)),
            latency: l.map(latency_entries).unwrap_or_default(),
            daemon: Daemon::all_na("no Lumen: there is no hook, no server and no daemon to time"),
        });
    }
    for v in job.versions {
        if let Some((_, why)) = job.na.iter().find(|(n, _)| n == v) {
            let kind = job
                .manifest
                .get(v)
                .map(|e| {
                    if e.hook_cli {
                        HookKind::Binary
                    } else {
                        HookKind::Script
                    }
                })
                .unwrap_or(HookKind::Script);
            out.push(VersionData::unavailable(v, kind, why));
            continue;
        }
        let b = job
            .builds
            .iter()
            .find(|b| &b.label == v)
            .ok_or_else(|| format!("{v}: neither a build nor a reason it has none"))?;
        let caps = b
            .caps
            .as_ref()
            .ok_or_else(|| format!("{v}: a build with no capability map"))?;
        let l = lat
            .iter()
            .find(|l| l.label == b.label)
            .ok_or_else(|| format!("{v}: the latency run returned nothing for it"))?;
        let d = take_daemon(&mut daemons, &b.label)?;
        out.push(VersionData {
            version: v.clone(),
            no_lumen: false,
            provenance: caps
                .asset
                .clone()
                .ok_or_else(|| format!("{v}: a build with no asset provenance"))?,
            labels: caps.labels(),
            hook_kind: hook_kind(caps),
            tokens: report::Cell::Value(token_data(&take(&mut files, &b.label)?)),
            latency: latency_entries(l),
            daemon: d,
        });
    }
    let mut notes = job.notes.clone();
    if let Some(l) = lat.iter().find(|l| !l.load_before.is_empty()) {
        notes.push(format!(
            "machine load when the latency passes began: {}; when they ended: {}",
            l.load_before, l.load_after
        ));
    }
    Ok(Measurements {
        schema: report::SCHEMA,
        os: job.os.to_string(),
        platform: job.platform.to_string(),
        measured_r: job.measured_r,
        notes,
        versions: out,
    })
}

fn take(files: &mut Vec<(String, Vec<FileResult>)>, label: &str) -> Res<Vec<FileResult>> {
    let i = files
        .iter()
        .position(|(l, _)| l == label)
        .ok_or_else(|| format!("{label}: no token figures"))?;
    Ok(files.remove(i).1)
}

fn take_daemon(d: &mut Vec<(String, Daemon)>, label: &str) -> Res<Daemon> {
    let i = d
        .iter()
        .position(|(l, _)| l == label)
        .ok_or_else(|| format!("{label}: no daemon figures"))?;
    Ok(d.remove(i).1)
}

// --- output ----------------------------------------------------------------------------------

/// Write `<os>.json` and `<os>.md` into `out`. The Markdown is rendered from the JSON as it is
/// written, parsed back, so anyone rendering that file again gets the same bytes.
pub fn write_results(out: &Path, m: &Measurements) -> Res<(Vec<PathBuf>, report::Rendered)> {
    std::fs::create_dir_all(out).ctx(&format!("create {}", out.display()))?;
    let json = m.to_json()?;
    let again = Measurements::parse(&json)?;
    let rendered = report::render(&again)?;
    let jp = out.join(format!("{}.json", m.os));
    let mp = out.join(format!("{}.md", m.os));
    std::fs::write(&jp, json).ctx(&format!("write {}", jp.display()))?;
    std::fs::write(&mp, &rendered.markdown).ctx(&format!("write {}", mp.display()))?;
    Ok((vec![jp, mp], rendered))
}

/// The commands that file the regressions' issues, ready to paste. Printed, never run.
pub fn issue_commands(r: &report::Rendered) -> String {
    if r.issues.is_empty() {
        return "no regressions: no issue to file\n".to_string();
    }
    let mut s = format!(
        "{} regression(s). The commands below file their issues; they are printed, not run:\n\n",
        r.issues.len()
    );
    for i in &r.issues {
        s.push_str(&i.command);
        s.push_str("\n\n");
    }
    s
}

// --- options ---------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct Opts {
    pub versions: Option<Vec<String>>,
    pub out: Option<PathBuf>,
    pub cache: Option<PathBuf>,
    pub measured_r: Option<u32>,
    pub runs: usize,
    pub warmup: usize,
    pub ingest_runs: usize,
    pub cold_runs: usize,
    pub idle_settle: u64,
    pub idle_window: u64,
    pub dry_run: bool,
}

pub fn parse(args: &[String]) -> Res<Option<Opts>> {
    let std_plan = daemon::Plan::standard();
    let mut o = Opts {
        versions: None,
        out: None,
        cache: None,
        measured_r: None,
        runs: 200,
        warmup: 20,
        ingest_runs: std_plan.ingest_runs,
        cold_runs: std_plan.cold_runs,
        idle_settle: std_plan.idle.settle.as_secs(),
        idle_window: std_plan.idle.window.as_secs(),
        dry_run: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Res<String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--versions" => {
                o.versions = Some(
                    value("--versions")?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                );
            }
            "--out" => o.out = Some(PathBuf::from(value("--out")?)),
            "--cache" => o.cache = Some(PathBuf::from(value("--cache")?)),
            "--measured-r" => {
                o.measured_r = Some(value("--measured-r")?.parse().ctx("--measured-r")?)
            }
            "--runs" => o.runs = value("--runs")?.parse().ctx("--runs")?,
            "--warmup" => o.warmup = value("--warmup")?.parse().ctx("--warmup")?,
            "--ingest-runs" => {
                o.ingest_runs = value("--ingest-runs")?.parse().ctx("--ingest-runs")?
            }
            "--cold-runs" => o.cold_runs = value("--cold-runs")?.parse().ctx("--cold-runs")?,
            "--idle-settle" => {
                o.idle_settle = value("--idle-settle")?.parse().ctx("--idle-settle")?
            }
            "--idle-window" => {
                o.idle_window = value("--idle-window")?.parse().ctx("--idle-window")?
            }
            "--dry-run" => o.dry_run = true,
            other => return Err(format!("unknown option {other:?}; see --help")),
        }
    }
    if o.runs < MIN_RUNS {
        return Err(format!(
            "--runs {} is below the minimum of {MIN_RUNS}: a p99 needs at least that many samples",
            o.runs
        ));
    }
    if o.ingest_runs == 0 || o.cold_runs == 0 || o.idle_window == 0 {
        return Err("--ingest-runs, --cold-runs and --idle-window must be at least 1".into());
    }
    if o.measured_r.is_some_and(|r| r > crate::score::MAX_R) {
        return Err(format!(
            "--measured-r is over the supported {}",
            crate::score::MAX_R
        ));
    }
    Ok(Some(o))
}

impl Opts {
    fn plan(&self) -> daemon::Plan {
        let mut p = daemon::Plan::standard();
        p.ingest_runs = self.ingest_runs;
        p.cold_runs = self.cold_runs;
        p.idle.settle = std::time::Duration::from_secs(self.idle_settle);
        p.idle.window = std::time::Duration::from_secs(self.idle_window);
        p
    }

    /// The run's settings as sentences for the results: a reduced run says so.
    pub fn notes(&self, versions: &[String], all: &[String]) -> Vec<String> {
        let std_plan = daemon::Plan::standard();
        let cfg = LatencyConfig {
            runs: self.runs,
            warmup: self.warmup,
        };
        let (passes, per_pass) = latency::plan_passes(&cfg);
        let mut n = vec![
            format!(
                "latency: {} recorded runs per series in {passes} passes of {per_pass}, {} \
                 warm-up runs per series per pass",
                passes * per_pass,
                self.warmup
            ),
            format!(
                "daemon: {} ingest runs of a seeded {} MB transcript set, {} cold-start runs per \
                 series, idle CPU and memory sampled once a second for {} s after {} s to settle",
                self.ingest_runs,
                daemon::TARGET_BYTES / (1024 * 1024),
                self.cold_runs,
                self.idle_window,
                self.idle_settle
            ),
        ];
        let reduced = self.runs != 200
            || self.warmup != 20
            || self.ingest_runs != std_plan.ingest_runs
            || self.cold_runs != std_plan.cold_runs
            || self.idle_settle != std_plan.idle.settle.as_secs()
            || self.idle_window != std_plan.idle.window.as_secs();
        if reduced {
            n.push(
                "REDUCED SETTINGS: this run used other than the default settings of METHOD.md; \
                 it is a smoke run, not the benchmark"
                    .to_string(),
            );
        }
        if versions != all {
            n.push(format!(
                "SUBSET: only {} of the manifest's {} versions were measured",
                versions.len(),
                all.len()
            ));
        }
        n
    }
}

// --- the dry run -----------------------------------------------------------------------------

/// What `--dry-run` prints: the platform, each version's pinned asset (or why it has none), what
/// is measured, and where the results go. Pure: it reads the manifest and nothing else.
pub fn dry_run_plan(
    m: &VersionsManifest,
    versions: &[String],
    platform: &str,
    os: &str,
    o: &Opts,
    out: &Path,
) -> Res<String> {
    let mut s = String::from(
        "lumen-bench versions all: dry run, nothing is fetched, started or written\n\n",
    );
    s.push_str(&format!(
        "platform: {platform} (os {os})\nversions, oldest first:\n"
    ));
    for v in versions {
        match m.asset(v, platform)? {
            Ok(a) => s.push_str(&format!(
                "  {v}: {} (sha256 {}); fetched if not cached, refused on a mismatch, never rebuilt\n",
                a.name, a.sha256
            )),
            Err(why) => s.push_str(&format!("  {v}: n/a — {why}\n")),
        }
    }
    s.push_str("  no Lumen: nothing to fetch; a file is read whole, with the Read cap\n\n");
    let plan = o.plan();
    s.push_str("measured for each version, one after the other, in the sandbox under the real-ledger guard:\n");
    s.push_str("  tokens: smart_read, recall_file, repeat-read stub and delta, per file of the pinned corpora\n");
    s.push_str(&format!(
        "  latency: binary hooks, script hooks (up to 1.5.1) and the server, {} runs per series, {} warm-up\n",
        o.runs, o.warmup
    ));
    s.push_str(&format!(
        "  daemon: {} ingest runs, {} cold-start runs, idle CPU and memory ({} s settle, {} s window)\n\n",
        plan.ingest_runs, plan.cold_runs, o.idle_settle, o.idle_window
    ));
    s.push_str(
        "scored with today's engine (the MODELLED primary reference session), then reported:\n",
    );
    s.push_str(&format!(
        "  {}\n  {}\n",
        out.join(format!("{os}.json")).display(),
        out.join(format!("{os}.md")).display()
    ));
    s.push_str("the gh issue command of every regression is printed, never run\n");
    Ok(s)
}

// --- the real measurements -------------------------------------------------------------------

struct Real<'a> {
    manifest: &'a VersionsManifest,
    corpora: Vec<Corpus>,
    prepared: Vec<Vec<Prepared>>,
    subjects: Vec<Subject>,
    cfg: LatencyConfig,
    work: PathBuf,
    plan: daemon::Plan,
    guard: &'a RealLedgerGuard,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

impl Measure for Real<'_> {
    fn tokens(&self, b: &Build) -> Res<Vec<FileResult>> {
        let mut all = Vec::new();
        for (c, prep) in self.corpora.iter().zip(&self.prepared) {
            let (f, _version) =
                tokens::measure(b, c, prep, &self.work.join("tokens").join(&b.label))?;
            all.extend(f);
        }
        // The harness's own cross-check, as in a recorded run: with repeat reads on, the first
        // read must be the outline, or the stub and delta figures are not comparable to it.
        if let Some(bad) = all.iter().find(|f| !f.first_matches_off) {
            return Err(format!(
                "{}: with repeat reads on, the first read of {} is not the repeat-off outline",
                b.label, bad.path
            ));
        }
        Ok(all)
    }

    fn latency(&self, builds: &[Build]) -> Res<Vec<BuildLatency>> {
        let (passes, per_pass) = latency::plan_passes(&self.cfg);
        eprintln!(
            "lumen-bench: latency, {} builds, {passes} passes of {per_pass} runs",
            builds.len()
        );
        latency::measure_builds(
            builds,
            &self.subjects,
            &self.cfg,
            &self.work.join("latency"),
        )
    }

    fn daemon(&self, b: &Build) -> Res<daemon::Report> {
        let entry = self.manifest.get(&b.label)?;
        let bin = b.bin.with_file_name(exe("lumen-daemon"));
        let scratch = self.work.join("daemon").join(&b.label);
        let target = Target {
            version: &b.label,
            entry,
            daemon: &bin,
            mcp: Some(&b.bin),
            daemon_args: Vec::new(),
            scratch: &scratch,
            os: std::env::consts::OS,
        };
        let deps = Deps {
            port_check: &ensure_port_free,
            guard: self.guard,
        };
        daemon::run(&target, &self.plan, &deps)
    }
}

fn repo_root() -> Res<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .ctx("the repository root")
}

/// Entry point for `lumen-bench versions all <args>`.
pub fn entry_all(args: &[String]) -> Res<()> {
    let Some(o) = parse(args)? else {
        print!("{HELP}");
        return Ok(());
    };
    let repo = repo_root()?;
    let cache = o
        .cache
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("lumen-bench-cache"));
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| repo.join("benchmarks").join("versions").join("results"));
    assets::ensure_outside_repo(&cache, &repo)?;
    let platform = host_platform().ok_or("this machine has no column in the manifest")?;
    let os = std::env::consts::OS;
    let m = VersionsManifest::load_committed()?;
    let all = m.meta.versions.clone();
    let versions = o.versions.clone().unwrap_or_else(|| all.clone());
    for v in &versions {
        m.get(v)?;
    }
    if o.dry_run {
        print!("{}", dry_run_plan(&m, &versions, platform, os, &o, &out)?);
        return Ok(());
    }

    // Nothing a run spawns may touch a real ledger: checked read-only before it starts and again
    // when it ends, also if it panics.
    let guard = RealLedgerGuard::new()?;
    let scope = guard.begin()?;
    let result = measure_all(
        &o, &m, &versions, &all, platform, os, &repo, &cache, &out, &guard,
    );
    let after = scope.finish();
    let rendered = result?;
    after?;
    print!("{}", issue_commands(&rendered));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn measure_all(
    o: &Opts,
    m: &VersionsManifest,
    versions: &[String],
    all: &[String],
    platform: &str,
    os: &str,
    repo: &Path,
    cache: &Path,
    out: &Path,
    guard: &RealLedgerGuard,
) -> Res<report::Rendered> {
    let work = cache.join("work-versions-all");
    if work.exists() {
        std::fs::remove_dir_all(&work).ctx(&format!("clear {}", work.display()))?;
    }
    std::fs::create_dir_all(&work).ctx("create the scratch")?;
    let (builds, na) = super::run_builds(m, versions, true, platform, cache, repo, &work)?;
    for (v, why) in &na {
        eprintln!("lumen-bench: {v}: n/a on {platform}: {why}");
    }
    let probed = run::probe_builds(&builds, &work.join("probe"))?;
    let mut corpora: Vec<Corpus> = Vec::new();
    for pin in corpus::PINS {
        eprintln!("lumen-bench: corpus {} @ {}", pin.name, pin.sha);
        corpora.push(corpus::load(pin, cache, Some(repo))?);
    }
    let prepared: Vec<Vec<Prepared>> = corpora
        .iter()
        .map(tokens::prepare)
        .collect::<Res<Vec<_>>>()?;
    let first = corpora.first().ok_or("no corpus to measure")?;
    let subjects = latency::pick_subjects(&first.root, &prepared[0]);
    if subjects.is_empty() {
        return Err(format!(
            "{}: no Rust file with a named item and an edit to time on",
            first.name
        ));
    }
    let real = Real {
        manifest: m,
        corpora,
        prepared,
        subjects,
        cfg: LatencyConfig {
            runs: o.runs,
            warmup: o.warmup,
        },
        work: work.clone(),
        plan: o.plan(),
        guard,
    };
    let job = Job {
        manifest: m,
        os,
        platform,
        versions,
        builds: &probed,
        na: &na,
        measured_r: o.measured_r,
        notes: o.notes(versions, all),
    };
    let measurements = collect(&job, &real)?;
    let (written, rendered) = write_results(out, &measurements)?;
    for p in written {
        println!("wrote {}", p.display());
    }
    if let Err(e) = std::fs::remove_dir_all(&work) {
        eprintln!(
            "lumen-bench: could not remove the scratch {}: {e}",
            work.display()
        );
    }
    Ok(rendered)
}

/// Entry point for `lumen-bench versions report FILE`: render a results JSON again.
pub fn entry_report(args: &[String]) -> Res<()> {
    let [file] = args else {
        return Err("usage: versions report FILE (a <os>.json)".into());
    };
    let text = std::fs::read_to_string(file).ctx(&format!("read {file}"))?;
    let m = Measurements::parse(&text)?;
    let r = report::render(&m)?;
    print!("{}", r.markdown);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::Identity;
    use crate::latency::Series;
    use crate::stats::lat;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn manifest() -> VersionsManifest {
        VersionsManifest::load_committed().unwrap()
    }

    fn build(m: &VersionsManifest, v: &str) -> Build {
        let e = m.get(v).unwrap();
        let caps = Caps::from_entry(v, e)
            .unwrap()
            .with_asset(format!("asset-of-{v} (sha256 x)"));
        Build {
            label: v.to_string(),
            role: "version",
            sha: e.commit.clone(),
            dirty: false,
            bin: PathBuf::from("/nowhere/lumen-mcp"),
            profile: "release",
            profile_note: String::new(),
            source: format!("release asset asset-of-{v}"),
            identity: Identity::Env,
            inproc: false,
            caps: Some(caps),
        }
    }

    fn file(path: &str, lines: usize, full: i64, outline: i64) -> FileResult {
        FileResult {
            corpus: "c".to_string(),
            path: path.to_string(),
            language: "rust".to_string(),
            lines,
            bucket: "<300".to_string(),
            full,
            outline,
            outline_route: "smart_read".to_string(),
            recall_item: None,
            recall: Some(40),
            recall_route: Some("recall_file".to_string()),
            stub: 10,
            stub_route: "unchanged".to_string(),
            delta_line: None,
            delta: Some(20),
            delta_route: Some("delta".to_string()),
            first_matches_off: true,
            repeat_na: None,
            outline_self_saved: None,
        }
    }

    fn series(id: &str) -> Series {
        Series {
            id: id.to_string(),
            what: String::new(),
            subject: None,
            status: "measured".to_string(),
            lat: lat(&[1_000_000, 2_000_000, 3_000_000]),
            routes: BTreeMap::new(),
            pass_medians_ns: vec![2_000_000; 3],
            raw: Vec::new(),
        }
    }

    fn report_of(version: &str) -> daemon::Report {
        fn na<T>() -> daemon::Cell<T> {
            daemon::Cell::Na("stub".to_string())
        }
        daemon::Report {
            version: version.to_string(),
            os: "macos".to_string(),
            cache: daemon::WARM.to_string(),
            ingest: na(),
            cold_daemon_ms: na(),
            cold_mcp_ms: na(),
            cold_hook_ms: na(),
            idle_daemon_empty: na(),
            idle_daemon_after_ingest: na(),
            idle_mcp: na(),
        }
    }

    /// Records what it was asked for, in order.
    struct Stub {
        log: RefCell<Vec<String>>,
        fail_daemon_of: Option<&'static str>,
    }

    impl Measure for Stub {
        fn tokens(&self, b: &Build) -> Res<Vec<FileResult>> {
            self.log.borrow_mut().push(format!("tokens {}", b.label));
            Ok(vec![file("a.rs", 100, 1000, 300)])
        }
        fn latency(&self, builds: &[Build]) -> Res<Vec<BuildLatency>> {
            let labels: Vec<&str> = builds.iter().map(|b| b.label.as_str()).collect();
            self.log
                .borrow_mut()
                .push(format!("latency {}", labels.join(",")));
            Ok(builds
                .iter()
                .map(|b| BuildLatency {
                    label: b.label.clone(),
                    reported_version: String::new(),
                    load_before: "load 1.0".to_string(),
                    load_after: "load 1.1".to_string(),
                    series: vec![series("hook.floor"), series("hook.floor.inproc")],
                    derived: Vec::new(),
                })
                .collect())
        }
        fn daemon(&self, b: &Build) -> Res<daemon::Report> {
            self.log.borrow_mut().push(format!("daemon {}", b.label));
            if self.fail_daemon_of == Some(b.label.as_str()) {
                return Err("the daemon died".to_string());
            }
            Ok(report_of(&b.label))
        }
    }

    fn job<'a>(
        m: &'a VersionsManifest,
        versions: &'a [String],
        builds: &'a [Build],
        na: &'a [(String, String)],
    ) -> Job<'a> {
        Job {
            manifest: m,
            os: "linux",
            platform: "linux-x86_64",
            versions,
            builds,
            na,
            measured_r: None,
            notes: vec!["a note".to_string()],
        }
    }

    #[test]
    fn collect_measures_every_build_in_order_and_gives_each_version_its_kind() {
        let m = manifest();
        let versions: Vec<String> = ["1.0.0", "1.1.5", "1.6.0"].map(String::from).to_vec();
        let builds = vec![build(&m, "1.1.5"), build(&m, "1.6.0"), Build::no_lumen()];
        let na = vec![("1.0.0".to_string(), "no Linux asset".to_string())];
        let stub = Stub {
            log: RefCell::new(Vec::new()),
            fail_daemon_of: None,
        };
        let out = collect(&job(&m, &versions, &builds, &na), &stub).unwrap();
        // Tokens per build, then the latency of all builds at once, then each daemon, and the
        // daemon of no Lumen is never asked for.
        assert_eq!(
            *stub.log.borrow(),
            [
                "tokens 1.1.5",
                "tokens 1.6.0",
                "tokens no-lumen",
                "latency 1.1.5,1.6.0,no-lumen",
                "daemon 1.1.5",
                "daemon 1.6.0"
            ]
        );
        let names: Vec<&str> = out.versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(names, ["no Lumen", "1.0.0", "1.1.5", "1.6.0"]);
        let kinds: Vec<HookKind> = out.versions.iter().map(|v| v.hook_kind).collect();
        assert_eq!(
            kinds,
            [
                HookKind::None,
                HookKind::Script,
                HookKind::Script,
                HookKind::Binary
            ]
        );
        // 1.0.0 has no asset: every cell says why, nothing was measured.
        let v100 = &out.versions[1];
        assert!(matches!(&v100.tokens, report::Cell::Na(w) if w == "no Linux asset"));
        assert!(v100.latency.is_empty());
        // The in-process series of a prebuilt version are dropped.
        assert_eq!(out.versions[2].latency.len(), 1);
        assert!(out.notes.iter().any(|n| n.contains("load 1.0")));
        assert_eq!(out.os, "linux");
        // The result renders.
        report::render(&out).unwrap();
    }

    #[test]
    fn a_failed_measurement_stops_the_run_with_the_step_named() {
        let m = manifest();
        let versions: Vec<String> = ["1.1.5", "1.6.0"].map(String::from).to_vec();
        let builds = vec![build(&m, "1.1.5"), build(&m, "1.6.0")];
        let stub = Stub {
            log: RefCell::new(Vec::new()),
            fail_daemon_of: Some("1.6.0"),
        };
        let e = collect(&job(&m, &versions, &builds, &[]), &stub).unwrap_err();
        assert!(
            e.contains("daemon metrics of 1.6.0") && e.contains("the daemon died"),
            "{e}"
        );
    }

    #[test]
    fn a_version_with_neither_a_build_nor_a_reason_is_an_error() {
        let m = manifest();
        let versions: Vec<String> = ["1.1.5", "1.6.0"].map(String::from).to_vec();
        let builds = vec![build(&m, "1.1.5")];
        let stub = Stub {
            log: RefCell::new(Vec::new()),
            fail_daemon_of: None,
        };
        let e = collect(&job(&m, &versions, &builds, &[]), &stub).unwrap_err();
        assert!(e.contains("1.6.0"), "{e}");
    }

    fn opts(args: &[&str]) -> Res<Option<Opts>> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn the_dry_run_names_every_asset_and_writes_nothing() {
        let m = manifest();
        let o = opts(&["--dry-run"]).unwrap().unwrap();
        let all = m.meta.versions.clone();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("results");
        let text = dry_run_plan(&m, &all, "linux-x86_64", "linux", &o, &out).unwrap();
        assert!(text.contains("dry run, nothing is fetched"), "{text}");
        for v in &all {
            assert!(
                text.contains(&format!("  {v}: ")),
                "{v} missing from the plan"
            );
        }
        assert!(text.contains("lumen_1.1.5_amd64.deb (sha256 "), "{text}");
        assert!(
            text.contains("1.0.0: n/a — 1.0.0 shipped no Linux app asset"),
            "{text}"
        );
        assert!(text.contains("no Lumen: nothing to fetch"));
        assert!(text.contains("linux.json") && text.contains("linux.md"));
        assert!(text.contains("never run"));
        // Pure: no directory was made for the results.
        assert!(!out.exists());
    }

    #[test]
    fn options_are_checked_and_a_reduced_run_says_so() {
        assert!(opts(&["--runs", "99"]).is_err(), "below the minimum");
        assert!(opts(&["--bogus"]).is_err());
        assert!(opts(&["--ingest-runs", "0"]).is_err());
        assert!(opts(&["--measured-r", "999999"]).is_err());
        assert!(opts(&["--help"]).unwrap().is_none());
        let m = manifest();
        let all = m.meta.versions.clone();
        let full = opts(&[]).unwrap().unwrap();
        let notes = full.notes(&all, &all);
        assert!(
            !notes
                .iter()
                .any(|n| n.contains("REDUCED") || n.contains("SUBSET")),
            "{notes:?}"
        );
        let quick = opts(&["--idle-window", "5", "--idle-settle", "1"])
            .unwrap()
            .unwrap();
        let sub = vec!["1.9.0".to_string()];
        let notes = quick.notes(&sub, &all);
        assert!(
            notes.iter().any(|n| n.starts_with("REDUCED SETTINGS")),
            "{notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.starts_with("SUBSET: only 1 of")),
            "{notes:?}"
        );
    }

    #[test]
    fn the_written_markdown_is_the_render_of_the_written_json() {
        let m = manifest();
        let versions: Vec<String> = ["1.1.5", "1.6.0"].map(String::from).to_vec();
        let builds = vec![build(&m, "1.1.5"), build(&m, "1.6.0"), Build::no_lumen()];
        let stub = Stub {
            log: RefCell::new(Vec::new()),
            fail_daemon_of: None,
        };
        let out = collect(&job(&m, &versions, &builds, &[]), &stub).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (paths, rendered) = write_results(dir.path(), &out).unwrap();
        assert_eq!(paths.len(), 2);
        let json = std::fs::read_to_string(dir.path().join("linux.json")).unwrap();
        let md = std::fs::read_to_string(dir.path().join("linux.md")).unwrap();
        assert_eq!(md, rendered.markdown);
        assert_eq!(
            report::render(&Measurements::parse(&json).unwrap())
                .unwrap()
                .markdown,
            md
        );
        // No path of this machine is in either file.
        let here = dir.path().to_string_lossy().into_owned();
        assert!(!json.contains(&here) && !md.contains(&here));
        assert!(
            issue_commands(&rendered).contains("regression")
                || issue_commands(&rendered).contains("no regressions")
        );
    }
}
