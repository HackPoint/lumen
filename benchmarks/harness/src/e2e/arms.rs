//! The arms of the end-to-end runs: what each installs into a run's scratch HOME, how Claude Code
//! is configured for it, and how to describe it.
//!
//! An arm never touches the real home. `install` writes only under the `home` it is given (a
//! fresh scratch directory) through [`write_under`], which refuses an absolute path or a `..`,
//! and returns an [`ArmPlan`]: the commands that must run inside the container before `claude`,
//! the environment, and a command prefix (Headroom wraps `claude`). The container itself is the
//! executor's business.
//!
//! The Lumen arms have no headless Setup, so they write what Setup writes: the `mcpServers.lumen`
//! entry of `.claude.json` and the hooks of `.claude/settings.json`, with the same matchers and
//! the same entry shape as `lumenator/src-tauri/src/setup.rs`. `tests/e2e.rs` compares them with
//! a golden produced by setup.rs's own writers, and checks setup.rs's source for drift.

use crate::assets;
use crate::manifest::{Arm as ArmEntry, ToolAsset, Tools, bench_dir};
use crate::util::{Ctx, Res};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// The platform every end-to-end container is.
pub const PLATFORM: &str = "linux-x86_64";

/// Where the Lumen sidecars of a run go, relative to HOME.
pub const LUMEN_BIN_DIR: &str = ".lumen-bench/bin";
pub const LUMEN_LEDGER: &str = ".lumen-bench/ledger.db";
pub const LUMEN_SPOOL: &str = ".lumen-bench/faults.jsonl";

/// PreToolUse matchers Setup registers (setup.rs `INTERCEPT_MATCHERS`): `Read` is the intercept,
/// the three Lumen tools carry the identity hook, which exists from 1.8.0.
const IDENTITY_FROM: (u32, u32, u32) = (1, 8, 0);
/// From 1.6.0 the hooks are `lumen-mcp hook ...`; before it they are the tag's own scripts.
const HOOK_CLI_FROM: (u32, u32, u32) = (1, 6, 0);

/// Everything an install needs from the run.
pub struct InstallCtx<'a> {
    /// The scratch HOME on this machine. The only place an install writes.
    pub home: &'a Path,
    /// The same directory as Claude Code sees it (`/home/bench` in a container). Configuration
    /// files name paths in these terms.
    pub claude_home: &'a str,
    pub fetcher: &'a dyn Fetcher,
    /// The tag's committed `.claude/hooks`, for a version whose hooks are scripts (before
    /// 1.6.0): `lumen_meter.sh` and `lumen_read_intercept.sh`.
    pub hook_scripts: Option<&'a Path>,
}

/// Puts a pinned asset on disk and returns where. The real one downloads and verifies the
/// sha256; a test hands back a fixture.
pub trait Fetcher {
    fn fetch(&self, arm: &ArmEntry, asset: &ToolAsset) -> Res<PathBuf>;
}

/// Downloads from the arm's GitHub release into a cache outside the repository, and refuses a
/// sha256 that does not match the pin.
pub struct NetworkFetcher {
    pub cache: PathBuf,
    pub repo_root: PathBuf,
}

impl Fetcher for NetworkFetcher {
    fn fetch(&self, arm: &ArmEntry, asset: &ToolAsset) -> Res<PathBuf> {
        let repo = arm
            .repo
            .as_deref()
            .and_then(|r| r.strip_prefix("https://github.com/"))
            .ok_or_else(|| format!("arm {} has no github.com repo", arm.id))?;
        let tag = arm
            .tag
            .as_deref()
            .ok_or_else(|| format!("arm {} has no tag", arm.id))?;
        let f = assets::fetch(
            &self.cache,
            &self.repo_root,
            assets::Source { repo, tag },
            &asset.name,
            &asset.sha256,
        )?;
        Ok(f.path)
    }
}

/// What the executor must do besides running `claude`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ArmPlan {
    pub arm: String,
    /// Environment of the `claude` process: Claude Code's pins, the arm's telemetry switches and
    /// its one other setting, if any.
    pub env: BTreeMap<String, String>,
    /// argv lists to run in the container, in order, before `claude`. Each must succeed.
    pub setup: Vec<Vec<String>>,
    /// Directories (as Claude Code sees them) put first on PATH.
    pub path_prepend: Vec<String>,
    /// Put before `claude` and its arguments: `headroom wrap claude --`.
    pub claude_prefix: Vec<String>,
    /// Files written under HOME, relative to it.
    pub written: Vec<String>,
    /// Name and sha256 of every asset used, never a path.
    pub provenance: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArmDescription {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub version: Option<String>,
    pub commit: Option<String>,
    /// How a table labels it: 1.1.5 stands in for 1.0.0 and says so.
    pub label: String,
    pub shared: bool,
    pub install_steps: Vec<String>,
    pub telemetry_env: BTreeMap<String, String>,
    pub extra_env: BTreeMap<String, String>,
    pub assets: Vec<String>,
    /// Deviations from the tool's defaults the method records.
    pub deviations: Vec<String>,
}

pub trait Arm {
    fn id(&self) -> &str;
    fn describe(&self) -> ArmDescription;
    /// Install into `ctx.home` only. Returns what the container still has to do.
    fn install(&self, ctx: &InstallCtx<'_>) -> Res<ArmPlan>;
}

// --- construction ------------------------------------------------------------------------------

/// The arm called `id` in tools.toml.
pub fn arm_for(tools: &Tools, id: &str) -> Res<Box<dyn Arm>> {
    let entry = tools.arm(id)?.clone();
    let claude_env = tools.claude_code.env.clone();
    Ok(match entry.kind.as_str() {
        "none" => Box::new(NoLumen { entry, claude_env }),
        "lumen" => Box::new(LumenArm { entry, claude_env }),
        "competitor" => Box::new(ToolArm { entry, claude_env }),
        other => return Err(format!("arm {id} has an unknown kind {other:?}")),
    })
}

/// Every arm of tools.toml.
pub fn all_arms(tools: &Tools) -> Res<Vec<Box<dyn Arm>>> {
    tools.arm.iter().map(|a| arm_for(tools, &a.id)).collect()
}

// --- shared pieces -----------------------------------------------------------------------------

/// Write `bytes` at `rel` under `home`. `rel` must be relative and have no `..`: an install that
/// could name a path outside the scratch home would be the way a real `~/.claude` gets touched.
pub fn write_under(home: &Path, rel: &str, bytes: &[u8], exec: bool) -> Res<()> {
    let p = Path::new(rel);
    if rel.is_empty()
        || p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "{rel:?} is not a path under the scratch home: an arm writes only there"
        ));
    }
    let to = home.join(p);
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).ctx(&format!("create {}", d.display()))?;
    }
    std::fs::write(&to, bytes).ctx(&format!("write {rel}"))?;
    set_exec(&to, exec)
}

#[cfg(unix)]
fn set_exec(p: &Path, exec: bool) -> Res<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
        .ctx(&format!("chmod {}", p.display()))
}

#[cfg(not(unix))]
fn set_exec(_: &Path, _: bool) -> Res<()> {
    Ok(())
}

fn json_bytes(v: &Value) -> Vec<u8> {
    serde_json::to_string_pretty(v)
        .unwrap_or_default()
        .into_bytes()
}

/// `.claude.json` of every arm. `hasCompletedOnboarding` keeps a first start from asking
/// questions of a non-interactive run; the E3 pilot confirms Claude Code needs nothing else.
fn base_claude_json() -> Value {
    json!({ "hasCompletedOnboarding": true })
}

fn plan_env(entry: &ArmEntry, claude_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut env = claude_env.clone();
    env.extend(entry.telemetry_env.clone());
    env.extend(entry.extra_env.clone());
    env
}

fn describe_entry(entry: &ArmEntry, deviations: Vec<String>) -> ArmDescription {
    let label = match raw_str(&entry.id, &["stands_in_for"]) {
        Ok(v) => format!("{} (stands in for {v})", entry.name),
        Err(_) => entry.name.clone(),
    };
    ArmDescription {
        id: entry.id.clone(),
        name: entry.name.clone(),
        kind: entry.kind.clone(),
        version: entry.version.clone(),
        commit: entry.commit.clone(),
        label,
        shared: entry.shared,
        install_steps: entry.install.clone(),
        telemetry_env: entry.telemetry_env.clone(),
        extra_env: entry.extra_env.clone(),
        assets: entry
            .asset
            .iter()
            .map(|a| assets::provenance(&a.name, &a.sha256))
            .collect(),
        deviations,
    }
}

fn telemetry_deviation(entry: &ArmEntry) -> Vec<String> {
    let mut d = Vec::new();
    if !entry.telemetry_env.is_empty() {
        d.push(format!(
            "telemetry switches set: {}",
            entry
                .telemetry_env
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !entry.extra_env.is_empty() {
        d.push(format!(
            "non-default setting: {}",
            entry
                .extra_env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    d
}

/// A value of the arm's raw table in tools.toml that the typed manifest does not carry
/// (`install_script`, `model`, `stands_in_for`).
fn raw_str(arm_id: &str, keys: &[&str]) -> Res<String> {
    let path = bench_dir().join("competitors").join("tools.toml");
    let text = std::fs::read_to_string(&path).ctx(&format!("read {}", path.display()))?;
    let doc: toml::Table = text.parse().ctx("tools.toml")?;
    let arm = doc
        .get("arm")
        .and_then(|a| a.as_array())
        .and_then(|a| {
            a.iter()
                .find(|t| t.get("id").and_then(|i| i.as_str()) == Some(arm_id))
        })
        .ok_or_else(|| format!("no arm {arm_id:?} in tools.toml"))?;
    let mut v: &toml::Value = arm;
    for k in keys {
        v = v
            .get(k)
            .ok_or_else(|| format!("arm {arm_id} has no {}", keys.join(".")))?;
    }
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("arm {arm_id}: {} is not a string", keys.join(".")))
}

fn linux_asset(entry: &ArmEntry) -> Res<&ToolAsset> {
    entry
        .asset
        .iter()
        .find(|a| a.platform == PLATFORM)
        .ok_or_else(|| format!("arm {} has no {PLATFORM} asset", entry.id))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn parse_version(v: &str) -> Res<(u32, u32, u32)> {
    let mut it = v.split('.').map(|p| p.parse::<u32>());
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b)), Some(Ok(c)), None) => Ok((a, b, c)),
        _ => Err(format!("{v:?} is not a major.minor.patch version")),
    }
}

// --- no Lumen ------------------------------------------------------------------------------------

/// Stock Claude Code at the pinned version: no hooks, no MCP servers.
pub struct NoLumen {
    entry: ArmEntry,
    claude_env: BTreeMap<String, String>,
}

impl Arm for NoLumen {
    fn id(&self) -> &str {
        &self.entry.id
    }
    fn describe(&self) -> ArmDescription {
        describe_entry(&self.entry, telemetry_deviation(&self.entry))
    }
    fn install(&self, ctx: &InstallCtx<'_>) -> Res<ArmPlan> {
        write_under(
            ctx.home,
            ".claude.json",
            &json_bytes(&base_claude_json()),
            false,
        )?;
        Ok(ArmPlan {
            arm: self.entry.id.clone(),
            env: plan_env(&self.entry, &self.claude_env),
            written: vec![".claude.json".into()],
            ..ArmPlan::default()
        })
    }
}

// --- Lumen at a version ----------------------------------------------------------------------------

pub struct LumenArm {
    entry: ArmEntry,
    claude_env: BTreeMap<String, String>,
}

/// The two hook scripts' names, as Setup installs them under `~/.claude/lumen/`.
pub const METER_SCRIPT: &str = "lumen_meter.sh";
pub const INTERCEPT_SCRIPT: &str = "lumen_read_intercept.sh";

/// A hook script that execs `lumen-mcp hook <args>`: the happy path of the script Setup writes
/// (setup.rs `METER_TEMPLATE` and `INTERCEPT_TEMPLATE`), without its fault reporting for a
/// missing binary, which cannot happen here because the sidecar sits next to it.
fn hook_shim(version: &str, claude_home: &str, hook_args: &str) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # lumen-generator: {version}\n\
         # written by the benchmark harness for a scratch HOME; the happy path of Lumen Setup's script\n\
         default_mcp={mcp}\n\
         default_db={db}\n\
         default_spool={spool}\n\
         LUMEN_MCP_BIN=\"${{LUMEN_MCP_BIN:-$default_mcp}}\"\n\
         export LUMEN_DB=\"${{LUMEN_DB:-$default_db}}\"\n\
         export LUMEN_FAULT_SPOOL=\"${{LUMEN_FAULT_SPOOL:-$default_spool}}\"\n\
         shopt -s execfail\n\
         bin=\"$LUMEN_MCP_BIN\"\n\
         [ -x \"$bin\" ] || bin=\"$(command -v lumen-mcp 2>/dev/null)\"\n\
         [ -n \"$bin\" ] && exec \"$bin\" hook {hook_args}\n\
         cat >/dev/null\n\
         exit 0\n",
        mcp = sh_quote(&format!("{claude_home}/{LUMEN_BIN_DIR}/lumen-mcp")),
        db = sh_quote(&format!("{claude_home}/{LUMEN_LEDGER}")),
        spool = sh_quote(&format!("{claude_home}/{LUMEN_SPOOL}")),
    )
}

/// `mcpServers.lumen` as setup.rs `step_register_mcp_in` writes it (`LUMEN_TOK` only for the
/// script-hook versions, which read it).
pub fn mcp_entry(claude_home: &str, script_hooks: bool) -> Value {
    let mut env = serde_json::Map::new();
    env.insert(
        "LUMEN_DB".into(),
        json!(format!("{claude_home}/{LUMEN_LEDGER}")),
    );
    if script_hooks {
        env.insert(
            "LUMEN_TOK".into(),
            json!(format!("{claude_home}/{LUMEN_BIN_DIR}/lumen-tok")),
        );
    }
    json!({
        "type": "stdio",
        "command": format!("{claude_home}/{LUMEN_BIN_DIR}/lumen-mcp"),
        "args": [],
        "env": env,
    })
}

/// `hooks` as setup.rs `step_install_hooks_in` writes it for `version`: one entry per matcher,
/// each with one command hook naming the script as one quoted word. Bash is metered from 1.6.0;
/// the identity matchers exist from 1.8.0.
pub fn hooks_value(version: (u32, u32, u32), claude_home: &str) -> Value {
    let cmd = |script: &str| sh_quote(&format!("{claude_home}/.claude/lumen/{script}"));
    let entry = |matcher: &str, script: &str| {
        json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": cmd(script) }],
        })
    };
    let mut pre = vec![entry("Read", INTERCEPT_SCRIPT)];
    if version >= IDENTITY_FROM {
        for m in lumen_core::identity::MATCHERS {
            pre.push(entry(m, INTERCEPT_SCRIPT));
        }
    }
    let mut post = vec![entry("Read", METER_SCRIPT)];
    if version >= HOOK_CLI_FROM {
        post.push(entry("Bash", METER_SCRIPT));
    }
    json!({ "PreToolUse": pre, "PostToolUse": post })
}

impl Arm for LumenArm {
    fn id(&self) -> &str {
        &self.entry.id
    }
    fn describe(&self) -> ArmDescription {
        let mut dev = telemetry_deviation(&self.entry);
        dev.push(
            "no headless Setup: the MCP entry and hooks are written the way Setup writes them, \
             and the daemon is not started"
                .into(),
        );
        if self
            .entry
            .version
            .as_deref()
            .map(parse_version)
            .transpose()
            .ok()
            .flatten()
            < Some(HOOK_CLI_FROM)
        {
            dev.push("hooks are the tag's committed bash and python3 scripts".into());
        }
        describe_entry(&self.entry, dev)
    }

    fn install(&self, ctx: &InstallCtx<'_>) -> Res<ArmPlan> {
        let entry = &self.entry;
        let version_text = entry
            .version
            .as_deref()
            .ok_or_else(|| format!("arm {} has no version", entry.id))?;
        let version = parse_version(version_text)?;
        let script_hooks = version < HOOK_CLI_FROM;
        let asset = linux_asset(entry)?;
        // Check what we need before anything is written.
        let scripts = if script_hooks {
            let dir = ctx.hook_scripts.ok_or_else(|| {
                format!(
                    "arm {} runs the tag's own hook scripts: pass the tag's .claude/hooks directory",
                    entry.id
                )
            })?;
            let mut s = Vec::new();
            for n in [METER_SCRIPT, INTERCEPT_SCRIPT] {
                s.push((
                    n,
                    std::fs::read(dir.join(n)).ctx(&format!("read the tag's {n}"))?,
                ));
            }
            Some(s)
        } else {
            None
        };

        let package = ctx.fetcher.fetch(entry, asset)?;
        let bin = ctx.home.join(LUMEN_BIN_DIR);
        let sidecars = assets::extract(&package, &asset.name, &bin)?;
        for s in &sidecars {
            set_exec(s, true)?;
        }

        let mut written: Vec<String> = Vec::new();
        for s in assets::SIDECARS {
            written.push(format!("{LUMEN_BIN_DIR}/{s}"));
        }

        let mut claude_json = base_claude_json();
        claude_json["mcpServers"] = json!({ "lumen": mcp_entry(ctx.claude_home, script_hooks) });
        write_under(ctx.home, ".claude.json", &json_bytes(&claude_json), false)?;
        written.push(".claude.json".into());

        let settings = json!({ "hooks": hooks_value(version, ctx.claude_home) });
        write_under(
            ctx.home,
            ".claude/settings.json",
            &json_bytes(&settings),
            false,
        )?;
        written.push(".claude/settings.json".into());

        let mut env = plan_env(entry, &self.claude_env);
        match scripts {
            Some(scripts) => {
                for (n, bytes) in scripts {
                    write_under(ctx.home, &format!(".claude/lumen/{n}"), &bytes, true)?;
                    written.push(format!(".claude/lumen/{n}"));
                }
                // The scripts of those versions read these two from their environment.
                env.insert(
                    "LUMEN_DB".into(),
                    format!("{}/{LUMEN_LEDGER}", ctx.claude_home),
                );
                env.insert(
                    "LUMEN_TOK".into(),
                    format!("{}/{LUMEN_BIN_DIR}/lumen-tok", ctx.claude_home),
                );
            }
            None => {
                for (n, args) in [
                    (METER_SCRIPT, "meter --writer lumen_meter.sh"),
                    (INTERCEPT_SCRIPT, "intercept"),
                ] {
                    write_under(
                        ctx.home,
                        &format!(".claude/lumen/{n}"),
                        hook_shim(version_text, ctx.claude_home, args).as_bytes(),
                        true,
                    )?;
                    written.push(format!(".claude/lumen/{n}"));
                }
            }
        }
        Ok(ArmPlan {
            arm: entry.id.clone(),
            env,
            written,
            provenance: vec![assets::provenance(&asset.name, &asset.sha256)],
            ..ArmPlan::default()
        })
    }
}

// --- the four tools --------------------------------------------------------------------------------

/// squeez, RTK, Headroom and Caveman, each installed the way tools.toml says.
pub struct ToolArm {
    entry: ArmEntry,
    claude_env: BTreeMap<String, String>,
}

impl Arm for ToolArm {
    fn id(&self) -> &str {
        &self.entry.id
    }
    fn describe(&self) -> ArmDescription {
        describe_entry(&self.entry, telemetry_deviation(&self.entry))
    }

    fn install(&self, ctx: &InstallCtx<'_>) -> Res<ArmPlan> {
        let entry = &self.entry;
        let ch = ctx.claude_home;
        write_under(
            ctx.home,
            ".claude.json",
            &json_bytes(&base_claude_json()),
            false,
        )?;
        let mut plan = ArmPlan {
            arm: entry.id.clone(),
            env: plan_env(entry, &self.claude_env),
            written: vec![".claude.json".into()],
            ..ArmPlan::default()
        };
        match entry.id.as_str() {
            "squeez" => {
                let asset = linux_asset(entry)?;
                let file = ctx.fetcher.fetch(entry, asset)?;
                let bytes = std::fs::read(&file).ctx("read the squeez asset")?;
                write_under(ctx.home, ".claude/squeez/bin/squeez", &bytes, true)?;
                plan.written.push(".claude/squeez/bin/squeez".into());
                plan.provenance
                    .push(assets::provenance(&asset.name, &asset.sha256));
                // Defaults: registers into every detected host, here only Claude Code.
                plan.setup.push(vec![
                    format!("{ch}/.claude/squeez/bin/squeez"),
                    "setup".into(),
                ]);
            }
            "rtk" => {
                let asset = linux_asset(entry)?;
                let file = ctx.fetcher.fetch(entry, asset)?;
                let bin = ctx.home.join(".local").join("bin");
                assets::extract(&file, &asset.name, &bin)?;
                plan.written.push(".local/bin/".into());
                plan.provenance
                    .push(assets::provenance(&asset.name, &asset.sha256));
                plan.path_prepend.push(format!("{ch}/.local/bin"));
                // The recommended global hook-first setup; the optional proxy is declined.
                plan.setup.push(vec![
                    format!("{ch}/.local/bin/rtk"),
                    "init".into(),
                    "-g".into(),
                    "--auto-patch".into(),
                ]);
            }
            "headroom" => {
                let version = entry.version.as_deref().ok_or("headroom has no version")?;
                let wheel = linux_asset(entry)?;
                let revision = raw_str(&entry.id, &["model", "commit"])?;
                let weights = raw_str(&entry.id, &["model", "repo"])?;
                plan.provenance
                    .push(assets::provenance(&wheel.name, &wheel.sha256));
                plan.path_prepend.push(format!("{ch}/.local/bin"));
                plan.setup.push(vec![
                    "uv".into(),
                    "tool".into(),
                    "install".into(),
                    "--python".into(),
                    "3.13".into(),
                    "--exclude-newer".into(),
                    format!("{}T00:00:00Z", resolved_on()?),
                    format!("headroom-ai[all]=={version}"),
                ]);
                // Weights at a pinned revision, then the run itself is offline (HF_HUB_OFFLINE).
                // The pilot confirms this command in the image; it is the one line here that
                // tools.toml describes but does not spell out.
                plan.setup.push(vec![
                    "uvx".into(),
                    "--from".into(),
                    "huggingface_hub".into(),
                    "hf".into(),
                    "download".into(),
                    weights,
                    "--revision".into(),
                    revision,
                ]);
                plan.claude_prefix = vec![
                    "headroom".into(),
                    "wrap".into(),
                    "claude".into(),
                    "--".into(),
                ];
            }
            "caveman" => {
                let tag = entry.tag.as_deref().ok_or("caveman has no tag")?;
                let sha = raw_str(&entry.id, &["install_script", "sha256"])?;
                let url = format!(
                    "https://raw.githubusercontent.com/JuliusBrussee/caveman/{tag}/install.sh"
                );
                // The script is run only if it is the one whose sha256 is pinned.
                let script = format!(
                    "set -e; f=\"$HOME/caveman-install.sh\"; curl -fsSL -o \"$f\" {url}; \
                     echo \"{sha}  $f\" | sha256sum -c -; \
                     bash \"$f\" --only claude --non-interactive"
                );
                plan.setup.push(vec!["bash".into(), "-c".into(), script]);
            }
            other => return Err(format!("no install is written for the tool arm {other:?}")),
        }
        Ok(plan)
    }
}

fn resolved_on() -> Res<String> {
    let path = bench_dir().join("competitors").join("tools.toml");
    let text = std::fs::read_to_string(&path).ctx(&format!("read {}", path.display()))?;
    let doc: toml::Table = text.parse().ctx("tools.toml")?;
    doc.get("meta")
        .and_then(|m| m.get("resolved_on"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| "tools.toml has no meta.resolved_on".to_string())
}
