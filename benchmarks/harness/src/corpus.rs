//! The corpus: three repositories pinned by full commit SHA, fetched, verified and extracted
//! outside the checkout.
//!
//! Nothing here trusts a name. A repository is fetched by `git fetch --depth 1 <url> <sha>`
//! and the object that arrives must be exactly that commit, or the run stops. Files come out of
//! the object database byte for byte (`git cat-file --batch`), never through a working-tree
//! checkout, so `core.autocrlf`, `.gitattributes` and the platform's line-ending policy cannot
//! change a single token count. The identity of a corpus is a SHA-256 over the path and git blob
//! id of every measured file, which anyone can recompute with `git ls-tree`.

use crate::util::{Ctx, Res, run_text, sha256_hex};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One pinned repository.
pub struct Pin {
    pub name: &'static str,
    pub url: &'static str,
    pub license: &'static str,
    /// The full 40-character commit SHA. Never a branch or a tag.
    pub sha: &'static str,
    /// What the pin is, for the README table.
    pub note: &'static str,
}

pub const PINS: &[Pin] = &[
    Pin {
        name: "lumen",
        url: "https://github.com/HackPoint/lumen",
        license: "MIT",
        sha: "3b951e93dbe55e46dc0e15951666b303110861e7",
        note: "this repository (Rust, plus the app's TypeScript), main before the benchmarks landed",
    },
    Pin {
        name: "react-hook-form",
        url: "https://github.com/react-hook-form/react-hook-form",
        license: "MIT",
        sha: "8ac7cc0bcc1f50f887230d50b1929d977b4d3fb4",
        note: "TypeScript and TSX, tag v7.90.0",
    },
    Pin {
        name: "pytest",
        url: "https://github.com/pytest-dev/pytest",
        license: "MIT",
        sha: "cf470ec0bf7eb89cd97dd56df4859eae5db46447",
        note: "Python, tag 9.1.1",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
}

pub const LANGS: [Lang; 4] = [Lang::Rust, Lang::Python, Lang::TypeScript, Lang::Tsx];

impl Lang {
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
        }
    }

    pub fn from_name(s: &str) -> Option<Lang> {
        LANGS.into_iter().find(|l| l.name() == s)
    }

    /// The language of a path by extension, for the files the server can outline. `.d.ts`
    /// declaration files are excluded: they are generated and carry no bodies.
    pub fn of_path(rel: &str) -> Option<Lang> {
        if rel.ends_with(".d.ts") {
            return None;
        }
        match rel.rsplit_once('.')?.1 {
            "rs" => Some(Lang::Rust),
            "py" => Some(Lang::Python),
            "ts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            _ => None,
        }
    }

    /// The comment marker used by the deterministic edit.
    pub fn comment(self) -> &'static str {
        match self {
            Lang::Python => "#",
            _ => "//",
        }
    }
}

/// Directory names that are never measured: dependencies and build output.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "third_party",
    "__pycache__",
    ".git",
    ".yarn",
];

/// File-size buckets, by newline count: the same count the intercept hook compares to its
/// 300-line threshold. Below 300 the hook lets a Read through, so that bucket shows what a
/// smart_read call costs on a file that would not have been intercepted.
pub struct Bucket {
    pub name: &'static str,
    pub min: usize,
    /// Inclusive; `None` is open-ended.
    pub max: Option<usize>,
}

pub const BUCKETS: [Bucket; 4] = [
    Bucket {
        name: "<300",
        min: 0,
        max: Some(299),
    },
    Bucket {
        name: "300-999",
        min: 300,
        max: Some(999),
    },
    Bucket {
        name: "1000-2999",
        min: 1000,
        max: Some(2999),
    },
    Bucket {
        name: "3000+",
        min: 3000,
        max: None,
    },
];

pub fn bucket_of(lines: usize) -> usize {
    BUCKETS
        .iter()
        .position(|b| lines >= b.min && b.max.is_none_or(|m| lines <= m))
        .expect("the buckets cover every line count")
}

/// A file the benchmark measures.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Path relative to the corpus root, `/`-separated.
    pub rel: String,
    pub lang: Lang,
    /// Newline bytes (`wc -l`).
    pub lines: usize,
}

pub struct Corpus {
    pub name: String,
    pub url: String,
    pub license: String,
    pub sha: String,
    pub note: String,
    pub root: PathBuf,
    pub files: Vec<SourceFile>,
    /// SHA-256 over `path TAB blob-id NL` of every candidate file, sorted by path.
    pub fingerprint: String,
    /// Where the commit came from on this run: the cache, a local checkout, or the URL.
    pub fetched_from: String,
    /// Candidate files in the tree, before the UTF-8 filter.
    pub candidates: usize,
    /// Candidates the server cannot read as text, and why.
    pub skipped: Vec<(String, String)>,
}

impl Corpus {
    pub fn abs(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

/// A corpus that already sits on disk: a fixture. It has no commit, so it has no pin.
pub fn from_dir(name: &str, root: &Path) -> Res<Corpus> {
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut rels = Vec::new();
    list_files(root, root, &mut rels)?;
    rels.sort();
    let mut fp = String::new();
    for rel in &rels {
        let Some(lang) = Lang::of_path(rel) else {
            continue;
        };
        let bytes = std::fs::read(root.join(rel)).ctx(&format!("read {rel}"))?;
        fp.push_str(&format!("{rel}\t{}\n", sha256_hex(&bytes)));
        match std::str::from_utf8(&bytes) {
            Ok(_) => files.push(SourceFile {
                rel: rel.clone(),
                lang,
                lines: bytes.iter().filter(|&&b| b == b'\n').count(),
            }),
            Err(e) => skipped.push((rel.clone(), format!("not UTF-8 ({e})"))),
        }
    }
    Ok(Corpus {
        name: name.to_string(),
        url: "local fixture".to_string(),
        license: "n/a".to_string(),
        sha: "none".to_string(),
        note: "a directory on disk, not a pinned repository".to_string(),
        root: root.to_path_buf(),
        candidates: files.len() + skipped.len(),
        files,
        fingerprint: sha256_hex(fp.as_bytes()),
        fetched_from: "local directory".to_string(),
        skipped,
    })
}

fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Res<()> {
    for entry in std::fs::read_dir(dir).ctx(&format!("list {}", dir.display()))? {
        let entry = entry.ctx("list a directory entry")?;
        let path = entry.path();
        let kind = entry.file_type().ctx("stat a directory entry")?;
        if kind.is_dir() {
            list_files(root, &path, out)?;
        } else if kind.is_file() {
            let rel = path
                .strip_prefix(root)
                .ctx("a path outside its root")?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

struct Entry {
    rel: String,
    oid: String,
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).env("GIT_TERMINAL_PROMPT", "0");
    c
}

fn commit_present(dir: &Path, sha: &str) -> bool {
    git(dir)
        .args(["cat-file", "-e"])
        .arg(format!("{sha}^{{commit}}"))
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `LUMEN_BENCH_SOURCE_<NAME>` (a URL or a path) is tried first, so a mirror or an offline copy
/// can stand in. Whatever it serves must still be the pinned commit.
fn env_key(name: &str) -> String {
    format!(
        "LUMEN_BENCH_SOURCE_{}",
        name.to_ascii_uppercase().replace('-', "_")
    )
}

fn sources(pin: &Pin, local_checkout: Option<&Path>) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(s) = std::env::var(env_key(pin.name))
        && !s.trim().is_empty()
    {
        v.push(s);
    }
    if pin.name == "lumen"
        && let Some(dir) = local_checkout
        && commit_present(dir, pin.sha)
    {
        v.push(dir.to_string_lossy().into_owned());
    }
    v.push(pin.url.to_string());
    v
}

/// Fetch (if needed), verify, and extract one pinned repository.
pub fn load(pin: &Pin, cache: &Path, local_checkout: Option<&Path>) -> Res<Corpus> {
    if pin.sha.len() != 40 || !pin.sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "{}: the pin {:?} is not a full 40-character commit SHA",
            pin.name, pin.sha
        ));
    }
    let bare = cache.join("git").join(pin.name);
    if !bare.join("HEAD").exists() {
        std::fs::create_dir_all(&bare).ctx(&format!("create {}", bare.display()))?;
        run_text(
            Command::new("git")
                .args(["init", "--bare", "-q"])
                .arg(&bare),
        )?;
    }

    let fetched_from = if commit_present(&bare, pin.sha) {
        "cache".to_string()
    } else {
        let mut errors = Vec::new();
        let mut got = None;
        for source in sources(pin, local_checkout) {
            eprintln!(
                "lumen-bench: fetching {} @ {} from {source}",
                pin.name, pin.sha
            );
            let fetched = run_text(
                git(&bare)
                    .args(["fetch", "--depth", "1", "--no-tags", "--quiet"])
                    .arg(&source)
                    .arg(pin.sha),
            );
            match fetched {
                Ok(_) => {
                    got = Some(source);
                    break;
                }
                Err(e) => errors.push(format!("  {source}: {e}")),
            }
        }
        got.ok_or_else(|| {
            format!(
                "could not fetch {} at {}:\n{}",
                pin.name,
                pin.sha,
                errors.join("\n")
            )
        })?
    };

    // The object that arrived must be the commit that was pinned. `rev-parse` resolves the
    // name through the object database, so this fails if the SHA is absent rather than
    // accepting whatever FETCH_HEAD happens to hold.
    let resolved = run_text(
        git(&bare)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{}^{{commit}}", pin.sha)),
    )
    .ctx(&format!(
        "{}: the pinned commit is not in the fetched repository",
        pin.name
    ))?;
    if resolved.trim() != pin.sha {
        return Err(format!(
            "{}: pinned {} but the repository resolved it to {}",
            pin.name,
            pin.sha,
            resolved.trim()
        ));
    }
    // Keeps the commit reachable, so a `git gc` cannot prune the cache out from under a run.
    run_text(
        git(&bare)
            .args(["update-ref", "refs/lumen-bench/pin"])
            .arg(pin.sha),
    )?;

    let entries = tree_entries(&bare, pin.sha)?;
    let mut fp = String::new();
    for e in &entries {
        fp.push_str(&format!("{}\t{}\n", e.rel, e.oid));
    }
    let fingerprint = sha256_hex(fp.as_bytes());

    let root = cache
        .join("work")
        .join(format!("{}-{}", pin.name, &pin.sha[..12]));
    let manifest = cache
        .join("work")
        .join(format!("{}-{}.manifest", pin.name, &pin.sha[..12]));
    ensure_worktree(&bare, &root, &manifest, &entries, &fingerprint)?;

    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for e in &entries {
        let bytes = std::fs::read(root.join(&e.rel)).ctx(&format!("read {}", e.rel))?;
        let lang = Lang::of_path(&e.rel).ok_or_else(|| format!("{}: not a source file", e.rel))?;
        match std::str::from_utf8(&bytes) {
            Ok(_) => files.push(SourceFile {
                rel: e.rel.clone(),
                lang,
                lines: bytes.iter().filter(|&&b| b == b'\n').count(),
            }),
            Err(err) => skipped.push((e.rel.clone(), format!("not UTF-8 ({err})"))),
        }
    }

    Ok(Corpus {
        name: pin.name.to_string(),
        url: pin.url.to_string(),
        license: pin.license.to_string(),
        sha: pin.sha.to_string(),
        note: pin.note.to_string(),
        root,
        candidates: entries.len(),
        files,
        fingerprint,
        fetched_from,
        skipped,
    })
}

fn tree_entries(bare: &Path, sha: &str) -> Res<Vec<Entry>> {
    let out = git(bare)
        .args(["ls-tree", "-r", "-z", sha])
        .output()
        .ctx("run git ls-tree")?;
    if !out.status.success() {
        return Err(format!(
            "git ls-tree failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut entries = Vec::new();
    for rec in out.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let text = std::str::from_utf8(rec).ctx("git ls-tree printed a non-UTF-8 path")?;
        let (meta, rel) = text
            .split_once('\t')
            .ok_or_else(|| format!("unparseable ls-tree record {text:?}"))?;
        let mut m = meta.split(' ');
        let (mode, kind, oid) = (m.next(), m.next(), m.next());
        let (Some(mode), Some(kind), Some(oid)) = (mode, kind, oid) else {
            return Err(format!("unparseable ls-tree record {text:?}"));
        };
        // Regular files only: a symlink's blob is its target, and a gitlink is another repo.
        if kind != "blob" || !(mode == "100644" || mode == "100755") {
            continue;
        }
        if Lang::of_path(rel).is_none() {
            continue;
        }
        if rel.split('/').any(|part| SKIP_DIRS.contains(&part)) {
            continue;
        }
        entries.push(Entry {
            rel: rel.to_string(),
            oid: oid.to_string(),
        });
    }
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    if entries.is_empty() {
        return Err(format!("{sha}: the tree holds no source files"));
    }
    Ok(entries)
}

/// Make `root` hold exactly the entries, byte for byte. A manifest records the SHA-256 of every
/// extracted file, so a file an interrupted run left edited is detected and the tree rebuilt.
fn ensure_worktree(
    bare: &Path,
    root: &Path,
    manifest: &Path,
    entries: &[Entry],
    fingerprint: &str,
) -> Res<()> {
    if worktree_intact(root, manifest, entries, fingerprint)? {
        return Ok(());
    }
    if root.exists() {
        std::fs::remove_dir_all(root).ctx(&format!("clear {}", root.display()))?;
    }
    std::fs::create_dir_all(root).ctx(&format!("create {}", root.display()))?;

    let mut child = git(bare)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ctx("start git cat-file")?;
    let mut stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
    let stdout = child.stdout.take().ok_or("git cat-file has no stdout")?;
    let oids: Vec<String> = entries.iter().map(|e| e.oid.clone()).collect();
    // Fed from a thread: git answers as it reads, and a full pipe in either direction would
    // otherwise stop both ends.
    let feeder = std::thread::spawn(move || -> std::io::Result<()> {
        for oid in oids {
            writeln!(stdin, "{oid}")?;
        }
        Ok(())
    });
    let mut out = BufReader::new(stdout);
    let mut lines = format!("fingerprint {fingerprint}\n");
    for e in entries {
        let mut header = String::new();
        out.read_line(&mut header)
            .ctx("read a git cat-file header")?;
        let mut h = header.split_whitespace();
        let (oid, kind, size) = (h.next(), h.next(), h.next());
        let (Some(oid), Some(kind), Some(size)) = (oid, kind, size) else {
            return Err(format!("git cat-file answered {header:?} for {}", e.rel));
        };
        if oid != e.oid || kind != "blob" {
            return Err(format!("git cat-file answered {header:?} for {}", e.rel));
        }
        let size: usize = size.parse().ctx("a git cat-file size")?;
        let mut body = vec![0u8; size];
        out.read_exact(&mut body)
            .ctx(&format!("read the blob of {}", e.rel))?;
        let mut nl = [0u8; 1];
        out.read_exact(&mut nl)
            .ctx("read the newline after a blob")?;
        let dest = root.join(&e.rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
        }
        std::fs::write(&dest, &body).ctx(&format!("write {}", dest.display()))?;
        lines.push_str(&format!("{} {}\n", sha256_hex(&body), e.rel));
    }
    feeder
        .join()
        .map_err(|_| "the git cat-file feeder panicked".to_string())?
        .ctx("feed git cat-file")?;
    let status = child.wait().ctx("wait for git cat-file")?;
    if !status.success() {
        return Err(format!("git cat-file exited with {status}"));
    }
    if let Some(parent) = manifest.parent() {
        std::fs::create_dir_all(parent).ctx(&format!("create {}", parent.display()))?;
    }
    std::fs::write(manifest, lines).ctx(&format!("write {}", manifest.display()))
}

fn worktree_intact(
    root: &Path,
    manifest: &Path,
    entries: &[Entry],
    fingerprint: &str,
) -> Res<bool> {
    if !root.is_dir() || !manifest.is_file() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(manifest).ctx(&format!("read {}", manifest.display()))?;
    let mut lines = text.lines();
    if lines.next() != Some(&format!("fingerprint {fingerprint}")) {
        return Ok(false);
    }
    let recorded: Vec<&str> = lines.collect();
    if recorded.len() != entries.len() {
        return Ok(false);
    }
    for (line, e) in recorded.iter().zip(entries) {
        let Some((digest, rel)) = line.split_once(' ') else {
            return Ok(false);
        };
        if rel != e.rel {
            return Ok(false);
        }
        match std::fs::read(root.join(rel)) {
            Ok(bytes) if sha256_hex(&bytes) == digest => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_partition_every_line_count() {
        assert_eq!(bucket_of(0), 0);
        assert_eq!(bucket_of(299), 0);
        assert_eq!(bucket_of(300), 1);
        assert_eq!(bucket_of(999), 1);
        assert_eq!(bucket_of(1000), 2);
        assert_eq!(bucket_of(2999), 2);
        assert_eq!(bucket_of(3000), 3);
        assert_eq!(bucket_of(1_000_000), 3);
    }

    #[test]
    fn languages_come_from_the_extension_and_declaration_files_are_excluded() {
        assert_eq!(Lang::of_path("a/b.rs"), Some(Lang::Rust));
        assert_eq!(Lang::of_path("a/b.py"), Some(Lang::Python));
        assert_eq!(Lang::of_path("a/b.ts"), Some(Lang::TypeScript));
        assert_eq!(Lang::of_path("a/b.tsx"), Some(Lang::Tsx));
        assert_eq!(Lang::of_path("a/b.d.ts"), None);
        assert_eq!(Lang::of_path("a/b.md"), None);
        assert_eq!(Lang::of_path("Makefile"), None);
    }

    #[test]
    fn every_pin_is_a_full_sha() {
        for p in PINS {
            assert_eq!(p.sha.len(), 40, "{}", p.name);
            assert!(p.sha.bytes().all(|b| b.is_ascii_hexdigit()), "{}", p.name);
        }
    }
}
