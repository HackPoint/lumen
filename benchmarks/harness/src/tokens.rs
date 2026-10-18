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
use crate::driver::{Identity, LedgerRow, Reply, Server, ledger_rows};
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
fn off_pass(
    bin: &Path,
    identity: Identity,
    c: &Corpus,
    prep: &[Prepared],
    work: &Path,
    tag: &str,
) -> Res<(Vec<Off>, String)> {
    let iso = Sandbox::create(&work.join(format!("off-{}", c.name)))?;
    let session = format!("bench-off-{}", c.name);
    let mut s = Server::start(bin, &c.root, &iso, &session, false, identity)?;
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
fn on_pass(
    bin: &Path,
    identity: Identity,
    c: &Corpus,
    prep: &[Prepared],
    off: &[Off],
    work: &Path,
    tag: &str,
) -> Res<Vec<On>> {
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

/// Measure one build on one corpus. Returns the per-file results and the version the build says
/// it is. `work` is the build's own scratch directory.
pub fn measure(
    build: &Build,
    c: &Corpus,
    prep: &[Prepared],
    work: &Path,
) -> Res<(Vec<FileResult>, String)> {
    let (bin, identity, tag) = (build.bin.as_path(), build.identity, build.label.as_str());
    let (off, version) = off_pass(bin, identity, c, prep, work, tag)?;
    let on = on_pass(bin, identity, c, prep, &off, work, tag)?;
    let mut out = Vec::with_capacity(prep.len());
    for ((p, o), n) in prep.iter().zip(off).zip(on) {
        let (recall, recall_route) = match o.recall {
            Some((r, route)) => (Some(r.returned), Some(route)),
            None => (None, None),
        };
        let (delta, delta_route) = match n.delta {
            Some((t, route)) => (Some(t), Some(route)),
            None => (None, None),
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
            stub: n.stub,
            stub_route: n.stub_route,
            delta_line: p.edit.as_ref().map(|e| e.line),
            delta,
            delta_route,
            first_matches_off: n.first_matches_off,
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
        stub: mode_stat(files, |f| Some((f.stub, f.stub_route.as_str()))),
        delta: mode_stat(files, |f| f.delta.zip(f.delta_route.as_deref())),
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
}
