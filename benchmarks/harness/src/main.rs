use lumen_bench::builds::{self, Build};
use lumen_bench::corpus::{self, Corpus};
use lumen_bench::latency::{LatencyConfig, MIN_RUNS};
use lumen_bench::run::{self, Plan};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Lumen 1.7.0: the last release before the work measured here.
const BASELINE_LABEL: &str = "v1.7.0";
const BASELINE_SHA: &str = "5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94";

const HELP: &str = "\
lumen-bench: token and latency benchmarks for Lumen, baseline against candidate.

usage: cargo run --release -p lumen-bench -- [options]     (or benchmarks/run.sh [options])

  --label NAME           name of the result files (default current-main). A MAJOR.MINOR.PATCH
                         label is a release result; any other label is marked pre-release.
  --out DIR              where the results go (default benchmarks/results)
  --cache DIR            where corpora and the baseline build live (default: a directory under
                         the system temp dir). Never inside the repository.
  --baseline-sha SHA     the baseline commit, 40 characters (default v1.7.0, 5d1ec04...)
  --baseline-label NAME  its label in the output (default v1.7.0)
  --no-baseline          measure the candidate only
  --candidate-bin PATH   use a prebuilt lumen-mcp as the candidate instead of building HEAD
  --baseline-bin PATH    use a prebuilt lumen-mcp as the baseline instead of building its commit
  --tokens-only          skip the latency part
  --latency-only         skip the token part (only the Lumen corpus is fetched)
  --runs N               recorded runs per latency series (default 200, at least 100), pooled over
                         5 passes in which the builds take turns
  --warmup N             discarded warm-up runs per series per pass (default 20)
  --allow-dirty          measure a checkout with uncommitted changes (throwaway runs only)
";

struct Opts {
    label: String,
    out: Option<PathBuf>,
    cache: Option<PathBuf>,
    baseline_sha: String,
    baseline_label: String,
    no_baseline: bool,
    candidate_bin: Option<PathBuf>,
    baseline_bin: Option<PathBuf>,
    tokens: bool,
    latency: bool,
    runs: usize,
    warmup: usize,
    allow_dirty: bool,
}

fn parse(args: &[String]) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        label: "current-main".to_string(),
        out: None,
        cache: None,
        baseline_sha: BASELINE_SHA.to_string(),
        baseline_label: BASELINE_LABEL.to_string(),
        no_baseline: false,
        candidate_bin: None,
        baseline_bin: None,
        tokens: true,
        latency: true,
        runs: 200,
        warmup: 20,
        allow_dirty: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--label" => o.label = value("--label")?,
            "--out" => o.out = Some(PathBuf::from(value("--out")?)),
            "--cache" => o.cache = Some(PathBuf::from(value("--cache")?)),
            "--baseline-sha" => o.baseline_sha = value("--baseline-sha")?,
            "--baseline-label" => o.baseline_label = value("--baseline-label")?,
            "--no-baseline" => o.no_baseline = true,
            "--candidate-bin" => o.candidate_bin = Some(PathBuf::from(value("--candidate-bin")?)),
            "--baseline-bin" => o.baseline_bin = Some(PathBuf::from(value("--baseline-bin")?)),
            "--tokens-only" => o.latency = false,
            "--latency-only" => o.tokens = false,
            "--runs" => {
                o.runs = value("--runs")?
                    .parse()
                    .map_err(|e| format!("--runs: {e}"))?;
            }
            "--warmup" => {
                o.warmup = value("--warmup")?
                    .parse()
                    .map_err(|e| format!("--warmup: {e}"))?;
            }
            "--allow-dirty" => o.allow_dirty = true,
            other => return Err(format!("unknown option {other:?}; see --help")),
        }
    }
    if !o.tokens && !o.latency {
        return Err("--tokens-only and --latency-only together leave nothing to measure".into());
    }
    if o.latency && o.runs < MIN_RUNS {
        return Err(format!(
            "--runs {} is below the minimum of {MIN_RUNS}: a p99 needs at least that many samples",
            o.runs
        ));
    }
    if o.label.is_empty() || o.label.contains(['/', '\\']) {
        return Err(format!("--label {:?} is not a usable file name", o.label));
    }
    Ok(Some(o))
}

fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| format!("the repository root: {e}"))
}

fn main_inner() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(o) = parse(&args)? else {
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
        .unwrap_or_else(|| repo.join("benchmarks").join("results"));
    if cache.starts_with(&repo) {
        return Err(format!(
            "the cache {} is inside the repository; the corpus must stay out of it",
            cache.display()
        ));
    }

    // The builds first: a dirty tree or an unknown baseline stops the run before any fetching.
    let mut builds: Vec<Build> = Vec::new();
    if !o.no_baseline {
        builds.push(match &o.baseline_bin {
            Some(bin) => builds::prebuilt(&o.baseline_label, "baseline", &o.baseline_sha, bin)?,
            None => builds::baseline(&repo, &cache, &o.baseline_label, &o.baseline_sha)?,
        });
    }
    let mut candidate = builds::candidate(&repo, true, o.allow_dirty || o.candidate_bin.is_some())?;
    if let Some(bin) = &o.candidate_bin {
        let mut p = builds::prebuilt("current", "candidate", &candidate.sha, bin)?;
        p.dirty = candidate.dirty;
        candidate = p;
    }
    builds.push(candidate);

    // Latency runs on the Lumen corpus alone, so a latency-only run fetches only that.
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
        work: cache.join("work-run"),
    };
    let output = run::execute(&plan)?;
    for p in run::write_outputs(&out, &output)? {
        println!("wrote {}", p.display());
    }
    Ok(())
}

fn main() -> ExitCode {
    match main_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lumen-bench: {e}");
            ExitCode::FAILURE
        }
    }
}
