//! Installing the arms from their pins: assets by sha256, never `latest`, never globally.
//!
//! Everything lands under the cache directory (outside the repository) or in a per-case scratch
//! HOME. Each download is checked against the sha256 in `tools.toml`; for squeez and RTK the
//! asset's sha256 must also appear in the release's own checksum file, itself pinned by sha256.
//! A tool whose pin has no asset for this platform is `n/a`, with that reason.
//!
//! The network is used only when `net` is true (`--prefetch`); otherwise a missing asset is an
//! error that says to prefetch.

use crate::assets::{self, Fetched, Source};
use crate::manifest::{Arm, Tools, host_platform};
use crate::util::{Ctx, Res};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A platform has no pinned asset: the arm's `n/a`.
pub fn na_platform(arm: &str, platform: Option<&str>) -> String {
    format!(
        "n/a — tools.toml pins no {arm} asset for {}",
        platform.unwrap_or("this platform")
    )
}

fn source_of(arm: &Arm) -> Res<(String, String)> {
    let repo = arm
        .repo
        .as_deref()
        .and_then(|r| r.strip_prefix("https://github.com/"))
        .ok_or_else(|| format!("{}: no github.com repo", arm.id))?;
    let tag = arm
        .tag
        .clone()
        .ok_or_else(|| format!("{}: no tag", arm.id))?;
    Ok((repo.to_string(), tag))
}

fn refuse_offline(src: Source<'_>, name: &str, _dest: &Path) -> Res<()> {
    Err(format!(
        "{name} of {} {} is not in the cache; run `competitors replay --prefetch` first (the one step that uses the network)",
        src.repo, src.tag
    ))
}

/// Fetch (or find in the cache) `name` of `arm`'s release, checked against `sha256`.
pub fn fetch(
    arm: &Arm,
    name: &str,
    sha256: &str,
    cache: &Path,
    repo_root: &Path,
    net: bool,
) -> Res<Fetched> {
    let (repo, tag) = source_of(arm)?;
    let src = Source {
        repo: &repo,
        tag: &tag,
    };
    if net {
        assets::fetch(cache, repo_root, src, name, sha256)
    } else {
        assets::fetch_with(cache, repo_root, src, name, sha256, &refuse_offline)
    }
}

/// The `checksum_file` table of an arm in `tools.toml` (not in the shared `Arm` type).
fn checksum_file(arm_id: &str) -> Res<Option<(String, String)>> {
    let text = std::fs::read_to_string(crate::manifest::bench_dir().join("competitors/tools.toml"))
        .ctx("read tools.toml")?;
    let v: toml::Value = toml::from_str(&text).ctx("parse tools.toml")?;
    let arm = v["arm"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["id"].as_str() == Some(arm_id));
    Ok(arm.and_then(|a| a.get("checksum_file")).and_then(|c| {
        Some((
            c.get("name")?.as_str()?.to_string(),
            c.get("sha256")?.as_str()?.to_string(),
        ))
    }))
}

/// A checksum file's lines (`<sha256>  <name>`) must list `name` with `sha256`.
pub fn listed_in(checksums: &str, name: &str, sha256: &str) -> bool {
    checksums.lines().any(|l| {
        let mut f = l.split_whitespace();
        f.next() == Some(sha256) && f.next().map(|n| n.trim_start_matches('*')) == Some(name)
    })
}

/// The asset of `arm` for this platform, fetched, and cross-checked against the release's checksum
/// file where `tools.toml` pins one.
pub fn asset_for(
    tools: &Tools,
    arm_id: &str,
    cache: &Path,
    repo_root: &Path,
    net: bool,
) -> Res<Result<Fetched, String>> {
    let arm = tools.arm(arm_id)?;
    let platform = host_platform();
    let Some(a) = platform.and_then(|p| arm.asset.iter().find(|a| a.platform == p)) else {
        return Ok(Err(na_platform(arm_id, platform)));
    };
    let f = fetch(arm, &a.name, &a.sha256, cache, repo_root, net)?;
    if let Some((cname, csha)) = checksum_file(arm_id)? {
        let c = fetch(arm, &cname, &csha, cache, repo_root, net)?;
        let text = std::fs::read_to_string(&c.path).ctx(&format!("read {cname}"))?;
        if !listed_in(&text, &a.name, &a.sha256) {
            return Err(format!(
                "REFUSED: {} of {arm_id} is pinned in tools.toml but {cname} (itself pinned) does not list it with that sha256",
                a.name
            ));
        }
    }
    Ok(Ok(f))
}

/// Unpack a `.tar.gz` asset once into the cache and return the file called `file` in it.
pub fn unpacked(asset: &Fetched, cache: &Path, file: &str) -> Res<PathBuf> {
    let dest = cache.join("unpacked").join(&asset.sha256);
    if !dest.exists() {
        assets::extract(&asset.path, &asset.name, &dest)?;
    }
    let p = dest.join(file);
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("{} holds no {file}", asset.name))
    }
}

// ---------------------------------------------------------------------------------- headroom

/// An installed Headroom: a Python environment `uv` made in the cache, and the model weights.
#[derive(Debug, Clone)]
pub struct Headroom {
    pub python: PathBuf,
    pub hf_home: PathBuf,
}

/// Headroom pins its own model revisions (`headroom.onnx_runtime._PINNED_REVISIONS`) and asks the
/// Hub for exactly those. The weights fetched are those, read from the installed package, so a
/// run at defaults finds what it asks for. The big files are only what the ONNX path loads.
const WEIGHTS_SCRIPT: &str = r#"
import os
from huggingface_hub import snapshot_download
from headroom.onnx_runtime import _PINNED_REVISIONS as P
for repo, sha in P.items():
    pats = None
    if repo.startswith("answerdotai/"):
        pats = ["tokenizer*.json", "special_tokens_map.json", "config.json"]
    if repo.startswith("chopratejas/kompress"):
        pats = ["onnx/*", "tokenizer*", "special_tokens_map.json", "config.json"]
    p = snapshot_download(repo, revision=sha, allow_patterns=pats)
    print("fetched", repo, sha)
"#;

fn headroom_dir(cache: &Path, wheel_sha: &str) -> PathBuf {
    cache.join("headroom").join(&wheel_sha[..12])
}

fn headroom_python(dir: &Path) -> PathBuf {
    dir.join("tool")
        .join("headroom-ai")
        .join("bin")
        .join("python")
}

/// A command with a clean environment: `PATH` and the given variables only.
fn clean(cmd: &mut Command, home: &Path, vars: &[(&str, &Path)]) {
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("DO_NOT_TRACK", "1")
        .env("HEADROOM_BEACON", "off")
        .env("HF_HUB_DISABLE_TELEMETRY", "1");
    for (k, v) in vars {
        cmd.env(k, v);
    }
}

/// Install Headroom from its pinned wheel and fetch the weights. `net` false only locates an
/// install that a prefetch made.
pub fn headroom(
    tools: &Tools,
    cache: &Path,
    repo_root: &Path,
    net: bool,
) -> Res<Result<Headroom, String>> {
    if cfg!(windows) {
        return Ok(Err(
            "n/a — replay shims and the Python install are POSIX only".to_string(),
        ));
    }
    let wheel = match asset_for(tools, "headroom", cache, repo_root, net)? {
        Ok(w) => w,
        Err(na) => return Ok(Err(na)),
    };
    let dir = headroom_dir(cache, &wheel.sha256);
    let python = headroom_python(&dir);
    let hf_home = dir.join("hf");
    let done = dir.join("weights.ok");
    if python.is_file() && done.is_file() {
        let fetched = std::fs::read_to_string(&done).ctx("read the weights marker")?;
        check_model_pin(tools, &fetched)?;
        return Ok(Ok(Headroom { python, hf_home }));
    }
    if !net {
        return Err(format!(
            "Headroom is not installed in {}; run `competitors replay --prefetch` first",
            dir.display()
        ));
    }
    let uv =
        assets::find_tool(&["uv"]).ok_or("uv is not installed (needed to install Headroom)")?;
    for d in ["home", "tool", "bin", "uv-cache", "py", "hf"] {
        std::fs::create_dir_all(dir.join(d)).ctx("create the Headroom directories")?;
    }
    let exclude = format!("{}T00:00:00Z", tools.meta.resolved_on);
    let spec = format!("headroom-ai[all] @ file://{}", wheel.path.display());
    let mut cmd = Command::new(&uv);
    clean(
        &mut cmd,
        &dir.join("home"),
        &[
            ("UV_TOOL_DIR", &dir.join("tool")),
            ("UV_TOOL_BIN_DIR", &dir.join("bin")),
            ("UV_CACHE_DIR", &dir.join("uv-cache")),
            ("UV_PYTHON_INSTALL_DIR", &dir.join("py")),
        ],
    );
    cmd.args([
        "tool",
        "install",
        "--python",
        "3.13",
        "--exclude-newer",
        &exclude,
        &spec,
    ])
    .stdin(Stdio::null());
    run_ok(&mut cmd, "uv tool install headroom-ai")?;
    if !python.is_file() {
        return Err(format!("uv installed no Python at {}", python.display()));
    }
    let mut cmd = Command::new(&python);
    clean(&mut cmd, &dir.join("home"), &[("HF_HOME", &hf_home)]);
    cmd.args(["-I", "-c", WEIGHTS_SCRIPT]).stdin(Stdio::null());
    let fetched = run_stdout(&mut cmd, "fetch Headroom's pinned model weights")?;
    check_model_pin(tools, &fetched)?;
    std::fs::write(&done, fetched).ctx("mark the weights fetched")?;
    Ok(Ok(Headroom { python, hf_home }))
}

/// `tools.toml` records the Kompress revision the run uses; it must be the one Headroom pinned
/// (`fetched` is the weights script's output, one `fetched <repo> <sha>` line per model).
pub fn check_model_pin(tools: &Tools, fetched: &str) -> Res<()> {
    let _ = tools;
    let text = std::fs::read_to_string(crate::manifest::bench_dir().join("competitors/tools.toml"))
        .ctx("read tools.toml")?;
    let v: toml::Value = toml::from_str(&text).ctx("parse tools.toml")?;
    let model = v["arm"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["id"].as_str() == Some("headroom"))
        .and_then(|a| a.get("model"))
        .ok_or("tools.toml has no [arm.model] for headroom")?;
    let repo = model["repo"].as_str().ok_or("model.repo")?;
    let want = model["commit"].as_str().ok_or("model.commit")?;
    let used = fetched
        .lines()
        .find_map(|l| l.strip_prefix(&format!("fetched {repo} ")))
        .ok_or_else(|| format!("Headroom pins no revision of {repo}"))?;
    if used.trim() != want {
        return Err(format!(
            "tools.toml records {repo} at {want}, but Headroom 0.41.0 pins {}; the run uses Headroom's pin, so tools.toml must say so",
            used.trim()
        ));
    }
    Ok(())
}

fn run_stdout(cmd: &mut Command, what: &str) -> Res<String> {
    let out = cmd.output().ctx(&format!("cannot run: {what}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let tail = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = tail.lines().rev().take(12).collect();
    Err(format!(
        "{what} exited with {}: {}",
        out.status,
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    ))
}

fn run_ok(cmd: &mut Command, what: &str) -> Res<()> {
    let out = cmd.output().ctx(&format!("cannot run: {what}"))?;
    if out.status.success() {
        return Ok(());
    }
    let tail = String::from_utf8_lossy(&out.stderr);
    let tail: Vec<&str> = tail.lines().rev().take(12).collect();
    Err(format!(
        "{what} exited with {}: {}",
        out.status,
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checksum_file_must_list_the_name_with_that_sha() {
        let sums = format!(
            "{}  squeez-linux-x86_64\n{}  other\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert!(listed_in(&sums, "squeez-linux-x86_64", &"a".repeat(64)));
        // The right name with another asset's hash, and a hash with the wrong name.
        assert!(!listed_in(&sums, "squeez-linux-x86_64", &"b".repeat(64)));
        assert!(!listed_in(&sums, "missing", &"a".repeat(64)));
        // A binary-mode star before the name is still the name.
        let star = format!("{} *rtk.tar.gz\n", "c".repeat(64));
        assert!(listed_in(&star, "rtk.tar.gz", &"c".repeat(64)));
    }

    #[test]
    fn tools_toml_records_the_revision_headroom_pins_and_a_mismatch_is_refused() {
        let tools = Tools::load_committed().unwrap();
        let good = "fetched answerdotai/ModernBERT-base 8949b909ec900327062f0ebf497f51aef5e6f0c8\n\
                    fetched chopratejas/kompress-v2-base b1563631b35bfdcee37587ad530147497d820d4c\n";
        check_model_pin(&tools, good).unwrap();
        let stale = good.replace(
            "b1563631b35bfdcee37587ad530147497d820d4c",
            "e00cc5e60ff73b01674ef8fa6da6c5acf6d54181",
        );
        assert!(
            check_model_pin(&tools, &stale)
                .unwrap_err()
                .contains("Headroom 0.41.0 pins")
        );
        assert!(check_model_pin(&tools, "").is_err());
    }

    #[test]
    fn an_offline_fetch_refuses_before_any_download() {
        let tools = Tools::load_committed().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let arm = tools.arm("squeez").unwrap();
        let a = arm.asset.first().unwrap();
        let e = fetch(arm, &a.name, &a.sha256, cache.path(), &repo, false).unwrap_err();
        assert!(e.contains("--prefetch"), "{e}");
        assert!(
            !cache
                .path()
                .join("assets")
                .join(&a.sha256)
                .join(&a.name)
                .exists()
        );
    }
}
