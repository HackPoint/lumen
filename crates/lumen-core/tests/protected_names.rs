//! No tracked file names the company Lumen's app identifier once carried, or a related product,
//! project or person.
//!
//! The words are written nowhere in the repository, not even in pieces: this file holds only
//! their SHA-256. Every tracked file, text or binary, is lowercased and every run of letters
//! long enough is searched window by window, so a word inside a longer one
//! (`io.<word>.lumen`, `<word>-ws`, `/Users/<word>/`) is found as well as a bare one.
//!
//! The scan is a function over `(path, bytes)` pairs so the tests can plant a made-up word and
//! watch it fail; the last test runs it over the repository. There is no allow-list.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// What may not appear: a name for the message, the word's length, and the SHA-256 of the word
/// in lower case.
const PROTECTED: [(&str, usize, &str); 4] = [
    (
        "the company in the old app identifier",
        8,
        "091aa63110cac1f0e0a8c9179222b3e9a37642c887dd45d072a7e19afb2b9c08",
    ),
    (
        "a related product name",
        8,
        "76e9dd3a9dffb7010202c21ee96a0eeb4b0c776811c69d978d8271e6b1112964",
    ),
    (
        "a private project name",
        9,
        "7cbfef7507d823c8c741769d741312c2e2d28ed8791f9a30cee7f554249f65d5",
    ),
    (
        "a personal username",
        8,
        "f86249a9c32b86f331dc2ea90095dcf324a2fd304401ce501a4c413487943bed",
    ),
];

struct Needle {
    what: String,
    len: usize,
    hash: [u8; 32],
}

fn hex32(s: &str) -> [u8; 32] {
    assert_eq!(s.len(), 64, "{s}");
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn protected() -> Vec<Needle> {
    PROTECTED
        .iter()
        .map(|&(what, len, hash)| Needle {
            what: what.to_string(),
            len,
            hash: hex32(hash),
        })
        .collect()
}

/// The needles of words chosen by a test.
fn made_up(words: &[&str]) -> Vec<Needle> {
    words
        .iter()
        .map(|w| Needle {
            what: format!("the word {w}"),
            len: w.len(),
            hash: Sha256::digest(w.as_bytes()).into(),
        })
        .collect()
}

/// What `bytes` holds, as the names of the needles found (each once).
fn found_in(bytes: &[u8], needles: &[Needle]) -> Vec<String> {
    let lower = bytes.to_ascii_lowercase();
    let shortest = needles.iter().map(|n| n.len).min().unwrap_or(usize::MAX);
    let mut hits: Vec<String> = Vec::new();
    for run in lower.split(|b| !b.is_ascii_lowercase()) {
        if run.len() < shortest {
            continue;
        }
        for n in needles {
            if run.len() < n.len || hits.contains(&n.what) {
                continue;
            }
            if run
                .windows(n.len)
                .any(|w| <[u8; 32]>::from(Sha256::digest(w)) == n.hash)
            {
                hits.push(n.what.clone());
            }
        }
    }
    hits
}

/// Every occurrence in `files`, as `path: what`.
fn scan(files: &[(String, Vec<u8>)], needles: &[Needle]) -> Vec<String> {
    files
        .iter()
        .flat_map(|(path, bytes)| {
            found_in(bytes, needles)
                .into_iter()
                .map(move |what| format!("{path}: {what}"))
        })
        .collect()
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// The tracked files, or, outside a git checkout, every file but build output.
fn tracked_files(root: &Path) -> Vec<String> {
    let listed = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success());
    match listed {
        Some(o) => o
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect(),
        None => {
            let mut out = Vec::new();
            walk(root, root, &mut out);
            out
        }
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !matches!(
                name,
                "target" | "node_modules" | "dist" | ".angular" | ".git"
            ) {
                walk(root, &p, out);
            }
        } else {
            out.push(
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
}

#[test]
fn no_tracked_file_names_a_protected_word() {
    let root = root();
    let files: Vec<(String, Vec<u8>)> = tracked_files(&root)
        .into_iter()
        // A symlink or a file removed in the working tree is not content.
        .filter_map(|p| std::fs::read(root.join(&p)).ok().map(|b| (p, b)))
        .collect();
    assert!(
        files.len() > 100,
        "only {} files found; is the listing broken?",
        files.len()
    );
    let found = scan(&files, &protected());
    assert!(
        found.is_empty(),
        "remove these; Lumen has no relation to them:\n  {}",
        found.join("\n  ")
    );
}

// ── The guard itself, on planted words ──────────────────────────────────────

fn plants(path: &str, text: &[u8]) -> Vec<String> {
    scan(
        &[(path.to_string(), text.to_vec())],
        &made_up(&["acmecorp", "zedaxis"]),
    )
}

#[test]
fn a_word_is_caught_in_any_case_and_inside_a_longer_one() {
    for text in [
        "Copyright (c) 2026 AcmeCorp",
        "ACMECORP",
        "io.acmecorp.lumen",
        "/Users/x/dev/acmecorp-ws/app",
        "user@acmecorp.example",
        "theacmecorpvpn",
    ] {
        let got = plants("LICENSE", text.as_bytes());
        assert_eq!(
            got,
            vec!["LICENSE: the word acmecorp".to_string()],
            "{text}"
        );
    }
    let got = plants("docs/x.md", b"zedaxis and acmecorp");
    assert_eq!(got.len(), 2, "{got:?}");
}

#[test]
fn a_word_split_by_anything_but_a_letter_is_not_the_word() {
    for text in [
        "acme corp",
        "acme-corp",
        "acme_corp",
        "acme1corp",
        "acmecor",
    ] {
        assert_eq!(
            plants("x.rs", text.as_bytes()),
            Vec::<String>::new(),
            "{text}"
        );
    }
}

#[test]
fn a_binary_file_is_scanned_too() {
    let mut bytes = vec![0u8, 159, 146, 150, 0];
    bytes.extend_from_slice(b"ACMEcorp");
    bytes.extend_from_slice(&[0, 255]);
    assert_eq!(plants("x.png", &bytes).len(), 1);
}

#[test]
fn the_protected_list_is_four_distinct_words_with_real_hashes() {
    let n = protected();
    assert_eq!(n.len(), 4);
    for (i, a) in n.iter().enumerate() {
        assert!(a.len >= 8, "{}", a.what);
        assert!(n[i + 1..].iter().all(|b| b.hash != a.hash), "{}", a.what);
    }
    // A planted word that hashes to a protected entry would be the word itself; the guard is
    // only as good as its hashes, so check one is not the hash of an empty or trivial string.
    let trivial: [u8; 32] = Sha256::digest(b"").into();
    assert!(n.iter().all(|x| x.hash != trivial));
}
