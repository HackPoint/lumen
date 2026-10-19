//! Typed parsing of the committed method files of the version and competitor benchmarks:
//! `benchmarks/versions/manifest.toml` and `benchmarks/competitors/{tools,tasks}.toml`.
//!
//! The files are committed before any run, so what they say cannot be chosen after seeing a
//! result. This module only reads them. The version manifest is strict (an unknown key is an
//! error, so a typo in the capability map cannot pass as "not set"); the competitor files carry
//! prose fields that later tasks read as they need, so those ignore keys this module does not
//! name.

use crate::util::{Ctx, Res};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// The platforms an asset can be for, as the manifest names them.
pub const PLATFORMS: [&str; 3] = ["macos-arm64", "linux-x86_64", "windows-x64"];

/// The platform of the machine running the harness, in the manifest's words, if it is one the
/// manifest has a column for.
pub fn host_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("windows", "x86_64") => Some("windows-x64"),
        _ => None,
    }
}

// --- benchmarks/versions/manifest.toml -------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionsManifest {
    pub meta: Meta,
    pub version: BTreeMap<String, VersionEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub repo: String,
    pub resolved_on: String,
    /// Every measured version, oldest first.
    pub versions: Vec<String>,
    /// The versions that are end-to-end arms.
    pub e2e: Vec<String>,
    /// A version that has no E2E build, and the one that stands in for it.
    pub e2e_stand_in: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionEntry {
    pub tag: String,
    pub commit: String,
    /// `clamped` or `signed`.
    pub saved_tokens: String,
    /// `scripts`, `scripts+cli` or `scripts+cli+identity`.
    pub hook_kind: String,
    /// Whether the binary has a `hook` subcommand. Where false, `lumen-mcp hook ...` starts an
    /// MCP server instead, and must never be spawned.
    pub hook_cli: bool,
    pub bash_meter: bool,
    /// `none`, `opt-in` or `default`.
    pub repeat_reads: String,
    /// `none`, `env` or `hook`.
    pub identity: String,
    pub daemon_env: Vec<String>,
    pub daemon_fixed_port: bool,
    pub daemon_reads_home: bool,
    pub e2e: bool,
    pub asset: BTreeMap<String, AssetSpec>,
}

/// One platform's entry of a version: a pinned file, or the reason there is none.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum AssetSpec {
    Pinned(PinnedAsset),
    Na(NaAsset),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedAsset {
    pub name: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NaAsset {
    pub na: String,
}

impl VersionsManifest {
    pub fn parse(text: &str) -> Res<Self> {
        let m: Self = toml::from_str(text).ctx("manifest.toml")?;
        m.check()?;
        Ok(m)
    }

    pub fn load(path: &Path) -> Res<Self> {
        let text = std::fs::read_to_string(path).ctx(&format!("read {}", path.display()))?;
        Self::parse(&text).ctx(&path.display().to_string())
    }

    /// The default location, in this checkout.
    pub fn load_committed() -> Res<Self> {
        Self::load(&bench_dir().join("versions").join("manifest.toml"))
    }

    pub fn get(&self, version: &str) -> Res<&VersionEntry> {
        self.version.get(version).ok_or_else(|| {
            format!(
                "version {version:?} is not in the manifest (it has {})",
                self.meta.versions.join(", ")
            )
        })
    }

    /// Structure the types cannot say: the list in `meta` and the tables agree, and every
    /// version answers for every platform (a file or an explicit `na`).
    fn check(&self) -> Res<()> {
        for v in &self.meta.versions {
            let e = self
                .version
                .get(v)
                .ok_or_else(|| format!("meta.versions lists {v} but it has no [version.{v:?}]"))?;
            for p in PLATFORMS {
                if !e.asset.contains_key(p) {
                    return Err(format!("version {v} has neither an asset nor `na` for {p}"));
                }
            }
            for p in e.asset.keys() {
                if !PLATFORMS.contains(&p.as_str()) {
                    return Err(format!("version {v} names an unknown platform {p:?}"));
                }
            }
        }
        for v in self.version.keys() {
            if !self.meta.versions.contains(v) {
                return Err(format!("[version.{v:?}] is not listed in meta.versions"));
            }
        }
        Ok(())
    }

    /// The asset of `version` for `platform`: `Ok(Some)` for a pinned file, `Ok(None)` with the
    /// manifest's reason for an `na`.
    pub fn asset(&self, version: &str, platform: &str) -> Res<Result<&PinnedAsset, &str>> {
        let e = self.get(version)?;
        match e.asset.get(platform) {
            Some(AssetSpec::Pinned(a)) => Ok(Ok(a)),
            Some(AssetSpec::Na(n)) => Ok(Err(n.na.as_str())),
            None => Err(format!("{version} has no entry for platform {platform:?}")),
        }
    }
}

// --- benchmarks/competitors/tools.toml -------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Tools {
    pub meta: ToolsMeta,
    pub claude_code: ClaudeCode,
    pub arm: Vec<Arm>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolsMeta {
    pub resolved_on: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeCode {
    pub package: String,
    pub version: String,
    pub npm_integrity: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// One arm. `kind` is `none`, `lumen` or `competitor`; only the fields every arm of its kind has
/// are required.
#[derive(Debug, Clone, Deserialize)]
pub struct Arm {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub shared: bool,
    pub repo: Option<String>,
    pub license: Option<String>,
    pub version: Option<String>,
    pub tag: Option<String>,
    pub commit: Option<String>,
    #[serde(default)]
    pub install: Vec<String>,
    #[serde(default)]
    pub telemetry_env: BTreeMap<String, String>,
    #[serde(default)]
    pub extra_env: BTreeMap<String, String>,
    #[serde(default)]
    pub asset: Vec<ToolAsset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolAsset {
    pub platform: String,
    pub name: String,
    pub sha256: String,
}

impl Tools {
    pub fn parse(text: &str) -> Res<Self> {
        toml::from_str(text).ctx("tools.toml")
    }
    pub fn load(path: &Path) -> Res<Self> {
        let text = std::fs::read_to_string(path).ctx(&format!("read {}", path.display()))?;
        Self::parse(&text).ctx(&path.display().to_string())
    }
    pub fn load_committed() -> Res<Self> {
        Self::load(&bench_dir().join("competitors").join("tools.toml"))
    }
    pub fn arm(&self, id: &str) -> Res<&Arm> {
        self.arm
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| format!("no arm {id:?} in tools.toml"))
    }
}

// --- benchmarks/competitors/tasks.toml -------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Tasks {
    pub swe_bench: SweBench,
    pub swe: Vec<SweTask>,
    pub scenario: Vec<Scenario>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweBench {
    pub dataset: String,
    pub revision: String,
    pub parquet: String,
    pub parquet_sha256: String,
    pub rows: u32,
    pub seed: String,
    pub count: u32,
    pub ids_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweTask {
    pub id: String,
    pub repo: String,
    pub base_commit: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    pub repo: String,
    pub license: String,
    pub base_commit: String,
    pub gold_commit: String,
    pub language: String,
    pub prompt: String,
    pub success: String,
}

impl Tasks {
    pub fn parse(text: &str) -> Res<Self> {
        toml::from_str(text).ctx("tasks.toml")
    }
    pub fn load(path: &Path) -> Res<Self> {
        let text = std::fs::read_to_string(path).ctx(&format!("read {}", path.display()))?;
        Self::parse(&text).ctx(&path.display().to_string())
    }
    pub fn load_committed() -> Res<Self> {
        Self::load(&bench_dir().join("competitors").join("tasks.toml"))
    }
}

/// `benchmarks/` of this checkout.
pub fn bench_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
