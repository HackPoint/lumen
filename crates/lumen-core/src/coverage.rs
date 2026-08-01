//! One answer to "would Lumen have handled this file?".
//!
//! Five places used to decide this independently, and they disagreed:
//!
//! | place | predicate |
//! | --- | --- |
//! | `.claude/hooks/lumen_read_intercept.sh` | `rs py pyi ts tsx` + `log out txt`, ≥300 lines |
//! | `INTERCEPT_TEMPLATE` in `setup.rs` | the same list, duplicated as a second source |
//! | [`crate::structure::detect_lang`] | `rs py pyi ts tsx` — no `mts`/`cts` |
//! | [`crate::ranked::TagLang::detect`] | `rs py pyi ts mts cts tsx` |
//! | `lumen-mcp`'s efficiency corpus | `TagLang::detect().is_some()` |
//!
//! The disagreement was not cosmetic. It let the published efficiency report claim
//! "64.9% of reads never reached a Lumen tool" — a figure computed by dividing every recorded
//! read of every file type by the total, so it measured the file mix of one machine rather than
//! anything about routing. On the honest denominator (an extension the hook intercepts, at or
//! above the threshold) the leak rate was **0 of 1,286**.
//!
//! [`Scope`] is the vocabulary that makes the difference expressible: a read that *should* have
//! been intercepted and was not is a defect, and a read of a PNG is not.

/// Extensions whose structure Lumen can outline.
///
/// `mts`/`cts` are here because [`crate::ranked::TagLang::detect`] has always accepted them
/// while [`crate::structure::detect_lang`] did not — so `smart_read` on a `.mts` file fell to
/// `Lang::Unknown`, produced a one-item whole-file "outline", and metered it as a ~95% saving
/// of a file it never looked inside.
///
/// Note this is deliberately **wider than the hooks' intercept list**: adding an extension here
/// makes the tools handle it correctly when called, which is a bug fix. Adding it to the hooks
/// would change *which reads get intercepted*, which is a coverage decision with its own cost.
pub const SOURCE_EXTS: &[&str] = &["rs", "py", "pyi", "ts", "mts", "cts", "tsx"];

/// Extensions routed to `compress_logs` rather than to an outline.
///
/// `output` is here because `out` was, and the two are the same kind of file. A build or test run
/// redirected to `something.output` was read whole — 54 recorded reads, 9,547 tokens at or above the
/// threshold — purely because the list stopped one word short. Extension matching is exact, so
/// `.output` never matched `out`.
pub const LOG_EXTS: &[&str] = &["log", "out", "output", "txt"];

/// Extensions the hooks actually intercept today.
///
/// Kept separate from [`SOURCE_EXTS`] so the asymmetry above is stated rather than implied, and
/// so the hook-drift test can assert against the list the hooks really carry.
pub const INTERCEPTED_SOURCE_EXTS: &[&str] = &["rs", "py", "pyi", "ts", "tsx"];

/// Below this many lines, interception does not fire. Overridden by `LUMEN_LINE_THRESHOLD`.
pub const DEFAULT_LINE_THRESHOLD: i64 = 300;

/// File extensions with no meaningful token count.
///
/// A built-in Read of one of these is not a missed optimization: there is no outline to return
/// and no saving that was available to miss. From 1.2.1 the meter labels them
/// `token_source = 'unsupported'`, but rows written earlier cannot be identified that way — a
/// failing tokenizer produced a bytes/4 estimate labelled `estimated`, indistinguishable from a
/// genuinely broken tokenizer on real source.
///
/// The distortion is not marginal. **Half of the missed-optimization token total — 3,439,369 of
/// 6,885,532 — comes from 125 binary reads out of 2,778**, and the largest rows in the whole
/// table are screenshots, whose bytes/4 estimate overstates a PNG by roughly 40×. Matching on
/// the extension corrects the figure for all history; rewriting those rows would mutate a ledger
/// that is otherwise append-only.
pub const UNMEASURABLE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "pdf", "tiff", "tif", "zip", "gz", "tar",
    "dmg", "woff", "woff2", "ttf", "so", "dylib", "o", "a",
];

/// Why a read was, or was not, a candidate for interception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// An extension the hooks intercept, at or above the threshold. The only bucket in which a
    /// `builtin_read` row is a **leak** — a read that should have been routed and was not.
    Optimizable,
    /// Under the line threshold. Passing these through is the threshold working.
    BelowThreshold,
    /// A binary or otherwise untokenizable file. There was no saving available to miss, and its
    /// `full_tokens` is a bytes/4 guess that must not enter any baseline.
    Unmeasurable,
    /// Big enough, but a kind Lumen does not handle. This is the coverage backlog: real tokens
    /// spent, nothing wrong with the router.
    UncoveredKind,
}

/// Lowercased extension, or `""` when there is none.
///
/// An extensionless file yields `""` rather than the whole basename — `"Makefile".rsplit('.')`
/// returns `"Makefile"`, which the shell hooks' `${path##*.}` also does, and which would make
/// `Makefile` look like an extension.
pub fn ext_of(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Classify a recorded read.
///
/// `lines` is the value the meter stored (`wc -l`), not a recount. The two differ by one on a
/// file with no trailing newline, and using the stored value keeps the ledger metric consistent
/// with what the hook actually decided at the time.
pub fn classify(path: &str, lines: Option<i64>) -> Scope {
    let ext = ext_of(path);
    if UNMEASURABLE_EXTS.contains(&ext.as_str()) {
        return Scope::Unmeasurable;
    }
    let intercepted =
        INTERCEPTED_SOURCE_EXTS.contains(&ext.as_str()) || LOG_EXTS.contains(&ext.as_str());
    // Unknown line count: cannot claim it was over the threshold, and calling an unknown a leak
    // would manufacture defects out of missing data.
    match lines {
        Some(n) if n >= DEFAULT_LINE_THRESHOLD => {
            if intercepted {
                Scope::Optimizable
            } else {
                Scope::UncoveredKind
            }
        }
        Some(_) => Scope::BelowThreshold,
        None => Scope::BelowThreshold,
    }
}

/// `AND lower(path) NOT LIKE '%.ext'` for every unmeasurable extension.
///
/// Built from compile-time literals, so nothing user-supplied reaches the SQL and an
/// `AssertSqlSafe` at the call site stays sound.
pub fn not_unmeasurable_sql() -> String {
    UNMEASURABLE_EXTS
        .iter()
        .map(|e| format!(" AND lower(path) NOT LIKE '%.{e}'"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercased_and_pathless() {
        assert_eq!(ext_of("/a/b/c.RS"), "rs");
        assert_eq!(ext_of("c.tsx"), "tsx");
        assert_eq!(ext_of(r"C:\src\a.PY"), "py");
    }

    #[test]
    fn an_extensionless_file_has_no_extension_rather_than_its_own_name() {
        // `${path##*.}` in the shell yields "Makefile" here, which would read as an extension.
        assert_eq!(ext_of("/a/Makefile"), "");
        assert_eq!(ext_of("Dockerfile"), "");
        assert_eq!(ext_of("/a/.gitignore"), "");
    }

    #[test]
    fn a_large_source_file_is_optimizable_and_is_the_only_leakable_kind() {
        assert_eq!(classify("a.rs", Some(400)), Scope::Optimizable);
        assert_eq!(classify("a.ts", Some(300)), Scope::Optimizable);
        assert_eq!(classify("a.log", Some(9000)), Scope::Optimizable);
    }

    #[test]
    fn exactly_at_the_threshold_counts_as_over_it() {
        // The hook tests `lines < THRESHOLD`, so 300 is intercepted. An off-by-one here would
        // report a leak on every file of exactly 300 lines.
        assert_eq!(classify("a.rs", Some(299)), Scope::BelowThreshold);
        assert_eq!(classify("a.rs", Some(300)), Scope::Optimizable);
    }

    #[test]
    fn a_small_file_is_below_threshold_whatever_its_kind() {
        assert_eq!(classify("a.rs", Some(10)), Scope::BelowThreshold);
        assert_eq!(classify("a.md", Some(10)), Scope::BelowThreshold);
    }

    #[test]
    fn binaries_are_unmeasurable_regardless_of_size() {
        // Checked before the threshold: a PNG's "line count" is meaningless, and its full_tokens
        // is a bytes/4 guess that inflated the baseline by 3.4M tokens.
        assert_eq!(classify("shot.png", Some(2039)), Scope::Unmeasurable);
        assert_eq!(classify("shot.png", Some(3)), Scope::Unmeasurable);
        assert_eq!(classify("a.dylib", None), Scope::Unmeasurable);
    }

    #[test]
    fn output_files_are_intercepted_like_every_other_log_kind() {
        // `.out` was in the list and `.output` was not, so a build or test run redirected to
        // `something.output` was read whole. Extension matching is exact — `.output` never matched
        // `out` — which is why one missing word cost 9,547 tokens across 54 recorded reads.
        assert_eq!(classify("build.output", Some(500)), Scope::Optimizable);
        assert_eq!(classify("/tmp/test.OUTPUT", Some(300)), Scope::Optimizable);
        // Still subject to the threshold like anything else.
        assert_eq!(classify("build.output", Some(10)), Scope::BelowThreshold);
    }

    #[test]
    fn the_log_kinds_are_the_ones_compress_logs_can_actually_help_with() {
        for e in LOG_EXTS {
            assert_eq!(
                classify(&format!("a.{e}"), Some(500)),
                Scope::Optimizable,
                "{e} is in LOG_EXTS but is not classified as optimizable"
            );
        }
        // A near-miss that must stay out: `.outputs` is not `.output`.
        assert_eq!(classify("a.outputs", Some(500)), Scope::UncoveredKind);
    }

    #[test]
    fn a_large_unhandled_kind_is_the_coverage_backlog_not_a_leak() {
        // md/scss/css/js at >=300 lines: real tokens spent, but nothing wrong with the router.
        for p in ["README.md", "a.scss", "a.css", "a.js", "a.yml"] {
            assert_eq!(classify(p, Some(500)), Scope::UncoveredKind, "for {p}");
        }
    }

    #[test]
    fn an_unknown_line_count_is_never_reported_as_a_leak() {
        // Missing data must not manufacture a defect.
        assert_eq!(classify("a.rs", None), Scope::BelowThreshold);
    }

    #[test]
    fn the_tool_extension_list_is_a_superset_of_what_the_hooks_intercept() {
        // Deliberate: the tools must handle mts/cts correctly when called directly, but adding
        // them to the hooks would change which reads get intercepted — a separate decision.
        for e in INTERCEPTED_SOURCE_EXTS {
            assert!(SOURCE_EXTS.contains(e), "{e} intercepted but not supported");
        }
        assert!(SOURCE_EXTS.contains(&"mts"));
        assert!(!INTERCEPTED_SOURCE_EXTS.contains(&"mts"));
    }

    #[test]
    fn the_exclusion_sql_is_built_only_from_literals() {
        let sql = not_unmeasurable_sql();
        assert!(sql.contains("NOT LIKE '%.png'"));
        assert_eq!(sql.matches("NOT LIKE").count(), UNMEASURABLE_EXTS.len());
        assert!(!sql.contains('?'), "no bind params: {sql}");
    }
}
