//! Every version-bearing file in the repository must agree.
//!
//! Three separate drifts reached users or nearly did, all the same shape — a file the
//! release script did not know about:
//!
//!   - `crates/lumen-stats` was absent from the script's crate list, so no release ever
//!     bumped it and it only stayed in step when someone noticed by hand.
//!   - `Casks/lumen.rb` was renamed to `Casks/lumen-app.rb` and the script was not
//!     updated, so the bump failed under `set -e` and aborted partway through — leaving
//!     some versions changed and others not, which is precisely why it went unnoticed.
//!   - `.claude-plugin/plugin.json` was never in the list at all. Users read that version
//!     in `/plugin`, so a stale one misreports which build's hooks they are running.
//!
//! Adding a version file and forgetting the script is evidently easy. This test makes it
//! loud instead: it discovers the files rather than being told about them, so a new crate
//! is covered the moment it exists.

use std::path::{Path, PathBuf};

/// Walk up from this crate to the workspace root.
fn workspace_root() -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(d) = dir {
        if d.join("Cargo.toml").is_file() && d.join("crates").is_dir() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// First `version = "…"` or `version "…"` in a file, whichever style it uses.
fn version_in(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let t = line.trim();
        // `version = "x"` (Cargo, TOML) or `version "x"` (Homebrew Ruby DSL).
        // `continue`, not `?`: the latter returns from the function on the first line
        // that is not a version line, which for a Cargo.toml is `[package]`.
        let Some(rest) = t
            .strip_prefix("version = \"")
            .or_else(|| t.strip_prefix("version \""))
        else {
            continue;
        };
        return rest.split('"').next().map(str::to_string);
    }
    None
}

fn json_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    // Deliberately not a JSON parser: this crate has serde_json, but a hand-rolled scan
    // keeps the test honest about reading the bytes on disk rather than a re-serialised
    // view of them.
    let key = "\"version\"";
    let i = text.find(key)?;
    let after = &text[i + key.len()..];
    let q = after.find('"')?;
    let rest = &after[q + 1..];
    rest.split('"').next().map(str::to_string)
}

#[test]
fn every_version_file_agrees_with_the_crate_version() {
    let Some(root) = workspace_root() else {
        // Packaged build with no repository around it; nothing to compare.
        return;
    };
    let expected = env!("CARGO_PKG_VERSION");

    let mut checked: Vec<(String, String)> = Vec::new();

    // Every workspace member with its own version — discovered, not listed, so a crate
    // added tomorrow is covered without touching this test.
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ exists") {
        let manifest = entry.expect("readable entry").path().join("Cargo.toml");
        if let Some(v) = version_in(&manifest) {
            checked.push((
                manifest
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                v,
            ));
        }
    }

    for rel in ["lumenator/src-tauri/Cargo.toml"] {
        if let Some(v) = version_in(&root.join(rel)) {
            checked.push((rel.to_string(), v));
        }
    }

    for rel in [
        "lumenator/package.json",
        "lumenator/src-tauri/tauri.conf.json",
        ".claude-plugin/plugin.json",
    ] {
        if let Some(v) = json_version(&root.join(rel)) {
            checked.push((rel.to_string(), v));
        }
    }

    // Homebrew templates. CI stamps the tap from the resolved tag, so a stale value here
    // never reached a user — but it is what made the aborted release look successful.
    for rel in ["Formula/lumen-cli.rb", "Casks/lumen-app.rb"] {
        if let Some(v) = version_in(&root.join(rel)) {
            checked.push((rel.to_string(), v));
        }
    }

    assert!(
        checked.len() >= 9,
        "expected to find at least 9 version files, found {}: {checked:?}",
        checked.len()
    );

    let wrong: Vec<&(String, String)> = checked.iter().filter(|(_, v)| v != expected).collect();
    assert!(
        wrong.is_empty(),
        "version drift — these disagree with the crate version {expected}:\n{}",
        wrong
            .iter()
            .map(|(f, v)| format!("  {f}: {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The release script has to know about every file the test above checks, or the next
/// release bumps some and leaves others — the exact failure that shipped three times.
#[test]
fn the_release_script_knows_every_version_file() {
    let Some(root) = workspace_root() else { return };
    let script = match std::fs::read_to_string(root.join("scripts/release.sh")) {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut required: Vec<String> = vec![
        "lumenator/src-tauri/Cargo.toml".into(),
        "lumenator/package.json".into(),
        "lumenator/src-tauri/tauri.conf.json".into(),
        ".claude-plugin/plugin.json".into(),
        "Formula/lumen-cli.rb".into(),
        "Casks/lumen-app.rb".into(),
    ];
    for entry in std::fs::read_dir(root.join("crates")).expect("crates/ exists") {
        let p = entry.expect("readable entry").path();
        if p.join("Cargo.toml").is_file() {
            required.push(format!(
                "crates/{}/Cargo.toml",
                p.file_name().unwrap().to_string_lossy()
            ));
        }
    }

    let missing: Vec<&String> = required.iter().filter(|r| !script.contains(*r)).collect();
    assert!(
        missing.is_empty(),
        "scripts/release.sh does not mention these version files, so a release would \
         leave them behind:\n{}",
        missing
            .iter()
            .map(|m| format!("  {m}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
