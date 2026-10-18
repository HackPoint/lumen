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
//! (median, 10th and 90th percentile, and the smallest and largest of any one sample). Claude's
//! count there is the provider's own usage data, taken as the difference between the same
//! prompt sent with and without the sample; `benchmarks/calibration/README.md` has the method
//! and its validation. A figure built on the proxy goes through [`calibrate`] (the percentiles,
//! what a typical sample came out at) or [`observed`] (the extremes, a band a floor can rest on)
//! to become an estimate of Claude's count with a band, and says so. The samples are raw file
//! text: an answer Lumen makes from a file, and what a `Read` costs beyond the file (its line
//! numbers and the tool-result wrapper), are in no row.
//!
//! The gap is not small, and it depends on the generation. On every one of the 60 samples,
//! Claude Opus 4.7 and every later model measured counted 1.34 to 1.79 times as many tokens as
//! `cl100k_base` (1.59 at the median), and Opus 4.6, Sonnet 4.6, Opus 4.5 and Haiku 4.5 counted
//! 1.06 to 1.34 times as many (1.24 at the median). Within each of the two groups every model
//! counted every sample alike.
//!
//! Rows are keyed by model, not by family name: one family spans generations, and generations
//! tokenize differently. The models fall into two [`TOKENIZERS`]. A model the table has no row
//! for gets the two tokenizers' rows weighted alike, with a band that covers them both: eight
//! models were measured on one and four on the other, and the table's rows over every model
//! weigh them by that count, which says how many models were sent the corpus and nothing about
//! which tokenizer an unmeasured model has.
//!
//! The savings engine ([`crate::savings`]) converts every count it shows through [`observed`],
//! by the file's content type and the model of the turn that read it: a saving is a difference
//! of two such counts, and its dollar floor takes the file at the smallest ratio its row
//! measured and Lumen's answer, whose own ratio no row measured, at the largest measured for the
//! model. What the hooks and the tools return, and what the ledger records, stay `cl100k_base`
//! counts: the calibration is for what is shown.

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
            Some((stem, ext)) if !stem.is_empty() => ext,
            _ => return ContentType::Other,
        };
        // Lowercased on the stack: the savings engine asks for every count it converts. No
        // extension below is longer than eight bytes, so a longer one is none of them.
        let mut buf = [0u8; 8];
        if ext.len() > buf.len() {
            return ContentType::Other;
        }
        for (b, c) in buf.iter_mut().zip(ext.bytes()) {
            *b = c.to_ascii_lowercase();
        }
        match &buf[..ext.len()] {
            b"rs" => ContentType::Rust,
            b"py" | b"pyi" => ContentType::Python,
            b"ts" | b"tsx" | b"mts" | b"cts" | b"js" | b"jsx" | b"mjs" | b"cjs" => {
                ContentType::TsJs
            }
            b"go" => ContentType::Go,
            b"sh" | b"bash" | b"zsh" => ContentType::Shell,
            b"json" | b"jsonl" | b"ndjson" => ContentType::Json,
            b"log" | b"out" | b"output" | b"stderr" | b"stdout" => ContentType::Log,
            b"md" | b"markdown" | b"rst" | b"adoc" => ContentType::Prose,
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
        // Compared ignoring ASCII case rather than lowercased, so nothing is allocated: the
        // savings engine asks for every count it converts.
        let m = model.trim();
        let m = match m.len().checked_sub(4) {
            Some(at) if m.is_char_boundary(at) && m[at..].eq_ignore_ascii_case("[1m]") => &m[..at],
            _ => m,
        };
        // A dated release (`-20251101`) is the same model as its base id.
        let base = match m.rsplit_once('-') {
            Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
                head
            }
            _ => m,
        };
        MODELS
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(base))
            .map(|(id, _)| Model(id))
    }

    /// The base id, as the benchmark's tables write it.
    pub fn id(self) -> &'static str {
        self.0
    }

    /// The tokenizer this model counted the corpus with.
    pub fn tokenizer(self) -> &'static Tokenizer {
        TOKENIZERS
            .iter()
            .find(|t| t.models.contains(&self))
            .expect("every measured model is in one tokenizer")
    }
}

/// Models that counted every sample of the corpus alike: one tokenizer, as far as the corpus
/// can tell (a test holds each group to `results/samples.tsv`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tokenizer {
    /// Stable, for the summary to key on.
    pub key: &'static str,
    /// The models measured on it, as the copy names them.
    pub label: &'static str,
    pub models: &'static [Model],
}

/// The two tokenizers the corpus found, newest first.
pub const TOKENIZERS: [Tokenizer; 2] = [
    Tokenizer {
        key: "opus47on",
        label: "Claude Opus 4.7, Opus 4.8, Opus 5, Sonnet 5, Fable 5, Opus 5.5, Sonnet 5.5 and \
                Haiku 5.5",
        models: &[
            OPUS_5_5, SONNET_5_5, HAIKU_5_5, FABLE_5, OPUS_5, SONNET_5, OPUS_4_8, OPUS_4_7,
        ],
    },
    Tokenizer {
        key: "opus46before",
        label: "Claude Opus 4.6, Sonnet 4.6, Opus 4.5 and Haiku 4.5",
        models: &[OPUS_4_6, SONNET_4_6, OPUS_4_5, HAIKU_4_5],
    },
];

impl Tokenizer {
    /// Its ratio for one content type (`None`: pooled over every type), in thousandths: the
    /// median of its models' rows, which count alike, so it is any one of them but for a sample
    /// one model would not answer.
    pub fn median(&self, content_type: Option<ContentType>) -> u32 {
        let mut m: Vec<u32> = self
            .models
            .iter()
            .filter_map(|&m| row_for(content_type, Some(m)))
            .map(|r| r.median)
            .collect();
        m.sort_unstable();
        m[(m.len() - 1) / 2]
    }

    /// Its typical band for one content type (`None`: pooled), in thousandths: the lowest 10th
    /// and the highest 90th percentile of its models' rows, as the copy quotes the spread. The
    /// savings figures take [`Tokenizer::range`] instead.
    pub fn band(&self, content_type: Option<ContentType>) -> (u32, u32) {
        let rows = || {
            self.models
                .iter()
                .filter_map(move |&m| row_for(content_type, Some(m)))
        };
        (
            rows()
                .map(|r| r.p10)
                .min()
                .expect("every tokenizer's models have rows"),
            rows()
                .map(|r| r.p90)
                .max()
                .expect("every tokenizer's models have rows"),
        )
    }
}

impl Tokenizer {
    /// The smallest and the largest ratio any sample came out at on its models, for one
    /// content type (`None`: pooled), in thousandths.
    pub fn range(&self, content_type: Option<ContentType>) -> (u32, u32) {
        let rows = || {
            self.models
                .iter()
                .filter_map(move |&m| row_for(content_type, Some(m)))
        };
        (
            rows()
                .map(|r| r.min)
                .min()
                .expect("every tokenizer's models have rows"),
            rows()
                .map(|r| r.max)
                .max()
                .expect("every tokenizer's models have rows"),
        )
    }
}

/// One content type's ratio (`None`: pooled) for a model the table has no row for, in
/// thousandths: the mean of the two tokenizers' medians, each weighted alike, since nothing
/// says which tokenizer such a model has.
pub fn across_tokenizers(content_type: Option<ContentType>) -> u32 {
    let sum: u32 = TOKENIZERS.iter().map(|t| t.median(content_type)).sum();
    let n = TOKENIZERS.len() as u32;
    (sum + n / 2) / n
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
    /// The smallest and the largest ratio of any one sample in the row.
    pub min: u32,
    pub max: u32,
}

/// A row's ratios in thousandths: median, p10, p90, and the smallest and largest of any sample.
type Stats = [u32; 5];

const fn row(
    content_type: Option<ContentType>,
    model: Option<Model>,
    n: u32,
    models: u32,
    [median, p10, p90, min, max]: Stats,
) -> Row {
    Row {
        content_type,
        model,
        n,
        models,
        median,
        p10,
        p90,
        min,
        max,
    }
}

use ContentType::{Go, Json, Log, Prose, Python, Rust, Shell, TsJs};

/// A cell of one content type and one model.
const fn cell(t: ContentType, m: Model, n: u32, stats: Stats) -> Row {
    row(Some(t), Some(m), n, 1, stats)
}

/// One model, pooled over every content type.
const fn pooled_for(m: Model, n: u32, stats: Stats) -> Row {
    row(None, Some(m), n, 1, stats)
}

/// One content type, pooled over every model.
const fn all_models(t: ContentType, n: u32, models: u32, stats: Stats) -> Row {
    row(Some(t), None, n, models, stats)
}

/// Every content type and every model.
const fn everything(n: u32, models: u32, stats: Stats) -> Row {
    row(None, None, n, models, stats)
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
/// to): median, p10, p90, and the smallest and largest per-sample ratio. Percentiles interpolate
/// linearly between the closest ranks.
pub const TABLE: &[Row] = &[
    cell(Go, OPUS_5_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, SONNET_5_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, HAIKU_5_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, FABLE_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, OPUS_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, SONNET_5, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, OPUS_4_8, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, OPUS_4_7, 7, [1572, 1513, 1677, 1503, 1685]),
    cell(Go, OPUS_4_6, 7, [1252, 1182, 1283, 1138, 1288]),
    cell(Go, SONNET_4_6, 7, [1252, 1182, 1283, 1138, 1288]),
    cell(Go, OPUS_4_5, 7, [1252, 1182, 1283, 1138, 1288]),
    cell(Go, HAIKU_4_5, 7, [1252, 1182, 1283, 1138, 1288]),
    all_models(Go, 7, 12, [1519, 1239, 1671, 1138, 1685]),
    cell(Json, OPUS_5_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, SONNET_5_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, HAIKU_5_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, FABLE_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, OPUS_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, SONNET_5, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, OPUS_4_8, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, OPUS_4_7, 8, [1473, 1439, 1626, 1391, 1693]),
    cell(Json, OPUS_4_6, 8, [1216, 1198, 1235, 1195, 1237]),
    cell(Json, SONNET_4_6, 8, [1216, 1198, 1235, 1195, 1237]),
    cell(Json, OPUS_4_5, 8, [1216, 1198, 1235, 1195, 1237]),
    cell(Json, HAIKU_4_5, 8, [1216, 1198, 1235, 1195, 1237]),
    all_models(Json, 8, 12, [1462, 1213, 1597, 1195, 1693]),
    cell(Log, OPUS_5_5, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, SONNET_5_5, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, HAIKU_5_5, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, FABLE_5, 7, [1444, 1358, 1659, 1343, 1678]),
    cell(Log, OPUS_5, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, SONNET_5, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, OPUS_4_8, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, OPUS_4_7, 8, [1441, 1341, 1655, 1335, 1678]),
    cell(Log, OPUS_4_6, 8, [1160, 1091, 1315, 1056, 1328]),
    cell(Log, SONNET_4_6, 8, [1160, 1091, 1315, 1056, 1328]),
    cell(Log, OPUS_4_5, 8, [1160, 1091, 1315, 1056, 1328]),
    cell(Log, HAIKU_4_5, 8, [1160, 1091, 1315, 1056, 1328]),
    all_models(Log, 8, 12, [1369, 1111, 1645, 1056, 1678]),
    cell(Prose, OPUS_5_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, SONNET_5_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, HAIKU_5_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, FABLE_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, OPUS_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, SONNET_5, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, OPUS_4_8, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, OPUS_4_7, 8, [1559, 1480, 1625, 1448, 1646]),
    cell(Prose, OPUS_4_6, 8, [1165, 1127, 1231, 1124, 1233]),
    cell(Prose, SONNET_4_6, 8, [1165, 1127, 1231, 1124, 1233]),
    cell(Prose, OPUS_4_5, 8, [1165, 1127, 1231, 1124, 1233]),
    cell(Prose, HAIKU_4_5, 8, [1165, 1127, 1231, 1124, 1233]),
    all_models(Prose, 8, 12, [1510, 1145, 1616, 1124, 1646]),
    cell(Python, OPUS_5_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, SONNET_5_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, HAIKU_5_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, FABLE_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, OPUS_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, SONNET_5, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, OPUS_4_8, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, OPUS_4_7, 7, [1658, 1564, 1694, 1493, 1701]),
    cell(Python, OPUS_4_6, 7, [1267, 1245, 1307, 1227, 1322]),
    cell(Python, SONNET_4_6, 7, [1267, 1245, 1307, 1227, 1322]),
    cell(Python, OPUS_4_5, 7, [1267, 1245, 1307, 1227, 1322]),
    cell(Python, HAIKU_4_5, 7, [1267, 1245, 1307, 1227, 1322]),
    all_models(Python, 7, 12, [1611, 1261, 1690, 1227, 1701]),
    cell(Rust, OPUS_5_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, SONNET_5_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, HAIKU_5_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, FABLE_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, OPUS_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, SONNET_5, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, OPUS_4_8, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, OPUS_4_7, 8, [1587, 1487, 1628, 1435, 1699]),
    cell(Rust, OPUS_4_6, 8, [1254, 1222, 1309, 1181, 1323]),
    cell(Rust, SONNET_4_6, 8, [1254, 1222, 1309, 1181, 1323]),
    cell(Rust, OPUS_4_5, 8, [1254, 1222, 1309, 1181, 1323]),
    cell(Rust, HAIKU_4_5, 8, [1254, 1222, 1309, 1181, 1323]),
    all_models(Rust, 8, 12, [1543, 1251, 1598, 1181, 1699]),
    cell(Shell, OPUS_5_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, SONNET_5_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, HAIKU_5_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, FABLE_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, OPUS_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, SONNET_5, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, OPUS_4_8, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, OPUS_4_7, 6, [1700, 1610, 1785, 1571, 1794]),
    cell(Shell, OPUS_4_6, 6, [1228, 1203, 1250, 1189, 1254]),
    cell(Shell, SONNET_4_6, 6, [1228, 1203, 1250, 1189, 1254]),
    cell(Shell, OPUS_4_5, 6, [1228, 1203, 1250, 1189, 1254]),
    cell(Shell, HAIKU_4_5, 6, [1228, 1203, 1250, 1189, 1254]),
    all_models(Shell, 6, 12, [1649, 1218, 1792, 1189, 1794]),
    cell(TsJs, OPUS_5_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, SONNET_5_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, HAIKU_5_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, FABLE_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, OPUS_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, SONNET_5, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, OPUS_4_8, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, OPUS_4_7, 8, [1633, 1583, 1748, 1566, 1761]),
    cell(TsJs, OPUS_4_6, 8, [1304, 1249, 1328, 1241, 1344]),
    cell(TsJs, SONNET_4_6, 8, [1304, 1249, 1328, 1241, 1344]),
    cell(TsJs, OPUS_4_5, 8, [1304, 1249, 1328, 1241, 1344]),
    cell(TsJs, HAIKU_4_5, 8, [1304, 1249, 1328, 1241, 1344]),
    all_models(TsJs, 8, 12, [1606, 1290, 1743, 1241, 1761]),
    pooled_for(OPUS_5_5, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(SONNET_5_5, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(HAIKU_5_5, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(FABLE_5, 59, [1593, 1447, 1699, 1343, 1794]),
    pooled_for(OPUS_5, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(SONNET_5, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(OPUS_4_8, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(OPUS_4_7, 60, [1592, 1444, 1699, 1335, 1794]),
    pooled_for(OPUS_4_6, 60, [1238, 1143, 1314, 1056, 1344]),
    pooled_for(SONNET_4_6, 60, [1238, 1143, 1314, 1056, 1344]),
    pooled_for(OPUS_4_5, 60, [1238, 1143, 1314, 1056, 1344]),
    pooled_for(HAIKU_4_5, 60, [1238, 1143, 1314, 1056, 1344]),
    everything(60, 12, [1503, 1213, 1686, 1056, 1794]),
];

/// The row for one content type (`None`: pooled) and one model (`None`: every model).
pub fn row_for(content_type: Option<ContentType>, model: Option<Model>) -> Option<Row> {
    TABLE
        .iter()
        .find(|r| r.content_type == content_type && r.model == model)
        .copied()
}

/// Claude's count of some text, estimated from its `cl100k_base` count: `value` from the
/// median ratio, and `low..=high` from the band the function that made it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Estimate {
    pub value: u64,
    pub low: u64,
    pub high: u64,
}

/// `cl100k_tokens` at ratios in thousandths: the value rounded, the low end down and the high
/// end up. In u128, so no count overflows on the way; a result past u64 is held at its maximum.
fn scaled(cl100k_tokens: u64, value: u32, low: u32, high: u32) -> Estimate {
    let scale = |ratio: u32, round: fn(u128, u128) -> u128| {
        let milli = u128::from(cl100k_tokens) * u128::from(ratio);
        round(milli, 1000).min(u128::from(u64::MAX)) as u64
    };
    Estimate {
        value: scale(value, |a, b| (a + b / 2) / b),
        low: scale(low, |a, b| a / b),
        high: scale(high, |a, b| a.div_ceil(b)),
    }
}

/// Claude's count of a text Lumen counted as `cl100k_tokens`, with the 10th to 90th percentile
/// as its band: what a typical sample of the type came out at.
///
/// A measured content type and model get their own cell. When one of them is not known, the
/// value comes from what is known: the model's pooled row for an `Other` file; for an unknown
/// model, the content type's ratio on each of the two [`TOKENIZERS`], weighted alike
/// ([`across_tokenizers`]), and the same pooled over every type when neither is known. The band
/// is then widened to cover every row the fallback stands in for, so it never claims to know
/// what it was not told.
pub fn calibrate(cl100k_tokens: u64, content_type: ContentType, model: Option<Model>) -> Estimate {
    let r = ratio(content_type, model);
    scaled(cl100k_tokens, r.median, r.p10, r.p90)
}

/// What a count is a count of, as far as the calibration goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    /// A file's raw text, which the corpus is.
    File,
    /// An answer Lumen made from a file: an outline (names, signatures, line ranges), a slice
    /// with its header, a stub or a delta. No such text was in the corpus, so its ratio was
    /// never measured.
    Answer,
}

/// Claude's count of a text Lumen counted as `cl100k_tokens`, with the band a floor can rest
/// on: the smallest to the largest ratio any one sample came out at, not a percentile.
///
/// A file takes its own row's extremes (with [`calibrate`]'s fallbacks, widened the same way).
/// An answer takes the file's median, since it is made of the file's own names and text, and the
/// widest span measured for the model, over every content type (over every model where the model
/// is not known): what kind of text an answer is was never measured, so nothing narrower can be
/// claimed for it.
pub fn observed(
    cl100k_tokens: u64,
    content_type: ContentType,
    model: Option<Model>,
    text: Text,
) -> Estimate {
    let r = ratio(content_type, model);
    let (low, high) = match text {
        Text::File => (r.min, r.max),
        Text::Answer => {
            let widest = row_for(None, model).expect("every model, and none, has a pooled row");
            (widest.min.min(r.median), widest.max.max(r.median))
        }
    };
    scaled(cl100k_tokens, r.median, low, high)
}

/// The ratios a count is converted by, in thousandths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ratio {
    pub median: u32,
    pub p10: u32,
    pub p90: u32,
    /// The smallest and the largest ratio of any sample the row (or the rows a fallback stands
    /// in for) holds.
    pub min: u32,
    pub max: u32,
}

/// Every content type, [`ContentType::Other`] last.
const ALL_TYPES: [ContentType; 9] = [
    Rust,
    Python,
    TsJs,
    Go,
    Shell,
    Json,
    Log,
    Prose,
    ContentType::Other,
];

/// [`ratio`] for every content type and model (the last column: none), worked out once. The
/// savings engine converts every count it reads, and a fallback scans the whole table.
type Ratios = [[Ratio; MODELS.len() + 1]; ALL_TYPES.len()];

/// The ratios [`calibrate`] and [`observed`] convert a file by.
pub fn ratio(content_type: ContentType, model: Option<Model>) -> Ratio {
    static RATIOS: std::sync::OnceLock<Ratios> = std::sync::OnceLock::new();
    let all = RATIOS.get_or_init(|| {
        let mut all = [[Ratio::default(); MODELS.len() + 1]; ALL_TYPES.len()];
        for (t, row) in ALL_TYPES.iter().zip(all.iter_mut()) {
            for (m, cell) in MODELS
                .iter()
                .map(|(id, _)| Some(Model(id)))
                .chain([None])
                .zip(row.iter_mut())
            {
                *cell = worked_out(*t, m);
            }
        }
        all
    });
    let t = ALL_TYPES
        .iter()
        .position(|t| *t == content_type)
        .expect("every content type is listed");
    let m = match model {
        Some(m) => MODELS
            .iter()
            .position(|(id, _)| *id == m.0)
            .expect("a Model is a measured model"),
        None => MODELS.len(),
    };
    all[t][m]
}

/// [`ratio`], from the table.
fn worked_out(content_type: ContentType, model: Option<Model>) -> Ratio {
    let typed = Some(content_type).filter(|t| *t != ContentType::Other);
    let cell = typed
        .zip(model)
        .and_then(|(t, m)| row_for(Some(t), Some(m)));
    match cell {
        Some(r) => Ratio {
            median: r.median,
            p10: r.p10,
            p90: r.p90,
            min: r.min,
            max: r.max,
        },
        None => {
            let (median, covers): (u32, &dyn Fn(&Row) -> bool) = match (typed, model) {
                (_, Some(m)) => (
                    row_for(None, Some(m))
                        .expect("every model has a pooled row")
                        .median,
                    &move |r: &Row| r.model == Some(m),
                ),
                (Some(t), None) => (across_tokenizers(Some(t)), &move |r: &Row| {
                    r.content_type == Some(t)
                }),
                (None, None) => (across_tokenizers(None), &|_: &Row| true),
            };
            let covered = || TABLE.iter().filter(|r| covers(r));
            Ratio {
                median,
                p10: covered().map(|r| r.p10).fold(median, u32::min),
                p90: covered().map(|r| r.p90).fold(median, u32::max),
                min: covered().map(|r| r.min).fold(median, u32::min),
                max: covered().map(|r| r.max).fold(median, u32::max),
            }
        }
    }
}

/// What [`calibrate`] ignores, as one sentence the surfaces quote: the corpus is raw file text.
pub const NOT_COVERED: &str = "The ratios are of raw file text: an answer Lumen makes from a \
     file (an outline, a slice, a stub), a Read's line numbers and the tool-result wrapper \
     around any answer, a Read's or Lumen's, are in no row.";

#[cfg(test)]
mod tests {
    use super::*;

    const TYPES: [ContentType; 8] = [Rust, Python, TsJs, Go, Shell, Json, Log, Prose];

    fn measured() -> impl Iterator<Item = Model> {
        MODELS.iter().map(|(id, _)| Model::from_model(id).unwrap())
    }

    #[test]
    fn the_ratios_worked_out_once_are_the_tables() {
        for t in ALL_TYPES {
            for m in measured().map(Some).chain([None]) {
                assert_eq!(ratio(t, m), worked_out(t, m), "{t:?} {m:?}");
            }
        }
        assert_eq!(ALL_TYPES.len(), TYPES.len() + 1);
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
            ("notes.MARKDOWN", Prose),
            ("x.markdowns", ContentType::Other),
            ("x.é", ContentType::Other),
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
        // A dated release is its base model, in any case, with or without the 1M context.
        assert_eq!(
            Family::from_model("claude-opus-4-5-20251101").map(Model::id),
            Some("claude-opus-4-5")
        );
        assert_eq!(
            Model::from_model(" Claude-Opus-4-5-20251101[1M] ").map(Model::id),
            Some("claude-opus-4-5")
        );
        assert_eq!(Model::from_model("[1m]"), None);
        assert_eq!(Model::from_model("é[1m]"), None);
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
            assert!(r.min <= r.p10 && r.p90 <= r.max, "{r:?}");
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

    /// The band a floor rests on: a file from its row's smallest to largest per-sample ratio,
    /// an answer Lumen made (never measured) from the smallest to the largest of any sample the
    /// model was sent, and of any sample at all where the model is not known. Both at the file's
    /// median.
    #[test]
    fn observed_bands_a_file_by_its_row_and_an_answer_by_the_widest_span_measured() {
        for m in measured().map(Some).chain([None]) {
            for t in TYPES.into_iter().chain([ContentType::Other]) {
                let r = ratio(t, m);
                let file = observed(1_000, t, m, Text::File);
                assert_eq!(
                    (file.value, file.low, file.high),
                    (u64::from(r.median), u64::from(r.min), u64::from(r.max)),
                    "{t:?} {m:?}"
                );
                let widest = row_for(None, m).unwrap();
                let answer = observed(1_000, t, m, Text::Answer);
                assert_eq!(answer.value, file.value, "{t:?} {m:?}");
                assert_eq!(
                    (answer.low, answer.high),
                    (u64::from(widest.min), u64::from(widest.max)),
                    "{t:?} {m:?}"
                );
                assert!(
                    answer.low <= file.low && file.high <= answer.high,
                    "{t:?} {m:?}"
                );
                // Wider than the percentiles calibrate() gives.
                let typical = calibrate(1_000, t, m);
                assert!(file.low <= typical.low && typical.high <= file.high);
            }
        }
        let sonnet = Model::from_model("claude-sonnet-4-6");
        let a = observed(1_000, Rust, sonnet, Text::Answer);
        assert_eq!((a.low, a.value, a.high), (1_056, 1_254, 1_344));
        let f = observed(10_000, Rust, sonnet, Text::File);
        assert_eq!((f.low, f.value, f.high), (11_810, 12_540, 13_230));
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
            let e = calibrate(1_000, t, None);
            assert_eq!(e.value, u64::from(across_tokenizers(Some(t))), "{t:?}");
            for r in TABLE.iter().filter(|r| r.content_type == Some(t)) {
                assert!(
                    e.low <= u64::from(r.p10) && u64::from(r.p90) <= e.high,
                    "{r:?}"
                );
            }
        }
    }

    #[test]
    fn neither_known_gets_both_tokenizers_alike_and_a_band_over_every_row() {
        let e = calibrate(1_000, ContentType::Other, None);
        assert_eq!(e.value, u64::from(across_tokenizers(None)));
        for r in TABLE {
            assert!(
                e.low <= u64::from(r.p10) && u64::from(r.p90) <= e.high,
                "{r:?}"
            );
        }
    }

    /// Eight models were measured on one tokenizer and four on the other. The rows over every
    /// model weigh them eight to four, which says how many models were sent the corpus, not
    /// which tokenizer a model the table does not know has: an unknown model takes the two
    /// tokenizers' ratios for the type, each weighted alike.
    #[test]
    fn an_unknown_model_weighs_the_two_tokenizers_alike() {
        let [later, earlier] = &TOKENIZERS;
        assert!(later.models.len() != earlier.models.len(), "premise");
        for t in TYPES.map(Some).into_iter().chain([None]) {
            let (a, b) = (later.median(t), earlier.median(t));
            let want = (u64::from(a) + u64::from(b)).div_ceil(2);
            let e = calibrate(1_000, t.unwrap_or(ContentType::Other), None);
            assert_eq!(e.value, want, "{t:?}: the mean of {a} and {b}");
            // Not the rows over every model, which lean toward the tokenizer with more models.
            let all = row_for(t, None).unwrap();
            assert_ne!(e.value, u64::from(all.median), "{t:?}");
            // The band spans both tokenizers.
            let ((lo_a, hi_a), (lo_b, hi_b)) = (later.band(t), earlier.band(t));
            assert!(e.low <= u64::from(lo_a.min(lo_b)), "{t:?}");
            assert!(e.high >= u64::from(hi_a.max(hi_b)), "{t:?}");
        }
        // A tokenizer's ratio is its models' own, whichever of them is asked.
        for g in &TOKENIZERS {
            for &m in g.models {
                assert_eq!(m.tokenizer(), g);
                assert_eq!(
                    row_for(Some(Rust), Some(m)).unwrap().median,
                    g.median(Some(Rust))
                );
            }
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
                [
                    milli(c[4]),
                    milli(c[5]),
                    milli(c[6]),
                    milli(c[7]),
                    milli(c[8]),
                ],
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

    #[test]
    fn each_tokenizer_counted_every_sample_alike() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/calibration/results");
        let tsv = std::fs::read_to_string(dir.join("samples.tsv")).expect("samples.tsv");
        let mut lines = tsv.lines();
        let head: Vec<&str> = lines.next().unwrap().split('\t').collect();
        let col = |m: &Model| {
            head.iter()
                .position(|h| *h == format!("claude:{}", m.id()))
                .unwrap_or_else(|| panic!("no column for {m:?}"))
        };
        let rows: Vec<Vec<&str>> = lines.map(|l| l.split('\t').collect()).collect();
        let grouped: usize = TOKENIZERS.iter().map(|t| t.models.len()).sum();
        assert_eq!(
            grouped,
            MODELS.len(),
            "every measured model is in one tokenizer"
        );
        for m in measured() {
            assert_eq!(
                TOKENIZERS.iter().filter(|t| t.models.contains(&m)).count(),
                1,
                "{m:?}"
            );
        }
        for group in &TOKENIZERS {
            let first = col(&group.models[0]);
            for m in &group.models[1..] {
                let other = col(m);
                for r in &rows {
                    // A sample a model would not answer (an empty cell) is not a disagreement.
                    if !r[first].is_empty() && !r[other].is_empty() {
                        assert_eq!(r[first], r[other], "{m:?} on {}", r[0]);
                    }
                }
            }
        }
        // And the two differ, or they would be one.
        let (a, b) = (col(&TOKENIZERS[0].models[0]), col(&TOKENIZERS[1].models[0]));
        assert!(rows.iter().any(|r| r[a] != r[b]));
    }

    /// A sample a model stopped on safety grounds has no count for that model (Claude Code
    /// answered with another one), so it is dropped for it. The runner aborts past one such
    /// sample per model and one per content type (`FALLBACKS_PER_MODEL`, `FALLBACKS_PER_TYPE` in
    /// `calibrate.py`): more would leave a row resting on fewer texts than its `n` says the
    /// corpus has, unseen. The committed run is held to the same caps here, and every sample
    /// is accounted for: counted, dropped for disagreeing, or fallen back.
    #[test]
    fn the_run_drops_no_more_safety_fallbacks_than_the_runner_allows() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/calibration");
        let script = std::fs::read_to_string(dir.join("calibrate.py")).expect("calibrate.py");
        let cap = |name: &str| -> usize {
            let line = script
                .lines()
                .find(|l| l.starts_with(&format!("{name} = ")))
                .unwrap_or_else(|| panic!("calibrate.py has no {name}"));
            line[name.len() + 3..].trim().parse().expect(name)
        };
        let (per_model, per_type) = (cap("FALLBACKS_PER_MODEL"), cap("FALLBACKS_PER_TYPE"));
        assert_eq!(
            (per_model, per_type),
            (1, 1),
            "the caps the table was published under"
        );

        let counts = std::fs::read_to_string(dir.join("results/counts.tsv")).expect("counts.tsv");
        let mut lines = counts.lines();
        let head: Vec<&str> = lines.next().unwrap().split('\t').collect();
        let (id, ty) = (
            head.iter().position(|h| *h == "id").unwrap(),
            head.iter().position(|h| *h == "content_type").unwrap(),
        );
        let type_of: std::collections::HashMap<&str, &str> = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let c: Vec<&str> = l.split('\t').collect();
                (c[id], c[ty])
            })
            .collect();

        let run = std::fs::read_to_string(dir.join("results/run.json")).expect("run.json");
        let run: serde_json::Value = serde_json::from_str(&run).unwrap();
        let mut by_type: std::collections::HashMap<&str, usize> = Default::default();
        let mut total = 0;
        for (model, m) in run["models"].as_object().unwrap() {
            let fell: Vec<&str> = m["fell_back_to_another_model"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert!(fell.len() <= per_model, "{model}: {fell:?}");
            let dropped = m["dropped"].as_array().unwrap().len();
            let samples = m["samples"].as_u64().unwrap() as usize;
            assert_eq!(
                samples + dropped + fell.len(),
                type_of.len(),
                "{model}: every sample counted, dropped or fallen back"
            );
            for sid in fell {
                let t = type_of
                    .get(sid)
                    .unwrap_or_else(|| panic!("{sid} is not in the corpus"));
                *by_type.entry(t).or_default() += 1;
                total += 1;
            }
        }
        for (t, n) in &by_type {
            assert!(*n <= per_type, "{t}: {n} fallbacks");
        }
        eprintln!("{total} safety fallback(s) in the run, by type {by_type:?}");
    }

    /// Wherever the copy quotes how far the proxy is from Claude, it names the models measured
    /// on each tokenizer and quotes that tokenizer's pooled row and the table's date, so a
    /// re-run that moves a ratio fails here until the copy moves with it. "Opus 4.7 and later"
    /// or "earlier models" alone would speak for models nobody measured.
    #[test]
    fn the_copy_names_the_measured_models_and_quotes_their_pooled_rows() {
        let x = |milli: u32| format!("{:.2}", f64::from(milli) / 1000.0);
        let named = |t: &Tokenizer| t.label.trim_start_matches("Claude ").to_string();
        let [later, earlier] = &TOKENIZERS;
        let (lo_l, hi_l) = later.band(None);
        let (lo_e, hi_e) = earlier.band(None);
        let quotes = [
            format!(
                "({}) count about {}× as many tokens ({}–{}×)",
                named(later),
                x(later.median(None)),
                x(lo_l),
                x(hi_l)
            ),
            format!(
                "the earlier models measured ({}) about {}× as many ({}–{}×)",
                named(earlier),
                x(earlier.median(None)),
                x(lo_e),
                x(hi_e)
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
            for unqualified in ["4.7 and later count", "and earlier models about"] {
                assert!(
                    !text.contains(unqualified),
                    "{file} says {unqualified:?}: name the models measured"
                );
            }
        }
    }
}
