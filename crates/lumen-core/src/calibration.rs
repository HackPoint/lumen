//! How far Lumen's token counts are from Claude's.
//!
//! Lumen counts tokens with `cl100k_base` ([`crate::tokenizer`]), OpenAI's tokenizer, because
//! Claude's is not available offline. The count is deterministic, but it is a count in
//! `cl100k_base`'s units, and Claude's count of the same text is a different number. Until this
//! module the copy called the proxy "measured to the token" and "never estimated", which was
//! true of the proxy and said nothing about how far it was from what Claude counts.
//!
//! [`TABLE`] is that distance, measured by `benchmarks/calibration` on a pinned public corpus:
//! for each content type and Claude model, the ratio of Claude's count to `cl100k_base`'s
//! (median, 10th and 90th percentile over the samples). Claude's count there is the provider's
//! own usage data, taken as the difference between the same prompt sent with and without the
//! sample; `benchmarks/calibration/README.md` has the method and its validation. A figure built
//! on the proxy goes through [`calibrate`] to become an estimate of Claude's count with a band,
//! and says so. The samples are raw file text: what a `Read` costs also includes its line
//! numbers and the tool-result wrapper, which no row here covers.
//!
//! The gap is not small, and it depends on the generation. On every one of the 60 samples,
//! Claude Opus 4.7 and every later model measured counted 1.34 to 1.79 times as many tokens as
//! `cl100k_base` (1.59 at the median), and Opus 4.6, Sonnet 4.6, Opus 4.5 and Haiku 4.5 counted
//! 1.06 to 1.34 times as many (1.24 at the median). Within each of the two groups every model
//! counted every sample alike.
//!
//! Rows are keyed by model, not by family name: one family spans generations, and generations
//! tokenize differently. A model the table has no row for gets the row pooled over every model,
//! with a band that covers them all.
//!
//! Nothing calls this yet. The savings figures are still `cl100k_base` counts; wiring them
//! through here is the savings engine's change, and what the hooks and tools return is not
//! touched.

/// The day the last model in the table was measured. A later Claude Code or model can move
/// every row, so the table carries its date wherever it is quoted; [`MODELS`] has each model's.
pub const AS_OF: &str = "2026-10-14";

/// The Claude Code version whose usage data the table was measured through.
pub const CLAUDE_CODE_VERSION: &str = "2.1.296";

/// Every model measured, by base id, with the day it was measured.
pub const MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "2026-10-13"),
    ("claude-sonnet-5-5", "2026-10-13"),
    ("claude-haiku-5-5", "2026-10-13"),
    ("claude-fable-5", "2026-10-14"),
    ("claude-opus-5", "2026-10-14"),
    ("claude-sonnet-5", "2026-10-14"),
    ("claude-opus-4-8", "2026-10-14"),
    ("claude-opus-4-7", "2026-10-14"),
    ("claude-opus-4-6", "2026-10-14"),
    ("claude-sonnet-4-6", "2026-10-14"),
    ("claude-opus-4-5", "2026-10-14"),
    ("claude-haiku-4-5", "2026-10-14"),
];

/// What a file holds, as far as tokenization goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentType {
    Rust,
    Python,
    /// TypeScript and JavaScript, TSX and JSX included: one row, since the corpus measured
    /// them together.
    TsJs,
    Go,
    Shell,
    Json,
    /// Build and test output, and runtime logs.
    Log,
    /// Markdown, reStructuredText, AsciiDoc.
    Prose,
    /// Anything else. It has no row of its own, so it gets the pooled one with a wider band.
    Other,
}

impl ContentType {
    /// The content type of a file, by its extension.
    ///
    /// `.txt` is `Other`, not `Log` or `Prose`: [`crate::coverage::LOG_EXTS`] routes it to
    /// `compress_logs`, but a `.txt` file is as often notes as output, and a guess here would
    /// claim a narrower band than either row supports.
    pub fn from_path(path: &str) -> ContentType {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let ext = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
            _ => return ContentType::Other,
        };
        match ext.as_str() {
            "rs" => ContentType::Rust,
            "py" | "pyi" => ContentType::Python,
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => ContentType::TsJs,
            "go" => ContentType::Go,
            "sh" | "bash" | "zsh" => ContentType::Shell,
            "json" | "jsonl" | "ndjson" => ContentType::Json,
            "log" | "out" | "output" | "stderr" | "stdout" => ContentType::Log,
            "md" | "markdown" | "rst" | "adoc" => ContentType::Prose,
            _ => ContentType::Other,
        }
    }

    /// The name the benchmark's tables use.
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::Rust => "rust",
            ContentType::Python => "python",
            ContentType::TsJs => "ts_js",
            ContentType::Go => "go",
            ContentType::Shell => "shell",
            ContentType::Json => "json",
            ContentType::Log => "log",
            ContentType::Prose => "prose",
            ContentType::Other => "other",
        }
    }
}

/// A measured Claude model, by its base id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Model(&'static str);

/// The name the savings engine's seam uses for [`Model`]. It was a family (Opus, Sonnet,
/// Haiku) until the calibration covered older generations and found that they tokenize
/// differently, so a family name alone does not say which row applies.
pub type Family = Model;

impl Model {
    /// The measured model a model id names: `claude-opus-4-5-20251101` and
    /// `claude-opus-4-5[1m]` name `claude-opus-4-5`. `None` for any model the table has no row
    /// for, an alias such as `opus` included: matching the family word would hand an
    /// unmeasured generation another generation's band.
    pub fn from_model(model: &str) -> Option<Model> {
        let m = model.trim().to_ascii_lowercase();
        let m = m.strip_suffix("[1m]").unwrap_or(&m);
        // A dated release (`-20251101`) is the same model as its base id.
        let base = match m.rsplit_once('-') {
            Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
                head
            }
            _ => m,
        };
        MODELS
            .iter()
            .find(|(id, _)| *id == base)
            .map(|(id, _)| Model(id))
    }

    /// The base id, as the benchmark's tables write it.
    pub fn id(self) -> &'static str {
        self.0
    }
}

/// One cell of the table: Claude's tokens per `cl100k_base` token, in thousandths, over the
/// samples of one content type (`None`: all of them) sent to one model (`None`: every model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub content_type: Option<ContentType>,
    pub model: Option<Model>,
    /// How many distinct samples the ratios are taken over.
    pub n: u32,
    /// How many models each sample was sent to: a row over every model holds `n × models`
    /// ratios, but only `n` texts.
    pub models: u32,
    pub median: u32,
    pub p10: u32,
    pub p90: u32,
}

const fn row(
    content_type: Option<ContentType>,
    model: Option<Model>,
    n: u32,
    models: u32,
    median: u32,
    p10: u32,
    p90: u32,
) -> Row {
    Row {
        content_type,
        model,
        n,
        models,
        median,
        p10,
        p90,
    }
}

use ContentType::{Go, Json, Log, Prose, Python, Rust, Shell, TsJs};

/// A cell of one content type and one model.
const fn cell(t: ContentType, m: Model, n: u32, median: u32, p10: u32, p90: u32) -> Row {
    row(Some(t), Some(m), n, 1, median, p10, p90)
}

/// One model, pooled over every content type.
const fn pooled_for(m: Model, n: u32, median: u32, p10: u32, p90: u32) -> Row {
    row(None, Some(m), n, 1, median, p10, p90)
}

/// One content type, pooled over every model.
const fn all_models(t: ContentType, n: u32, models: u32, median: u32, p10: u32, p90: u32) -> Row {
    row(Some(t), None, n, models, median, p10, p90)
}

/// Every content type and every model.
const fn everything(n: u32, models: u32, median: u32, p10: u32, p90: u32) -> Row {
    row(None, None, n, models, median, p10, p90)
}

const OPUS_5_5: Model = Model("claude-opus-5-5");
const SONNET_5_5: Model = Model("claude-sonnet-5-5");
const HAIKU_5_5: Model = Model("claude-haiku-5-5");
const FABLE_5: Model = Model("claude-fable-5");
const OPUS_5: Model = Model("claude-opus-5");
const SONNET_5: Model = Model("claude-sonnet-5");
const OPUS_4_8: Model = Model("claude-opus-4-8");
const OPUS_4_7: Model = Model("claude-opus-4-7");
const OPUS_4_6: Model = Model("claude-opus-4-6");
const SONNET_4_6: Model = Model("claude-sonnet-4-6");
const OPUS_4_5: Model = Model("claude-opus-4-5");
const HAIKU_4_5: Model = Model("claude-haiku-4-5");

/// Measured by `benchmarks/calibration` (see `results/table.tsv` there, which a test holds this
/// to). Percentiles interpolate linearly between the closest ranks.
pub const TABLE: &[Row] = &[
    cell(Go, OPUS_5_5, 7, 1572, 1513, 1677),
    cell(Go, SONNET_5_5, 7, 1572, 1513, 1677),
    cell(Go, HAIKU_5_5, 7, 1572, 1513, 1677),
    cell(Go, FABLE_5, 7, 1572, 1513, 1677),
    cell(Go, OPUS_5, 7, 1572, 1513, 1677),
    cell(Go, SONNET_5, 7, 1572, 1513, 1677),
    cell(Go, OPUS_4_8, 7, 1572, 1513, 1677),
    cell(Go, OPUS_4_7, 7, 1572, 1513, 1677),
    cell(Go, OPUS_4_6, 7, 1252, 1182, 1283),
    cell(Go, SONNET_4_6, 7, 1252, 1182, 1283),
    cell(Go, OPUS_4_5, 7, 1252, 1182, 1283),
    cell(Go, HAIKU_4_5, 7, 1252, 1182, 1283),
    all_models(Go, 7, 12, 1519, 1239, 1671),
    cell(Json, OPUS_5_5, 8, 1473, 1439, 1626),
    cell(Json, SONNET_5_5, 8, 1473, 1439, 1626),
    cell(Json, HAIKU_5_5, 8, 1473, 1439, 1626),
    cell(Json, FABLE_5, 8, 1473, 1439, 1626),
    cell(Json, OPUS_5, 8, 1473, 1439, 1626),
    cell(Json, SONNET_5, 8, 1473, 1439, 1626),
    cell(Json, OPUS_4_8, 8, 1473, 1439, 1626),
    cell(Json, OPUS_4_7, 8, 1473, 1439, 1626),
    cell(Json, OPUS_4_6, 8, 1216, 1198, 1235),
    cell(Json, SONNET_4_6, 8, 1216, 1198, 1235),
    cell(Json, OPUS_4_5, 8, 1216, 1198, 1235),
    cell(Json, HAIKU_4_5, 8, 1216, 1198, 1235),
    all_models(Json, 8, 12, 1462, 1213, 1597),
    cell(Log, OPUS_5_5, 8, 1441, 1341, 1655),
    cell(Log, SONNET_5_5, 8, 1441, 1341, 1655),
    cell(Log, HAIKU_5_5, 8, 1441, 1341, 1655),
    cell(Log, FABLE_5, 7, 1444, 1358, 1659),
    cell(Log, OPUS_5, 8, 1441, 1341, 1655),
    cell(Log, SONNET_5, 8, 1441, 1341, 1655),
    cell(Log, OPUS_4_8, 8, 1441, 1341, 1655),
    cell(Log, OPUS_4_7, 8, 1441, 1341, 1655),
    cell(Log, OPUS_4_6, 8, 1160, 1091, 1315),
    cell(Log, SONNET_4_6, 8, 1160, 1091, 1315),
    cell(Log, OPUS_4_5, 8, 1160, 1091, 1315),
    cell(Log, HAIKU_4_5, 8, 1160, 1091, 1315),
    all_models(Log, 8, 12, 1369, 1111, 1645),
    cell(Prose, OPUS_5_5, 8, 1559, 1480, 1625),
    cell(Prose, SONNET_5_5, 8, 1559, 1480, 1625),
    cell(Prose, HAIKU_5_5, 8, 1559, 1480, 1625),
    cell(Prose, FABLE_5, 8, 1559, 1480, 1625),
    cell(Prose, OPUS_5, 8, 1559, 1480, 1625),
    cell(Prose, SONNET_5, 8, 1559, 1480, 1625),
    cell(Prose, OPUS_4_8, 8, 1559, 1480, 1625),
    cell(Prose, OPUS_4_7, 8, 1559, 1480, 1625),
    cell(Prose, OPUS_4_6, 8, 1165, 1127, 1231),
    cell(Prose, SONNET_4_6, 8, 1165, 1127, 1231),
    cell(Prose, OPUS_4_5, 8, 1165, 1127, 1231),
    cell(Prose, HAIKU_4_5, 8, 1165, 1127, 1231),
    all_models(Prose, 8, 12, 1510, 1145, 1616),
    cell(Python, OPUS_5_5, 7, 1658, 1564, 1694),
    cell(Python, SONNET_5_5, 7, 1658, 1564, 1694),
    cell(Python, HAIKU_5_5, 7, 1658, 1564, 1694),
    cell(Python, FABLE_5, 7, 1658, 1564, 1694),
    cell(Python, OPUS_5, 7, 1658, 1564, 1694),
    cell(Python, SONNET_5, 7, 1658, 1564, 1694),
    cell(Python, OPUS_4_8, 7, 1658, 1564, 1694),
    cell(Python, OPUS_4_7, 7, 1658, 1564, 1694),
    cell(Python, OPUS_4_6, 7, 1267, 1245, 1307),
    cell(Python, SONNET_4_6, 7, 1267, 1245, 1307),
    cell(Python, OPUS_4_5, 7, 1267, 1245, 1307),
    cell(Python, HAIKU_4_5, 7, 1267, 1245, 1307),
    all_models(Python, 7, 12, 1611, 1261, 1690),
    cell(Rust, OPUS_5_5, 8, 1587, 1487, 1628),
    cell(Rust, SONNET_5_5, 8, 1587, 1487, 1628),
    cell(Rust, HAIKU_5_5, 8, 1587, 1487, 1628),
    cell(Rust, FABLE_5, 8, 1587, 1487, 1628),
    cell(Rust, OPUS_5, 8, 1587, 1487, 1628),
    cell(Rust, SONNET_5, 8, 1587, 1487, 1628),
    cell(Rust, OPUS_4_8, 8, 1587, 1487, 1628),
    cell(Rust, OPUS_4_7, 8, 1587, 1487, 1628),
    cell(Rust, OPUS_4_6, 8, 1254, 1222, 1309),
    cell(Rust, SONNET_4_6, 8, 1254, 1222, 1309),
    cell(Rust, OPUS_4_5, 8, 1254, 1222, 1309),
    cell(Rust, HAIKU_4_5, 8, 1254, 1222, 1309),
    all_models(Rust, 8, 12, 1543, 1251, 1598),
    cell(Shell, OPUS_5_5, 6, 1700, 1610, 1785),
    cell(Shell, SONNET_5_5, 6, 1700, 1610, 1785),
    cell(Shell, HAIKU_5_5, 6, 1700, 1610, 1785),
    cell(Shell, FABLE_5, 6, 1700, 1610, 1785),
    cell(Shell, OPUS_5, 6, 1700, 1610, 1785),
    cell(Shell, SONNET_5, 6, 1700, 1610, 1785),
    cell(Shell, OPUS_4_8, 6, 1700, 1610, 1785),
    cell(Shell, OPUS_4_7, 6, 1700, 1610, 1785),
    cell(Shell, OPUS_4_6, 6, 1228, 1203, 1250),
    cell(Shell, SONNET_4_6, 6, 1228, 1203, 1250),
    cell(Shell, OPUS_4_5, 6, 1228, 1203, 1250),
    cell(Shell, HAIKU_4_5, 6, 1228, 1203, 1250),
    all_models(Shell, 6, 12, 1649, 1218, 1792),
    cell(TsJs, OPUS_5_5, 8, 1633, 1583, 1748),
    cell(TsJs, SONNET_5_5, 8, 1633, 1583, 1748),
    cell(TsJs, HAIKU_5_5, 8, 1633, 1583, 1748),
    cell(TsJs, FABLE_5, 8, 1633, 1583, 1748),
    cell(TsJs, OPUS_5, 8, 1633, 1583, 1748),
    cell(TsJs, SONNET_5, 8, 1633, 1583, 1748),
    cell(TsJs, OPUS_4_8, 8, 1633, 1583, 1748),
    cell(TsJs, OPUS_4_7, 8, 1633, 1583, 1748),
    cell(TsJs, OPUS_4_6, 8, 1304, 1249, 1328),
    cell(TsJs, SONNET_4_6, 8, 1304, 1249, 1328),
    cell(TsJs, OPUS_4_5, 8, 1304, 1249, 1328),
    cell(TsJs, HAIKU_4_5, 8, 1304, 1249, 1328),
    all_models(TsJs, 8, 12, 1606, 1290, 1743),
    pooled_for(OPUS_5_5, 60, 1592, 1444, 1699),
    pooled_for(SONNET_5_5, 60, 1592, 1444, 1699),
    pooled_for(HAIKU_5_5, 60, 1592, 1444, 1699),
    pooled_for(FABLE_5, 59, 1593, 1447, 1699),
    pooled_for(OPUS_5, 60, 1592, 1444, 1699),
    pooled_for(SONNET_5, 60, 1592, 1444, 1699),
    pooled_for(OPUS_4_8, 60, 1592, 1444, 1699),
    pooled_for(OPUS_4_7, 60, 1592, 1444, 1699),
    pooled_for(OPUS_4_6, 60, 1238, 1143, 1314),
    pooled_for(SONNET_4_6, 60, 1238, 1143, 1314),
    pooled_for(OPUS_4_5, 60, 1238, 1143, 1314),
    pooled_for(HAIKU_4_5, 60, 1238, 1143, 1314),
    everything(60, 12, 1503, 1213, 1686),
];

/// The row for one content type (`None`: pooled) and one model (`None`: every model).
pub fn row_for(content_type: Option<ContentType>, model: Option<Model>) -> Option<Row> {
    TABLE
        .iter()
        .find(|r| r.content_type == content_type && r.model == model)
        .copied()
}

/// Claude's count of some text, estimated from its `cl100k_base` count: `value` from the
/// median ratio, and `low..=high` from the 10th to the 90th percentile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub value: u64,
    pub low: u64,
    pub high: u64,
}

/// Claude's count of a text Lumen counted as `cl100k_tokens`.
///
/// A measured content type and model get their own cell. When one of them is not known, the
/// value comes from the row pooled over what is missing: the model's pooled row for an `Other`
/// file, the content type's row over every model for an unknown model, and the row pooled over
/// everything when neither is known. The band is then widened to cover every row the fallback
/// stands in for, so it never claims to know what it was not told.
pub fn calibrate(cl100k_tokens: u64, content_type: ContentType, model: Option<Model>) -> Estimate {
    let typed = Some(content_type).filter(|t| *t != ContentType::Other);
    let cell = typed
        .zip(model)
        .and_then(|(t, m)| row_for(Some(t), Some(m)));
    let (median, p10, p90) = match cell {
        Some(r) => (r.median, r.p10, r.p90),
        None => {
            let (pooled, covers): (Row, &dyn Fn(&Row) -> bool) = match (typed, model) {
                (_, Some(m)) => (
                    row_for(None, Some(m)).expect("every model has a pooled row"),
                    &move |r: &Row| r.model == Some(m),
                ),
                (Some(t), None) => (
                    row_for(Some(t), None).expect("every content type has a row over all models"),
                    &move |r: &Row| r.content_type == Some(t),
                ),
                (None, None) => (
                    row_for(None, None).expect("the table has a row pooled over everything"),
                    &|_: &Row| true,
                ),
            };
            let covered = TABLE.iter().filter(|r| covers(r));
            let p10 = covered.clone().map(|r| r.p10).fold(pooled.p10, u32::min);
            let p90 = covered.map(|r| r.p90).fold(pooled.p90, u32::max);
            (pooled.median, p10, p90)
        }
    };
    // In u128, so no count overflows on the way; a result past u64 is held at its maximum.
    let scale = |ratio: u32, round: fn(u128, u128) -> u128| {
        let milli = u128::from(cl100k_tokens) * u128::from(ratio);
        round(milli, 1000).min(u128::from(u64::MAX)) as u64
    };
    Estimate {
        value: scale(median, |a, b| (a + b / 2) / b),
        low: scale(p10, |a, b| a / b),
        high: scale(p90, |a, b| a.div_ceil(b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPES: [ContentType; 8] = [Rust, Python, TsJs, Go, Shell, Json, Log, Prose];

    fn measured() -> impl Iterator<Item = Model> {
        MODELS.iter().map(|(id, _)| Model::from_model(id).unwrap())
    }

    #[test]
    fn content_type_comes_from_the_extension() {
        for (path, t) in [
            ("crates/lumen-core/src/lib.rs", Rust),
            ("src/_pytest/python.py", Python),
            ("stubs/x.pyi", Python),
            ("src/form.tsx", TsJs),
            ("scripts/build.mjs", TsJs),
            ("a/b.JS", TsJs),
            ("src/bufio/bufio.go", Go),
            ("scripts/release.sh", Shell),
            ("package.json", Json),
            ("events.jsonl", Json),
            ("build.log", Log),
            ("tests/ui/x.stderr", Log),
            ("run.output", Log),
            ("README.md", Prose),
            ("doc/index.rst", Prose),
            (r"C:\work\notes.md", Prose),
            ("notes.txt", ContentType::Other),
            ("Makefile", ContentType::Other),
            (".bashrc", ContentType::Other),
            ("dir.json/file", ContentType::Other),
            ("image.png", ContentType::Other),
        ] {
            assert_eq!(ContentType::from_path(path), t, "{path}");
        }
    }

    #[test]
    fn only_a_measured_model_gets_its_own_rows() {
        for (id, _) in MODELS {
            assert_eq!(Model::from_model(id).map(Model::id), Some(*id));
            assert_eq!(
                Model::from_model(&format!("{id}[1m]")).map(Model::id),
                Some(*id)
            );
            assert_eq!(
                Model::from_model(&id.to_uppercase()).map(Model::id),
                Some(*id)
            );
        }
        // A dated release is its base model.
        assert_eq!(
            Family::from_model("claude-opus-4-5-20251101").map(Model::id),
            Some("claude-opus-4-5")
        );
        // Another generation of a measured family is not that family's row, and neither is an
        // alias, which names whatever the newest model is.
        for other in [
            "claude-3-haiku-20240307",
            "claude-3-5-sonnet-20241022",
            "claude-opus-4-1",
            "claude-sonnet-4-5",
            "opus",
            "sonnet",
            "gpt-4o",
            "",
        ] {
            assert_eq!(Model::from_model(other), None, "{other}");
        }
    }

    #[test]
    fn every_measured_cell_is_in_the_table_and_ordered() {
        for m in measured() {
            for t in TYPES {
                assert!(
                    row_for(Some(t), Some(m)).is_some(),
                    "no row for {t:?} x {m:?}"
                );
            }
            assert!(row_for(None, Some(m)).is_some(), "no pooled row for {m:?}");
        }
        for t in TYPES {
            assert!(
                row_for(Some(t), None).is_some(),
                "no all-model row for {t:?}"
            );
        }
        assert!(row_for(None, None).is_some());
        for r in TABLE {
            assert!(r.n > 0 && r.models > 0, "{r:?}");
            assert_eq!(r.models == 1, r.model.is_some(), "{r:?}");
            assert!(r.p10 <= r.median && r.median <= r.p90, "{r:?}");
            assert!(r.p10 > 0, "{r:?}");
        }
        assert!(
            !TABLE
                .iter()
                .any(|r| r.content_type == Some(ContentType::Other))
        );
        assert_eq!(row_for(None, None).unwrap().models as usize, MODELS.len());
    }

    #[test]
    fn a_measured_cell_scales_by_its_median_and_bands_by_its_percentiles() {
        for m in measured() {
            let r = row_for(Some(Rust), Some(m)).unwrap();
            let e = calibrate(10_000, Rust, Some(m));
            assert_eq!(e.value, 10 * u64::from(r.median));
            assert_eq!(e.low, 10 * u64::from(r.p10));
            assert_eq!(e.high, 10 * u64::from(r.p90));
        }
    }

    #[test]
    fn rounding_keeps_the_band_around_the_value() {
        for n in [1, 7, 333, 1_001] {
            for t in TYPES.into_iter().chain([ContentType::Other]) {
                for m in measured().map(Some).chain([None]) {
                    let e = calibrate(n, t, m);
                    assert!(
                        e.low <= e.value && e.value <= e.high,
                        "{n} {t:?} {m:?}: {e:?}"
                    );
                }
            }
        }
        let m = measured().next();
        assert_eq!(
            calibrate(0, Rust, m),
            Estimate {
                value: 0,
                low: 0,
                high: 0
            }
        );
        // No overflow at the top of the range.
        let e = calibrate(u64::MAX, Json, None);
        assert!(e.low <= e.value && e.value <= e.high);
    }

    #[test]
    fn an_unknown_type_gets_the_model_pooled_row_with_a_band_over_the_model() {
        for m in measured() {
            let pooled = row_for(None, Some(m)).unwrap();
            let rows: Vec<&Row> = TABLE.iter().filter(|r| r.model == Some(m)).collect();
            let e = calibrate(1_000, ContentType::Other, Some(m));
            assert_eq!(e.value, u64::from(pooled.median));
            assert_eq!(e.low, u64::from(rows.iter().map(|r| r.p10).min().unwrap()));
            assert_eq!(e.high, u64::from(rows.iter().map(|r| r.p90).max().unwrap()));
        }
    }

    #[test]
    fn an_unknown_model_keeps_the_type_and_widens_the_band_over_every_model() {
        for t in TYPES {
            let all = row_for(Some(t), None).unwrap();
            let e = calibrate(1_000, t, None);
            assert_eq!(e.value, u64::from(all.median), "{t:?}");
            for r in TABLE.iter().filter(|r| r.content_type == Some(t)) {
                assert!(
                    e.low <= u64::from(r.p10) && u64::from(r.p90) <= e.high,
                    "{r:?}"
                );
            }
        }
    }

    #[test]
    fn neither_known_gets_the_row_pooled_over_everything_and_a_band_over_every_row() {
        let pooled = row_for(None, None).unwrap();
        let e = calibrate(1_000, ContentType::Other, None);
        assert_eq!(e.value, u64::from(pooled.median));
        for r in TABLE {
            assert!(
                e.low <= u64::from(r.p10) && u64::from(r.p90) <= e.high,
                "{r:?}"
            );
        }
    }

    /// The table here is the benchmark's, row for row, and so are its dates and version: a
    /// re-run that moves a ratio fails this until the constants are updated with it.
    #[test]
    fn the_table_is_the_benchmarks() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/calibration/results");
        let tsv = std::fs::read_to_string(dir.join("table.tsv")).expect("table.tsv is committed");
        let mut rows = Vec::new();
        for line in tsv.lines().skip(1).filter(|l| !l.trim().is_empty()) {
            let c: Vec<&str> = line.split('\t').collect();
            let t = match c[0] {
                "pooled" => None,
                name => Some(
                    TYPES
                        .into_iter()
                        .find(|t| t.as_str() == name)
                        .unwrap_or_else(|| panic!("unknown content type {name}")),
                ),
            };
            let m = match c[1] {
                "all" => None,
                id => Some(Model::from_model(id).unwrap_or_else(|| panic!("unmeasured {id}"))),
            };
            let milli = |s: &str| (s.parse::<f64>().unwrap() * 1000.0).round() as u32;
            rows.push(row(
                t,
                m,
                c[2].parse().unwrap(),
                c[3].parse().unwrap(),
                milli(c[4]),
                milli(c[5]),
                milli(c[6]),
            ));
        }
        assert_eq!(TABLE, rows.as_slice());

        let run = std::fs::read_to_string(dir.join("run.json")).expect("run.json is committed");
        let run: serde_json::Value = serde_json::from_str(&run).unwrap();
        assert_eq!(run["as_of"], AS_OF);
        assert_eq!(run["claude_code"], CLAUDE_CODE_VERSION);
        let ran = run["models"].as_object().unwrap();
        assert_eq!(ran.len(), MODELS.len());
        for (id, day) in MODELS {
            assert_eq!(ran[*id]["as_of"], *day, "{id}");
        }
    }

    /// The two tokenizers the copy names. Every model from Opus 4.7 on counted every sample
    /// as `claude-opus-5-5` did, and every earlier one as `claude-opus-4-6` did, so the copy
    /// quotes those two models' pooled rows for the two groups.
    const LATER: &[&str] = &[
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable-5",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
    ];
    const EARLIER: &[&str] = &[
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-4-5",
        "claude-haiku-4-5",
    ];

    #[test]
    fn the_two_groups_the_copy_names_count_alike() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/calibration/results");
        let tsv = std::fs::read_to_string(dir.join("samples.tsv")).expect("samples.tsv");
        let mut lines = tsv.lines();
        let head: Vec<&str> = lines.next().unwrap().split('\t').collect();
        let col = |id: &str| {
            head.iter()
                .position(|h| *h == format!("claude:{id}"))
                .unwrap_or_else(|| panic!("no column for {id}"))
        };
        let rows: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
        assert_eq!(LATER.len() + EARLIER.len(), MODELS.len());
        for group in [LATER, EARLIER] {
            let first = col(group[0]);
            for id in &group[1..] {
                let other = col(id);
                for r in &rows {
                    // A sample a model would not answer (an empty cell) is not a disagreement.
                    if !r[first].is_empty() && !r[other].is_empty() {
                        assert_eq!(r[first], r[other], "{} on {}", id, r[0]);
                    }
                }
            }
        }
    }

    /// Wherever the copy quotes how far the proxy is from Claude, it quotes the two groups'
    /// pooled rows and the table's date, so a re-run that moves a ratio fails here until the
    /// copy moves with it.
    #[test]
    fn the_copy_quotes_the_pooled_rows() {
        let x = |milli: u32| format!("{:.2}", f64::from(milli) / 1000.0);
        let pooled = |id: &str| row_for(None, Model::from_model(id)).unwrap();
        let (later, earlier) = (pooled(LATER[0]), pooled(EARLIER[0]));
        let quotes = [
            format!(
                "about {}× as many tokens ({}–{}×)",
                x(later.median),
                x(later.p10),
                x(later.p90)
            ),
            format!(
                "earlier models about {}× as many ({}–{}×)",
                x(earlier.median),
                x(earlier.p10),
                x(earlier.p90)
            ),
            format!("calibration of {AS_OF}"),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (file, times) in [
            ("README.md", 2),
            ("docs/efficiency.md", 1),
            ("benchmarks/README.md", 1),
            ("lumenator/src/app/pages/optimizer/optimizer.ts", 1),
        ] {
            let text = std::fs::read_to_string(root.join(file)).expect(file);
            // One space for every run of whitespace, and adjacent TypeScript string
            // literals joined, so a sentence wrapped across lines still reads as one.
            let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            for joint in ["' + '", "' + \"", "\" + '", "\" + \""] {
                text = text.replace(joint, "");
            }
            for quote in &quotes {
                assert_eq!(
                    text.matches(quote.as_str()).count(),
                    times,
                    "{file} should quote {quote:?} {times} time(s)"
                );
            }
        }
    }
}
