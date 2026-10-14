//! No user-facing text may call a proxy token count exact.
//!
//! Lumen counts tokens with `cl100k_base`, OpenAI's tokenizer, because Claude's is not
//! available offline. The count is deterministic, and for a long time the copy said more
//! than that: the README called tool calls and built-in `Read` events "measured to the
//! token", and the Optimizer footer read "Counted to the token by a local tokenizer, never
//! estimated." Both were true only in `cl100k_base`'s own units. Measured against Claude's
//! usage data (`benchmarks/calibration`), Claude's count of the same text is a different
//! number, by a ratio that depends on the model family and the kind of file, so a figure
//! built on the proxy is an approximation of what Claude counts, and says so with its error
//! (`lumen_core::calibration`).
//!
//! Provider usage data is different: Claude Code's transcripts carry the token counts the
//! API reported, and Lumen reads them as they are. "Measured" stays right for those, and
//! this test does not forbid the word. It forbids the phrases that claim exactness, wherever
//! a person or the model reads them: the README, the messaging and design contracts, `docs/`, the
//! benchmark READMEs, the app's templates and TypeScript (spec files excepted), and every
//! string literal in the crates' and the tray's Rust sources, which is where the CLI's
//! output, the tool descriptions and the hook templates live. A use that is right, such as
//! a sentence about provider data or one quoting the copy a fix replaced, is listed in
//! [`ALLOWED`] with the reason.
//!
//! A text scan, not a reader. It finds the phrases below, across line breaks and in any
//! case; it does not find a claim made in other words. Code comments in Rust are not read
//! (only string literals are), and comments in TypeScript and HTML are, because those files
//! are scanned whole.

use std::path::{Path, PathBuf};

/// Phrases that claim a token count is exact. Matched case-insensitively, on word
/// boundaries, with every run of whitespace read as one space.
const PHRASES: &[&str] = &[
    "to the token",
    "counts are exact",
    "count is exact",
    "figures are exact",
    "never estimated",
    "not estimated",
    "no estimation",
    "exact token",
    "exact tokens",
    "tokens are exact",
    "token count is exact",
    "token-exact",
    "token exact",
    "exact count",
    "exact counts",
    "exact bpe",
    "exact in its units",
    "precise count",
    "counted exactly",
    "exactly counted",
];

/// A use of a phrase that is not a claim about a proxy count, and why.
///
/// It covers the hits in `file` whose surrounding text contains `needle`, and must cover
/// exactly `n` of them: an entry that matches nothing is stale, and one that matches more
/// than it says has started excusing text nobody looked at.
struct Allowed {
    file: &'static str,
    needle: &'static str,
    n: usize,
    why: &'static str,
}

const fn allow(file: &'static str, needle: &'static str, why: &'static str) -> Allowed {
    Allowed {
        file,
        needle,
        n: 1,
        why,
    }
}

impl Allowed {
    const fn times(self, n: usize) -> Allowed {
        Allowed { n, ..self }
    }
}

const ALLOWED: &[Allowed] = &[
    allow(
        "lumenator/src/app/pages/optimizer/optimizer.ts",
        "before 1.9.0 read",
        "A code comment quoting the footer this file replaced, to say why it was replaced. \
         It is not rendered. Two phrases, one quote.",
    )
    .times(2),
    allow(
        "benchmarks/calibration/README.md",
        "repeatable to the token",
        "About provider usage data: the totals are Claude's own usage figures, and two \
         sends of the same prompt reported the same one.",
    ),
];

/// Directories never walked: build output, dependencies, other checkouts.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".angular", "dist"];

#[derive(Debug)]
struct Hit {
    /// Relative to the workspace root, `/`-separated on every OS.
    file: String,
    line: usize,
    phrase: &'static str,
    /// The text around the phrase, normalized. Entries match against it.
    context: String,
}

impl std::fmt::Display for Hit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{} \"{}\" in: …{}…",
            self.file, self.line, self.phrase, self.context
        )
    }
}

/// Walk up from this crate to the workspace root.
fn workspace_root() -> PathBuf {
    let mut dir: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("crates").is_dir() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    // Not an early return: a guard that passes because it found nothing to scan is the
    // bug it exists to catch.
    panic!("no workspace root above {}", env!("CARGO_MANIFEST_DIR"));
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every regular file under `dir`, symlinks not followed. An unreadable directory panics
/// rather than being passed over in silence.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        let kind = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()));
        let path = entry.path();
        if kind.is_dir() {
            let name = entry.file_name();
            if !SKIP_DIRS.iter().any(|s| name == *s) {
                walk(&path, out);
            }
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

fn ext_is(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.contains(&e))
}

/// `text` lowercased with every run of whitespace made one space, and for each character
/// kept, the 1-based line it came from.
fn normalize(text: &str, first_line: usize) -> (Vec<char>, Vec<usize>) {
    let mut out = Vec::new();
    let mut lines = Vec::new();
    let mut line = first_line;
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
                lines.push(line);
            }
            space = true;
        } else {
            out.extend(c.to_lowercase());
            while lines.len() < out.len() {
                lines.push(line);
            }
            space = false;
        }
        if c == '\n' {
            line += 1;
        }
    }
    (out, lines)
}

fn is_word(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || *c == '_')
}

/// Every phrase in `text`, which starts on line `first_line` of `file`.
fn find(file: &str, text: &str, first_line: usize, hits: &mut Vec<Hit>) {
    let (norm, lines) = normalize(text, first_line);
    for phrase in PHRASES {
        let p: Vec<char> = phrase.chars().collect();
        let mut i = 0;
        while i + p.len() <= norm.len() {
            if norm[i..i + p.len()] == p[..]
                && !is_word(if i == 0 { None } else { norm.get(i - 1) })
                && !is_word(norm.get(i + p.len()))
            {
                let from = i.saturating_sub(80);
                let to = (i + p.len() + 80).min(norm.len());
                hits.push(Hit {
                    file: file.to_string(),
                    line: lines[i],
                    phrase,
                    context: norm[from..to].iter().collect(),
                });
            }
            i += 1;
        }
    }
}

/// The string literals in Rust source, each with the line it starts on. Comments and char
/// literals are stepped over, so a `"` inside either does not open a string.
fn rust_strings(src: &str) -> Vec<(usize, String)> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let count_lines = |from: usize, to: usize| s[from..to].iter().filter(|&&c| c == '\n').count();
    while i < s.len() {
        let c = s[i];
        let prev_word = i > 0 && is_word(s.get(i - 1));
        if c == '/' && s.get(i + 1) == Some(&'/') {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && s.get(i + 1) == Some(&'*') {
            // Block comments nest in Rust.
            let start = i;
            let mut depth = 0;
            while i < s.len() {
                if s[i] == '/' && s.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            line += count_lines(start, i);
        } else if c == '\'' {
            // A char literal ('x', '\n', '\'', '\u{..}') or a lifetime ('a).
            if s.get(i + 1) == Some(&'\\') {
                let mut j = i + 3;
                while j < s.len() && s[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
            } else if s.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
            }
        } else if (c == 'r' || (c == 'b' && s.get(i + 1) == Some(&'r'))) && !prev_word && {
            let mut j = i + if c == 'b' { 2 } else { 1 };
            while s.get(j) == Some(&'#') {
                j += 1;
            }
            s.get(j) == Some(&'"')
        } {
            // A raw string: r"…", r#"…"#, br"…".
            let mut j = i + if c == 'b' { 2 } else { 1 };
            let mut hashes = 0;
            while s[j] == '#' {
                hashes += 1;
                j += 1;
            }
            let body = j + 1;
            let mut k = body;
            loop {
                if k >= s.len() {
                    break;
                }
                if s[k] == '"' && (0..hashes).all(|h| s.get(k + 1 + h) == Some(&'#')) {
                    break;
                }
                k += 1;
            }
            let end = k.min(s.len());
            out.push((line, s[body..end].iter().collect()));
            line += count_lines(i, end);
            i = end + 1 + hashes;
        } else if c == '"' {
            let body = i + 1;
            let mut k = body;
            while k < s.len() && s[k] != '"' {
                if s[k] == '\\' {
                    k += 1;
                }
                k += 1;
            }
            let end = k.min(s.len());
            // A `\` before a line break continues the string; it is not a character.
            let text: String = s[body..end].iter().collect();
            out.push((line, text.replace("\\\n", " ").replace("\\\r\n", " ")));
            line += count_lines(i, end);
            i = end + 1;
        } else {
            if c == '\n' {
                line += 1;
            }
            i += 1;
        }
    }
    out
}

/// The newest entry of a changelog, from its first `## ` heading to the next, and the line it
/// starts on. The whole text when there is no heading.
fn newest_entry(changelog: &str) -> (usize, &str) {
    let mut start = None;
    let mut offset = 0;
    for (i, line) in changelog.split_inclusive('\n').enumerate() {
        if line.starts_with("## ") {
            match start {
                None => start = Some((i + 1, offset)),
                Some((first, from)) => return (first, &changelog[from..offset]),
            }
        }
        offset += line.len();
    }
    match start {
        Some((first, from)) => (first, &changelog[from..]),
        None => (1, changelog),
    }
}

#[derive(Default)]
struct Scan {
    text_files: Vec<String>,
    rust_files: Vec<String>,
    literals: usize,
    hits: Vec<Hit>,
}

fn scan(root: &Path) -> Scan {
    let mut scan = Scan::default();

    // ── Prose and the app's frontend, read whole ───────────────────────────────────
    let mut text = vec![
        root.join("README.md"),
        root.join("MESSAGING_CONTRACT.md"),
        root.join("benchmarks/README.md"),
        root.join("benchmarks/calibration/README.md"),
        root.join("DESIGN_CONTRACT.md"),
    ];
    let mut commands = Vec::new();
    walk(&root.join("commands"), &mut commands);
    text.extend(commands.into_iter().filter(|p| ext_is(p, &["md"])));
    let mut docs = Vec::new();
    walk(&root.join("docs"), &mut docs);
    text.extend(
        docs.into_iter()
            .filter(|p| ext_is(p, &["md", "html", "txt"])),
    );
    let mut app = Vec::new();
    walk(&root.join("lumenator/src"), &mut app);
    text.extend(
        app.into_iter()
            .filter(|p| ext_is(p, &["html", "ts"]) && !p.to_string_lossy().ends_with(".spec.ts")),
    );
    text.sort();
    for p in &text {
        let body = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
        let file = rel(root, p);
        find(&file, &body, 1, &mut scan.hits);
        scan.text_files.push(file);
    }

    // ── The changelog's newest entry only: older entries are history, and rewriting what
    // a release said would hide what it said. ─────────────────────────────────────────
    let changelog = std::fs::read_to_string(root.join("CHANGELOG.md")).expect("CHANGELOG.md");
    let (first_line, newest) = newest_entry(&changelog);
    find("CHANGELOG.md", newest, first_line, &mut scan.hits);
    scan.text_files.push("CHANGELOG.md".to_string());

    // ── Rust: the string literals of every crate's src, and the tray's ──────────────
    let mut rust_roots = vec![root.join("lumenator/src-tauri/src")];
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ is listable") {
        rust_roots.push(entry.expect("crates/ entry").path().join("src"));
    }
    let mut files = Vec::new();
    for r in rust_roots.iter().filter(|r| r.is_dir()) {
        walk(r, &mut files);
    }
    files.retain(|p| ext_is(p, &["rs"]));
    files.sort();
    for p in &files {
        let body = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
        let file = rel(root, p);
        for (line, lit) in rust_strings(&body) {
            scan.literals += 1;
            find(&file, &lit, line, &mut scan.hits);
        }
        scan.rust_files.push(file);
    }
    scan
}

/// What the scan must have read for a pass to mean anything.
fn premises(scan: &Scan) {
    for must in [
        "README.md",
        "docs/efficiency.md",
        "lumenator/src/app/pages/optimizer/optimizer.ts",
        "lumenator/src/app/pages/optimizer/optimizer.html",
        "DESIGN_CONTRACT.md",
        "commands/lumen-report.md",
        "CHANGELOG.md",
        "lumenator/src/app/pages/details/details.html",
        "lumenator/src/app/pages/details/details.ts",
        "lumenator/src/app/pages/settings/settings.html",
        "lumenator/src/app/pages/settings/settings.ts",
        "lumenator/src/app/pages/why/why.html",
        "lumenator/src/app/pages/why/why.ts",
        "lumenator/src/app/components/savings-headline/savings-headline.html",
        "lumenator/src/app/components/savings-headline/savings-headline.ts",
        "lumenator/src/app/components/billing-prompt/billing-prompt.html",
        "lumenator/src/app/components/billing-prompt/billing-prompt.ts",
        "lumenator/src/app/components/savings-status/savings-status.html",
        "lumenator/src/app/components/savings-status/savings-status.ts",
        "lumenator/src/app/pages/method/method.html",
        "lumenator/src/app/pages/method/method.ts",
        "lumenator/src/app/savings-text.ts",
    ] {
        assert!(
            scan.text_files.iter().any(|f| f == must),
            "{must} was not scanned; the walk has stopped seeing it"
        );
    }
    assert!(
        !scan
            .text_files
            .iter()
            .any(|f| f.ends_with(".spec.ts") || f.contains("node_modules")),
        "spec files and dependencies are not user-facing and must not be scanned"
    );
    for must in [
        "crates/lumen-cli/src/main.rs",
        "crates/lumen-cli/src/savings.rs",
        "crates/lumen-core/src/savings/text.rs",
        "crates/lumen-mcp/src/lib.rs",
        "lumenator/src-tauri/src/setup.rs",
    ] {
        assert!(
            scan.rust_files.iter().any(|f| f == must),
            "{must} was not scanned"
        );
    }
    assert!(
        scan.literals > 1000,
        "only {} string literals found; the lexer has stopped reading Rust",
        scan.literals
    );
}

#[test]
fn no_user_facing_text_calls_a_proxy_count_exact() {
    let root = workspace_root();
    let scan = scan(&root);
    premises(&scan);
    eprintln!(
        "scanned {} text files and {} string literals in {} Rust files; {} phrase(s) found; {} allowed",
        scan.text_files.len(),
        scan.literals,
        scan.rust_files.len(),
        scan.hits.len(),
        ALLOWED.len()
    );

    let mut used = vec![0usize; ALLOWED.len()];
    let mut problems = String::new();
    for hit in &scan.hits {
        let by: Vec<usize> = (0..ALLOWED.len())
            .filter(|&a| ALLOWED[a].file == hit.file && hit.context.contains(ALLOWED[a].needle))
            .collect();
        match by.as_slice() {
            [] => problems.push_str(&format!("  {hit}\n")),
            [a] => used[*a] += 1,
            _ => problems.push_str(&format!(
                "  covered by {} entries {by:?}: {hit}\n",
                by.len()
            )),
        }
    }
    for (a, n) in ALLOWED.iter().zip(&used).filter(|(a, n)| a.n != **n) {
        problems.push_str(&format!(
            "  ALLOWED entry for {} {:?} covers {n} hit(s), not {}: {}\n",
            a.file,
            a.needle,
            a.n,
            if *n == 0 {
                "it is stale — remove it"
            } else {
                "it now excuses text nobody looked at"
            },
        ));
    }
    assert!(
        problems.is_empty(),
        "\nUser-facing text claims a proxy token count is exact:\n{problems}\n\
         Lumen counts with cl100k_base, a proxy for Claude's tokenizer. Say what is counted, \
         with what, and how close it is (lumen_core::calibration, benchmarks/calibration). \
         If the text is about provider usage data or is not a claim at all, add it to \
         ALLOWED in {} saying why.",
        file!()
    );
}

#[test]
fn every_allowed_entry_states_a_reason() {
    for a in ALLOWED {
        assert!(
            !a.needle.trim().is_empty(),
            "{}: an empty needle covers the whole file",
            a.file
        );
        assert!(
            !a.why.trim().is_empty(),
            "{} {:?}: no reason given",
            a.file,
            a.needle
        );
        assert!(
            a.n > 0,
            "{} {:?}: covers nothing by design",
            a.file,
            a.needle
        );
    }
}

/// The matcher, against what it names and against lookalikes. Without the second half a
/// matcher that fires on everything passes.
#[test]
fn the_phrases_are_found_across_lines_and_cases_and_nothing_else() {
    let hits = |text: &str| {
        let mut h = Vec::new();
        find("t", text, 1, &mut h);
        h.into_iter()
            .map(|h| (h.line, h.phrase))
            .collect::<Vec<_>>()
    };
    assert_eq!(hits("Measured to the token."), vec![(1, "to the token")]);
    assert_eq!(
        hits("measured to the\n  token, never\nestimated"),
        vec![(1, "to the token"), (2, "never estimated")]
    );
    assert_eq!(hits("**Exact token** counts"), vec![(1, "exact token")]);
    assert_eq!(
        hits("Tokens are exact; latency is not."),
        vec![(1, "tokens are exact")]
    );
    assert_eq!(hits("a token-exact figure"), vec![(1, "token-exact")]);
    assert_eq!(hits("token\nexact"), vec![(1, "token exact")]);
    assert_eq!(hits("its exact count"), vec![(1, "exact count")]);
    assert!(
        hits("passed to the tokenizer").is_empty(),
        "a word boundary is required"
    );
    assert!(hits("tokens are exactly as before").is_empty());
    assert!(hits("unexact counting").is_empty());
    assert!(hits("estimated, and labelled so").is_empty());
}

#[test]
fn string_literals_are_read_and_comments_are_not() {
    let src = r####"
// "to the token" in a comment
/* "never estimated" /* nested */ still a comment */
let c = '"';
fn f<'a>(x: &'a str) {}
let a = "first";
let b = r#"raw "quoted" text"#;
let d = "continued \
         line";
let e = b"bytes";
"####;
    let lits: Vec<(usize, String)> = rust_strings(src);
    let texts: Vec<&str> = lits.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "first",
            r#"raw "quoted" text"#,
            "continued           line",
            "bytes"
        ]
    );
    assert_eq!(
        lits[0].0, 6,
        "line numbers survive comments and char literals"
    );
    assert_eq!(lits[3].0, 10, "and multi-line strings");
}

#[test]
fn only_the_newest_changelog_entry_is_read() {
    let log = "# Changelog\n\n## [Unreleased]\nnew text\n\n## [1.8.0] — 2026-10-13\nold text\n";
    assert_eq!(newest_entry(log), (3, "## [Unreleased]\nnew text\n\n"));
    assert_eq!(
        newest_entry("## [1.0.0]\nonly\n"),
        (1, "## [1.0.0]\nonly\n")
    );
    assert_eq!(newest_entry("no headings"), (1, "no headings"));
}
