//! The identifier the app had before 1.10.0 lives in the migration module and nowhere else.
//!
//! `legacy::LEGACY_APP_ID` is the one place that names the old data directory, because the
//! migration has to find it. Everything else derives the current identifier from
//! `meter::APP_ID`, or says "the legacy data directory". This fails, case-insensitively and
//! across every tracked file, on:
//!
//! - the whole legacy identifier;
//! - the bare name of the company inside it (taken from the constant, never written here);
//! - a related product name and the owner's username, which an earlier scan list and a
//!   scrubbed commit once carried (both assembled from parts, so this file does not match
//!   itself).
//!
//! Allowed only in `crates/lumen-core/src/legacy.rs` and `crates/lumen-core/src/legacy/`, and,
//! for the whole identifier only, in the 1.10.0 block of CHANGELOG.md (the release notes say
//! what resets for a user of the old one).
//!
//! The scan is a function over `(path, bytes)` pairs so the tests can plant an occurrence and
//! watch it fail; the last test runs it over the repository.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// What may not appear, with a name for the message. Lower case.
fn needles(legacy_id: &str) -> Vec<(&'static str, String)> {
    let company = legacy_id
        .split('.')
        .nth(1)
        .expect("the legacy identifier is io.<company>.<app>")
        .to_ascii_lowercase();
    assert!(company.len() >= 4, "company name {company:?}");
    vec![
        ("the legacy identifier", legacy_id.to_ascii_lowercase()),
        ("the company name", company),
        ("a related product name", String::from("redacted")),
        ("a private project name", String::from("redacted")),
        ("the owner's username", String::from("redacted")),
    ]
}

fn allowed_everywhere(path: &str) -> bool {
    path == "crates/lumen-core/src/legacy.rs" || path.starts_with("crates/lumen-core/src/legacy/")
}

/// The text of the 1.10.0 block of the changelog: from its `## [1.10.0` heading to the next
/// `## [` heading.
fn changelog_block(text: &str) -> Option<std::ops::Range<usize>> {
    let start = text
        .match_indices("## [1.10.0")
        .map(|(i, _)| i)
        .find(|&i| i == 0 || text.as_bytes()[i - 1] == b'\n')?;
    let end = text[start + 3..]
        .match_indices("\n## [")
        .next()
        .map_or(text.len(), |(i, _)| start + 3 + i);
    Some(start..end)
}

/// Every occurrence in `files`, as `path: what`.
fn scan(files: &[(String, Vec<u8>)], legacy_id: &str) -> Vec<String> {
    let needles = needles(legacy_id);
    let mut found = Vec::new();
    for (path, bytes) in files {
        if allowed_everywhere(path) {
            continue;
        }
        let mut lower = bytes.to_ascii_lowercase();
        if path == "CHANGELOG.md"
            && let Ok(text) = std::str::from_utf8(&lower)
            && let Some(block) = changelog_block(text)
        {
            // In the 1.10.0 block the whole identifier is allowed: blank it out, so the
            // company name inside it does not count, while a bare one still does.
            let id = needles[0].1.as_bytes();
            let mut i = block.start;
            while i + id.len() <= block.end {
                if &lower[i..i + id.len()] == id {
                    lower[i..i + id.len()].fill(b' ');
                    i += id.len();
                } else {
                    i += 1;
                }
            }
        }
        for (what, needle) in &needles {
            if lower.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                found.push(format!("{path}: {what}"));
            }
        }
    }
    found
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
fn no_tracked_file_outside_the_migration_module_names_the_old_identifier() {
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
    let found = scan(&files, lumen_core::legacy::LEGACY_APP_ID);
    assert!(
        found.is_empty(),
        "reword these to the current identifier (`meter::APP_ID`) or \"the legacy data \
         directory\"; only the migration module may name the old one:\n  {}",
        found.join("\n  ")
    );
}

// ── The guard itself, on planted occurrences ─────────────────────────────────

/// A legacy identifier made up for the tests, so they plant something real in shape.
const FAKE: &str = "io.acmecorp.lumen";

fn file(path: &str, text: &str) -> (String, Vec<u8>) {
    (path.to_string(), text.as_bytes().to_vec())
}

fn plants(path: &str, text: &str) -> Vec<String> {
    scan(&[file(path, text)], FAKE)
}

#[test]
fn the_whole_identifier_is_caught_in_any_case_and_any_file() {
    for text in [
        "~/Library/Application Support/io.acmecorp.lumen/lumen.db",
        "IO.ACMECORP.LUMEN",
        "x Io.AcmeCorp.Lumen y",
    ] {
        for path in ["README.md", "lumenator/e2e/app.e2e.mjs", "scripts/x.sh"] {
            assert!(!plants(path, text).is_empty(), "{path}: {text}");
        }
    }
}

#[test]
fn the_bare_company_name_is_caught_wherever_it_hides() {
    for text in [
        "Copyright (c) 2026 AcmeCorp",
        "ACMECORP",
        "the-acmecorp-vpn",
        "https://acmecorp.example/x",
    ] {
        let got = plants("LICENSE", text);
        assert_eq!(got.len(), 1, "{text}: {got:?}");
        assert!(got[0].contains("company name"), "{got:?}");
    }
}

#[test]
fn the_product_name_and_the_username_are_caught() {
    let product = String::from("redacted");
    let user = String::from("redacted");
    let got = plants("benchmarks/x.rs", &format!("scan for {product} here"));
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("product name"), "{got:?}");
    let got = plants("docs/y.md", &format!("/Users/{user}/dev"));
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].contains("username"), "{got:?}");
}

#[test]
fn a_binary_file_is_scanned_too() {
    let mut bytes = vec![0u8, 159, 146, 150, 0];
    bytes.extend_from_slice(FAKE.as_bytes());
    bytes.extend_from_slice(&[0, 255]);
    assert!(!scan(&[("x.png".into(), bytes)], FAKE).is_empty());
}

#[test]
fn the_migration_module_may_name_it() {
    for path in [
        "crates/lumen-core/src/legacy.rs",
        "crates/lumen-core/src/legacy/merge.rs",
        "crates/lumen-core/src/legacy/anything/deeper.rs",
    ] {
        assert_eq!(plants(path, FAKE), Vec::<String>::new(), "{path}");
    }
    // Beside it, and under a similar name, it may not.
    for path in [
        "crates/lumen-core/src/legacy_other.rs",
        "crates/lumen-core/src/meter.rs",
        "crates/lumen-core/tests/legacy_migrate.rs",
    ] {
        assert!(!plants(path, FAKE).is_empty(), "{path}");
    }
}

#[test]
fn the_1_10_0_changelog_block_may_hold_the_whole_identifier_and_nothing_else() {
    let log = |block: &str| {
        format!(
            "# Changelog\n\n## [1.11.0] — soon\n\nnothing\n\n## [1.10.0] — soon\n\n{block}\n\n\
             ## [1.9.0] — 2026-10-18\n\nold text\n"
        )
    };
    // The whole identifier, in any case, inside the block: fine.
    assert_eq!(
        plants(
            "CHANGELOG.md",
            &log("Data moves from io.acmecorp.lumen and IO.ACMECORP.LUMEN.")
        ),
        Vec::<String>::new()
    );
    // The bare name inside the block is not.
    assert!(!plants("CHANGELOG.md", &log("Made by AcmeCorp.")).is_empty());
    // Another file with the same text is not.
    assert!(!plants("README.md", &log("Data moves from io.acmecorp.lumen.")).is_empty());
    // Neither is the whole identifier in any other block.
    let other =
        "# Changelog\n\n## [1.10.0] — soon\n\nfine\n\n## [1.9.0]\n\nfrom io.acmecorp.lumen\n";
    assert!(!plants("CHANGELOG.md", other).is_empty());
    let above = "# Changelog\n\nfrom io.acmecorp.lumen\n\n## [1.10.0] — soon\n\nfine\n";
    assert!(!plants("CHANGELOG.md", above).is_empty());
    // With no 1.10.0 block, there is no allowance.
    assert!(!plants("CHANGELOG.md", "## [1.9.0]\n\nio.acmecorp.lumen\n").is_empty());
}

#[test]
fn clean_text_passes() {
    assert_eq!(
        plants(
            "README.md",
            "Data lives in io.hackpoint.lumen and the legacy data directory."
        ),
        Vec::<String>::new()
    );
}

#[test]
fn the_needles_come_from_the_constant() {
    let n = needles(lumen_core::legacy::LEGACY_APP_ID);
    assert_eq!(
        n[0].1,
        lumen_core::legacy::LEGACY_APP_ID.to_ascii_lowercase()
    );
    assert!(n[0].1.contains(&n[1].1));
    assert_ne!(
        n[1].1, "hackpoint",
        "the company is the old one, not the current"
    );
}

// ── The flip itself ──────────────────────────────────────────────────────────

/// `tauri.conf.json` cannot import the constant, so the two are held equal here: the GUI, the
/// daemon and the hooks must agree on one data directory.
#[test]
fn the_apps_identifier_is_the_constant_the_hooks_use() {
    let conf = std::fs::read_to_string(root().join("lumenator/src-tauri/tauri.conf.json")).unwrap();
    let conf: serde_json::Value = serde_json::from_str(&conf).unwrap();
    assert_eq!(
        conf["identifier"].as_str(),
        Some(lumen_core::meter::APP_ID),
        "tauri.conf.json `identifier` and meter::APP_ID drifted apart"
    );
    assert_ne!(lumen_core::meter::APP_ID, lumen_core::legacy::LEGACY_APP_ID);
    assert_eq!(
        conf["bundle"]["publisher"].as_str(),
        Some("HackPoint"),
        "Windows shows the publisher; without it Tauri derives one from the identifier"
    );
}

/// Shell scripts cannot import it either. These two resolve the ledger without the binary, and
/// name the data directory themselves.
#[test]
fn the_shell_scripts_that_resolve_the_ledger_use_the_current_identifier() {
    let root = root();
    for rel in [
        ".claude/lumen_report.sh",
        ".claude/hooks/lumen_meter.sh",
        ".claude/hooks/lumen_read_intercept.sh",
    ] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        assert!(
            text.contains(lumen_core::meter::APP_ID),
            "{rel} does not name {}",
            lumen_core::meter::APP_ID
        );
    }
}
