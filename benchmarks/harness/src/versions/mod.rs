//! `lumen-bench versions ...`: the version-over-version benchmark (V2-V5).
//!
//! `main.rs` dispatches here and nowhere else, so the tasks that fill this in add sibling
//! modules without editing the dispatch.
//!
//! V2 (this file and [`caps`]): N builds from release assets plus "no Lumen", measured by the
//! same token and latency runners as the two-build mode. The daemon metrics (V4), the script
//! hooks (V3) and the version x metric report (V5) plug in beside [`run_builds`]: they take the
//! builds it returns and the [`caps::Caps`] each carries.

pub mod caps;
pub mod daemon;
pub mod script_hooks;

use crate::builds::{self, Build};
use crate::corpus::{self, Corpus};
use crate::latency::{LatencyConfig, MIN_RUNS};
use crate::manifest::{VersionsManifest, host_platform};
use crate::run::{self, Plan};
use crate::sandbox::RealLedgerGuard;
use crate::util::{Ctx, Res};
use std::path::{Path, PathBuf};

pub const HELP: &str = "\
usage: benchmarks/run.sh versions [options]

  measure released versions from their release assets (never rebuilt), and no Lumen, with the
  token and latency runners. The daemon metrics, script hooks and the report are later tasks.

  --versions A,B,..  versions of benchmarks/versions/manifest.toml to measure
  --all              every version of the manifest (one of --versions and --all is required: a
                     bare `versions` starts nothing)
  --no-lumen         add the arm with no Lumen (tokens are the whole file, with the Read cap)
  --label NAME       name of the result files (default versions)
  --out DIR          where the results go (default benchmarks/results/versions)
  --cache DIR        assets and corpora (default: a directory under the system temp dir; never
                     inside the repository)
  --tokens-only      skip the latency part
  --latency-only     skip the token part
  --runs N           recorded runs per latency series (default 200, at least 100)
  --warmup N         discarded warm-up runs per series per pass (default 20)
";

/// The builds of a run, and the versions that have no asset on the platform with the
/// manifest's reason.
pub type RunBuilds = (Vec<Build>, Vec<(String, String)>);

/// The builds of `versions` (manifest order kept) for `platform`, each unpacked under
/// `work/builds/<version>`, then "no Lumen" if asked. A version with no asset on the platform
/// is returned with the manifest's reason instead of a build: its cells are `n/a`, never 0.
pub fn run_builds(
    m: &VersionsManifest,
    versions: &[String],
    no_lumen: bool,
    platform: &str,
    cache: &Path,
    repo_root: &Path,
    work: &Path,
) -> Res<RunBuilds> {
    let mut builds = Vec::new();
    let mut na = Vec::new();
    for v in versions {
        let dest = work.join("builds").join(v);
        match builds::from_asset(m, v, platform, cache, repo_root, &dest)? {
            Ok(b) => builds.push(b),
            Err(reason) => na.push((v.clone(), reason)),
        }
    }
    if no_lumen {
        builds.push(Build::no_lumen());
    }
    Ok((builds, na))
}

struct Opts {
    versions: Option<Vec<String>>,
    all: bool,
    no_lumen: bool,
    label: String,
    out: Option<PathBuf>,
    cache: Option<PathBuf>,
    tokens: bool,
    latency: bool,
    runs: usize,
    warmup: usize,
}

fn parse(args: &[String]) -> Res<Option<Opts>> {
    let mut o = Opts {
        versions: None,
        all: false,
        no_lumen: false,
        label: "versions".to_string(),
        out: None,
        cache: None,
        tokens: true,
        latency: true,
        runs: 200,
        warmup: 20,
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
            "--all" => o.all = true,
            "--no-lumen" => o.no_lumen = true,
            "--label" => o.label = value("--label")?,
            "--out" => o.out = Some(PathBuf::from(value("--out")?)),
            "--cache" => o.cache = Some(PathBuf::from(value("--cache")?)),
            "--tokens-only" => o.latency = false,
            "--latency-only" => o.tokens = false,
            "--runs" => o.runs = value("--runs")?.parse().ctx("--runs")?,
            "--warmup" => o.warmup = value("--warmup")?.parse().ctx("--warmup")?,
            other => return Err(format!("unknown option {other:?}; see --help")),
        }
    }
    if o.versions.is_none() && !o.all {
        return Err(
            "versions: say what to measure: --versions A,B,.. or --all (see --help)".into(),
        );
    }
    if !o.tokens && !o.latency {
        return Err("--tokens-only and --latency-only together leave nothing to measure".into());
    }
    if o.latency && o.runs < MIN_RUNS {
        return Err(format!(
            "--runs {} is below the minimum of {MIN_RUNS}",
            o.runs
        ));
    }
    if o.label.is_empty() || o.label.contains(['/', '\\']) {
        return Err(format!("--label {:?} is not a usable file name", o.label));
    }
    Ok(Some(o))
}

/// Entry point for `lumen-bench versions <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    let Some(o) = parse(args)? else {
        print!("{HELP}");
        return Ok(());
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .ctx("the repository root")?;
    let cache = o
        .cache
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("lumen-bench-cache"));
    let out = o
        .out
        .clone()
        .unwrap_or_else(|| repo.join("benchmarks").join("results").join("versions"));
    crate::assets::ensure_outside_repo(&cache, &repo)?;
    let platform = host_platform().ok_or("this machine has no column in the manifest")?;
    let m = VersionsManifest::load_committed()?;
    let versions = o
        .versions
        .clone()
        .unwrap_or_else(|| m.meta.versions.clone());
    for v in &versions {
        m.get(v)?;
    }
    let work = cache.join("work-versions");
    let (builds, na) = run_builds(&m, &versions, o.no_lumen, platform, &cache, &repo, &work)?;
    for (v, why) in &na {
        eprintln!("lumen-bench: {v}: n/a on {platform}: {why}");
    }
    let pins: &[corpus::Pin] = if o.tokens {
        corpus::PINS
    } else {
        &corpus::PINS[..1]
    };
    let mut corpora: Vec<Corpus> = Vec::new();
    for pin in pins {
        eprintln!("lumen-bench: corpus {} @ {}", pin.name, pin.sha);
        corpora.push(corpus::load(pin, &cache, Some(&repo))?);
    }
    let plan = Plan {
        label: o.label.clone(),
        corpora,
        builds,
        tokens: o.tokens,
        latency: o.latency.then_some(LatencyConfig {
            runs: o.runs,
            warmup: o.warmup,
        }),
        work: work.join("run"),
    };
    // Nothing a run spawns may touch a real ledger: checked read-only before it starts and again
    // when it ends, whatever the run did.
    let guard = RealLedgerGuard::new()?;
    guard.check("before")?;
    let result = run::execute(&plan).and_then(|output| {
        for p in run::write_outputs(&out, &output)? {
            println!("wrote {}", p.display());
        }
        Ok(())
    });
    let after = guard.check("after");
    result?;
    after
}
