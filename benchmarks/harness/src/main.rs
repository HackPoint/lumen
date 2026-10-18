use lumen_bench::builds::{self, Build};
use lumen_bench::compare;
use lumen_bench::corpus::{self, Corpus};
use lumen_bench::latency::{LatencyConfig, MIN_RUNS};
use lumen_bench::run::{self, Plan};
use lumen_bench::sandbox::RealLedgerGuard;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Lumen 1.8.0: the last release before the work measured here.
const BASELINE_LABEL: &str = "v1.8.0";
const BASELINE_SHA: &str = "12ad305679cf92b23167b032f56412a2a38e72a1";

const DEFAULT_RUNS: usize = 200;
const DEFAULT_WARMUP: usize = 20;

const HELP: &str = "\
lumen-bench: token and latency benchmarks for Lumen, baseline against candidate.

usage: benchmarks/run.sh [options]                  record a run
       benchmarks/run.sh compare FILE [options]     measure latency again and set it against the
                                                    committed results FILE (a <label>-<os>.json)
       benchmarks/run.sh versions|competitors|e2e ...  the P2 benchmarks (not yet implemented)

  --label NAME           name of the result files (default pre-release-<12 characters of the
                         candidate's commit>). A MAJOR.MINOR.PATCH label is a release result; any
                         other label is marked pre-release.
  --out DIR              where the results go (default benchmarks/results)
  --cache DIR            where corpora and the baseline build live (default: a directory under
                         the system temp dir). Never inside the repository.
  --baseline-sha SHA     the baseline commit, 40 characters (default v1.8.0, 12ad305...)
  --baseline-label NAME  its label in the output (default v1.8.0)
  --no-baseline          measure the candidate only
  --candidate-bin PATH   use a prebuilt lumen-mcp as the candidate instead of building HEAD; needs
                         --candidate-sha, and the checkout is neither built nor required clean
  --candidate-sha SHA    the commit that binary was built from, 40 characters
  --candidate-label NAME its label in the output (default pre-<7 characters of its commit>)
  --baseline-bin PATH    use a prebuilt lumen-mcp as the baseline instead of building its commit
  --tokens-only          skip the latency part
  --latency-only         skip the token part (only the Lumen corpus is fetched)
  --runs N               recorded runs per latency series (default 200, at least 100), pooled over
                         5 passes in which the builds take turns (compare: the committed run's)
  --warmup N             discarded warm-up runs per series per pass (default 20; compare: the
                         committed run's)
  --allow-dirty          measure a checkout with uncommitted changes (throwaway runs only)
";

struct Opts {
    /// `Some(file)` for `compare`.
    compare: Option<PathBuf>,
    label: Option<String>,
    out: Option<PathBuf>,
    cache: Option<PathBuf>,
    baseline_sha: String,
    baseline_label: String,
    no_baseline: bool,
    candidate_bin: Option<PathBuf>,
    candidate_sha: Option<String>,
    candidate_label: Option<String>,
    baseline_bin: Option<PathBuf>,
    tokens: bool,
    latency: bool,
    runs: Option<usize>,
    warmup: Option<usize>,
    allow_dirty: bool,
}

fn parse(args: &[String]) -> Result<Option<Opts>, String> {
    let mut o = Opts {
        compare: None,
        label: None,
        out: None,
        cache: None,
        baseline_sha: BASELINE_SHA.to_string(),
        baseline_label: BASELINE_LABEL.to_string(),
        no_baseline: false,
        candidate_bin: None,
        candidate_sha: None,
        candidate_label: None,
        baseline_bin: None,
        tokens: true,
        latency: true,
        runs: None,
        warmup: None,
        allow_dirty: false,
    };
    let mut it = args.iter().peekable();
    if it.peek().map(|a| a.as_str()) == Some("compare") {
        it.next();
        let file = it
            .next()
            .ok_or("compare needs the committed results file: compare FILE")?;
        o.compare = Some(PathBuf::from(file));
        // A comparison is of latency, on the Lumen corpus.
        o.tokens = false;
    }
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--label" => o.label = Some(value("--label")?),
            "--out" => o.out = Some(PathBuf::from(value("--out")?)),
            "--cache" => o.cache = Some(PathBuf::from(value("--cache")?)),
            "--baseline-sha" => o.baseline_sha = value("--baseline-sha")?,
            "--baseline-label" => o.baseline_label = value("--baseline-label")?,
            "--no-baseline" => o.no_baseline = true,
            "--candidate-bin" => o.candidate_bin = Some(PathBuf::from(value("--candidate-bin")?)),
            "--candidate-sha" => o.candidate_sha = Some(value("--candidate-sha")?),
            "--candidate-label" => o.candidate_label = Some(value("--candidate-label")?),
            "--baseline-bin" => o.baseline_bin = Some(PathBuf::from(value("--baseline-bin")?)),
            "--tokens-only" if o.compare.is_none() => o.latency = false,
            "--latency-only" => o.tokens = false,
            "--runs" => {
                o.runs = Some(
                    value("--runs")?
                        .parse()
                        .map_err(|e| format!("--runs: {e}"))?,
                );
            }
            "--warmup" => {
                o.warmup = Some(
                    value("--warmup")?
                        .parse()
                        .map_err(|e| format!("--warmup: {e}"))?,
                );
            }
            "--allow-dirty" => o.allow_dirty = true,
            other => return Err(format!("unknown option {other:?}; see --help")),
        }
    }
    if !o.tokens && !o.latency {
        return Err("--tokens-only and --latency-only together leave nothing to measure".into());
    }
    if o.latency && o.runs.is_some_and(|r| r < MIN_RUNS) {
        return Err(format!(
            "--runs {} is below the minimum of {MIN_RUNS}: a p99 needs at least that many samples",
            o.runs.unwrap_or_default()
        ));
    }
    if let Some(l) = &o.label
        && (l.is_empty() || l.contains(['/', '\\']))
    {
        return Err(format!("--label {l:?} is not a usable file name"));
    }
    if o.candidate_bin.is_some() != o.candidate_sha.is_some() {
        return Err(
            "--candidate-bin and --candidate-sha go together: a binary's provenance is the \
             commit it was built from, and the harness does not guess it"
                .into(),
        );
    }
    if o.candidate_label
        .as_ref()
        .is_some_and(|l| l.is_empty() || l.contains(['/', '\\']))
    {
        return Err("--candidate-label is not a usable name".into());
    }
    Ok(Some(o))
}

fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .map_err(|e| format!("the repository root: {e}"))
}

/// The builds to measure: the baseline, then the candidate. A dirty tree or an unknown baseline
/// stops the run before any fetching.
fn make_builds(o: &Opts, repo: &Path, cache: &Path) -> Result<Vec<Build>, String> {
    let mut builds: Vec<Build> = Vec::new();
    if !o.no_baseline {
        builds.push(match &o.baseline_bin {
            Some(bin) => builds::prebuilt(&o.baseline_label, "baseline", &o.baseline_sha, bin)?,
            None => builds::baseline(repo, cache, &o.baseline_label, &o.baseline_sha)?,
        });
    }
    let candidate = match (&o.candidate_bin, &o.candidate_sha) {
        (Some(bin), Some(sha)) => {
            let label = o
                .candidate_label
                .clone()
                .unwrap_or_else(|| builds::pre_release_label(sha));
            builds::prebuilt(&label, "candidate", sha, bin)?
        }
        _ => {
            let mut c = builds::candidate(repo, true, o.allow_dirty)?;
            if let Some(label) = &o.candidate_label {
                c.label.clone_from(label);
            }
            c
        }
    };
    builds.push(candidate);
    Ok(builds)
}

fn load_corpora(o: &Opts, repo: &Path, cache: &Path) -> Result<Vec<Corpus>, String> {
    // Latency runs on the Lumen corpus alone, so a latency-only run fetches only that.
    let pins: &[corpus::Pin] = if o.tokens {
        corpus::PINS
    } else {
        &corpus::PINS[..1]
    };
    let mut corpora: Vec<Corpus> = Vec::new();
    for pin in pins {
        eprintln!("lumen-bench: corpus {} @ {}", pin.name, pin.sha);
        corpora.push(corpus::load(pin, cache, Some(repo))?);
    }
    Ok(corpora)
}

fn main_inner() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Each subcommand lives in its own module, which owns its arguments.
    match args.first().map(String::as_str) {
        Some("versions") => return lumen_bench::versions::entry(&args[1..]),
        Some("competitors") => return lumen_bench::competitors::entry(&args[1..]),
        Some("e2e") => return lumen_bench::e2e::entry(&args[1..]),
        _ => {}
    }
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
    lumen_bench::assets::ensure_outside_repo(&cache, &repo)?;

    // The committed file is read first: an unreadable one stops the run before any building.
    let committed = match &o.compare {
        Some(file) => {
            let json = std::fs::read_to_string(file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            Some(compare::parse_committed(&json)?)
        }
        None => None,
    };
    let runs = o
        .runs
        .or_else(|| {
            committed
                .as_ref()
                .map(|c| c.runs)
                .filter(|r| *r >= MIN_RUNS)
        })
        .unwrap_or(DEFAULT_RUNS);
    let warmup = o
        .warmup
        .or_else(|| committed.as_ref().map(|c| c.warmup))
        .unwrap_or(DEFAULT_WARMUP);

    let builds = make_builds(&o, &repo, &cache)?;
    let corpora = load_corpora(&o, &repo, &cache)?;
    let candidate_sha = builds.last().map(|b| b.sha.clone()).unwrap_or_default();
    let label = o.label.clone().unwrap_or_else(|| {
        format!(
            "pre-release-{}",
            &candidate_sha[..candidate_sha.len().min(12)]
        )
    });

    let plan = Plan {
        label,
        corpora,
        builds,
        tokens: o.tokens,
        latency: o.latency.then_some(LatencyConfig { runs, warmup }),
        work: cache.join("work-run"),
    };

    // Nothing a run spawns may touch a real ledger: checked read-only before it starts and again
    // when it ends, whatever the run did.
    let guard = RealLedgerGuard::new()?;
    guard.check("before")?;
    let result = measure(&plan, committed, &out);
    let after = guard.check("after");
    result?;
    after
}

fn measure(plan: &Plan, committed: Option<compare::Committed>, out: &Path) -> Result<(), String> {
    if let Some(committed) = committed {
        let (subjects, now) = run::latency_only(plan)?;
        let sides = compare::sides(&committed, &plan.builds, &now)?;
        let now_subjects: Vec<String> = subjects.iter().map(|s| s.rel.clone()).collect();
        let corpus_sha = plan
            .corpora
            .first()
            .map(|c| c.sha.clone())
            .unwrap_or_default();
        print!(
            "{}",
            compare::render(&committed, &corpus_sha, &now_subjects, &sides)
        );
        return Ok(());
    }

    let output = run::execute(plan)?;
    for p in run::write_outputs(out, &output)? {
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
