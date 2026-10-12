//! One run: the token figures and the latency figures for a set of builds over a set of corpora,
//! and the three documents that come out of it.
//!
//! The builds are measured one after the other, never at once, so two builds never compete for
//! the machine. Every build sees byte-identical inputs: the corpora are prepared once.

use crate::builds::Build;
use crate::corpus::Corpus;
use crate::driver::probe_identity;
use crate::env;
use crate::latency::{self, BuildLatency, LatencyConfig, MIN_RUNS, Subject};
use crate::report::{
    BuildInfo, CorpusInfo, LatencySection, Results, SAMPLES_NOTE, SCHEMA, TokensSection,
    build_info, corpus_info, is_release_label, model, percentile_note, summary, tokenizer_info,
};
use crate::tokens::{self, FileResult, Prepared, TSV_HEADER};
use crate::util::{Ctx, Res};
use std::path::{Path, PathBuf};

pub struct Plan {
    pub label: String,
    pub corpora: Vec<Corpus>,
    /// Baseline first, candidate last.
    pub builds: Vec<Build>,
    pub tokens: bool,
    pub latency: Option<LatencyConfig>,
    /// Scratch space for the run; nothing in it is kept.
    pub work: PathBuf,
}

pub struct Output {
    pub results: Results,
    /// The per-file token table, all builds: deterministic, so it can be compared byte for byte.
    /// `None` when the run measured no tokens.
    pub tsv: Option<String>,
    pub summary: String,
}

fn token_pass(
    build: &Build,
    corpora: &[Corpus],
    prepared: &[Vec<Prepared>],
    work: &Path,
) -> Res<(Vec<FileResult>, String)> {
    let mut files = Vec::new();
    let mut version: Option<String> = None;
    for (c, prep) in corpora.iter().zip(prepared) {
        let (f, v) = tokens::measure(build, c, prep, &work.join(&build.label))?;
        match &version {
            Some(seen) if *seen != v => {
                return Err(format!(
                    "{} reported version {seen} on one corpus and {v} on another",
                    build.label
                ));
            }
            Some(_) => {}
            None => version = Some(v),
        }
        files.extend(f);
    }
    let version = version.ok_or("no corpus to measure")?;
    Ok((files, version))
}

/// Every token figure must come out the same whichever way the run is invoked, so a build that
/// fails the harness's own cross-checks stops the run rather than being reported.
fn require_first_read_is_the_outline(build: &Build, files: &[FileResult]) -> Res<()> {
    let bad: Vec<&str> = files
        .iter()
        .filter(|f| !f.first_matches_off)
        .map(|f| f.path.as_str())
        .take(3)
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{}: with repeat reads on, the first read of {} is not the same text as with them off; \
             the stub and delta figures would not be comparable to the outline figures",
            build.label,
            bad.join(", ")
        ))
    }
}

/// Scratch space starts empty: a ledger left by an earlier run would be counted as this run's rows.
fn fresh_scratch(work: &Path) -> Res<()> {
    if work.exists() {
        std::fs::remove_dir_all(work).ctx(&format!("clear the scratch {}", work.display()))?;
    }
    std::fs::create_dir_all(work).ctx(&format!("create {}", work.display()))
}

/// Each build with the way it learns whose call it is, found by running it (see
/// `driver::probe_identity`): what a build does is asked of the build, not read from its usage
/// text.
pub fn probe_builds(builds: &[Build], work: &Path) -> Res<Vec<Build>> {
    let mut probed: Vec<Build> = Vec::with_capacity(builds.len());
    for b in builds {
        let mut b = b.clone();
        b.identity = probe_identity(&b.bin, &work.join(format!("probe-{}", b.label)))?;
        eprintln!(
            "lumen-bench: {}: per-call identity by {}",
            b.label,
            b.identity.label()
        );
        probed.push(b);
    }
    Ok(probed)
}

/// The latency series alone, for `compare`: the same builds, subjects and passes as a recorded
/// run, and nothing written. The plan's `tokens` and `label` are not used.
pub fn latency_only(plan: &Plan) -> Res<(Vec<Subject>, Vec<BuildLatency>)> {
    let cfg = plan.latency.ok_or("no latency configuration")?;
    let first = plan.corpora.first().ok_or("no corpus to measure")?;
    if plan.builds.is_empty() {
        return Err("no build to measure".to_string());
    }
    fresh_scratch(&plan.work)?;
    let probed = probe_builds(&plan.builds, &plan.work)?;
    let prepared = tokens::prepare(first)?;
    let subjects = latency::pick_subjects(&first.root, &prepared);
    if subjects.is_empty() {
        return Err(format!(
            "{}: no Rust file with a named item and an edit to time on",
            first.name
        ));
    }
    let (passes, per_pass) = latency::plan_passes(&cfg);
    eprintln!(
        "lumen-bench: latency, {} builds, {passes} passes of {per_pass} runs",
        probed.len()
    );
    let builds = latency::measure_builds(&probed, &subjects, &cfg, &plan.work)?;
    if let Err(e) = std::fs::remove_dir_all(&plan.work) {
        eprintln!(
            "lumen-bench: could not remove the scratch {}: {e}",
            plan.work.display()
        );
    }
    Ok((subjects, builds))
}

pub fn execute(plan: &Plan) -> Res<Output> {
    if plan.builds.is_empty() {
        return Err("no build to measure".to_string());
    }
    if plan.corpora.is_empty() {
        return Err("no corpus to measure".to_string());
    }
    fresh_scratch(&plan.work)?;
    let mut environment = env::probe();

    // What each build does about per-call identity, found by asking it (see
    // `driver::probe_identity`): the token and latency passes feed a build the identity it reads.
    let probed = probe_builds(&plan.builds, &plan.work)?;
    let prepared: Vec<Vec<Prepared>> = plan
        .corpora
        .iter()
        .map(tokens::prepare)
        .collect::<Res<Vec<_>>>()?;

    let vpt = lumen_core::econ::Econ::default().value_per_token();
    let mut tsv = String::from(TSV_HEADER);
    let mut infos: Vec<BuildInfo> = Vec::new();
    let mut versions: Vec<String> = Vec::new();
    for build in &probed {
        if plan.tokens {
            let (files, version) = token_pass(build, &plan.corpora, &prepared, &plan.work)?;
            require_first_read_is_the_outline(build, &files)?;
            let table = tokens::tsv(&build.label, &files);
            let section = TokensSection {
                digest: tokens::digest(&files),
                files: files.len(),
                aggregates: tokens::aggregate(&files, vpt),
            };
            tsv.push_str(&table);
            infos.push(build_info(build, &version, Some(section)));
            versions.push(version);
        } else {
            infos.push(build_info(build, "not measured", None));
            versions.push(String::new());
        }
    }

    let latency = match plan.latency {
        None => None,
        Some(cfg) => {
            let first = plan.corpora.first().ok_or("no corpus")?;
            let subjects = latency::pick_subjects(&first.root, &prepared[0]);
            if subjects.is_empty() {
                return Err(format!(
                    "{}: no Rust file with a named item and an edit to time on",
                    first.name
                ));
            }
            let (passes, per_pass) = latency::plan_passes(&cfg);
            eprintln!(
                "lumen-bench: latency, {} builds, {passes} passes of {per_pass} runs",
                probed.len()
            );
            let builds = latency::measure_builds(&probed, &subjects, &cfg, &plan.work)?;
            for (info, l) in infos.iter_mut().zip(&builds) {
                if !plan.tokens {
                    info.reported_version.clone_from(&l.reported_version);
                } else if info.reported_version != l.reported_version {
                    return Err(format!(
                        "{} reported version {} for tokens and {} for latency",
                        info.label, info.reported_version, l.reported_version
                    ));
                }
            }
            Some(LatencySection {
                runs: passes * per_pass,
                passes,
                runs_per_pass: per_pass,
                warmup_runs: cfg.warmup,
                minimum_runs: MIN_RUNS,
                clock: "std::time::Instant, a monotonic clock, nanosecond resolution".to_string(),
                warmup_note: "per series per pass, run the same way as the recorded runs, so \
                              caches, page tables and the ledger file are warm; hook spawns are \
                              warm-start, not first launch after boot"
                    .to_string(),
                percentile_method: percentile_note(passes * per_pass),
                samples_note: SAMPLES_NOTE.to_string(),
                subjects,
                builds,
            })
        }
    };

    environment.load_at_end = env::load_now(std::env::consts::OS).ok();
    let corpus: Vec<CorpusInfo> = plan.corpora.iter().map(corpus_info).collect();
    let results = Results {
        schema: SCHEMA,
        label: plan.label.clone(),
        pre_release: !is_release_label(&plan.label),
        tokenizer: tokenizer_info(),
        corpus,
        builds: infos,
        latency,
        model: model(),
        environment,
    };
    let summary = summary(&results);
    // Nothing in the scratch space is kept; say so if it cannot be removed.
    if let Err(e) = std::fs::remove_dir_all(&plan.work) {
        eprintln!(
            "lumen-bench: could not remove the scratch {}: {e}",
            plan.work.display()
        );
    }
    Ok(Output {
        results,
        tsv: plan.tokens.then_some(tsv),
        summary,
    })
}

/// Write `<label>-<os>.json`, `<label>-tokens.tsv` and `<label>-summary.md` into `out`.
pub fn write_outputs(out: &Path, o: &Output) -> Res<Vec<PathBuf>> {
    std::fs::create_dir_all(out).ctx(&format!("create {}", out.display()))?;
    let label = &o.results.label;
    let json_path = out.join(format!("{label}-{}.json", o.results.environment.os));
    let mut json = serde_json::to_string_pretty(&o.results).ctx("serialise the results")?;
    json.push('\n');
    let mut written = vec![json_path.clone()];
    std::fs::write(&json_path, json).ctx(&format!("write {}", json_path.display()))?;
    let summary_path = out.join(format!("{label}-summary.md"));
    std::fs::write(&summary_path, &o.summary).ctx(&format!("write {}", summary_path.display()))?;
    written.push(summary_path);
    if let Some(tsv) = &o.tsv {
        let tsv_path = out.join(format!("{label}-tokens.tsv"));
        std::fs::write(&tsv_path, tsv).ctx(&format!("write {}", tsv_path.display()))?;
        written.push(tsv_path);
    }
    Ok(written)
}
