//! The two builds that are compared: a baseline release and the candidate (the current
//! checkout). Both are built here, from source, with the same toolchain and the same cargo
//! profile, so a difference between them is a difference in the code.

use crate::driver::Identity;
use crate::util::{Ctx, Res, run_text};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct Build {
    /// `v1.7.0`, `current`.
    pub label: String,
    /// `baseline` or `candidate`.
    pub role: &'static str,
    /// The full commit SHA the binary was built from.
    pub sha: String,
    /// Candidate only: whether the checkout had uncommitted changes outside `benchmarks/results`.
    pub dirty: bool,
    pub bin: PathBuf,
    /// `release` or `debug` (the latter only for the smoke test).
    pub profile: &'static str,
    /// Whether the repository tunes the profile, from its own Cargo.toml.
    pub profile_note: String,
    /// How the source was obtained. Never a path: see [`prebuilt`].
    pub source: String,
    /// How the binary learns whose call it is serving. A constructor cannot know: it is set by
    /// [`crate::driver::probe_identity`] at the start of a run, and is `Env` until then.
    pub identity: Identity,
    /// Whether the in-process hook series (which call this checkout's `lumen_mcp::hook` library)
    /// describe this binary: only when it was built from the checkout.
    pub inproc: bool,
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn git(repo: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo);
    c
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether the repository sets any `[profile.release]` of its own: "release" with no override
/// is cargo's default, and the README says which one was measured.
fn profile_note(root: &Path) -> Res<String> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ctx("read the root Cargo.toml")?;
    Ok(if text.contains("[profile.release") {
        "release, with a [profile.release] section in the root Cargo.toml".to_string()
    } else {
        "release, cargo's default: the root Cargo.toml has no [profile.release]".to_string()
    })
}

fn cargo_build(root: &Path, target_dir: &Path, release: bool) -> Res<PathBuf> {
    let mut c = Command::new("cargo");
    c.current_dir(root)
        .args(["build", "--locked", "-p", "lumen-mcp", "--target-dir"])
        .arg(target_dir);
    if release {
        c.arg("--release");
    }
    // An inherited target directory or target triple would send the binary somewhere this function
    // does not look. `RUSTFLAGS` and profile overrides stay: they apply to both builds alike, and
    // the flags are recorded with the environment.
    for k in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET"] {
        c.env_remove(k);
    }
    eprintln!("lumen-bench: building lumen-mcp in {}", root.display());
    let status = c.stdout(Stdio::null()).status().ctx("run cargo build")?;
    if !status.success() {
        return Err(format!(
            "cargo build in {} failed: {status}",
            root.display()
        ));
    }
    let bin = target_dir
        .join(if release { "release" } else { "debug" })
        .join(exe("lumen-mcp"));
    if !bin.is_file() {
        return Err(format!("cargo built no {}", bin.display()));
    }
    Ok(bin)
}

/// The candidate's label in a result: `pre-` and the first seven characters of the commit that was
/// built, so a result never says "main" or "current" about a commit that has since moved.
pub fn pre_release_label(sha: &str) -> String {
    format!("pre-{}", &sha[..sha.len().min(7)])
}

/// The candidate: the checkout at `repo`, built in `repo/target`.
pub fn candidate(repo: &Path, release: bool, allow_dirty: bool) -> Res<Build> {
    let sha = run_text(git(repo).args(["rev-parse", "HEAD"]))?
        .trim()
        .to_string();
    if !is_full_sha(&sha) {
        return Err(format!("git rev-parse HEAD printed {sha:?}"));
    }
    let status = run_text(git(repo).args([
        "status",
        "--porcelain",
        "--",
        ".",
        ":(exclude)benchmarks/results",
    ]))?;
    let dirty = !status.trim().is_empty();
    if dirty && !allow_dirty {
        return Err(format!(
            "the checkout has uncommitted changes, so no commit describes what would be built:\n{}\n\
             commit or stash them, or pass --allow-dirty for a throwaway run",
            status.trim_end()
        ));
    }
    let bin = cargo_build(repo, &repo.join("target"), release)?;
    Ok(Build {
        label: pre_release_label(&sha),
        role: "candidate",
        sha,
        dirty,
        bin,
        profile: if release { "release" } else { "debug" },
        profile_note: profile_note(repo)?,
        // No path: a result file is committed and read elsewhere, and a path says whose home
        // directory it was made in.
        source: "the checkout this harness was run from, built in its own target directory"
            .to_string(),
        identity: Identity::Env,
        inproc: true,
    })
}

/// The baseline: the commit `sha` of `repo`, exported with `git archive` into the cache and built
/// there. The commit is checked, not assumed: an abbreviated or unknown SHA stops the run.
pub fn baseline(repo: &Path, cache: &Path, label: &str, sha: &str) -> Res<Build> {
    if !is_full_sha(sha) {
        return Err(format!(
            "the baseline {sha:?} is not a full 40-character commit SHA"
        ));
    }
    let resolved = run_text(
        git(repo)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{sha}^{{commit}}")),
    )
    .ctx(&format!(
        "the baseline commit {sha} is not in this repository (fetch its history)"
    ))?;
    if resolved.trim() != sha {
        return Err(format!(
            "the baseline {sha} resolved to {}",
            resolved.trim()
        ));
    }
    let src = cache.join("src").join(format!("{label}-{}", &sha[..12]));
    let marker = src.join(".lumen-bench-extracted");
    let want = format!("{sha}\n");
    let have = std::fs::read_to_string(&marker).ok();
    if have.as_deref() != Some(want.as_str()) {
        if src.exists() {
            std::fs::remove_dir_all(&src).ctx(&format!("clear {}", src.display()))?;
        }
        std::fs::create_dir_all(&src).ctx(&format!("create {}", src.display()))?;
        let mut archive = git(repo)
            .args(["archive", "--format=tar", sha])
            .stdout(Stdio::piped())
            .spawn()
            .ctx("start git archive")?;
        let tar_in = archive.stdout.take().ok_or("git archive has no stdout")?;
        let tar = Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(&src)
            .stdin(Stdio::from(tar_in))
            .status()
            .ctx("run tar")?;
        let arch = archive.wait().ctx("wait for git archive")?;
        if !arch.success() || !tar.success() {
            return Err(format!(
                "exporting {sha} failed (git archive {arch}, tar {tar})"
            ));
        }
        std::fs::write(&marker, want).ctx("write the export marker")?;
    }
    let target = cache.join("target").join(format!("{label}-{}", &sha[..12]));
    let bin = cargo_build(&src, &target, true)?;
    Ok(Build {
        label: label.to_string(),
        role: "baseline",
        sha: sha.to_string(),
        dirty: false,
        bin,
        profile: "release",
        profile_note: profile_note(&src)?,
        source: format!("git archive of {sha}"),
        identity: Identity::Env,
        inproc: false,
    })
}

/// A prebuilt binary, for a reviewer who has one: its provenance is whatever they say it is.
///
/// The record names the file (not where it is) and the commit the caller says it was built from,
/// which must be a full SHA: a result file is committed and read elsewhere, and a path says whose
/// home directory it was made in.
pub fn prebuilt(label: &str, role: &'static str, sha: &str, bin: &Path) -> Res<Build> {
    if !bin.is_file() {
        return Err(format!("{} is not a file", bin.display()));
    }
    // What the caller called it, not where it is, and not what a symlink points at.
    let name = bin.file_name().map_or_else(
        || "lumen-mcp".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    // The server is started with the corpus as its working directory, so a path relative to where
    // the harness was run would not be found there: spawn would say "No such file".
    let bin = bin
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", bin.display()))?;
    if !is_full_sha(sha) {
        return Err(format!(
            "the {role} binary {} needs the full 40-character commit SHA it was built from, got {sha:?}",
            bin.display()
        ));
    }
    Ok(Build {
        label: label.to_string(),
        role,
        sha: sha.to_string(),
        dirty: false,
        bin,
        profile: "release",
        profile_note: "release, as supplied by --candidate-bin / --baseline-bin".to_string(),
        source: format!("prebuilt binary `{name}`, supplied by the caller"),
        identity: Identity::Env,
        inproc: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_full_sha_is_a_pin() {
        assert!(is_full_sha("5d1ec0408ad25edcbae43cf1a6ecbc0ddba47c94"));
        assert!(!is_full_sha("5d1ec04"));
        assert!(!is_full_sha("v1.7.0"));
        assert!(!is_full_sha("zd1ec0408ad25edcbae43cf1a6ecbc0ddba47c94"));
    }

    #[test]
    fn a_prebuilt_build_records_its_file_name_and_never_where_the_file_is() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("lumen-mcp");
        std::fs::write(&bin, b"x").unwrap();
        let sha = "dd565a9f0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
        let b = prebuilt("current", "candidate", sha, &bin).unwrap();
        let where_it_is = dir.path().to_string_lossy().into_owned();
        assert!(b.source.contains("lumen-mcp"), "{}", b.source);
        assert!(!b.source.contains(&where_it_is), "{}", b.source);
        assert!(!b.profile_note.contains(&where_it_is));
        assert_eq!(b.sha, sha);
        // The commit is the caller's word and must be a whole one.
        assert!(prebuilt("current", "candidate", "dd565a9", &bin).is_err());
    }

    #[test]
    fn a_path_that_is_not_absolute_or_not_tidy_is_resolved_before_the_server_is_started() {
        // The server's working directory is the corpus, not where the harness was run, so what
        // reaches the spawn must be a whole path with no `..` in it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let bin = dir.path().join("lumen-mcp");
        std::fs::write(&bin, b"x").unwrap();
        let untidy = dir.path().join("sub").join("..").join("lumen-mcp");
        let sha = "dd565a9f0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e";
        let b = prebuilt("current", "candidate", sha, &untidy).unwrap();
        assert!(b.bin.is_absolute());
        assert!(
            !b.bin
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "{}",
            b.bin.display()
        );
        assert_eq!(b.bin, bin.canonicalize().unwrap());
        assert!(b.source.contains("`lumen-mcp`"), "{}", b.source);
    }

    #[test]
    fn the_candidate_is_labelled_by_the_commit_that_was_built() {
        assert_eq!(
            pre_release_label("5bf97e3b0a1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e"),
            "pre-5bf97e3"
        );
        assert_eq!(pre_release_label("abc"), "pre-abc");
    }
}
