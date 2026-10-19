//! C1: the Layer 1 corpus, recorded from pinned public sources.
//!
//! `lumen-bench competitors record` writes `benchmarks/competitors/corpus/`: one file per item
//! and a `manifest.toml` that lists every item with its category, its source (repository and
//! full commit SHA, or URL and sha256), its size, its sha256 and its needles. A needle is a fact
//! a later question needs (a definition line, a failing test, an error code); a tool that drops
//! one does not win the row.
//!
//! Nothing here runs a Lumen binary or a build, and nothing needs a sandbox: file reads, shell
//! listings and grep output come out of git's object store (`git ls-tree`, `git show`,
//! `git grep`, `git cat-file`) at a pinned commit, test and build logs are pinned blobs
//! (documentation pages quoting real pytest runs, and compiler test baselines of rustc and tsc),
//! and JSON is a pinned download. Every download is checked against a sha256 written in this
//! file before a byte is used, so a source that moves stops the record rather than changing the
//! corpus. Output is a function of the pins alone: two runs write identical bytes.
//!
//! The git commands run through `Sandbox` (scratch HOME, no user config, no pager, no colour) so
//! the machine cannot change a byte.

use crate::corpus::{BUCKETS, PINS, bucket_of};
use crate::sandbox::Sandbox;
use crate::util::{Ctx, Res, sha256_hex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The corpus may not grow past this, by the plan.
pub const MAX_TOTAL_BYTES: u64 = 5 * 1024 * 1024;

/// Substrings that must never appear in a recorded byte: a home directory, an employer.
/// Identity of the machine (user name, host name) is added by [`machine_identity`].
const FORBIDDEN: &[&str] = &["\\Users\\", "/var/folders", "C:/Users/"];
/// Absolute home directories. Matched only where the path starts (not after a name character),
/// so `src/app/pages/home/` is not mistaken for one.
const HOME_ROOTS: &[&str] = &["/Users/", "/home/", "/root/"];

/// A documentation example's own home directory is rewritten, not rejected: it is a placeholder
/// in a real pytest run, and the rewrite is recorded in the item's `normalised` field.
const NORMALISE: &[(&str, &str)] = &[("/home/sweet/", "/work/")];

// ------------------------------------------------------------------ manifest

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub corpus: Meta,
    #[serde(default)]
    pub item: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub schema: u32,
    pub note: String,
    pub items: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    /// file_read, shell, test_log, build_log, json, grep.
    pub category: String,
    /// For a file read, the size bucket by newline count (`<300`, `300-999`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    /// Path under the corpus directory.
    pub file: String,
    pub bytes: u64,
    pub lines: u64,
    pub sha256: String,
    /// `git` or `url`.
    pub source_kind: String,
    /// git: the repository URL; url: the download URL.
    pub source: String,
    /// git: the full 40-character commit SHA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// url: the sha256 of the download, checked before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    /// How the bytes were taken from the source.
    pub recipe: String,
    /// A rewrite applied to the bytes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalised: Option<String>,
    pub needles: Vec<String>,
}

// ------------------------------------------------------------------ the recipe

#[derive(Debug, Clone, Copy)]
enum Rule {
    /// First, middle and last substantial line.
    Positions,
    /// Definition lines (function, class, struct, interface) at a quarter, half, three quarters.
    Defs,
    /// First, middle and last line containing the text.
    Contains(&'static str),
    /// `"key": value` fragments, the first occurrence of each key.
    Fragments(&'static [&'static str]),
}

/// A git repository the record reads: a bare repository holding the pinned commit.
#[derive(Debug, Clone)]
pub struct Repo {
    pub name: String,
    pub url: String,
    pub sha: String,
    pub bare: PathBuf,
}

struct GitOut {
    id: &'static str,
    category: &'static str,
    repo: &'static str,
    /// Arguments after `git`, with `{sha}` standing for the commit.
    args: &'static [&'static str],
    needles: &'static [Rule],
}

struct BlobRange {
    id: &'static str,
    repo: &'static str,
    path: &'static str,
    /// 1-based, inclusive.
    start: usize,
    end: usize,
    /// Leading spaces removed from each line.
    dedent: usize,
    needles: &'static [Rule],
}

struct Download {
    id: &'static str,
    category: &'static str,
    url: String,
    sha256: &'static str,
    needles: &'static [Rule],
}

struct JsonBlob {
    id: &'static str,
    repo: &'static str,
    /// The largest `.json` blob (at most this many bytes) when `path` is empty.
    path: &'static str,
    needles: &'static [Rule],
}

pub struct Plan {
    file_read_repos: Vec<&'static str>,
    git_outs: Vec<GitOut>,
    blobs: Vec<BlobRange>,
    downloads: Vec<Download>,
    json_blobs: Vec<JsonBlob>,
}

const RUST_SHA: &str = "1159e78c4747b02ef996e55082b704c09b970588";
const BOOK_SHA: &str = "1500248d8f230566e4ec9f27fcbb8fe9e2898ab1";
const TS_SHA: &str = "c63de15a992d37f0d6cec03ac7631872838602cb";

pub fn real_plan() -> Plan {
    let p = &[Rule::Positions];
    Plan {
        file_read_repos: vec!["lumen", "react-hook-form", "pytest"],
        git_outs: vec![
            GitOut {
                id: "shell-ls-tree-lumen",
                category: "shell",
                repo: "lumen",
                args: &["ls-tree", "-r", "-l", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "shell-ls-tree-react-hook-form",
                category: "shell",
                repo: "react-hook-form",
                args: &["ls-tree", "-r", "-l", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "shell-ls-tree-pytest",
                category: "shell",
                repo: "pytest",
                args: &["ls-tree", "-r", "-l", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "shell-show-stat-lumen",
                category: "shell",
                repo: "lumen",
                args: &["show", "--stat=120,100", "--format=%H%n%s", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "shell-show-stat-react-hook-form",
                category: "shell",
                repo: "react-hook-form",
                args: &["show", "--stat=120,100", "--format=%H%n%s", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "shell-show-stat-pytest",
                category: "shell",
                repo: "pytest",
                args: &["show", "--stat=120,100", "--format=%H%n%s", "{sha}"],
                needles: p,
            },
            GitOut {
                id: "grep-lumen-pub-fn",
                category: "grep",
                repo: "lumen",
                args: &["grep", "-n", "-e", "pub fn", "{sha}", "--", "crates"],
                needles: p,
            },
            GitOut {
                id: "grep-lumen-pub-struct-context",
                category: "grep",
                repo: "lumen",
                args: &[
                    "grep",
                    "-n",
                    "-C",
                    "1",
                    "-e",
                    "pub struct",
                    "{sha}",
                    "--",
                    "crates",
                ],
                needles: p,
            },
            GitOut {
                id: "grep-react-hook-form-use-effect",
                category: "grep",
                repo: "react-hook-form",
                args: &["grep", "-n", "-e", "useEffect", "{sha}", "--", "src"],
                needles: p,
            },
            GitOut {
                id: "grep-react-hook-form-export-const",
                category: "grep",
                repo: "react-hook-form",
                args: &["grep", "-n", "-e", "export const", "{sha}", "--", "src"],
                needles: p,
            },
            GitOut {
                id: "grep-pytest-raise",
                category: "grep",
                repo: "pytest",
                args: &["grep", "-n", "-e", "raise ", "{sha}", "--", "src"],
                needles: p,
            },
            GitOut {
                id: "grep-pytest-fixture-docs",
                category: "grep",
                repo: "pytest",
                args: &[
                    "grep",
                    "-n",
                    "-i",
                    "-e",
                    "fixture",
                    "{sha}",
                    "--",
                    "doc/en/how-to",
                ],
                needles: p,
            },
            GitOut {
                id: "grep-pytest-count-def-test",
                category: "grep",
                repo: "pytest",
                args: &["grep", "-c", "-e", "def test_", "{sha}", "--", "testing"],
                needles: p,
            },
        ],
        blobs: vec![
            BlobRange {
                id: "testlog-pytest-failure-demo",
                repo: "pytest",
                path: "doc/en/example/reportingdemo.rst",
                start: 11,
                end: 724,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains(" failed in ")],
            },
            BlobRange {
                id: "testlog-pytest-verbosity-default",
                repo: "pytest",
                path: "doc/en/how-to/output.rst",
                start: 90,
                end: 136,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains("AssertionError")],
            },
            BlobRange {
                id: "testlog-pytest-verbosity-v",
                repo: "pytest",
                path: "doc/en/how-to/output.rst",
                start: 151,
                end: 208,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains("PASSED")],
            },
            BlobRange {
                id: "testlog-pytest-verbosity-vv",
                repo: "pytest",
                path: "doc/en/how-to/output.rst",
                start: 223,
                end: 338,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains("AssertionError")],
            },
            BlobRange {
                id: "testlog-pytest-report-chars-ra",
                repo: "pytest",
                path: "doc/en/how-to/output.rst",
                start: 430,
                end: 461,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains("ERROR ")],
            },
        ],
        downloads: vec![
            Download {
                id: "testlog-cargo-show-output",
                category: "test_log",
                url: raw_url(BOOK_SHA, "rust-lang/book", "listings/ch11-writing-automated-tests/output-only-01-show-output/output.txt"),
                sha256: "1738bcbe569e3b863cb22560e3f42ae552f9d8bb5f6eef3155e84cdb9b6512cf",
                needles: &[Rule::Contains("panicked at"), Rule::Contains("test tests::")],
            },
            Download {
                id: "testlog-cargo-should-panic-mismatch",
                category: "test_log",
                url: raw_url(BOOK_SHA, "rust-lang/book", "listings/ch11-writing-automated-tests/no-listing-09-guess-with-panic-msg-bug/output.txt"),
                sha256: "48ae84503684e8cf74a9fcf1bd405db84c085860c2fe66288682050dcf4dc069",
                needles: &[Rule::Contains("panic message"), Rule::Contains("test result: FAILED")],
            },
            Download {
                id: "testlog-cargo-assert-failure",
                category: "test_log",
                url: raw_url(BOOK_SHA, "rust-lang/book", "listings/ch11-writing-automated-tests/no-listing-03-introducing-a-bug/output.txt"),
                sha256: "8076c3a8cdaf9e886df5ed8555750a81ab6336831478a9259e2d9ee6c26ade0b",
                needles: &[Rule::Contains("panicked at"), Rule::Contains("test result: FAILED")],
            },
            Download {
                id: "buildlog-rustc-item-privacy",
                category: "build_log",
                url: raw_url(RUST_SHA, "rust-lang/rust", "tests/ui/traits/item-privacy.stderr"),
                sha256: "0ae2fae07a25d0c98170a6c286700bb5a13f9c886d0c8e789266813b10e4b021",
                needles: &[Rule::Contains("error[E"), Rule::Contains("error: aborting")],
            },
            Download {
                id: "buildlog-rustc-mismatched-lifetime-syntaxes",
                category: "build_log",
                url: raw_url(
                    RUST_SHA,
                    "rust-lang/rust",
                    "tests/ui/lifetimes/mismatched-lifetime-syntaxes.stderr",
                ),
                sha256: "ee1b8daa0f8e60a756e1a5dfb0f70afa394e8d1406d6a56776d8a8d168f988bd",
                needles: &[Rule::Contains("error: "), Rule::Contains("error: aborting")],
            },
            Download {
                id: "buildlog-rustc-recursion-hang",
                category: "build_log",
                url: raw_url(
                    RUST_SHA,
                    "rust-lang/rust",
                    "tests/ui/traits/issue-91949-hangs-on-recursion.stderr",
                ),
                sha256: "c833dc4bfb4a9d4437b8d11f3d26a7f607bde9fdad3a4bad50271499da5b584b",
                needles: &[Rule::Contains("error: "), Rule::Contains("error: aborting")],
            },
            Download {
                id: "buildlog-tsc-conditional-types",
                category: "build_log",
                url: raw_url(
                    TS_SHA,
                    "microsoft/TypeScript",
                    "tests/baselines/reference/conditionalTypes1.errors.txt",
                ),
                sha256: "d6016827385d720a2b08eb1862c1da14088aef86f094c6a9ddf8709d158c963f",
                needles: &[Rule::Contains("error TS")],
            },
            Download {
                id: "buildlog-tsc-mapped-type-errors",
                category: "build_log",
                url: raw_url(
                    TS_SHA,
                    "microsoft/TypeScript",
                    "tests/baselines/reference/mappedTypeErrors.errors.txt",
                ),
                sha256: "635021b5edb68b171d03c1efe26d1bdf20216fdf10fe9683e0ae65c393693e13",
                needles: &[Rule::Contains("error TS")],
            },
            Download {
                id: "buildlog-tsc-keyof-indexed-access",
                category: "build_log",
                url: raw_url(
                    TS_SHA,
                    "microsoft/TypeScript",
                    "tests/baselines/reference/keyofAndIndexedAccessErrors.errors.txt",
                ),
                sha256: "a926e2f774ac5b3ae9e80f4822e770d2e951050071d060d28bc9f1a8513653ce",
                needles: &[Rule::Contains("error TS")],
            },
            Download {
                id: "json-npm-react-hook-form-7.90.0",
                category: "json",
                url: "https://registry.npmjs.org/react-hook-form/7.90.0".to_string(),
                sha256: "bb3b76a54f2369771f29ed9e6168381bb78a2ee781d836b5b963ff51bf003a6d",
                needles: &[Rule::Fragments(&["\"name\"", "\"version\"", "\"shasum\"", "\"tarball\""])],
            },
            Download {
                id: "json-npm-typescript-5.9.3",
                category: "json",
                url: "https://registry.npmjs.org/typescript/5.9.3".to_string(),
                sha256: "80f0d5d237c08c433b4cee823d75c4f54469beee86cc2fad5e1800f86b6243d8",
                needles: &[Rule::Fragments(&["\"name\"", "\"version\"", "\"shasum\"", "\"tarball\""])],
            },
            Download {
                id: "json-npm-express-4.21.2",
                category: "json",
                url: "https://registry.npmjs.org/express/4.21.2".to_string(),
                sha256: "522a2879772019f3f69d6334e0a788d1cee07e1866862d0d36c79a2f29382f68",
                needles: &[Rule::Fragments(&["\"name\"", "\"version\"", "\"shasum\"", "\"tarball\""])],
            },
            Download {
                id: "json-github-commit-pytest",
                category: "json",
                url: "https://api.github.com/repos/pytest-dev/pytest/commits/cf470ec0bf7eb89cd97dd56df4859eae5db46447".to_string(),
                sha256: "be24114a9438212bb38189f2d5c13916390d60eecc3ee290af82fc7d2f14ae6a",
                needles: &[Rule::Fragments(&["\"sha\"", "\"message\"", "\"html_url\"", "\"total\""])],
            },
            Download {
                id: "json-pypi-pytest-9.1.1",
                category: "json",
                url: "https://pypi.org/pypi/pytest/9.1.1/json".to_string(),
                sha256: "8ed48304ba7fd0ae974a041f117487b499d553b40e5ce55a9ece677e946f68dd",
                needles: &[Rule::Fragments(&["\"version\"", "\"requires_python\"", "\"license\"", "\"name\""])],
            },
        ],
        json_blobs: vec![
            JsonBlob {
                id: "json-file-react-hook-form-package",
                repo: "react-hook-form",
                path: "package.json",
                needles: &[Rule::Fragments(&["\"name\"", "\"version\"", "\"main\""]), Rule::Positions],
            },
            JsonBlob {
                id: "json-file-lumen-largest",
                repo: "lumen",
                path: "",
                needles: &[Rule::Positions],
            },
            JsonBlob {
                id: "json-file-react-hook-form-largest",
                repo: "react-hook-form",
                path: "",
                needles: &[Rule::Positions],
            },
        ],
    }
}

fn raw_url(sha: &str, repo: &str, path: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/{sha}/{path}")
}

// ------------------------------------------------------------------ git access

/// One sandbox for every git child: a scratch HOME and temp dir, the caller's environment
/// otherwise dropped (no user config, no pager, no colour), so the machine cannot change a byte.
fn sandbox() -> &'static Sandbox {
    static SB: std::sync::OnceLock<Sandbox> = std::sync::OnceLock::new();
    SB.get_or_init(|| {
        let root = std::env::temp_dir().join("lumen-bench-record-sandbox");
        Sandbox::create(&root)
            .expect("create the record sandbox")
            .set("GIT_CONFIG_NOSYSTEM", "1")
            .set("GIT_CONFIG_GLOBAL", "/dev/null")
            .set("GIT_TERMINAL_PROMPT", "0")
            .set("GIT_PAGER", "cat")
            .set("LC_ALL", "C")
    })
}

fn git(bare: &Path) -> Command {
    let mut c = Command::new("git");
    sandbox().apply(&mut c);
    c.arg("-C")
        .arg(bare)
        .args(["-c", "color.ui=never", "-c", "core.quotepath=false"]);
    c
}

fn git_bytes(mut c: Command) -> Res<Vec<u8>> {
    let shown = format!("{c:?}");
    let out = c
        .stdin(Stdio::null())
        .output()
        .ctx(&format!("cannot run {shown}"))?;
    if !out.status.success() {
        return Err(format!(
            "{shown} exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

struct TreeEntry {
    oid: String,
    size: u64,
    path: String,
}

fn tree(repo: &Repo) -> Res<Vec<TreeEntry>> {
    let mut c = git(&repo.bare);
    c.args(["ls-tree", "-r", "-l", "-z", &repo.sha]);
    let out = git_bytes(c)?;
    let mut v = Vec::new();
    for rec in out.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let s = String::from_utf8_lossy(rec).into_owned();
        let (meta, path) = s.split_once('\t').ok_or("a malformed ls-tree record")?;
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() != 4 || f[0] != "100644" || f[1] != "blob" {
            continue; // symlinks, submodules, executables
        }
        v.push(TreeEntry {
            oid: f[2].to_string(),
            size: f[3].parse().unwrap_or(u64::MAX),
            path: path.to_string(),
        });
    }
    Ok(v)
}

fn blob(repo: &Repo, oid: &str) -> Res<Vec<u8>> {
    let mut c = git(&repo.bare);
    c.args(["cat-file", "blob", oid]);
    git_bytes(c)
}

fn blob_at(repo: &Repo, path: &str) -> Res<Vec<u8>> {
    let mut c = git(&repo.bare);
    c.arg("cat-file")
        .arg("blob")
        .arg(format!("{}:{path}", repo.sha));
    git_bytes(c)
}

// ------------------------------------------------------------------ scanning and needles

/// The user and host name of this machine, lowercased, when they are distinctive enough to
/// scan for. A short or common name would match ordinary text, so it is left out.
pub fn machine_identity() -> Vec<String> {
    let common = [
        "root",
        "user",
        "test",
        "runner",
        "admin",
        "localhost",
        "runneradmin",
        "ubuntu",
    ];
    let mut v = Vec::new();
    for key in ["USER", "LOGNAME", "USERNAME"] {
        if let Ok(s) = std::env::var(key) {
            v.push(s);
        }
    }
    if let Ok(out) = Command::new("hostname").output() {
        v.push(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    let mut v: Vec<String> = v
        .into_iter()
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 5 && !common.contains(&s.as_str()))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The first forbidden thing in `bytes`: a home path, an employer name, or one of `extra_names`
/// (case-insensitive). `None` when the bytes are clean.
pub fn forbidden_hit(bytes: &[u8], extra_names: &[String]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    for f in FORBIDDEN {
        if text.contains(f) {
            return Some((*f).to_string());
        }
    }
    for root in HOME_ROOTS {
        for (at, _) in text.match_indices(root) {
            let prev = text[..at].chars().next_back();
            let inside_name = prev.is_some_and(|c| c.is_alphanumeric() || "_.-@/".contains(c));
            if !inside_name {
                return Some((*root).to_string());
            }
        }
    }
    let lower = text.to_ascii_lowercase();
    extra_names
        .iter()
        .map(|s| s.to_ascii_lowercase())
        .find(|n| !n.is_empty() && lower.contains(n.as_str()))
}

fn normalise(bytes: Vec<u8>) -> (Vec<u8>, Option<String>) {
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let mut notes = Vec::new();
    for (from, to) in NORMALISE {
        if text.contains(from) {
            text = text.replace(from, to);
            notes.push(format!("{from} -> {to}"));
        }
    }
    if notes.is_empty() {
        (bytes, None)
    } else {
        (text.into_bytes(), Some(notes.join("; ")))
    }
}

/// Up to three indexes spread over `n` candidates: the first, the middle and the last.
fn spread(n: usize) -> Vec<usize> {
    let mut v = match n {
        0 => vec![],
        1 => vec![0],
        2 => vec![0, 1],
        _ => vec![0, n / 2, n - 1],
    };
    v.dedup();
    v
}

fn usable(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 12 && t.len() <= 200
}

fn is_def(trimmed: &str, ext: &str) -> bool {
    let starts: &[&str] = match ext {
        "rs" => &[
            "pub fn ",
            "fn ",
            "pub(crate) fn ",
            "pub struct ",
            "struct ",
            "pub enum ",
            "enum ",
            "pub trait ",
            "impl ",
        ],
        "py" => &["def ", "async def ", "class "],
        _ => &[
            "export function ",
            "function ",
            "export const ",
            "export class ",
            "class ",
            "export type ",
            "export interface ",
            "interface ",
        ],
    };
    starts.iter().any(|s| trimmed.starts_with(s))
}

fn fragment(text: &str, key: &str) -> Option<String> {
    let at = text.find(key)?;
    let rest = &text[at + key.len()..];
    let after = rest.trim_start();
    let after = after.strip_prefix(':')?.trim_start();
    let consumed = text.len() - after.len();
    let end = if after.starts_with('"') {
        let b = after.as_bytes();
        let mut i = 1;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        (i + 1).min(b.len())
    } else {
        after.find([',', '}', ']', '\n']).unwrap_or(after.len())
    };
    let frag = &text[at..consumed + end];
    (frag.len() <= 400 && !frag.contains('\n')).then(|| frag.trim_end().to_string())
}

fn needles_for(text: &str, ext: &str, rules: &[Rule]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    for rule in rules {
        match rule {
            Rule::Positions => {
                let c: Vec<&str> = text.lines().filter(|l| usable(l)).collect();
                for i in spread(c.len()) {
                    add(c[i].trim().to_string());
                }
            }
            Rule::Defs => {
                let c: Vec<&str> = text
                    .lines()
                    .filter(|l| usable(l) && is_def(l.trim(), ext))
                    .collect();
                let n = c.len();
                let idx: Vec<usize> = match n {
                    0 => vec![],
                    1 => vec![0],
                    2 => vec![0, 1],
                    _ => vec![n / 4, n / 2, 3 * n / 4],
                };
                for i in idx {
                    add(c[i].trim().to_string());
                }
            }
            Rule::Contains(s) => {
                let c: Vec<&str> = text
                    .lines()
                    .filter(|l| l.contains(s) && l.trim().len() <= 300)
                    .collect();
                for i in spread(c.len()) {
                    add(c[i].trim().to_string());
                }
            }
            Rule::Fragments(keys) => {
                for k in keys.iter() {
                    if let Some(f) = fragment(text, k) {
                        add(f);
                    }
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------------ building the items

struct Built {
    item: Item,
    bytes: Vec<u8>,
}

struct Ctxt<'a> {
    repos: &'a [Repo],
    identity: Vec<String>,
}

impl Ctxt<'_> {
    fn repo(&self, name: &str) -> Res<&Repo> {
        self.repos
            .iter()
            .find(|r| r.name == name)
            .ok_or_else(|| format!("no repository named {name}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        id: &str,
        category: &str,
        bucket: Option<String>,
        raw: Vec<u8>,
        ext: &str,
        needle_ext: &str,
        rules: &[Rule],
        source: Source,
        recipe: String,
    ) -> Res<Built> {
        let (bytes, normalised) = normalise(raw);
        if let Some(hit) = forbidden_hit(&bytes, &self.identity) {
            return Err(format!("{id}: the recorded bytes contain {hit:?}"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|e| format!("{id}: not UTF-8 ({e})"))?;
        let needles = needles_for(text, needle_ext, rules);
        if needles.is_empty() {
            return Err(format!("{id}: no needles found"));
        }
        let item = Item {
            id: id.to_string(),
            category: category.to_string(),
            bucket,
            file: format!("items/{id}.{ext}"),
            bytes: bytes.len() as u64,
            lines: bytes.iter().filter(|&&b| b == b'\n').count() as u64,
            sha256: sha256_hex(&bytes),
            source_kind: source.kind.to_string(),
            source: source.url,
            commit: source.commit,
            source_sha256: source.sha256,
            recipe,
            normalised,
            needles,
        };
        Ok(Built { item, bytes })
    }
}

struct Source {
    kind: &'static str,
    url: String,
    commit: Option<String>,
    sha256: Option<String>,
}

fn git_source(repo: &Repo) -> Source {
    Source {
        kind: "git",
        url: repo.url.clone(),
        commit: Some(repo.sha.clone()),
        sha256: None,
    }
}

fn file_reads(cx: &Ctxt, repo: &Repo, out: &mut Vec<Built>) -> Res<()> {
    let skip = [
        "node_modules/",
        "target/",
        "dist/",
        "build/",
        "vendor/",
        "third_party/",
        "__pycache__/",
        ".yarn/",
    ];
    let mut cand: Vec<(String, TreeEntry)> = tree(repo)?
        .into_iter()
        .filter(|e| {
            let ext = e.path.rsplit('.').next().unwrap_or("");
            matches!(ext, "rs" | "py" | "ts" | "tsx")
                && !e.path.ends_with(".d.ts")
                && e.size <= 400_000
                && !skip.iter().any(|s| e.path.contains(s))
        })
        .map(|e| {
            (
                sha256_hex(format!("{}\0{}", repo.name, e.path).as_bytes()),
                e,
            )
        })
        .collect();
    cand.sort_by(|a, b| a.0.cmp(&b.0));
    let mut taken: BTreeSet<usize> = BTreeSet::new();
    for (_, e) in &cand {
        if taken.len() == BUCKETS.len() {
            break;
        }
        // Cheap size screen first: a byte count under the bucket's floor cannot reach it.
        let bytes = blob(repo, &e.oid)?;
        if std::str::from_utf8(&bytes).is_err() || forbidden_hit(&bytes, &cx.identity).is_some() {
            continue;
        }
        let lines = bytes.iter().filter(|&&b| b == b'\n').count();
        let b = bucket_of(lines);
        if !taken.insert(b) {
            continue;
        }
        let ext = e.path.rsplit('.').next().unwrap_or("txt").to_string();
        let id = format!(
            "read-{}-{}",
            repo.name,
            ["lt300", "300-999", "1000-2999", "3000plus"][b]
        );
        out.push(cx.finish(
            &id,
            "file_read",
            Some(BUCKETS[b].name.to_string()),
            bytes,
            "txt",
            &ext,
            &[Rule::Defs, Rule::Positions],
            git_source(repo),
            format!("git cat-file blob {}:{}", repo.sha, e.path),
        )?);
    }
    Ok(())
}

fn git_out(cx: &Ctxt, g: &GitOut, out: &mut Vec<Built>) -> Res<()> {
    let repo = cx.repo(g.repo)?;
    let mut c = git(&repo.bare);
    for a in g.args {
        c.arg(a.replace("{sha}", &repo.sha));
    }
    let raw = git_bytes(c)?;
    // `git grep <tree>` prefixes every hit with `<sha>:` (context lines `<sha>-`); strip it so
    // the output reads like `grep -rn` in a checkout.
    let raw = if g.args.contains(&"grep") {
        strip_tree_prefix(&raw, &repo.sha)
    } else {
        raw
    };
    let shown: Vec<String> = g
        .args
        .iter()
        .map(|a| a.replace("{sha}", "<commit>"))
        .collect();
    out.push(cx.finish(
        g.id,
        g.category,
        None,
        raw,
        "txt",
        "txt",
        g.needles,
        git_source(repo),
        format!("git {}", shown.join(" ")),
    )?);
    Ok(())
}

fn strip_tree_prefix(raw: &[u8], sha: &str) -> Vec<u8> {
    let colon = format!("{sha}:");
    let dash = format!("{sha}-");
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|&b| b == b'\n') {
        let s = line;
        if s.starts_with(colon.as_bytes()) {
            out.extend_from_slice(&s[colon.len()..]);
        } else if s.starts_with(dash.as_bytes()) {
            out.extend_from_slice(&s[dash.len()..]);
        } else {
            out.extend_from_slice(s);
        }
    }
    out
}

fn blob_range(cx: &Ctxt, b: &BlobRange, out: &mut Vec<Built>) -> Res<()> {
    let repo = cx.repo(b.repo)?;
    let whole = blob_at(repo, b.path)?;
    let text = String::from_utf8(whole).ctx(b.path)?;
    let lines: Vec<&str> = text.lines().collect();
    if b.end > lines.len() || b.start == 0 || b.start > b.end {
        return Err(format!(
            "{}: lines {}-{} are outside {} ({} lines)",
            b.id,
            b.start,
            b.end,
            b.path,
            lines.len()
        ));
    }
    let mut body = String::new();
    for l in &lines[b.start - 1..b.end] {
        let cut = l.len() - l.trim_start_matches(' ').len();
        body.push_str(&l[cut.min(b.dedent)..]);
        body.push('\n');
    }
    out.push(cx.finish(
        b.id,
        "test_log",
        None,
        body.into_bytes(),
        "txt",
        "txt",
        b.needles,
        git_source(repo),
        format!(
            "git cat-file blob {}:{}, lines {}-{}, {} leading spaces removed",
            repo.sha, b.path, b.start, b.end, b.dedent
        ),
    )?);
    Ok(())
}

fn json_blob(cx: &Ctxt, j: &JsonBlob, out: &mut Vec<Built>) -> Res<()> {
    let repo = cx.repo(j.repo)?;
    let (path, bytes, how) = if !j.path.is_empty() {
        (
            j.path.to_string(),
            blob_at(repo, j.path)?,
            "git cat-file blob",
        )
    } else {
        let mut v: Vec<TreeEntry> = tree(repo)?
            .into_iter()
            .filter(|e| {
                e.path.ends_with(".json") && e.size <= 250_000 && !e.path.contains("node_modules/")
            })
            .collect();
        v.sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
        let mut found = None;
        for e in v {
            let bytes = blob(repo, &e.oid)?;
            if std::str::from_utf8(&bytes).is_ok() && forbidden_hit(&bytes, &cx.identity).is_none()
            {
                found = Some((e.path, bytes));
                break;
            }
        }
        let (p, b) = found.ok_or_else(|| format!("{}: no JSON file in {}", j.id, repo.name))?;
        (
            p,
            b,
            "git cat-file blob (the largest .json file under 250000 bytes)",
        )
    };
    out.push(cx.finish(
        j.id,
        "json",
        None,
        bytes,
        "json",
        "json",
        j.needles,
        git_source(repo),
        format!("{how} {}:{path}", repo.sha),
    )?);
    Ok(())
}

fn download(
    cx: &Ctxt,
    d: &Download,
    fetch: &dyn Fn(&str) -> Res<Vec<u8>>,
    out: &mut Vec<Built>,
) -> Res<()> {
    let raw = fetch(&d.url)?;
    let got = sha256_hex(&raw);
    if got != d.sha256 {
        return Err(format!(
            "{}: {} has sha256 {got}, pinned {}; the source moved, nothing was written",
            d.id, d.url, d.sha256
        ));
    }
    let ext = if d.category == "json" { "json" } else { "txt" };
    out.push(cx.finish(
        d.id,
        d.category,
        None,
        raw,
        ext,
        ext,
        d.needles,
        Source {
            kind: "url",
            url: d.url.clone(),
            commit: None,
            sha256: Some(d.sha256.to_string()),
        },
        "downloaded, checked against the pinned sha256, stored unchanged".to_string(),
    )?);
    Ok(())
}

/// Build every item in memory. Pure in its inputs: the repositories, the plan and `fetch`.
fn build(
    repos: &[Repo],
    plan: &Plan,
    identity: Vec<String>,
    fetch: &dyn Fn(&str) -> Res<Vec<u8>>,
) -> Res<Vec<Built>> {
    let cx = Ctxt { repos, identity };
    let mut out = Vec::new();
    for name in &plan.file_read_repos {
        file_reads(&cx, cx.repo(name)?, &mut out)?;
    }
    for g in &plan.git_outs {
        git_out(&cx, g, &mut out)?;
    }
    for b in &plan.blobs {
        blob_range(&cx, b, &mut out)?;
    }
    for d in &plan.downloads {
        download(&cx, d, fetch, &mut out)?;
    }
    for j in &plan.json_blobs {
        json_blob(&cx, j, &mut out)?;
    }
    out.sort_by(|a, b| a.item.id.cmp(&b.item.id));
    let mut seen = BTreeSet::new();
    for b in &out {
        if !seen.insert(b.item.id.clone()) {
            return Err(format!("two items share the id {}", b.item.id));
        }
    }
    Ok(out)
}

/// Written beside the items so a CRLF clone hashes the same bytes.
const GITATTRIBUTES: &str = "* -text -diff\n";

const NOTE: &str = "Layer 1 compression-replay corpus (C1): real tool outputs recorded from pinned public sources. \
Regenerate with `benchmarks/run.sh competitors record`; verify with `competitors record --verify`. \
A needle is a line or fragment a later question needs; a tool that drops one does not win the row.";

/// Write the corpus into `dir` (replacing `items/` and `manifest.toml`).
pub fn write(
    dir: &Path,
    repos: &[Repo],
    plan: &Plan,
    identity: Vec<String>,
    fetch: &dyn Fn(&str) -> Res<Vec<u8>>,
) -> Res<Manifest> {
    let built = build(repos, plan, identity, fetch)?;
    let total: u64 = built.iter().map(|b| b.bytes.len() as u64).sum();
    if total > MAX_TOTAL_BYTES {
        return Err(format!(
            "the corpus is {total} bytes, over the {MAX_TOTAL_BYTES} limit; nothing was written"
        ));
    }
    let items_dir = dir.join("items");
    if items_dir.exists() {
        std::fs::remove_dir_all(&items_dir).ctx("clear items/")?;
    }
    std::fs::create_dir_all(&items_dir).ctx("create items/")?;
    for b in &built {
        std::fs::write(dir.join(&b.item.file), &b.bytes).ctx(&b.item.file)?;
    }
    let manifest = Manifest {
        corpus: Meta {
            schema: 1,
            note: NOTE.to_string(),
            items: built.len(),
            total_bytes: total,
        },
        item: built.into_iter().map(|b| b.item).collect(),
    };
    let text = toml::to_string_pretty(&manifest).ctx("serialise the manifest")?;
    std::fs::write(dir.join("manifest.toml"), text).ctx("write manifest.toml")?;
    std::fs::write(dir.join(".gitattributes"), GITATTRIBUTES).ctx("write .gitattributes")?;
    Ok(manifest)
}

// ------------------------------------------------------------------ verification

#[derive(Debug)]
pub struct Verified {
    pub items: usize,
    pub total_bytes: u64,
    pub per_category: BTreeMap<String, usize>,
}

/// Check a recorded corpus: the manifest parses, every item's sha256 matches, every needle is
/// in its item, no home path or name is in any byte, nothing is unlisted, and the total fits.
pub fn verify(dir: &Path, extra_names: &[String], max_total: u64) -> Res<Verified> {
    let text = std::fs::read_to_string(dir.join("manifest.toml")).ctx("read manifest.toml")?;
    let m: Manifest = toml::from_str(&text).ctx("parse manifest.toml")?;
    let mut total = 0u64;
    let mut per: BTreeMap<String, usize> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut files = BTreeSet::new();
    for it in &m.item {
        if !ids.insert(it.id.clone()) {
            return Err(format!("duplicate id {}", it.id));
        }
        files.insert(it.file.clone());
        let bytes =
            std::fs::read(dir.join(&it.file)).ctx(&format!("{}: read {}", it.id, it.file))?;
        if bytes.len() as u64 != it.bytes {
            return Err(format!(
                "{}: {} bytes, manifest says {}",
                it.id,
                bytes.len(),
                it.bytes
            ));
        }
        let sha = sha256_hex(&bytes);
        if sha != it.sha256 {
            return Err(format!(
                "{}: sha256 {sha}, manifest says {}",
                it.id, it.sha256
            ));
        }
        if it.needles.is_empty() {
            return Err(format!("{}: no needles", it.id));
        }
        let body = String::from_utf8(bytes).map_err(|e| format!("{}: not UTF-8 ({e})", it.id))?;
        for n in &it.needles {
            if n.is_empty() || !body.contains(n.as_str()) {
                return Err(format!("{}: needle {n:?} is not in the item", it.id));
            }
        }
        if let Some(hit) = forbidden_hit(body.as_bytes(), extra_names) {
            return Err(format!("{}: contains {hit:?}", it.id));
        }
        if it.source_kind == "git" && it.commit.as_deref().is_none_or(|c| c.len() != 40) {
            return Err(format!(
                "{}: a git source needs a full 40-character commit",
                it.id
            ));
        }
        if it.source_kind == "url" && it.source_sha256.is_none() {
            return Err(format!("{}: a url source needs its sha256", it.id));
        }
        total += it.bytes;
        *per.entry(it.category.clone()).or_default() += 1;
    }
    if total > max_total {
        return Err(format!(
            "the corpus is {total} bytes, over the {max_total} limit"
        ));
    }
    if m.corpus.items != m.item.len() || m.corpus.total_bytes != total {
        return Err("the manifest's item count or total does not match its items".to_string());
    }
    let items_dir = dir.join("items");
    for e in std::fs::read_dir(&items_dir).ctx("list items/")? {
        let name = e
            .ctx("list items/")?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if !files.contains(&format!("items/{name}")) {
            return Err(format!("items/{name} is not in the manifest"));
        }
    }
    Ok(Verified {
        items: m.item.len(),
        total_bytes: total,
        per_category: per,
    })
}

// ------------------------------------------------------------------ command line

pub const HELP: &str = "\
usage: benchmarks/run.sh competitors record [--out DIR] [--cache DIR] [--verify]

  Records the Layer 1 corpus (benchmarks/competitors/corpus/) from pinned public sources:
  git object stores at full commit SHAs and downloads checked against pinned sha256s.
  Needs git, curl and the network. Two runs write identical bytes.

  --out DIR     where to write (default: benchmarks/competitors/corpus)
  --cache DIR   the harness cache that holds the pinned repositories
                (default: <system temp>/lumen-bench-cache)
  --verify      check the corpus already on disk (sha256s, needles, no home paths, size) and
                write nothing
";

pub fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../competitors/corpus")
}

fn curl(url: &str) -> Res<Vec<u8>> {
    let mut c = Command::new("curl");
    c.args(["-fsSL", "--retry", "3", "-A", "lumen-bench", url]);
    if url.starts_with("https://api.github.com/") {
        c.args(["-H", "Accept: application/vnd.github+json"]);
    }
    git_bytes(c)
}

pub fn run(args: &[String]) -> Res<()> {
    let mut out = default_dir();
    let mut cache = std::env::temp_dir().join("lumen-bench-cache");
    let mut verify_only = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = PathBuf::from(it.next().ok_or("--out needs a directory")?),
            "--cache" => cache = PathBuf::from(it.next().ok_or("--cache needs a directory")?),
            "--verify" => verify_only = true,
            other => return Err(format!("record: unknown argument {other}\n{HELP}")),
        }
    }
    if verify_only {
        let v = verify(&out, &machine_identity(), MAX_TOTAL_BYTES)?;
        print_summary(&v);
        return Ok(());
    }
    let mut repos = Vec::new();
    for pin in PINS {
        // Fetches, verifies and caches the pinned commit; the extracted work tree is not used.
        crate::corpus::load(pin, &cache, None)?;
        repos.push(Repo {
            name: pin.name.to_string(),
            url: pin.url.to_string(),
            sha: pin.sha.to_string(),
            bare: cache.join("git").join(pin.name),
        });
    }
    std::fs::create_dir_all(&out).ctx("create the output directory")?;
    write(&out, &repos, &real_plan(), machine_identity(), &curl)?;
    let v = verify(&out, &machine_identity(), MAX_TOTAL_BYTES)?;
    print_summary(&v);
    Ok(())
}

fn print_summary(v: &Verified) {
    println!("corpus: {} items, {} bytes", v.items, v.total_bytes);
    for (c, n) in &v.per_category {
        println!("  {c}: {n}");
    }
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPY: &[&str] = &["planted-user-x"];

    fn fixture_repo(dir: &Path, extra_file: Option<(&str, &str)>) -> Repo {
        let bare = dir.join("fx.git");
        let work = dir.join("fx-work");
        std::fs::create_dir_all(&work).unwrap();
        let run = |args: &[&str], cwd: &Path| {
            let out = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        run(&["init", "-q"], &work);
        let mut small = String::new();
        for i in 0..40 {
            small.push_str(&format!(
                "fn function_number_{i}() {{ let value = {i}; }}\n"
            ));
        }
        std::fs::write(work.join("small.rs"), small).unwrap();
        let mut big = String::new();
        for i in 0..400 {
            big.push_str(&format!(
                "def handler_number_{i}(argument):\n    return argument + {i}\n"
            ));
        }
        std::fs::write(work.join("big.py"), big).unwrap();
        std::fs::write(
            work.join("notes.rst"),
            "intro\n    $ pytest\n    FAILED test_a.py::test_one - assert 1 == 2\n    ===== 1 failed in 0.12s =====\n",
        )
        .unwrap();
        std::fs::write(
            work.join("data.json"),
            "{\"name\":\"fixture\",\"version\":\"1.2.3\"}",
        )
        .unwrap();
        if let Some((name, body)) = extra_file {
            std::fs::write(work.join(name), body).unwrap();
        }
        run(&["add", "."], &work);
        run(&["commit", "-q", "-m", "fixture"], &work);
        let sha = run(&["rev-parse", "HEAD"], &work);
        run(
            &[
                "clone",
                "-q",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir,
        );
        Repo {
            name: "fx".to_string(),
            url: "https://example.com/fx".to_string(),
            sha,
            bare,
        }
    }

    fn fixture_plan(body: &'static [u8]) -> Plan {
        let sha: &'static str = Box::leak(sha256_hex(body).into_boxed_str());
        Plan {
            file_read_repos: vec!["fx"],
            git_outs: vec![GitOut {
                id: "shell-ls-tree-fx",
                category: "shell",
                repo: "fx",
                args: &["ls-tree", "-r", "-l", "{sha}"],
                needles: &[Rule::Positions],
            }],
            blobs: vec![BlobRange {
                id: "testlog-fx",
                repo: "fx",
                path: "notes.rst",
                start: 2,
                end: 4,
                dedent: 4,
                needles: &[Rule::Contains("FAILED "), Rule::Contains(" failed in ")],
            }],
            downloads: vec![Download {
                id: "json-fx-download",
                category: "json",
                url: "https://example.com/doc.json".to_string(),
                sha256: sha,
                needles: &[Rule::Fragments(&["\"name\"", "\"version\""])],
            }],
            json_blobs: vec![JsonBlob {
                id: "json-file-fx",
                repo: "fx",
                path: "data.json",
                needles: &[Rule::Fragments(&["\"version\""])],
            }],
        }
    }

    const DOC: &[u8] = b"{\"name\":\"remote\",\"version\":\"9.9.9\",\"files\":[1,2,3]}";

    fn fake_fetch(url: &str) -> Res<Vec<u8>> {
        assert_eq!(url, "https://example.com/doc.json");
        Ok(DOC.to_vec())
    }

    fn record_fixture(dir: &Path, out: &Path) -> Manifest {
        let repo = fixture_repo(dir, None);
        std::fs::create_dir_all(out).unwrap();
        write(out, &[repo], &fixture_plan(DOC), vec![], &fake_fetch).unwrap()
    }

    #[test]
    fn the_committed_manifest_parses_and_lists_every_category() {
        let text = std::fs::read_to_string(default_dir().join("manifest.toml")).unwrap();
        let m: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(m.corpus.items, m.item.len());
        for cat in [
            "file_read",
            "shell",
            "test_log",
            "build_log",
            "json",
            "grep",
        ] {
            assert!(
                m.item.iter().any(|i| i.category == cat),
                "no item in the {cat} category"
            );
        }
        for b in BUCKETS.iter() {
            assert!(
                m.item.iter().any(|i| i.bucket.as_deref() == Some(b.name)),
                "no file read in the {} bucket",
                b.name
            );
        }
    }

    #[test]
    fn every_committed_item_matches_its_sha256_and_keeps_its_needles() {
        let v = verify(&default_dir(), &[], MAX_TOTAL_BYTES).unwrap();
        assert!(v.items >= 30, "only {} items", v.items);
    }

    #[test]
    fn no_committed_byte_names_a_home_path_a_user_or_an_employer() {
        let spy: Vec<String> = SPY.iter().map(|s| s.to_string()).collect();
        verify(&default_dir(), &spy, MAX_TOTAL_BYTES).unwrap();
    }

    #[test]
    fn the_committed_corpus_fits_in_five_megabytes() {
        let v = verify(&default_dir(), &[], MAX_TOTAL_BYTES).unwrap();
        assert!(v.total_bytes <= 5 * 1024 * 1024);
        // The same corpus fails a smaller limit, so the check is live.
        assert!(verify(&default_dir(), &[], v.total_bytes - 1).is_err());
    }

    #[test]
    fn a_planted_home_path_is_found() {
        for planted in [
            "/Users/someone/project",
            "see /home/alice/x",
            "C:\\Users\\bob\\x",
            "\"/root/.ssh\"",
        ] {
            assert!(
                forbidden_hit(planted.as_bytes(), &[]).is_some(),
                "{planted} was not found"
            );
        }
        assert!(forbidden_hit(b"lumenator/src/app/pages/home/home.ts", &[]).is_none());
        assert!(
            forbidden_hit(b"owned by planted-user-x", &["planted-user-x".to_string()]).is_some()
        );
    }

    #[test]
    fn a_home_path_in_a_recorded_source_stops_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let repo = fixture_repo(dir.path(), None);
        static LEAK: &[u8] = b"{\"name\":\"/Users/someone/proj\",\"version\":\"1\"}";
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let fetch = |_: &str| -> Res<Vec<u8>> { Ok(LEAK.to_vec()) };
        let e = write(&out, &[repo], &fixture_plan(LEAK), vec![], &fetch).unwrap_err();
        assert!(e.contains("/Users/"), "{e}");
    }

    #[test]
    fn a_planted_path_in_a_recorded_item_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        record_fixture(dir.path(), &out);
        verify(&out, &[], MAX_TOTAL_BYTES).unwrap();
        // Plant a path in an item and keep the manifest consistent: only the scan can object.
        let mut m: Manifest =
            toml::from_str(&std::fs::read_to_string(out.join("manifest.toml")).unwrap()).unwrap();
        let it = m
            .item
            .iter_mut()
            .find(|i| i.id == "shell-ls-tree-fx")
            .unwrap();
        let mut bytes = std::fs::read(out.join(&it.file)).unwrap();
        bytes.extend_from_slice(b"\n/Users/someone/dev/project/src/main.rs\n");
        std::fs::write(out.join(&it.file), &bytes).unwrap();
        it.bytes = bytes.len() as u64;
        it.sha256 = sha256_hex(&bytes);
        m.corpus.total_bytes = m.item.iter().map(|i| i.bytes).sum();
        std::fs::write(
            out.join("manifest.toml"),
            toml::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        let e = verify(&out, &[], MAX_TOTAL_BYTES).unwrap_err();
        assert!(e.contains("/Users/"), "{e}");
    }

    #[test]
    fn a_changed_byte_a_dropped_needle_and_an_unlisted_file_each_fail_verification() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        record_fixture(dir.path(), &out);
        let file = out.join("items/testlog-fx.txt");
        let good = std::fs::read(&file).unwrap();

        let mut bad = good.clone();
        bad[0] ^= 1;
        std::fs::write(&file, &bad).unwrap();
        assert!(
            verify(&out, &[], MAX_TOTAL_BYTES)
                .unwrap_err()
                .contains("sha256")
        );
        std::fs::write(&file, &good).unwrap();

        // A needle that is not in the item.
        let mut m: Manifest =
            toml::from_str(&std::fs::read_to_string(out.join("manifest.toml")).unwrap()).unwrap();
        m.item[0]
            .needles
            .push("a line that was never recorded".to_string());
        let kept = std::fs::read_to_string(out.join("manifest.toml")).unwrap();
        std::fs::write(
            out.join("manifest.toml"),
            toml::to_string_pretty(&m).unwrap(),
        )
        .unwrap();
        assert!(
            verify(&out, &[], MAX_TOTAL_BYTES)
                .unwrap_err()
                .contains("needle")
        );
        std::fs::write(out.join("manifest.toml"), kept).unwrap();

        std::fs::write(out.join("items/stray.txt"), "x").unwrap();
        assert!(
            verify(&out, &[], MAX_TOTAL_BYTES)
                .unwrap_err()
                .contains("stray")
        );
    }

    #[test]
    fn a_moved_download_stops_the_record_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let repo = fixture_repo(dir.path(), None);
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let fetch = |_: &str| -> Res<Vec<u8>> { Ok(b"{\"name\":\"changed\"}".to_vec()) };
        let e = write(&out, &[repo], &fixture_plan(DOC), vec![], &fetch).unwrap_err();
        assert!(e.contains("pinned"), "{e}");
        assert!(!out.join("manifest.toml").exists());
    }

    #[test]
    fn record_writes_identical_bytes_twice() {
        let dir = tempfile::tempdir().unwrap();
        let repo = fixture_repo(dir.path(), None);
        let plan = fixture_plan(DOC);
        let mut trees = Vec::new();
        for n in 0..2 {
            let out = dir.path().join(format!("out{n}"));
            std::fs::create_dir_all(&out).unwrap();
            write(
                &out,
                std::slice::from_ref(&repo),
                &plan,
                vec![],
                &fake_fetch,
            )
            .unwrap();
            let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            files.insert(
                "manifest.toml".into(),
                std::fs::read(out.join("manifest.toml")).unwrap(),
            );
            for e in std::fs::read_dir(out.join("items")).unwrap() {
                let e = e.unwrap();
                files.insert(
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                );
            }
            trees.push(files);
        }
        assert_eq!(trees[0], trees[1]);
        assert!(trees[0].len() >= 6, "{:?}", trees[0].keys());
    }

    #[test]
    fn the_fixture_record_has_the_expected_items_and_needles() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let m = record_fixture(dir.path(), &out);
        let ids: Vec<&str> = m.item.iter().map(|i| i.id.as_str()).collect();
        assert!(ids.contains(&"read-fx-lt300"), "{ids:?}");
        assert!(
            ids.contains(&"read-fx-1000-2999") || ids.contains(&"read-fx-300-999"),
            "{ids:?}"
        );
        let log = m.item.iter().find(|i| i.id == "testlog-fx").unwrap();
        assert!(
            log.needles
                .iter()
                .any(|n| n.starts_with("FAILED test_a.py::test_one"))
        );
        let j = m.item.iter().find(|i| i.id == "json-fx-download").unwrap();
        assert!(
            j.needles.contains(&"\"version\":\"9.9.9\"".to_string()),
            "{:?}",
            j.needles
        );
        assert_eq!(j.source_sha256.as_deref(), Some(sha256_hex(DOC).as_str()));
    }

    #[test]
    fn fragments_read_minified_and_pretty_json() {
        assert_eq!(
            fragment("{\"a\":{\"b\":\"x\\\"y\"}}", "\"b\"").unwrap(),
            "\"b\":\"x\\\"y\""
        );
        assert_eq!(
            fragment("{\n  \"n\": 12,\n}", "\"n\"").unwrap(),
            "\"n\": 12"
        );
        assert!(fragment("{}", "\"n\"").is_none());
    }

    #[test]
    fn git_grep_prefixes_are_stripped() {
        let sha = "a".repeat(40);
        let raw = format!("{sha}:src/a.rs:3:fn a()\n{sha}-src/a.rs-4-  body\n--\n");
        assert_eq!(
            String::from_utf8(strip_tree_prefix(raw.as_bytes(), &sha)).unwrap(),
            "src/a.rs:3:fn a()\nsrc/a.rs-4-  body\n--\n"
        );
    }
}
