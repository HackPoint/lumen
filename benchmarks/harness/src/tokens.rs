//! Token figures: what each way of reading a file costs, per file, measured through the real
//! `lumen-mcp` binary and counted with the same tokenizer as the meter.
//!
//! Five modes per file:
//!
//! | mode    | what is called                                                                  |
//! |---------|---------------------------------------------------------------------------------|
//! | full    | nothing: the file's own tokens (what a plain Read would put in context)         |
//! | outline | `smart_read(path)`, first read, repeat reads off (`LUMEN_REPEAT_READS=off`)     |
//! | recall  | `recall_file(path, names=[representative item])`, repeat reads off              |
//! | stub    | the second identical `smart_read(path)` with `LUMEN_REPEAT_READS=on`            |
//! | delta   | `smart_read(path)` a third time, after one small deterministic edit, flag on    |
//!
//! Tool-result tokens are the reply text only. The JSON-RPC envelope and `_meta` are not
//! counted, because the meter does not count them; real cost is therefore a few dozen tokens
//! higher per call, in every mode alike.
//!
//! Every figure the server reports is cross-checked three ways: against the harness's own count
//! of the file and of the reply text, and against the row the server wrote to its ledger. A
//! disagreement stops the run: a benchmark that quietly takes one of two numbers is not one.

use crate::builds::Build;
use crate::corpus::{BUCKETS, Corpus, LANGS, SourceFile, bucket_of};
use crate::driver::{LedgerRow, Reply, Server, ledger_rows};
use crate::edit::{self, Edit, Rep};
use crate::sandbox::Sandbox;
use crate::stats::{Dist, dist};
use crate::util::{Ctx, Res, sha256_hex};
use lumen_core::tokenizer::count_tokens;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// A corpus file with everything that does not depend on the build under test. Prepared once
/// and given to both builds, so the two see byte-identical inputs and identical choices.
pub struct Prepared {
    pub file: SourceFile,
    pub src: String,
    pub full: i64,
    pub rep: Option<Rep>,
    pub edit: Option<Edit>,
}

pub fn prepare(c: &Corpus) -> Res<Vec<Prepared>> {
    let mut out = Vec::with_capacity(c.files.len());
    for f in &c.files {
        let bytes = std::fs::read(c.abs(&f.rel)).ctx(&format!("read {}", f.rel))?;
        let src = String::from_utf8(bytes).ctx(&format!("{} is not UTF-8", f.rel))?;
        if f.rel.contains('\t') || f.rel.contains('\n') {
            return Err(format!(
                "a path with a tab or newline cannot be tabulated: {:?}",
                f.rel
            ));
        }
        let items = edit::items(&src, f.lang);
        let rep = edit::representative(&items);
        let edit = rep.as_ref().and_then(|r| edit::small_edit(&src, f.lang, r));
        out.push(Prepared {
            file: f.clone(),
            full: count_tokens(&src) as i64,
            src,
            rep,
            edit,
        });
    }
    Ok(out)
}

/// One file, one build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResult {
    pub corpus: String,
    pub path: String,
    pub language: String,
    pub lines: usize,
    pub bucket: String,
    pub full: i64,
    pub outline: i64,
    pub outline_route: String,
    pub recall_item: Option<String>,
    pub recall: Option<i64>,
    pub recall_route: Option<String>,
    pub stub: i64,
    pub stub_route: String,
    pub delta_line: Option<usize>,
    pub delta: Option<i64>,
    pub delta_route: Option<String>,
    /// Whether the first read with repeat reads on was word for word the repeat-off outline.
    pub first_matches_off: bool,
    /// Why the stub and delta figures cannot exist for this build (before 1.7.0, or no Lumen):
    /// `n/a — <reason>`. `stub` and `delta` then hold nothing and are never read as a figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_na: Option<String>,
    /// A clamped version's own `saved_tokens` for the outline, as reported: recorded, never used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline_self_saved: Option<i64>,
}

fn check(p: &Prepared, r: &Reply, tool: &str) -> Res<()> {
    if r.full != p.full {
        return Err(format!(
            "{}: {tool} counted {} tokens for the file and the harness {}: \
             the build and the harness do not share a tokenizer",
            p.file.rel, r.full, p.full
        ));
    }
    let recount = count_tokens(&r.text) as i64;
    if recount != r.returned {
        return Err(format!(
            "{}: {tool} reported {} returned tokens but its text is {recount}",
            p.file.rel, r.returned
        ));
    }
    Ok(())
}

fn row_matches(p: &Prepared, row: &LedgerRow, r: &Reply, tool: &str) -> Res<()> {
    if !row.tool.ends_with(tool) || row.returned != r.returned || row.full != r.full {
        return Err(format!(
            "{}: the ledger row ({} {} returned, {} full) is not the {tool} reply ({} returned, {} full)",
            p.file.rel, row.tool, row.returned, row.full, r.returned, r.full
        ));
    }
    Ok(())
}

fn label(row: &LedgerRow) -> String {
    match &row.repeat_plan {
        Some(p) => format!("{}/{p}", row.routed_via),
        None => row.routed_via.clone(),
    }
}

struct Off {
    outline: Reply,
    outline_route: String,
    recall: Option<(Reply, String)>,
}

/// Repeat reads off: the outline and the recall as shipped.
fn off_pass(build: &Build, c: &Corpus, prep: &[Prepared], work: &Path) -> Res<(Vec<Off>, String)> {
    let (bin, identity, tag) = (build.bin.as_path(), build.identity, build.label.as_str());
    let iso = Sandbox::create(&work.join(format!("off-{}", c.name)))?;
    let session = format!("bench-off-{}", c.name);
    let mut s = Server::start(bin, &c.root, &iso, &session, false, identity)?;
    s.set_clamped(build.clamped());
    let version = s.version.clone();
    let mut replies: Vec<(Reply, Option<Reply>)> = Vec::with_capacity(prep.len());
    for (i, p) in prep.iter().enumerate() {
        let a = s.smart_read(&p.file.rel)?;
        check(p, &a, "smart_read")?;
        let b = match &p.rep {
            Some(rep) => {
                let b = s.recall(&p.file.rel, &[rep.name.as_str()])?;
                check(p, &b, "recall_file")?;
                Some(b)
            }
            None => None,
        };
        replies.push((a, b));
        if (i + 1) % 250 == 0 {
            eprintln!(
                "lumen-bench: {tag} {} (repeat off): {}/{}",
                c.name,
                i + 1,
                prep.len()
            );
        }
    }
    s.finish()?;
    let rows = ledger_rows(&iso.db)?;
    let expected: usize = replies
        .iter()
        .map(|(_, b)| 1 + usize::from(b.is_some()))
        .sum();
    if rows.len() != expected {
        return Err(format!(
            "{}: {expected} calls but {} ledger rows: a call went unmetered",
            c.name,
            rows.len()
        ));
    }
    let mut it = rows.iter();
    let mut out = Vec::with_capacity(prep.len());
    for (p, (a, b)) in prep.iter().zip(replies) {
        let ra = it.next().ok_or("ledger ran out of rows")?;
        row_matches(p, ra, &a, "smart_read")?;
        let recall = match b {
            Some(b) => {
                let rb = it.next().ok_or("ledger ran out of rows")?;
                row_matches(p, rb, &b, "recall_file")?;
                Some((b, label(rb)))
            }
            None => None,
        };
        out.push(Off {
            outline: a,
            outline_route: label(ra),
            recall,
        });
    }
    Ok((out, version))
}

struct On {
    first_matches_off: bool,
    stub: i64,
    stub_route: String,
    delta: Option<(i64, String)>,
}

/// Repeat reads on: first read, an identical second read, and (when an edit exists) a third read
/// of the edited file. Runs on a private copy of the files so the corpus cache is never edited.
fn on_pass(build: &Build, c: &Corpus, prep: &[Prepared], off: &[Off], work: &Path) -> Res<Vec<On>> {
    let (bin, identity, tag) = (build.bin.as_path(), build.identity, build.label.as_str());
    let base = work.join(format!("on-{}", c.name));
    let scratch = base.join("files");
    for p in prep {
        let dest = scratch.join(&p.file.rel);
        let parent = dest.parent().ok_or("a scratch file with no parent")?;
        std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
        std::fs::write(&dest, p.src.as_bytes()).ctx(&format!("write {}", dest.display()))?;
    }
    let iso = Sandbox::create(&base)?;
    let mut s = Server::start(
        bin,
        &scratch,
        &iso,
        &format!("bench-on-{}", c.name),
        true,
        identity,
    )?;
    s.set_clamped(build.clamped());
    let mut replies: Vec<(Reply, Reply, Option<Reply>)> = Vec::with_capacity(prep.len());
    for (i, p) in prep.iter().enumerate() {
        let first = s.smart_read(&p.file.rel)?;
        check(p, &first, "smart_read")?;
        let second = s.smart_read(&p.file.rel)?;
        check(p, &second, "smart_read")?;
        let third = match &p.edit {
            Some(e) => {
                let dest = scratch.join(&p.file.rel);
                std::fs::write(&dest, e.new_src.as_bytes())
                    .ctx(&format!("write {}", dest.display()))?;
                let t = s.smart_read(&p.file.rel)?;
                // The edited file is a different file: its own token count is the yardstick.
                if t.full != count_tokens(&e.new_src) as i64 {
                    return Err(format!(
                        "{}: the edited file was counted differently",
                        p.file.rel
                    ));
                }
                if count_tokens(&t.text) as i64 != t.returned {
                    return Err(format!(
                        "{}: the delta reply was counted differently",
                        p.file.rel
                    ));
                }
                Some(t)
            }
            None => None,
        };
        replies.push((first, second, third));
        if (i + 1) % 250 == 0 {
            eprintln!(
                "lumen-bench: {tag} {} (repeat on): {}/{}",
                c.name,
                i + 1,
                prep.len()
            );
        }
    }
    s.finish()?;
    let rows = ledger_rows(&iso.db)?;
    let expected: usize = replies
        .iter()
        .map(|(_, _, t)| 2 + usize::from(t.is_some()))
        .sum();
    if rows.len() != expected {
        return Err(format!(
            "{}: {expected} calls but {} ledger rows: a call went unmetered",
            c.name,
            rows.len()
        ));
    }
    let mut it = rows.iter();
    let mut out = Vec::with_capacity(prep.len());
    for ((p, o), (first, second, third)) in prep.iter().zip(off).zip(replies) {
        let r1 = it.next().ok_or("ledger ran out of rows")?;
        let r2 = it.next().ok_or("ledger ran out of rows")?;
        row_matches(p, r1, &first, "smart_read")?;
        row_matches(p, r2, &second, "smart_read")?;
        if r1.repeat_plan.as_deref() != Some("first") {
            return Err(format!(
                "{}: the first read was planned as {:?}, not \"first\": a base already existed",
                p.file.rel, r1.repeat_plan
            ));
        }
        let delta = match third {
            Some(t) => {
                let r3 = it.next().ok_or("ledger ran out of rows")?;
                if r3.returned != t.returned {
                    return Err(format!(
                        "{}: the delta ledger row disagrees with its reply",
                        p.file.rel
                    ));
                }
                Some((t.returned, label(r3)))
            }
            None => None,
        };
        out.push(On {
            first_matches_off: first.text == o.outline.text,
            stub: second.returned,
            stub_route: label(r2),
            delta,
        });
    }
    Ok(out)
}

/// The read cap of Claude Code's Read tool, in tokens: what a plain Read of a big file returns.
/// The engine's own constant, compared with cl100k counts as everywhere else in the harness.
pub const READ_CAP: i64 = lumen_core::savings::READ_TOKEN_LIMIT;

/// The "no Lumen" arm: every way of reading a file is a Read of it, capped. There is no server,
/// so there is no version, no route and no stub or delta to speak of.
pub fn no_lumen(c: &Corpus, prep: &[Prepared]) -> Vec<FileResult> {
    let why = "n/a — no Lumen: a file is read whole each time, there is no repeat-read gate";
    prep.iter()
        .map(|p| {
            let read = p.full.min(READ_CAP);
            let route = if p.full > READ_CAP {
                "no_lumen/read_capped"
            } else {
                "no_lumen/read"
            };
            FileResult {
                corpus: c.name.clone(),
                path: p.file.rel.clone(),
                language: p.file.lang.name().to_string(),
                lines: p.file.lines,
                bucket: BUCKETS[bucket_of(p.file.lines)].name.to_string(),
                full: p.full,
                outline: read,
                outline_route: route.to_string(),
                // To get one item without Lumen is to Read the file.
                recall_item: p.rep.as_ref().map(|r| r.name.clone()),
                recall: p.rep.as_ref().map(|_| read),
                recall_route: p.rep.as_ref().map(|_| route.to_string()),
                stub: 0,
                stub_route: String::new(),
                delta_line: None,
                delta: None,
                delta_route: None,
                first_matches_off: true,
                repeat_na: Some(why.to_string()),
                outline_self_saved: None,
            }
        })
        .collect()
}

/// Measure one build on one corpus. Returns the per-file results and the version the build says
/// it is. `work` is the build's own scratch directory.
///
/// What the capability map says the build cannot do is not asked of it: a version without repeat
/// reads has no stub or delta to measure, so those cells are `n/a — <reason>` and never a figure.
pub fn measure(
    build: &Build,
    c: &Corpus,
    prep: &[Prepared],
    work: &Path,
) -> Res<(Vec<FileResult>, String)> {
    if build.is_no_lumen() {
        return Ok((no_lumen(c, prep), "n/a — no Lumen".to_string()));
    }
    let clamped = build.clamped();
    let repeat_na = build.caps.as_ref().and_then(|k| k.na_repeat());
    let (off, version) = off_pass(build, c, prep, work)?;
    let on: Vec<On> = if repeat_na.is_some() {
        Vec::new()
    } else {
        on_pass(build, c, prep, &off, work)?
    };
    let mut on = on.into_iter();
    let mut out = Vec::with_capacity(prep.len());
    for (p, o) in prep.iter().zip(off) {
        let n = match &repeat_na {
            None => Some(on.next().ok_or("the repeat-read pass ran out of files")?),
            Some(_) => None,
        };
        let (recall, recall_route) = match o.recall {
            Some((r, route)) => (Some(r.returned), Some(route)),
            None => (None, None),
        };
        let (stub, stub_route, delta, delta_route, delta_line, first_matches_off) = match n {
            Some(n) => {
                let (delta, delta_route) = match n.delta {
                    Some((t, route)) => (Some(t), Some(route)),
                    None => (None, None),
                };
                (
                    n.stub,
                    n.stub_route,
                    delta,
                    delta_route,
                    p.edit.as_ref().map(|e| e.line),
                    n.first_matches_off,
                )
            }
            None => (0, String::new(), None, None, None, true),
        };
        out.push(FileResult {
            corpus: c.name.clone(),
            path: p.file.rel.clone(),
            language: p.file.lang.name().to_string(),
            lines: p.file.lines,
            bucket: BUCKETS[bucket_of(p.file.lines)].name.to_string(),
            full: p.full,
            outline: o.outline.returned,
            outline_route: o.outline_route,
            recall_item: p.rep.as_ref().map(|r| r.name.clone()),
            recall,
            recall_route,
            stub,
            stub_route,
            delta_line,
            delta,
            delta_route,
            first_matches_off,
            repeat_na: repeat_na.as_ref().map(|r| format!("n/a — {r}")),
            outline_self_saved: clamped.then_some(o.outline.saved_reported),
        });
    }
    Ok((out, version))
}

// ── aggregation ──────────────────────────────────────────────────────────────

/// One mode over a group of files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeStat {
    /// Tokens the call returned: files measured, median, p90.
    pub returned: Option<Dist>,
    /// Median over files of (file tokens - returned tokens). Negative means the call cost more
    /// than reading the file.
    pub saved_median: Option<i64>,
    /// How the server routed each call, from its own ledger.
    pub routes: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupStat {
    pub files: usize,
    pub full: Option<Dist>,
    pub outline: ModeStat,
    pub recall: ModeStat,
    pub stub: ModeStat,
    pub delta: ModeStat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Named {
    pub bucket: Option<String>,
    pub language: Option<String>,
    pub stat: GroupStat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aggregates {
    pub all: GroupStat,
    pub by_bucket: Vec<Named>,
    pub by_language: Vec<Named>,
    pub by_bucket_language: Vec<Named>,
}

fn mode_stat(files: &[&FileResult], pick: impl Fn(&FileResult) -> Option<(i64, &str)>) -> ModeStat {
    let mut returned = Vec::new();
    let mut saved = Vec::new();
    let mut routes = BTreeMap::new();
    for f in files {
        if let Some((t, route)) = pick(f) {
            returned.push(t);
            saved.push(f.full - t);
            *routes.entry(route.to_string()).or_insert(0) += 1;
        }
    }
    ModeStat {
        returned: dist(&returned),
        saved_median: dist(&saved).map(|d| d.median),
        routes,
    }
}

fn group(files: &[&FileResult]) -> GroupStat {
    let full: Vec<i64> = files.iter().map(|f| f.full).collect();
    GroupStat {
        files: files.len(),
        full: dist(&full),
        outline: mode_stat(files, |f| Some((f.outline, f.outline_route.as_str()))),
        recall: mode_stat(files, |f| f.recall.zip(f.recall_route.as_deref())),
        stub: mode_stat(files, |f| {
            f.repeat_na
                .is_none()
                .then_some((f.stub, f.stub_route.as_str()))
        }),
        delta: mode_stat(files, |f| {
            f.delta
                .zip(f.delta_route.as_deref())
                .filter(|_| f.repeat_na.is_none())
        }),
    }
}

pub fn aggregate(files: &[FileResult]) -> Aggregates {
    let all: Vec<&FileResult> = files.iter().collect();
    let sel = |b: Option<&str>, l: Option<&str>| -> Vec<&FileResult> {
        files
            .iter()
            .filter(|f| b.is_none_or(|b| f.bucket == b) && l.is_none_or(|l| f.language == l))
            .collect()
    };
    let mut by_bucket = Vec::new();
    let mut by_language = Vec::new();
    let mut by_bucket_language = Vec::new();
    for b in &BUCKETS {
        let fs = sel(Some(b.name), None);
        if !fs.is_empty() {
            by_bucket.push(Named {
                bucket: Some(b.name.to_string()),
                language: None,
                stat: group(&fs),
            });
        }
        for l in LANGS {
            let fs = sel(Some(b.name), Some(l.name()));
            if !fs.is_empty() {
                by_bucket_language.push(Named {
                    bucket: Some(b.name.to_string()),
                    language: Some(l.name().to_string()),
                    stat: group(&fs),
                });
            }
        }
    }
    for l in LANGS {
        let fs = sel(None, Some(l.name()));
        if !fs.is_empty() {
            by_language.push(Named {
                bucket: None,
                language: Some(l.name().to_string()),
                stat: group(&fs),
            });
        }
    }
    Aggregates {
        all: group(&all),
        by_bucket,
        by_language,
        by_bucket_language,
    }
}

/// The per-file table, one line per file, columns tab-separated. Deterministic: the same corpus
/// SHAs give the same bytes on every machine, so its digest is the "tokens exact" check.
pub fn tsv(build: &str, files: &[FileResult]) -> String {
    let opt = |v: Option<i64>| v.map_or("-".to_string(), |v| v.to_string());
    let mut s = String::new();
    for f in files {
        // A cell that cannot exist says so, in words: never a 0.
        if let Some(why) = &f.repeat_na {
            s.push_str(&format!(
                "{build}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tn/a\t{why}\t-\tn/a\t{why}\t{}\n",
                f.corpus,
                f.path,
                f.language,
                f.lines,
                f.bucket,
                f.full,
                f.outline,
                f.outline_route,
                f.recall_item.as_deref().unwrap_or("-"),
                opt(f.recall),
                f.recall_route.as_deref().unwrap_or("-"),
                f.first_matches_off,
            ));
            continue;
        }
        s.push_str(&format!(
            "{build}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            f.corpus,
            f.path,
            f.language,
            f.lines,
            f.bucket,
            f.full,
            f.outline,
            f.outline_route,
            f.recall_item.as_deref().unwrap_or("-"),
            opt(f.recall),
            f.recall_route.as_deref().unwrap_or("-"),
            f.stub,
            f.stub_route,
            f.delta_line.map_or("-".to_string(), |l| l.to_string()),
            opt(f.delta),
            f.delta_route.as_deref().unwrap_or("-"),
            f.first_matches_off,
        ));
    }
    s
}

pub const TSV_HEADER: &str = "build\tcorpus\tpath\tlanguage\tlines\tbucket\tfull\toutline\toutline_route\t\
recall_item\trecall\trecall_route\tstub\tstub_route\tdelta_line\tdelta\tdelta_route\tfirst_matches_off\n";

/// SHA-256 of one build's rows with the `build` column held at a fixed value, so two builds whose
/// token figures are identical have identical digests whatever they are called.
pub fn digest(files: &[FileResult]) -> String {
    sha256_hex(tsv("-", files).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(full: i64) -> FileResult {
        FileResult {
            corpus: "c".to_string(),
            path: "a.rs".to_string(),
            language: "rust".to_string(),
            lines: 400,
            bucket: "300-999".to_string(),
            full,
            outline: 5,
            outline_route: "smart_read".to_string(),
            recall_item: None,
            recall: None,
            recall_route: None,
            stub: 3,
            stub_route: "smart_read_unchanged/unchanged".to_string(),
            delta_line: None,
            delta: None,
            delta_route: None,
            first_matches_off: true,
            repeat_na: None,
            outline_self_saved: None,
        }
    }

    #[test]
    fn the_digest_follows_the_figures_and_not_the_build_label() {
        let a = [file(10)];
        let b = [file(10)];
        let c = [file(11)];
        assert_eq!(digest(&a), digest(&b));
        assert_ne!(digest(&a), digest(&c));
        assert_eq!(digest(&a), sha256_hex(tsv("-", &a).as_bytes()));
        assert_ne!(tsv("v1.7.0", &a), tsv("current", &a));
    }

    #[test]
    fn every_row_has_one_field_per_header_column() {
        let row = tsv("b", &[file(10)]);
        let columns = TSV_HEADER.trim_end().split('\t').count();
        assert_eq!(row.trim_end().split('\t').count(), columns);
    }

    #[test]
    fn a_cell_that_cannot_exist_is_written_as_n_a_and_never_as_a_figure() {
        let mut f = file(10);
        f.repeat_na = Some("n/a — this version has no repeat reads".to_string());
        f.stub = 0;
        f.stub_route = String::new();
        let row = tsv("1.0.0", &[f.clone()]);
        let cols: Vec<&str> = row.trim_end().split('\t').collect();
        assert_eq!(cols.len(), TSV_HEADER.trim_end().split('\t').count());
        let h: Vec<&str> = TSV_HEADER.trim_end().split('\t').collect();
        let at = |name: &str| cols[h.iter().position(|c| *c == name).unwrap()];
        assert_eq!(at("stub"), "n/a");
        assert_eq!(at("delta"), "n/a");
        assert!(at("stub_route").starts_with("n/a — "));
        assert!(at("delta_route").starts_with("n/a — "));
        // Neither enters an aggregate: no median over zeros.
        let agg = aggregate(&[f]);
        assert!(agg.all.stub.returned.is_none() && agg.all.stub.saved_median.is_none());
        assert!(agg.all.delta.returned.is_none());
        assert!(agg.all.outline.returned.is_some());
    }

    fn fixture_corpus(dir: &Path, big_fns: usize) -> Corpus {
        let mut big = String::new();
        for i in 0..big_fns {
            big.push_str(&format!(
                "pub fn step_{i:05}(x: i64) -> i64 {{\n    let a = x + {i};\n    let b = a * 2;\n    \
                 let c = b - {i};\n    let d = c / 3;\n    d + a\n}}\n\n"
            ));
        }
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/big.rs"), big).unwrap();
        std::fs::write(
            dir.join("src/small.rs"),
            "pub fn add(a: i64, b: i64) -> i64 {\n    a + b\n}\n",
        )
        .unwrap();
        crate::corpus::from_dir("fx", dir).unwrap()
    }

    #[test]
    fn no_lumen_reads_the_whole_file_with_the_read_cap_and_has_no_stub_or_delta() {
        let dir = tempfile::tempdir().unwrap();
        let c = fixture_corpus(dir.path(), 4000);
        let prep = prepare(&c).unwrap();
        let rows = no_lumen(&c, &prep);
        let big = rows.iter().find(|r| r.path == "src/big.rs").unwrap();
        let small = rows.iter().find(|r| r.path == "src/small.rs").unwrap();
        assert!(
            big.full > READ_CAP,
            "the fixture must exceed the cap: {}",
            big.full
        );
        assert_eq!(big.outline, READ_CAP);
        assert_eq!(big.outline_route, "no_lumen/read_capped");
        assert_eq!(small.outline, small.full);
        assert_eq!(small.outline_route, "no_lumen/read");
        for r in &rows {
            assert!(
                r.repeat_na
                    .as_deref()
                    .unwrap()
                    .starts_with("n/a — no Lumen")
            );
            assert_eq!(
                r.recall,
                Some(r.outline),
                "to get an item is to Read the file"
            );
        }
        // No server was started: measure() answers without a binary.
        let b = Build::no_lumen();
        let (m, v) = measure(&b, &c, &prep, dir.path()).unwrap();
        assert_eq!(m.len(), rows.len());
        assert!(v.starts_with("n/a"));
    }
}
