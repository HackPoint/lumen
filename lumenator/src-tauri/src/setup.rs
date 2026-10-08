#![allow(clippy::unnecessary_map_or)] // map_or style kept for clarity in async context
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

// ── Result types ──────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum StepStatus {
    Ok,
    Warn,
    Error,
    Skip,
}

#[derive(Serialize, Clone, Debug)]
pub struct SetupStep {
    pub id: String,
    pub label: String,
    pub status: StepStatus,
    pub detail: String,
}

impl SetupStep {
    fn ok(id: &str, label: &str, detail: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Ok,
            detail: detail.into(),
        }
    }
    fn warn(id: &str, label: &str, detail: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Warn,
            detail: detail.into(),
        }
    }
    fn err(id: &str, label: &str, detail: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Error,
            detail: detail.into(),
        }
    }
    fn skip(id: &str, label: &str, detail: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: StepStatus::Skip,
            detail: detail.into(),
        }
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// Returns true when setup has not been completed yet.
#[tauri::command]
pub fn lumen_setup_needed() -> bool {
    !marker_path().exists()
}

/// Run all setup steps. Returns one entry per step.
#[tauri::command]
pub fn lumen_run_setup(app: AppHandle) -> Vec<SetupStep> {
    run_setup(&PluginAutoStart(&app))
}

/// Remove all Lumen configuration from ~/.claude/. Returns one entry per action.
#[tauri::command]
pub fn lumen_uninstall(app: AppHandle) -> Vec<SetupStep> {
    run_uninstall(&PluginAutoStart(&app))
}

// ── Launch at login ───────────────────────────────────────────────────────────

/// The slice of the autostart plugin this module needs.
///
/// The plugin's manager hangs off a live `AppHandle`, which cannot be built in a
/// unit test, so the steps below take this trait and the real implementation is a
/// thin wrapper over the plugin.
pub trait AutoStart {
    fn is_enabled(&self) -> Result<bool, String>;
    fn enable(&self) -> Result<(), String>;
    fn disable(&self) -> Result<(), String>;
}

const AUTOSTART_ID: &str = "autostart";
const AUTOSTART_LABEL: &str = "Start Lumen at login";

/// Register Lumen as a login item.
///
/// Failure is a warning, never an error: both windows start hidden and the tray
/// is the whole interface, so not being a login item is an inconvenience, not a
/// broken install — and `run_setup` only writes its completion marker when every
/// step is Ok or Warn. An Error here would make setup repeat forever on a machine
/// whose login-item mechanism is unavailable or locked down by policy.
pub fn step_enable_autostart(a: &dyn AutoStart) -> SetupStep {
    match a.is_enabled() {
        // Enabling something already enabled is not an error, but reporting it
        // as freshly done would be a lie.
        Ok(true) => SetupStep::ok(AUTOSTART_ID, AUTOSTART_LABEL, "Already enabled"),
        Ok(false) => match a.enable() {
            Ok(()) => SetupStep::ok(
                AUTOSTART_ID,
                AUTOSTART_LABEL,
                "Enabled — Lumen starts with your session",
            ),
            Err(e) => SetupStep::warn(
                AUTOSTART_ID,
                AUTOSTART_LABEL,
                &format!("Could not enable: {e}"),
            ),
        },
        Err(e) => SetupStep::warn(
            AUTOSTART_ID,
            AUTOSTART_LABEL,
            &format!("Could not read the current setting: {e}"),
        ),
    }
}

/// Deregister the login item. The mirror of [`step_enable_autostart`], so
/// uninstall leaves nothing behind that would relaunch a removed app.
pub fn step_disable_autostart(a: &dyn AutoStart) -> SetupStep {
    const LABEL: &str = "Remove login item";
    match a.is_enabled() {
        Ok(false) => SetupStep::skip(AUTOSTART_ID, LABEL, "Was not enabled"),
        Ok(true) => match a.disable() {
            Ok(()) => SetupStep::ok(AUTOSTART_ID, LABEL, "Lumen no longer starts at login"),
            Err(e) => SetupStep::warn(AUTOSTART_ID, LABEL, &format!("Could not disable: {e}")),
        },
        Err(e) => SetupStep::warn(
            AUTOSTART_ID,
            LABEL,
            &format!("Could not read the current setting: {e}"),
        ),
    }
}

/// Register the login item once, for installs that predate this feature.
///
/// [`step_enable_autostart`] only runs inside `run_setup`, which is skipped
/// entirely once `.setup_done` exists — so every user who had already set Lumen up
/// would never get a login item, no matter how many times they upgraded. This runs
/// at startup to close that gap.
///
/// Guarded by its own marker, not the setup one: a user who turns the toggle off
/// must not find it switched back on at the next launch. The marker is written
/// only once the item is actually registered (or found already registered), so a
/// transient failure is retried next time rather than silently given up on.
///
/// Returns true when it registered the item on this call.
pub fn ensure_autostart_once(a: &dyn AutoStart, marker: &Path, current_exe: &str) -> bool {
    let recorded = std::fs::read_to_string(marker).ok();
    // The marker stores the executable path it registered, so a login item left
    // pointing somewhere stale can be spotted and refreshed. That is not
    // hypothetical: the app's executable was renamed from `lumen` to `Lumen` when
    // the CLI sidecar stopped colliding with it, and an app moved out of
    // /Applications changes path too. A login item aimed at a path that no longer
    // exists fails silently at the one moment it is supposed to work.
    let stale = recorded.as_deref().is_some_and(|r| r.trim() != current_exe);
    if recorded.is_some() && !stale {
        return false;
    }

    // Not retried and not fatal, but said: without the marker the next launch takes the
    // item for never registered, and switches it back on if the user has turned it off.
    let record = || {
        let written = marker
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(marker, current_exe));
        if let Err(e) = written {
            log::warn!(
                "could not record the login item in {}: {e}; turning it off may not last past the next launch",
                marker.display()
            );
        }
    };

    match a.is_enabled() {
        Ok(true) if stale => {
            // Re-registering rewrites the entry with the current path. Enabling an
            // already-enabled item is how the plugin refreshes it.
            match a.enable() {
                Ok(()) => {
                    log::info!("refreshed the login item to {current_exe}");
                    record();
                    true
                }
                Err(e) => {
                    log::warn!("could not refresh the login item: {e}");
                    false
                }
            }
        }
        // Already on — nothing to do, but record it so a later opt-out sticks.
        Ok(true) => {
            record();
            false
        }
        // Disabled and previously recorded means the user turned it off. Update the
        // stored path so a later re-enable is not mistaken for staleness, but do
        // not switch it back on.
        Ok(false) if recorded.is_some() => {
            record();
            false
        }
        Ok(false) => match a.enable() {
            Ok(()) => {
                record();
                true
            }
            Err(e) => {
                log::warn!("could not register the login item: {e}");
                false
            }
        },
        Err(e) => {
            log::warn!("could not read the login-item state: {e}");
            false
        }
    }
}

/// Marker recording that the one-time login-item registration has happened.
fn autostart_marker_in(home: &Path) -> PathBuf {
    lumen_dir_in(home).join(".autostart_done")
}

/// Run the one-time registration against the real home, plugin and executable.
pub fn ensure_autostart_once_for(app: &AppHandle) -> bool {
    // An unresolvable current_exe would make every launch look stale and rewrite
    // the login item forever, so fall back to a constant instead of a guess.
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    ensure_autostart_once(&PluginAutoStart(app), &autostart_marker_in(&home()), &exe)
}

/// [`AutoStart`] backed by the real plugin.
struct PluginAutoStart<'a>(&'a AppHandle);

impl AutoStart for PluginAutoStart<'_> {
    fn is_enabled(&self) -> Result<bool, String> {
        use tauri_plugin_autostart::ManagerExt;
        self.0.autolaunch().is_enabled().map_err(|e| e.to_string())
    }
    fn enable(&self) -> Result<(), String> {
        use tauri_plugin_autostart::ManagerExt;
        self.0.autolaunch().enable().map_err(|e| e.to_string())
    }
    fn disable(&self) -> Result<(), String> {
        use tauri_plugin_autostart::ManagerExt;
        self.0.autolaunch().disable().map_err(|e| e.to_string())
    }
}

/// Is Lumen currently registered to start at login?
#[tauri::command]
pub fn lumen_autostart_enabled(app: AppHandle) -> bool {
    // A read failure reports "off" rather than propagating: the Setup screen
    // shows this as a toggle, and a toggle has to render something.
    PluginAutoStart(&app).is_enabled().unwrap_or(false)
}

/// Turn the login item on or off, returning the state actually achieved.
#[tauri::command]
pub fn lumen_set_autostart(app: AppHandle, enable: bool) -> Result<bool, String> {
    let a = PluginAutoStart(&app);
    if enable {
        a.enable()?
    } else {
        a.disable()?
    }
    a.is_enabled()
}

/// Bundle identifier, matching `identifier` in tauri.conf.json.
/// Re-exported from lumen-core, which owns the canonical value.
// Only the tests read this now that app_support_dir_in delegates to lumen-core.
// Kept because one of them pins it against tauri.conf.json's identifier, which is
// the check that stops the bundle id and the data directory drifting apart.
#[allow(dead_code)]
const APP_ID: &str = lumen_core::meter::APP_ID;

// ── Standard path helpers ─────────────────────────────────────────────────────
//
// Every path below is derived from a `home` argument, and the no-argument
// wrappers pass the real one. Tests call the `*_in` forms with a tempdir so they
// can never touch the developer's own ~/.claude — this module rewrites
// ~/.claude.json and ~/.claude/settings.json, so a test that resolved the real
// home would corrupt the machine it runs on.

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn lumen_dir_in(home: &Path) -> PathBuf {
    home.join(".claude/lumen")
}

fn marker_path_in(home: &Path) -> PathBuf {
    lumen_dir_in(home).join(".setup_done")
}

fn claude_json_path_in(home: &Path) -> PathBuf {
    home.join(".claude.json")
}

fn global_settings_path_in(home: &Path) -> PathBuf {
    home.join(".claude/settings.json")
}

#[cfg_attr(not(test), allow(dead_code))]
fn lumen_dir() -> PathBuf {
    lumen_dir_in(&home())
}

fn marker_path() -> PathBuf {
    marker_path_in(&home())
}

// ── Binary resolution ─────────────────────────────────────────────────────────
//
// In release (.app bundle): sidecars sit alongside the main exe in
// Contents/MacOS/, so current_exe().parent().join("lumen-mcp") resolves them.
//
// In dev: the Tauri app binary is somewhere in target/debug/, but the release
// sidecars are in target/release/. Walk up the exe path to find the workspace.

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn find_binary(name: &str) -> Option<PathBuf> {
    find_binary_from(&exe_dir(), name)
}

/// [`find_binary`] from an explicit directory, so the lookup is testable.
///
/// `name` is the bare sidecar name; the file is `name` plus the platform's
/// executable suffix. Windows bundles `lumen-mcp.exe`, and a probe for plain
/// `lumen-mcp` cannot match it.
fn find_binary_from(start: &Path, name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    // 1. Release path: sibling of the main exe
    let beside = start.join(&file);
    if beside.exists() {
        return Some(beside);
    }
    // 2. Dev path: walk up looking for workspace/target/release/<name>
    let mut probe = start.to_path_buf();
    for _ in 0..8 {
        let candidate = probe.join("target/release").join(&file);
        if candidate.exists() {
            return Some(candidate);
        }
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => break,
        }
    }
    None
}

/// Per-user application data directory, resolved per OS.
///
/// macOS keeps the path it has always used, so existing installs need no
/// migration. Linux follows the XDG default and Windows uses Roaming AppData.
/// The `$XDG_DATA_HOME` override is deliberately not honoured: every path in this
/// module derives from a single `home` argument so tests can never escape their
/// tempdir, and one env var reading differently in tests than in production is
/// exactly the class of bug that costs more than it saves.
fn app_support_dir_in(home: &Path) -> PathBuf {
    // Delegated to lumen-core so there is exactly one definition of where Lumen's
    // data lives. Two copies of this would be the split-ledger bug in a new place:
    // the GUI and the metering writers must agree on one directory, and a drift
    // between them fails silently — both writes succeed, to different files.
    lumen_core::meter::app_data_dir_in(home)
}

fn app_support_dir() -> PathBuf {
    app_support_dir_in(&home())
}

// macOS runs unsigned / quarantined apps from ephemeral, read-only locations:
// a mounted DMG (`/Volumes/…`) or a Gatekeeper App Translocation mount
// (`/private/var/folders/…/AppTranslocation/…`). Both disappear when the DMG is
// ejected or the app is moved, leaving the MCP `command` path we recorded in
// ~/.claude.json dangling — Claude Code then fails with
// `ENOENT … posix_spawn '/Volumes/…/lumen-mcp'`.
/// Both markers are macOS-specific, so this is always false on Linux and
/// Windows — where packages install to a stable prefix and the problem does not
/// arise. Kept unconditional so the logic has one shape on every platform.
fn is_ephemeral_path(p: &std::path::Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("/Volumes/") || s.contains("/AppTranslocation/")
}

// Resolve a sidecar binary to a path that survives the DMG being ejected. If the
// bundled binary lives on an ephemeral mount, copy it into a stable, user-writable
// location (`…/io.speedata.lumen/bin/<name>`) and return that; otherwise return
// the resolved path unchanged. Copying the standalone Mach-O preserves its
// embedded code signature, so it still launches.
fn stable_binary(name: &str) -> Option<PathBuf> {
    let found = find_binary(name)?;
    if !is_ephemeral_path(&found) {
        return Some(found);
    }
    let bin_dir = app_support_dir().join("bin");
    if std::fs::create_dir_all(&bin_dir).is_err() {
        return Some(found); // fall back rather than failing setup outright
    }
    let dest = bin_dir.join(found.file_name().unwrap_or(name.as_ref()));
    if std::fs::copy(&found, &dest).is_err() {
        return Some(found);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    Some(dest)
}

/// The two values Setup bakes into the hook scripts: the ledger they write and the
/// `lumen-mcp` they exec. `mcp` is empty when this build cannot find its sidecar.
///
/// Resolved from `home` rather than the real home, so a test that installs into a
/// scratch home validates against what it installed.
fn baked_paths(home: &Path) -> (String, String) {
    let db = std::env::var("LUMEN_DB").unwrap_or_else(|_| db_path_in(home));
    let mcp = stable_binary("lumen-mcp")
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    (db, mcp)
}

/// The metering DB inside the per-OS data directory. Derived from
/// [`app_support_dir_in`] so the two can never disagree about where data lives.
fn db_path_in(home: &Path) -> String {
    app_support_dir_in(home)
        .join("lumen.db")
        .to_string_lossy()
        .to_string()
}

// ── Atomic, mode-preserving writes ────────────────────────────────────────────

/// The file's current permission bits, or a private default when it is absent.
///
/// Defaulting to 0o600 rather than 0o644 keeps a newly created config private;
/// callers that need an executable bit pass it as `mode_floor` below.
#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600)
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> u32 {
    0
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// Flush the directory entry so the rename itself survives a crash.
///
/// Syncing the temp file only guarantees its *contents*; the rename is a
/// directory operation and needs the directory synced to be durable.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Write `contents` to `path` so a concurrent reader never sees a partial file.
///
/// `fs::write` truncates in place, so Claude Code — which may read
/// ~/.claude.json or settings.json at any moment, including while Setup runs —
/// can observe a half-written file. Writing to a temp file and renaming over the
/// target is atomic within a directory: a reader sees either the old file or the
/// new one, never a truncated one. A corrupted MCP config would be a worse bug
/// than the truncation window it replaces.
///
/// `mode_floor` is OR-ed into the existing mode rather than replacing it. Exact
/// preservation would perpetuate breakage — a script that already lost its exec
/// bit would stay dead — while ignoring the existing mode would discard a
/// deliberately tightened permission. Pass 0 when no bit is functionally
/// required.
fn write_atomic(path: &Path, contents: &str, mode_floor: u32) -> std::io::Result<()> {
    // Resolve symlinks first. Users who keep ~/.claude.json in a dotfile repo
    // symlink it, and renaming onto the link would replace it with a regular
    // file and silently detach their config. The temp must also live in the
    // *resolved* directory: a rename across filesystems fails with EXDEV, which
    // is exactly the case a dotfile repo on another volume produces.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = match target.parent() {
        Some(d) => d.to_path_buf(),
        None => return Err(std::io::Error::other("target has no parent directory")),
    };
    std::fs::create_dir_all(&dir)?;

    let mode = mode_of(&target) | mode_floor;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "lumen".to_string());
    let tmp = dir.join(format!(".{name}.lumen-tmp"));

    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    // Mode before rename, so the file is never visible at the target path with
    // the wrong permissions.
    set_mode(&tmp, mode)?;
    if let Err(e) = std::fs::rename(&tmp, &target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(&dir);
    Ok(())
}

// ── Artifact freshness ────────────────────────────────────────────────────────

/// Diagnostic marker naming the version that generated an artifact.
///
/// Deliberately excluded from the comparison below. If the version were part of
/// the compared content, every release would rewrite every artifact on every
/// machine — reintroducing the install-base-wide blast radius that generating
/// from content is meant to avoid.
const GENERATOR_PREFIX: &str = "# lumen-generator:";

fn stamp_line() -> String {
    format!("{GENERATOR_PREFIX} {}\n", env!("CARGO_PKG_VERSION"))
}

/// An artifact's content with the diagnostic stamp and trailing blanks removed.
fn functional(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with(GENERATOR_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// What a report-only artifact check concluded.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStatus {
    pub id: String,
    /// True when the artifact is present and everything baked into it resolves.
    pub healthy: bool,
    pub detail: String,
}

/// Bring the hook scripts back into line with this build, if they have drifted.
///
/// Scripts auto-repair; `mcp` and `hooks` deliberately do not. The scripts live in
/// `~/.claude/lumen/` and are Lumen's own, so a bad write damages nothing else —
/// whereas `~/.claude.json` holds every MCP server the user has, and corrupting it
/// would break Claude Code itself rather than just Lumen. Those two are validated
/// and reported, and repaired only on an explicit button press.
///
/// This is also what unblocks E7: the meter must be regenerated before it can
/// record session_id, req_key, file_mtime or token_source. Without this, the
/// migration would add the columns and a stale meter would write NULL into them
/// forever, while every report looked successful.
///
/// Returns a description of what it repaired, or None when nothing was needed.
pub fn ensure_scripts_fresh_in(home: &Path, db: &str, mcp_bin: &str) -> Option<String> {
    let dir = lumen_dir_in(home);
    // Absent directory means setup never ran; that is not drift, and creating
    // scripts for someone who never set Lumen up would be unasked-for.
    if !dir.exists() {
        return None;
    }

    let mut repaired = Vec::new();
    for (name, desired) in [
        ("lumen_meter.sh", desired_meter_script(db, mcp_bin)),
        (
            "lumen_read_intercept.sh",
            desired_intercept_script(db, mcp_bin),
        ),
    ] {
        let path = dir.join(name);
        if !script_needs_refresh(&path, &desired) {
            continue;
        }
        match write_atomic(&path, &desired, 0o755) {
            Ok(()) => repaired.push(name),
            Err(e) => log::warn!("could not refresh {name}: {e}"),
        }
    }

    if repaired.is_empty() {
        None
    } else {
        Some(format!("refreshed {}", repaired.join(", ")))
    }
}

/// Repair drifted hook scripts against the real home, resolving the same paths
/// Setup would bake. Called once at startup.
pub fn ensure_scripts_fresh() -> Option<String> {
    let home = home();
    let (db, mcp) = baked_paths(&home);
    if mcp.is_empty() {
        // Nothing to bake. The scripts on disk may name a lumen-mcp that still runs,
        // and a rewrite would replace it with a PATH lookup that may not. Leave them
        // and let validation report it.
        log::warn!("lumen-mcp not found; leaving hook scripts untouched");
        return None;
    }
    ensure_scripts_fresh_in(&home, &db, &mcp)
}

/// The reported artifacts, against the real home. Exposed to the Setup screen.
#[tauri::command]
pub fn lumen_artifact_health() -> Vec<ArtifactStatus> {
    validate_reported_artifacts_in(&home())
}

/// Check the artifacts that are *not* auto-repaired, for reporting only.
///
/// Nothing here writes. A user acts on the result via the Setup screen.
pub fn validate_reported_artifacts_in(home: &Path) -> Vec<ArtifactStatus> {
    let (db, mcp) = baked_paths(home);
    validate_reported_artifacts_with(home, &db, &mcp)
}

/// [`validate_reported_artifacts_in`] against explicit baked values: the scripts are
/// current when they match what this build would write for `db` and `mcp_bin`.
fn validate_reported_artifacts_with(home: &Path, db: &str, mcp_bin: &str) -> Vec<ArtifactStatus> {
    // Iterating PERSISTED_ARTIFACTS rather than hardcoding a list is what makes the
    // registry load-bearing: add an artifact and forget to handle it here, and the
    // wildcard arm reports it as unchecked instead of silently omitting it.
    PERSISTED_ARTIFACTS
        .iter()
        .map(|id| match *id {
            "scripts" => validate_scripts_in(home, db, mcp_bin),
            "mcp" => validate_mcp_in(home),
            "hooks" => validate_hooks_in(home),
            "autostart" => ArtifactStatus {
                id: "autostart".into(),
                healthy: true,
                detail: "user-owned; repaired by its own staleness check".into(),
            },
            "cli" => validate_cli(),
            other => ArtifactStatus {
                id: other.into(),
                healthy: false,
                detail: "no validator wired for this artifact".into(),
            },
        })
        .collect()
}

/// Are the hook scripts current with this build?
fn validate_scripts_in(home: &Path, db: &str, mcp_bin: &str) -> ArtifactStatus {
    let dir = lumen_dir_in(home);
    if !dir.exists() {
        return ArtifactStatus {
            id: "scripts".into(),
            healthy: false,
            detail: "not installed — run Setup".into(),
        };
    }
    let stale: Vec<&str> = [
        ("lumen_meter.sh", desired_meter_script(db, mcp_bin)),
        (
            "lumen_read_intercept.sh",
            desired_intercept_script(db, mcp_bin),
        ),
    ]
    .into_iter()
    .filter(|(name, want)| script_needs_refresh(&dir.join(name), want))
    .map(|(name, _)| name)
    .collect();
    if stale.is_empty() {
        ArtifactStatus {
            id: "scripts".into(),
            healthy: true,
            detail: "current with this build".into(),
        }
    } else {
        ArtifactStatus {
            id: "scripts".into(),
            healthy: false,
            detail: format!("stale: {} (repaired on next launch)", stale.join(", ")),
        }
    }
}

/// The CLI symlink. Never rewritten when Homebrew owns it — two managers fighting
/// over one path would be a new bug of the class this release closes.
fn validate_cli() -> ArtifactStatus {
    let link = cli_symlink_path();
    let expected = find_binary("lumen-cli").unwrap_or_default();
    let resolve = |p: &Path| std::fs::canonicalize(p).ok();
    let detail = match classify_cli(&link, &expected, &resolve) {
        CliState::Absent => "not installed (optional)".to_string(),
        CliState::Homebrew => "managed by Homebrew — left alone".to_string(),
        CliState::Current => "points at this build".to_string(),
        CliState::Stale => format!("stale target — re-run Install CLI ({})", link.display()),
    };
    let healthy = !detail.starts_with("stale");
    ArtifactStatus {
        id: "cli".into(),
        healthy,
        detail,
    }
}

fn validate_mcp_in(home: &Path) -> ArtifactStatus {
    // An unreadable file is reported as one. "No lumen entry — run Setup" would send
    // the user to a Setup that refuses the same file.
    let mcp = match read_config_object(&claude_json_path_in(home)) {
        Ok(v) => v.get("mcpServers").and_then(|m| m.get("lumen")).cloned(),
        Err(e) => {
            return ArtifactStatus {
                id: "mcp".into(),
                healthy: false,
                detail: e,
            }
        }
    };
    match mcp {
        None => ArtifactStatus {
            id: "mcp".into(),
            healthy: false,
            detail: "no lumen entry in ~/.claude.json — run Setup".into(),
        },
        Some(entry) => {
            let mut dead: Vec<String> = Vec::new();
            if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                if !Path::new(cmd).exists() {
                    dead.push(format!("command {cmd}"));
                }
            }
            if let Some(env) = entry.get("env").and_then(|e| e.as_object()) {
                for (k, v) in env {
                    if let Some(p) = v.as_str() {
                        // LUMEN_DB is created on demand, so its absence is normal.
                        // `is_absolute`, not a leading '/': on Windows a path starts
                        // with a drive letter, and the check skipped every one.
                        if k != "LUMEN_DB" && Path::new(p).is_absolute() && !Path::new(p).exists() {
                            dead.push(format!("{k} {p}"));
                        }
                    }
                }
            }
            if dead.is_empty() {
                ArtifactStatus {
                    id: "mcp".into(),
                    healthy: true,
                    detail: "registered, all paths resolve".into(),
                }
            } else {
                ArtifactStatus {
                    id: "mcp".into(),
                    healthy: false,
                    detail: format!("dangling: {}", dead.join("; ")),
                }
            }
        }
    }
}

fn validate_hooks_in(home: &Path) -> ArtifactStatus {
    // hooks: valid when the registered matcher set matches what this build wants AND
    // every lumen hook command points at a file that exists.
    //
    // The matcher comparison is the half that was missing. Checking only for dangling
    // paths meant an install carrying the pre-1.2.1 matcher set reported "5 hook
    // commands, all present — healthy" while `Bash` was not registered at all, so the
    // meter's Bash arm could never run and the Setup screen said nothing was wrong.
    // Hooks are validate-and-report rather than auto-repaired, which makes an accurate
    // report the entire mechanism: a validator that cannot see a stale desired-state
    // silently withholds the one prompt the user needs.
    //
    // It carries no stamp of its own — settings.json's schema is Claude Code's, and
    // injecting an unknown key risks rejection by a validator we do not own — so the
    // desired state is compared directly instead.
    //
    // A command is read the way Claude Code reads it, not as a path. Before 1.6.0
    // Setup registered the bare path, and a home with a space in it made that two
    // words: the file existed, so this reported healthy, and the hook never ran.
    let settings = global_settings_path_in(home);
    let mut dangling: Vec<String> = Vec::new();
    let mut unquoted: Vec<String> = Vec::new();
    let mut found = 0usize;
    let mut registered: Vec<(&str, String)> = Vec::new();

    let v = match read_config_object(&settings) {
        Ok(v) => v,
        Err(e) => {
            return ArtifactStatus {
                id: "hooks".into(),
                healthy: false,
                detail: e,
            }
        }
    };
    {
        for phase in ["PreToolUse", "PostToolUse"] {
            if let Some(arr) = v["hooks"][phase].as_array() {
                for entry in arr {
                    let matcher = entry["matcher"].as_str().unwrap_or("").to_string();
                    if let Some(hs) = entry["hooks"].as_array() {
                        for h in hs {
                            if let Some(c) = h["command"].as_str() {
                                if c.contains("lumen_") {
                                    found += 1;
                                    registered.push((phase, matcher.clone()));
                                    if command_needs_quotes(c) {
                                        unquoted.push(c.to_string());
                                    } else {
                                        let p = hook_command_path(c);
                                        if !Path::new(&p).exists() {
                                            dangling.push(p);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if found == 0 {
        return ArtifactStatus {
            id: "hooks".into(),
            healthy: false,
            detail: "no lumen hooks registered — run Setup".into(),
        };
    }

    let has = |phase: &str, m: &str| registered.iter().any(|(p, r)| *p == phase && r == m);

    let mut problems: Vec<String> = Vec::new();
    if !dangling.is_empty() {
        problems.push(format!("dangling: {}", dangling.join("; ")));
    }
    if !unquoted.is_empty() {
        problems.push(format!(
            "will not run as registered (unquoted path): {} — re-run Setup",
            unquoted.join("; ")
        ));
    }
    let absent: Vec<&str> = METER_MATCHERS
        .iter()
        .copied()
        .filter(|m| !has("PostToolUse", m))
        .collect();
    if !absent.is_empty() {
        problems.push(format!("PostToolUse not registered: {}", absent.join(", ")));
    }
    if !has("PreToolUse", INTERCEPT_MATCHER) {
        problems.push(format!("PreToolUse not registered: {INTERCEPT_MATCHER}"));
    }
    let stale: Vec<&str> = RETIRED_MATCHERS
        .iter()
        .copied()
        .filter(|m| has("PostToolUse", m))
        .collect();
    if !stale.is_empty() {
        problems.push(format!("retired, still registered: {}", stale.join(", ")));
    }

    if problems.is_empty() {
        ArtifactStatus {
            id: "hooks".into(),
            healthy: true,
            detail: format!("{found} hook commands, all present and current"),
        }
    } else {
        ArtifactStatus {
            id: "hooks".into(),
            healthy: false,
            detail: problems.join("; "),
        }
    }
}

/// Tools whose PostToolUse events the meter must receive.
///
/// One source of truth for the installer and the validator, deliberately. When
/// `step_install_hooks_in` held this list privately, changing it left
/// `validate_hooks_in` measuring the old contract — so an install missing `Bash`
/// reported itself healthy and the user was never prompted to repair it. Hooks are
/// validate-and-report rather than auto-repaired, which makes the report the whole
/// mechanism.
///
/// `Bash` is observation only: output volume, no PreToolUse hook, nothing intercepted.
const METER_MATCHERS: [&str; 2] = ["Read", "Bash"];

/// The tool the PreToolUse intercept applies to.
const INTERCEPT_MATCHER: &str = "Read";

/// Matchers earlier versions registered that must now be removed.
///
/// The `mcp__lumen__*` tools meter themselves in-process, so the meter script's `case`
/// fell straight through to `exit 0` for them — every `smart_read`, `recall_file` and
/// `compress_logs` call forked a bash and a python3 to do nothing. Their continued
/// presence is a defect the validator reports, not merely untidiness.
const RETIRED_MATCHERS: [&str; 3] = [
    "mcp__lumen__smart_read",
    "mcp__lumen__recall_file",
    "mcp__lumen__compress_logs",
];

/// Every artifact setup persists outside the app bundle.
///
/// This list exists so a fourth instance of the same bug becomes impossible
/// rather than merely unlikely. Three shipped already — MCP paths (1.0.1),
/// autostart (1.1.2), the tokenizer path (1.1.5) — and all three had the same
/// shape: logic reachable only from `run_setup`, which never runs again once its
/// marker exists. `every_step_run_setup_emits_is_accounted_for` fails if a new
/// step appears here or in [`NON_PERSISTING`] without a freshness check.
pub const PERSISTED_ARTIFACTS: &[&str] = &["scripts", "mcp", "hooks", "autostart", "cli"];

/// Steps that persist nothing and so need no freshness check.
///
/// "detect" — not "claude": the id comes from `step_detect_claude_in`'s own
/// `SetupStep::ok("detect", …)`. The registry test below caught this the first
/// time it ran, which is the behaviour it exists for.
// Consumed by the coverage test, not by production code: the contract is enforced
// at test time, which is what makes forgetting it a build failure rather than a
// silent omission.
#[allow(dead_code)]
pub const NON_PERSISTING: &[&str] = &["detect"];

/// Artifacts whose very existence is a user decision.
///
/// Repair them if present; never create them, never re-enable them. Creating one
/// unasked is the 1.1.2 bug wearing a different hat — a refresh that silently
/// switched a login item back on for someone who had turned it off.
#[allow(dead_code)]
pub const USER_OWNED: &[&str] = &["autostart", "cli"];

/// Is this path managed by Homebrew?
///
/// Lumen and `brew` must not both own `bin/lumen`. If the symlink resolves into a
/// Homebrew prefix then brew installed it, brew will re-link it on upgrade, and a
/// repair loop fighting brew would be a new bug of exactly the class this release
/// closes. Detected by path rather than by shelling out to `brew`, which may not
/// exist on the machine being repaired.
fn is_homebrew_managed(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.contains("/Cellar/")
        || s.starts_with("/opt/homebrew/")
        || s.starts_with("/usr/local/Homebrew/")
        || s.starts_with("/home/linuxbrew/")
}

/// What a `cli` artifact check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliState {
    /// No symlink. The user never asked for one; do nothing.
    Absent,
    /// Managed by Homebrew. Validate and report, never rewrite.
    Homebrew,
    /// Ours and pointing where it should.
    Current,
    /// Ours but pointing somewhere stale — 1.1.4 shipped one aimed at the GUI
    /// binary because the CLI sidecar had been overwritten.
    Stale,
}

/// Classify the CLI symlink without touching it.
///
/// `resolve` is injected so the decision table is testable without creating real
/// symlinks in a real PATH directory.
pub fn classify_cli(
    link: &Path,
    expected_target: &Path,
    resolve: &dyn Fn(&Path) -> Option<PathBuf>,
) -> CliState {
    match resolve(link) {
        None => CliState::Absent,
        Some(actual) if is_homebrew_managed(&actual) => CliState::Homebrew,
        Some(actual) if actual == expected_target => CliState::Current,
        Some(_) => CliState::Stale,
    }
}

/// Does the script on disk differ from what this build would generate?
///
/// This is the check that closes the staleness hole. A 0.1.0-era script whose
/// baked paths happen to still resolve passes every liveness test, so anything
/// keyed on "do the paths work" leaves it in place forever — and for E7 that
/// means a meter that never learned to record session_id, req_key or
/// token_source, writing NULLs while the migration reports success. Comparing
/// generated content against the file catches it, because the body differs.
///
/// A missing or unreadable file counts as needing refresh: absence must be loud,
/// which is the lesson of the 1.1.3 regression.
fn script_needs_refresh(path: &Path, desired: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(actual) => functional(&actual) != functional(desired),
        Err(_) => true,
    }
}

// ── Script templates ──────────────────────────────────────────────────────────
//
// Both hooks are shims around `lumen-mcp hook` since 1.6.0. They used to be bash
// driving python3, stat, wc, mktemp and date, and each was a portability hazard: on
// a machine with no `python3` — python.org's Windows layout ships `python` and `py`
// — the meter wrote nothing and the intercept blocked nothing, and neither said so,
// because the fault recorder ran on python3 as well. What is left in shell is
// finding the binary, and reporting when it cannot.
//
// Each template is filled two ways. `shim` makes the copy Setup installs, with three
// values baked in: `__LUMEN_MCP__`, `__LUMEN_DB__` and `__LUMEN_SPOOL__`, each as one
// single-quoted shell word. `plugin_hooks::plugin_script` makes the Claude Code
// plugin's copy in `.claude/hooks/`, which has nothing to bake — a plugin cannot know
// where Lumen is installed. That copy used to be a separate script, and it drifted
// until it wrote to a ledger with no schema and recorded nothing; one template for
// both is the fix.

const METER_TEMPLATE: &str = r#"#!/usr/bin/env bash
# lumen_meter.sh — __LUMEN_ORIGIN__
#
# PostToolUse hook for Read and Bash. `lumen-mcp hook meter` does the metering;
# this finds that binary, and when it cannot, leaves a fault and a line on stderr
# instead of losing the event in silence. It needs bash, cat and date.
#
__LUMEN_LOCATE__

# A failed exec falls through to the report below instead of ending the script.
shopt -s execfail
bin="$LUMEN_MCP_BIN"
[ -x "$bin" ] || bin="$(command -v lumen-mcp 2>/dev/null)"
[ -n "$bin" ] && exec "$bin" hook meter --writer __LUMEN_WRITER__

# Lumen was moved or removed. Exit 0 all the same: a meter must never fail the
# tool call it observes.
cat >/dev/null
kind=meter_write_failed
consequence="this event was not metered"
__LUMEN_REPORT__"#;

const INTERCEPT_TEMPLATE: &str = r#"#!/usr/bin/env bash
# lumen_read_intercept.sh — __LUMEN_ORIGIN__
#
# PreToolUse hook for Read. `lumen-mcp hook intercept` decides whether a large
# source or log file goes to the lumen tools (exit 2) or is read as asked (exit 0).
# Without that binary the MCP server cannot be running either, so the read is let
# through, with a fault and a line on stderr. LUMEN_HOOK_ENABLED=0 switches the
# intercept off.
#
__LUMEN_LOCATE__

# A failed exec falls through to the report below instead of ending the script.
shopt -s execfail
bin="$LUMEN_MCP_BIN"
[ -x "$bin" ] || bin="$(command -v lumen-mcp 2>/dev/null)"
[ -n "$bin" ] && exec "$bin" hook intercept

# Lumen was moved or removed. Exit 0: the tools a block would send the model to
# are served by the binary that is missing.
cat >/dev/null
[ "${LUMEN_HOOK_ENABLED:-1}" = "0" ] && exit 0
kind=hook_fail_open
consequence="this read was let through"
__LUMEN_REPORT__"#;

/// Where an installed shim says it came from.
const INSTALLED_ORIGIN: &str = "installed by Lumen Setup.
# Setup regenerates it when it drifts from the running build; do not hand-edit.";

/// How an installed shim finds the binary, the ledger and the spool: Setup baked them.
const INSTALLED_LOCATE: &str = r#"# Each baked value is a default, not a constant, so a test can point the hook at a
# scratch ledger and a stub binary without editing it.
default_mcp=__LUMEN_MCP__
default_db=__LUMEN_DB__
default_spool=__LUMEN_SPOOL__
LUMEN_MCP_BIN="${LUMEN_MCP_BIN:-$default_mcp}"
export LUMEN_DB="${LUMEN_DB:-$default_db}"
export LUMEN_FAULT_SPOOL="${LUMEN_FAULT_SPOOL:-$default_spool}""#;

/// The end of both shims, reached only when `lumen-mcp` cannot run. It writes the
/// fault `lumen_core::faults::FaultRecord::now_with_env` would have: the same
/// session and channel rules, and the version of the build that generated the
/// script, read from its own stamp.
const REPORT_TAIL: &str = r##"if [ -n "$bin" ]; then variant=lumen_mcp_unrunnable; else variant=lumen_mcp_missing; fi
echo "lumen: cannot run lumen-mcp (looked for '$LUMEN_MCP_BIN', then on PATH); $consequence" >&2
[ "${LUMEN_CAPTURE:-1}" = "0" ] && exit 0

# A value enters the JSON line only if it cannot break it.
json() { case "$1" in "" | *[!A-Za-z0-9._-]*) printf 'null' ;; *) printf '"%s"' "$1" ;; esac; }
version=""
while IFS= read -r line; do
    case "$line" in "# lumen-generator: "*) version="${line#"# lumen-generator: "}"; break ;; esac
done 2>/dev/null <"$0"
# lumen_core::meter::channel_from, in shell.
case "${CLAUDE_CODE_ENTRYPOINT:-}" in
    *vscode*) channel=vscode ;;
    ?*) channel=cli ;;
    *) if [ -n "${VSCODE_PID+x}${VSCODE_CWD+x}" ]; then channel=vscode; else channel=unknown; fi ;;
esac
case "${LUMEN_CHANNEL:-}" in "" | *[!A-Za-z0-9._-]*) ;; *) channel="$LUMEN_CHANNEL" ;; esac
printf '{"ts":"%s","kind":"%s","variant":"%s","path":null,"lines":null,"detail":null,"session_id":%s,"version":%s,"channel":"%s"}\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$kind" "$variant" \
    "$(json "${LUMEN_SESSION_ID:-${CLAUDE_CODE_SESSION_ID:-}}")" "$(json "$version")" "$channel" \
    2>/dev/null >>"$LUMEN_FAULT_SPOOL" \
    || echo "lumen: nor could the fault be written to '$LUMEN_FAULT_SPOOL'" >&2
exit 0
"##;

/// `s` as one single-quoted shell word. Inside single quotes nothing is special
/// except the quote itself, which closes, escapes and reopens: `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A path as the hook's shell should see it.
fn shell_path(p: &str) -> String {
    shell_path_for(p, cfg!(windows))
}

/// On Windows, forward slashes. Claude Code runs hooks there through Git Bash, and
/// both bash and Claude Code's own command scan treat an unquoted backslash as an
/// escape; `C:/Users/…` means the same file to bash, to Windows and to Rust. On
/// every other platform a backslash is an ordinary filename byte and is kept.
fn shell_path_for(p: &str, windows: bool) -> String {
    if windows {
        p.replace('\\', "/")
    } else {
        p.to_string()
    }
}

/// The `command` registered in settings.json for a hook script: its path as one
/// quoted word.
///
/// Claude Code hands this to `/bin/sh -c` on macOS and Linux and to Git Bash's
/// `bash -c` on Windows, and on Windows it first scans the command's first word —
/// quotes stripped, backslash escapes taken — and prefixes `bash ` when that word
/// ends in `.sh`. A bare path broke at the first space in it everywhere, and on
/// Windows lost every backslash to that scan. Quoted, the scan sees the whole path
/// on both platforms.
fn hook_command(script: &Path) -> String {
    sh_quote(&shell_path(&script.to_string_lossy()))
}

/// The program a hook `command` runs, read the way Claude Code reads it.
///
/// This is Claude Code's first-word scan (2.1.x): a quote runs to its partner — an
/// unterminated one to the end — a backslash takes the next character as is, and
/// whitespace outside quotes ends the word.
fn hook_command_path(command: &str) -> String {
    let mut out = String::new();
    let mut chars = command.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                for d in chars.by_ref() {
                    if d == c {
                        break;
                    }
                    out.push(d);
                }
            }
            '\\' => out.push(chars.next().unwrap_or('\\')),
            c if c.is_whitespace() => break,
            c => out.push(c),
        }
    }
    out
}

/// Would this command run as a different program than the path it names?
///
/// True for a bare path holding anything the shell does not take literally: a
/// space splits it, a backslash escapes, a `$` expands. Those are the commands
/// Setup registered before 1.6.0 that never ran.
fn command_needs_quotes(command: &str) -> bool {
    let c = command.trim();
    !c.starts_with(['\'', '"'])
        && c.chars()
            .any(|ch| !(ch.is_alphanumeric() || "/._-+:@%,".contains(ch)))
}

// ── Step implementations ──────────────────────────────────────────────────────

fn step_detect_claude_in(home: &Path) -> SetupStep {
    let dot_claude = home.join(".claude");
    if !dot_claude.exists() {
        return SetupStep::err(
            "detect",
            "Detect Claude Code",
            "~/.claude/ not found. Install Claude Code (https://claude.ai/code) first.",
        );
    }
    let has_cli = claude_on_path(std::env::var_os("PATH").as_deref());
    if has_cli {
        SetupStep::ok("detect", "Detect Claude Code", "Claude Code CLI detected")
    } else {
        SetupStep::warn(
            "detect",
            "Detect Claude Code",
            "~/.claude/ found but `claude` not on PATH — VS Code extension may be active",
        )
    }
}

/// Is `claude` on this PATH? Scanned directly rather than by running `which`, which
/// Windows does not ship; there the npm install is `claude.cmd`.
fn claude_on_path(path: Option<&std::ffi::OsStr>) -> bool {
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude"]
    } else {
        &["claude"]
    };
    path.is_some_and(|p| {
        std::env::split_paths(p).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
    })
}

fn step_install_scripts_in(home: &Path, db: &str, mcp_bin: &str) -> SetupStep {
    let dir = lumen_dir_in(home);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return SetupStep::err(
            "scripts",
            "Install hook scripts",
            &format!("mkdir ~/.claude/lumen: {e}"),
        );
    }

    let meter_path = dir.join("lumen_meter.sh");
    let intercept_path = dir.join("lumen_read_intercept.sh");

    // 0o755: the exec bit is a functional requirement, not a user preference —
    // a script without it stops firing silently, which is worse than the
    // truncation window write_atomic closes.
    for (path, content, what) in [
        (
            &meter_path,
            desired_meter_script(db, mcp_bin),
            "lumen_meter.sh",
        ),
        (
            &intercept_path,
            desired_intercept_script(db, mcp_bin),
            "lumen_read_intercept.sh",
        ),
    ] {
        if let Err(e) = write_atomic(path, &content, 0o755) {
            return SetupStep::err(
                "scripts",
                "Install hook scripts",
                &format!("write {what}: {e}"),
            );
        }
    }

    SetupStep::ok(
        "scripts",
        "Install hook scripts",
        "Written to ~/.claude/lumen/",
    )
}

/// Insert the diagnostic stamp *after* the shebang.
///
/// It cannot go first: `#!` is only honoured on line 1, so a stamp above it
/// turns the script into a plain text file and the hook stops firing with no
/// error anywhere — the exact silent-failure mode this release exists to remove.
fn with_stamp(script: &str) -> String {
    match script.split_once('\n') {
        Some((first, rest)) if first.starts_with("#!") => {
            format!("{first}\n{}{rest}", stamp_line())
        }
        // No shebang to protect, so position does not matter.
        _ => format!("{}{script}", stamp_line()),
    }
}

/// The meter script this build would install, stamped for diagnostics.
fn desired_meter_script(db: &str, mcp_bin: &str) -> String {
    shim(METER_TEMPLATE, db, mcp_bin)
}

/// The intercept script this build would install, stamped for diagnostics.
fn desired_intercept_script(db: &str, mcp_bin: &str) -> String {
    shim(INTERCEPT_TEMPLATE, db, mcp_bin)
}

/// A template with the shared tail and the three baked values filled in.
///
/// The spool is derived from the database rather than resolved in shell, so there
/// is one answer for where it lives: `lumen report` looks beside the database, and
/// a second resolution order in a script is how the two would drift apart.
fn shim(template: &str, db: &str, mcp_bin: &str) -> String {
    let spool = Path::new(db)
        .parent()
        .map(|d| d.join("faults.jsonl").to_string_lossy().into_owned())
        .unwrap_or_default();
    with_stamp(
        &template
            .replace("__LUMEN_ORIGIN__", INSTALLED_ORIGIN)
            .replace("__LUMEN_LOCATE__", INSTALLED_LOCATE)
            .replace("__LUMEN_WRITER__", "lumen_meter.sh")
            .replace("__LUMEN_REPORT__", REPORT_TAIL)
            .replace("__LUMEN_MCP__", &sh_quote(&shell_path(mcp_bin)))
            .replace("__LUMEN_DB__", &sh_quote(&shell_path(db)))
            .replace("__LUMEN_SPOOL__", &sh_quote(&shell_path(&spool))),
    )
}

/// Copy a Claude Code config to its `.lumen_bak` before Lumen rewrites it.
///
/// A failed copy used to be dropped and the rewrite went ahead regardless, so the one
/// undo for a rewrite that removed something might not exist, and nothing said so. The
/// callers now leave the file as it is when this fails.
fn back_up(path: &Path) -> Result<(), String> {
    let bak = path.with_extension("json.lumen_bak");
    std::fs::copy(path, &bak).map(|_| ()).map_err(|e| {
        format!(
            "cannot back up {} to {}: {e} — left unchanged",
            path.display(),
            bak.display()
        )
    })
}

/// A Claude Code config file as a JSON object, or why Setup must not write it.
///
/// Absent or blank is a fresh start. Anything else that does not parse is refused,
/// not replaced: Setup used to read an unparseable `~/.claude.json` as `{}` and
/// write back an object holding only Lumen, so one stray comma cost the user every
/// other MCP server they had, kept only in the `.lumen_bak` copy beside it.
fn read_config_object(path: &Path) -> Result<serde_json::Value, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err(format!(
            "{} is not a JSON object; left untouched — fix or remove it, then re-run Setup",
            path.display()
        )),
        Err(e) => Err(format!(
            "{} is not valid JSON ({e}); left untouched — fix or remove it, then re-run Setup",
            path.display()
        )),
    }
}

/// Why `key` in `root` cannot hold Lumen's entry, if it cannot. Absent or null is
/// fine; any other non-object would have to be replaced to proceed, and is the
/// user's to fix.
fn not_an_object(root: &serde_json::Value, key: &str, file: &str) -> Option<String> {
    match root.get(key) {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Object(_)) => None,
        Some(_) => Some(format!(
            "\"{key}\" in {file} is not an object; left untouched — fix it, then re-run Setup"
        )),
    }
}

fn step_register_mcp_in(home: &Path, mcp_bin: &str, db: &str) -> SetupStep {
    let path = claude_json_path_in(home);

    let mut root = match read_config_object(&path) {
        Ok(v) => v,
        Err(e) => return SetupStep::err("mcp", "Register MCP server", &e),
    };
    // Indexing into a non-object below would panic rather than fail the step.
    if let Some(e) = not_an_object(&root, "mcpServers", "~/.claude.json") {
        return SetupStep::err("mcp", "Register MCP server", &e);
    }

    if path.exists() {
        if let Err(e) = back_up(&path) {
            return SetupStep::err("mcp", "Register MCP server", &e);
        }
    }

    // No LUMEN_TOK: nothing has read it since the hooks moved into lumen-mcp, and
    // re-registering is what removes the one an earlier version wrote.
    let entry = serde_json::json!({
        "type":    "stdio",
        "command": mcp_bin,
        "args":    [],
        "env": {
            "LUMEN_DB":  db
        }
    });

    root["mcpServers"]["lumen"] = entry;

    match serde_json::to_string_pretty(&root) {
        Ok(s) => match write_atomic(&path, &s, 0) {
            Ok(_) => SetupStep::ok(
                "mcp",
                "Register MCP server",
                "lumen added to ~/.claude.json",
            ),
            Err(e) => SetupStep::err(
                "mcp",
                "Register MCP server",
                &format!("write ~/.claude.json: {e}"),
            ),
        },
        Err(e) => SetupStep::err("mcp", "Register MCP server", &format!("serialize: {e}")),
    }
}

fn step_install_hooks_in(home: &Path) -> SetupStep {
    let dir = lumen_dir_in(home);
    let meter = hook_command(&dir.join("lumen_meter.sh"));
    let intercept = hook_command(&dir.join("lumen_read_intercept.sh"));

    let path = global_settings_path_in(home);

    // Ensure ~/.claude/ exists
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return SetupStep::err("hooks", "Install hooks", &format!("mkdir ~/.claude: {e}"));
        }
    }

    let mut root = match read_config_object(&path) {
        Ok(v) => v,
        Err(e) => return SetupStep::err("hooks", "Install hooks", &e),
    };
    let shape = not_an_object(&root, "hooks", "settings.json").or_else(|| {
        ["PreToolUse", "PostToolUse"].into_iter().find_map(|phase| {
            match root.get("hooks").and_then(|h| h.get(phase)) {
                None | Some(serde_json::Value::Null) | Some(serde_json::Value::Array(_)) => None,
                Some(_) => Some(format!(
                    "\"hooks.{phase}\" in settings.json is not a list; left untouched — fix \
                     it, then re-run Setup"
                )),
            }
        })
    });
    if let Some(e) = shape {
        return SetupStep::err("hooks", "Install hooks", &e);
    }

    // Setup puts the Bash entry back, so a user who deleted it, as the README said to
    // through 1.5.1, would have command output recorded again. Their choice is carried
    // over as the opt-out Setup keeps.
    let keep_bash_off = bash_meter_removed(&root) && root["env"].get("LUMEN_METER_BASH").is_none();
    if keep_bash_off {
        if let Some(e) = not_an_object(&root, "env", "settings.json") {
            return SetupStep::err("hooks", "Install hooks", &e);
        }
    }

    if path.exists() {
        if let Err(e) = back_up(&path) {
            return SetupStep::err("hooks", "Install hooks", &e);
        }
    }

    // Ensure hooks object exists
    if !root["hooks"].is_object() {
        root["hooks"] = serde_json::json!({});
    }

    merge_hook_entry(
        &mut root["hooks"]["PreToolUse"],
        INTERCEPT_MATCHER,
        &intercept,
    );

    for matcher in METER_MATCHERS {
        merge_hook_entry(&mut root["hooks"]["PostToolUse"], matcher, &meter);
    }
    for matcher in RETIRED_MATCHERS {
        unmerge_hook_entry(&mut root["hooks"]["PostToolUse"], matcher);
    }
    if keep_bash_off {
        if !root["env"].is_object() {
            root["env"] = serde_json::json!({});
        }
        root["env"]["LUMEN_METER_BASH"] = "0".into();
    }

    match serde_json::to_string_pretty(&root) {
        Ok(s) => match write_atomic(&path, &s, 0) {
            Ok(_) => SetupStep::ok(
                "hooks",
                "Install hooks",
                if keep_bash_off {
                    "Hooks merged into ~/.claude/settings.json; the Bash entry you removed \
                     is kept off as LUMEN_METER_BASH=0 in its env"
                } else {
                    "Hooks merged into ~/.claude/settings.json"
                },
            ),
            Err(e) => SetupStep::err(
                "hooks",
                "Install hooks",
                &format!("write settings.json: {e}"),
            ),
        },
        Err(e) => SetupStep::err("hooks", "Install hooks", &format!("serialize: {e}")),
    }
}

/// Ensure `arr_val` (a JSON array or null/missing) contains exactly one entry
/// for `matcher` pointing to `cmd`. Adds if missing; updates command if present.
fn merge_hook_entry(arr_val: &mut serde_json::Value, matcher: &str, cmd: &str) {
    if !arr_val.is_array() {
        *arr_val = serde_json::json!([]);
    }
    let arr = arr_val.as_array_mut().unwrap();

    // Find existing entry for this matcher
    let pos = arr
        .iter()
        .position(|e| e["matcher"].as_str() == Some(matcher));

    let hook_obj = serde_json::json!({
        "type":    "command",
        "command": cmd
    });
    let entry = serde_json::json!({
        "matcher": matcher,
        "hooks":   [hook_obj]
    });

    if let Some(i) = pos {
        // Update in-place — replace the lumen hook command, preserve others
        if let Some(hooks) = arr[i]["hooks"].as_array_mut() {
            let lumen_pos = hooks.iter().position(|h| {
                h["command"]
                    .as_str()
                    .map_or(false, |c| c.contains("lumen_"))
            });
            if let Some(j) = lumen_pos {
                hooks[j]["command"] = serde_json::Value::String(cmd.to_string());
            } else {
                hooks.push(hook_obj);
            }
        }
    } else {
        arr.push(entry);
    }
}

/// Remove Lumen's hook from `matcher`, leaving anything else in place.
///
/// Deliberately narrower than `remove_lumen_hooks`, which drops a whole matcher
/// entry once it finds a lumen command in it. Here the entry may be shared with
/// another tool's hook, so only the lumen command is pulled and the entry is
/// dropped just when nothing is left. Removing a user's unrelated hook while
/// tidying up our own registration would be a far worse bug than the waste this
/// is cleaning up.
fn unmerge_hook_entry(arr_val: &mut serde_json::Value, matcher: &str) {
    let Some(arr) = arr_val.as_array_mut() else {
        return;
    };
    let Some(i) = arr
        .iter()
        .position(|e| e["matcher"].as_str() == Some(matcher))
    else {
        return;
    };
    if let Some(hooks) = arr[i]["hooks"].as_array_mut() {
        hooks.retain(|h| !h["command"].as_str().is_some_and(|c| c.contains("lumen_")));
        if hooks.is_empty() {
            arr.remove(i);
        }
    }
}

/// Whether the user took Bash out of the meter by deleting its entry, the opt-out the
/// README gave through 1.5.1.
///
/// A Lumen meter on `Read` with none on `Bash` says so, and nothing else does: every
/// Setup from 0.1.0 to 1.2.0 registered `Read` with the three retired matchers, and
/// every one since registers `Read` with `Bash`. An install still carrying a retired
/// matcher never had `Bash`, and gets it.
fn bash_meter_removed(root: &serde_json::Value) -> bool {
    let lumen_on = |matcher: &str| {
        root["hooks"]["PostToolUse"].as_array().is_some_and(|arr| {
            arr.iter().any(|e| {
                e["matcher"].as_str() == Some(matcher)
                    && e["hooks"].as_array().is_some_and(|hs| {
                        hs.iter()
                            .any(|h| h["command"].as_str().is_some_and(|c| c.contains("lumen_")))
                    })
            })
        })
    };
    lumen_on("Read") && !lumen_on("Bash") && !RETIRED_MATCHERS.iter().any(|m| lumen_on(m))
}

// ── Main orchestration ────────────────────────────────────────────────────────

fn run_setup(autostart: &dyn AutoStart) -> Vec<SetupStep> {
    run_setup_in(&home(), autostart)
}

/// Setup against an explicit home, so the step list can be asserted in a test
/// without touching the developer's real ~/.claude.
fn run_setup_in(home: &Path, autostart: &dyn AutoStart) -> Vec<SetupStep> {
    let (db, mcp) = baked_paths(home);
    run_setup_with(home, autostart, &db, &mcp)
}

/// Setup with the baked values explicit, so a test can install hooks that exec a
/// binary it chose and write a ledger it owns. An empty `mcp_bin` means this build
/// could not find its sidecar.
fn run_setup_with(
    home: &Path,
    autostart: &dyn AutoStart,
    db_str: &str,
    mcp_str: &str,
) -> Vec<SetupStep> {
    let mut steps = Vec::new();

    // 1. Detect Claude Code
    let detect = step_detect_claude_in(home);
    let fatal = detect.status == StepStatus::Error;
    steps.push(detect);
    if fatal {
        for &(id, label) in &[
            ("scripts", "Install hook scripts"),
            ("mcp", "Register MCP server"),
            ("hooks", "Install hooks"),
            // Skipped rather than enabled: with no Claude Code there is nothing
            // to monitor, so a login item would start an app with no purpose.
            // Setup did not complete, so this runs again on the next launch.
            (AUTOSTART_ID, AUTOSTART_LABEL),
        ] {
            steps.push(SetupStep::skip(id, label, "Skipped: Claude Code not found"));
        }
        return steps;
    }

    // 2. Install scripts. Both hooks exec lumen-mcp, so without it there is
    // nothing they can run.
    if mcp_str.is_empty() {
        steps.push(SetupStep::err(
            "scripts",
            "Install hook scripts",
            "lumen-mcp binary not found — rebuild sidecars with build-sidecar.sh",
        ));
    } else {
        steps.push(step_install_scripts_in(home, db_str, mcp_str));
    }

    // 3. Register MCP (needs mcp path)
    if mcp_str.is_empty() {
        steps.push(SetupStep::err(
            "mcp",
            "Register MCP server",
            "lumen-mcp binary not found — rebuild sidecars with build-sidecar.sh",
        ));
    } else {
        steps.push(step_register_mcp_in(home, mcp_str, db_str));
    }

    // 4. Install hooks (needs scripts installed first)
    let scripts_ok = steps
        .iter()
        .any(|s| s.id == "scripts" && s.status == StepStatus::Ok);
    if scripts_ok {
        steps.push(step_install_hooks_in(home));
    } else {
        steps.push(SetupStep::skip(
            "hooks",
            "Install hooks",
            "Skipped: hook scripts not installed",
        ));
    }

    // 5. Register the login item. Deliberately last: it is the only step that
    // touches something outside ~/.claude, and it must be inside the all_good
    // check below so a warning here still lets setup complete.
    //
    // Not over an opt-out. The marker is written once a login item existed, so the
    // marker with the item off means somebody turned it off. 1.6.0 asks Windows
    // users to re-run Setup to repair their hook commands, and that repair must not
    // switch the login item back on as a side effect: 1.1.2 shipped exactly that,
    // from the startup check.
    let opted_out =
        autostart_marker_in(home).exists() && matches!(autostart.is_enabled(), Ok(false));
    steps.push(if opted_out {
        SetupStep::ok(
            AUTOSTART_ID,
            AUTOSTART_LABEL,
            "Left off, as you set it — the toggle turns it back on",
        )
    } else {
        step_enable_autostart(autostart)
    });

    // 6. Write marker on full success
    let all_good = steps
        .iter()
        .all(|s| s.status == StepStatus::Ok || s.status == StepStatus::Warn);
    if all_good {
        let _ = std::fs::create_dir_all(lumen_dir_in(home));
        let _ = std::fs::write(marker_path_in(home), "");
    }

    steps
}

fn run_uninstall(autostart: &dyn AutoStart) -> Vec<SetupStep> {
    run_uninstall_in(&home(), autostart)
}

/// Uninstall against an explicit home. Tests drive this form so they can never
/// touch the developer's real ~/.claude — this function deletes directories and
/// rewrites two config files.
fn run_uninstall_in(home: &Path, autostart: &dyn AutoStart) -> Vec<SetupStep> {
    let mut steps = Vec::new();

    // Remove MCP entry from ~/.claude.json
    let claude_json = claude_json_path_in(home);
    if claude_json.exists() {
        match std::fs::read_to_string(&claude_json)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            Some(mut v) if v.is_object() => {
                remove_mcp_entry(&mut v);
                let written = back_up(&claude_json).and_then(|()| {
                    serde_json::to_string_pretty(&v)
                        .ok()
                        .and_then(|s| write_atomic(&claude_json, &s, 0).ok())
                        .ok_or_else(|| "Could not write ~/.claude.json".to_string())
                });
                steps.push(match written {
                    Ok(()) => {
                        SetupStep::ok("mcp", "Remove MCP entry", "Removed from ~/.claude.json")
                    }
                    Err(e) => SetupStep::err("mcp", "Remove MCP entry", &e),
                });
            }
            _ => steps.push(SetupStep::skip(
                "mcp",
                "Remove MCP entry",
                "~/.claude.json not found or not valid JSON",
            )),
        }
    } else {
        steps.push(SetupStep::skip(
            "mcp",
            "Remove MCP entry",
            "~/.claude.json not found",
        ));
    }

    // Remove lumen hooks from ~/.claude/settings.json
    let settings = global_settings_path_in(home);
    if settings.exists() {
        match std::fs::read_to_string(&settings)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            Some(mut v) if v.is_object() => {
                remove_lumen_hooks(&mut v);
                let written = back_up(&settings).and_then(|()| {
                    serde_json::to_string_pretty(&v)
                        .ok()
                        .and_then(|s| write_atomic(&settings, &s, 0).ok())
                        .ok_or_else(|| "Could not write ~/.claude/settings.json".to_string())
                });
                steps.push(match written {
                    Ok(()) => SetupStep::ok(
                        "hooks",
                        "Remove hooks",
                        "Removed from ~/.claude/settings.json",
                    ),
                    Err(e) => SetupStep::err("hooks", "Remove hooks", &e),
                });
            }
            _ => steps.push(SetupStep::skip(
                "hooks",
                "Remove hooks",
                "settings.json not found",
            )),
        }
    } else {
        steps.push(SetupStep::skip(
            "hooks",
            "Remove hooks",
            "~/.claude/settings.json not found",
        ));
    }

    // Delete ~/.claude/lumen/
    let dir = lumen_dir_in(home);
    if dir.exists() {
        match std::fs::remove_dir_all(&dir) {
            Ok(_) => steps.push(SetupStep::ok(
                "scripts",
                "Remove scripts",
                "Deleted ~/.claude/lumen/",
            )),
            Err(e) => steps.push(SetupStep::err(
                "scripts",
                "Remove scripts",
                &format!("rm ~/.claude/lumen: {e}"),
            )),
        }
    } else {
        steps.push(SetupStep::skip(
            "scripts",
            "Remove scripts",
            "~/.claude/lumen/ not found",
        ));
    }

    steps.push(step_remove_cli_symlink());
    // Last, and unconditional: leaving a login item behind would relaunch an app
    // the user just uninstalled.
    steps.push(step_disable_autostart(autostart));

    steps
}

// ── CLI symlink ────────────────────────────────────────────────────────────────

/// Symlink the bundled `lumen` CLI binary into PATH.
#[tauri::command]
pub fn lumen_install_cli() -> Vec<SetupStep> {
    vec![step_install_cli()]
}

fn step_install_cli() -> SetupStep {
    // "lumen-cli", not "lumen": the bundled CLI is staged under that name so it
    // cannot collide with the app's own `Lumen` executable on a case-insensitive
    // filesystem. Looking for "lumen" here found the GUI and symlinked *that* as
    // the `lumen` command.
    let Some(lumen_bin) = find_binary("lumen-cli") else {
        return SetupStep::err(
            "cli",
            "Install CLI",
            "lumen-cli binary not found in app bundle — rebuild sidecars with build-sidecar.sh",
        );
    };

    let target = cli_symlink_path();

    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return SetupStep::err(
                "cli",
                "Install CLI",
                &format!("mkdir {}: {e}", parent.display()),
            );
        }
    }

    let _ = std::fs::remove_file(&target);

    // Unix: symlink into a bin dir on PATH. Windows lacks user symlinks without
    // elevation/developer-mode, so copy the binary into place instead.
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&lumen_bin, &target);
    #[cfg(windows)]
    let result = std::fs::copy(&lumen_bin, &target).map(|_| ());

    match result {
        Ok(_) => SetupStep::ok("cli", "Install CLI", &format!("{}", target.display())),
        Err(e) => SetupStep::err("cli", "Install CLI", &format!("install: {e}")),
    }
}

fn step_remove_cli_symlink() -> SetupStep {
    let target = cli_symlink_path();
    if target.exists() || target.symlink_metadata().is_ok() {
        match std::fs::remove_file(&target) {
            Ok(_) => SetupStep::ok(
                "cli",
                "Remove CLI",
                &format!("Removed {}", target.display()),
            ),
            Err(e) => SetupStep::err(
                "cli",
                "Remove CLI",
                &format!("remove {}: {e}", target.display()),
            ),
        }
    } else {
        SetupStep::skip("cli", "Remove CLI", "Not installed")
    }
}

#[cfg(unix)]
fn cli_symlink_path() -> PathBuf {
    if std::path::Path::new("/usr/local/bin").is_dir() {
        PathBuf::from("/usr/local/bin/lumen")
    } else {
        dirs::home_dir()
            .map(|h| h.join(".local/bin/lumen"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin/lumen"))
    }
}

#[cfg(windows)]
fn cli_symlink_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".local").join("bin").join("lumen.exe"))
        .unwrap_or_else(|| PathBuf::from("lumen.exe"))
}

/// Remove Lumen's entry from a parsed ~/.claude.json.
///
/// `get_mut` for the same reason as `remove_lumen_hooks`: index-mutation would
/// insert `"mcpServers": null` into a config that never had the key.
fn remove_mcp_entry(root: &mut serde_json::Value) {
    if let Some(mcp) = root.get_mut("mcpServers").and_then(|m| m.as_object_mut()) {
        mcp.remove("lumen");
    }
}

/// Strip Lumen's hook entries from a parsed settings.json.
///
/// Uses `get_mut` rather than index-mutation throughout: `root["hooks"][phase]`
/// AUTO-VIVIFIES on a `&mut Value`, so on a settings file with no hooks it would
/// insert `"hooks": {"PreToolUse": null, "PostToolUse": null}` — and
/// `run_uninstall` writes the result straight back to disk. An uninstall must
/// never add keys to config it does not own.
fn remove_lumen_hooks(root: &mut serde_json::Value) {
    let Some(hooks) = root.get_mut("hooks") else {
        return;
    };
    for phase in ["PreToolUse", "PostToolUse"] {
        if let Some(arr) = hooks.get_mut(phase).and_then(|v| v.as_array_mut()) {
            arr.retain(|entry| {
                let hooks = entry["hooks"].as_array();
                let has_lumen = hooks.map_or(false, |hs| {
                    hs.iter().any(|h| {
                        h["command"]
                            .as_str()
                            .map_or(false, |c| c.contains("lumen_"))
                    })
                });
                !has_lumen
            });
        }
    }
}

#[cfg(test)]
mod hook_e2e;

#[cfg(test)]
mod plugin_hooks;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // NOTE: every test here uses a tempdir as "home". Nothing in this module may
    // be tested against the real home directory — run_setup rewrites
    // ~/.claude.json and ~/.claude/settings.json, and a test that resolved the
    // developer's actual home would corrupt their Claude Code install.
    //
    // The same rule covers login items: FakeAutoStart below keeps every autostart
    // assertion in memory, so no test can register a LaunchAgent (or a Windows
    // Run key) on the machine running the suite.

    /// Stands in for the app's own executable path in autostart assertions.
    const EXE: &str = "/Applications/Lumen.app/Contents/MacOS/Lumen";

    // ── Artifact freshness: the hole that would have defeated E7 ─────────────
    //
    // The first test here is the important one. An earlier design fingerprinted
    // setup's *inputs* and only repaired artifacts whose baked paths had stopped
    // resolving. A 0.1.0-era script with valid paths passes that check, so it
    // would have been blessed with a current fingerprint and never regenerated —
    // silently writing NULL for every column E7 adds, while the migration
    // reported success. Comparing generated content is what catches it.

    /// A meter script as 0.1.0 shipped it: valid paths, but an old body that
    /// records none of the columns added since.
    fn legacy_script(db: &str, tok: &str) -> String {
        format!(
            "#!/usr/bin/env bash\n\
             LUMEN_DB=\"{db}\"\n\
             LUMEN_TOK=\"{tok}\"\n\
             INPUT=$(cat)\n\
             sqlite3 \"$LUMEN_DB\" \"INSERT INTO read_events(ts,tool) VALUES('x','Read');\"\n"
        )
    }

    #[test]
    fn a_legacy_script_with_valid_paths_is_still_refreshed() {
        let h = TempDir::new().unwrap();
        // Paths that genuinely resolve, so no liveness check can flag this.
        let db = h.path().join("lumen.db");
        let tok = h.path().join("lumen-tok");
        let mcp = h.path().join("lumen-mcp");
        for f in [&db, &tok, &mcp] {
            std::fs::write(f, "").unwrap();
        }
        let script = h.path().join("lumen_meter.sh");
        std::fs::write(
            &script,
            legacy_script(&db.to_string_lossy(), &tok.to_string_lossy()),
        )
        .unwrap();

        let desired = desired_meter_script(&db.to_string_lossy(), &mcp.to_string_lossy());

        assert!(
            script_needs_refresh(&script, &desired),
            "a 0.1.0 script with working paths must still be regenerated — this is \
             the case an input fingerprint blesses forever, and it is how a stale \
             meter would write NULLs while E7's migration looked successful"
        );
    }

    #[test]
    fn a_current_script_is_left_alone() {
        // Negative control: the repair must not rewrite a healthy artifact.
        let h = TempDir::new().unwrap();
        let script = h.path().join("lumen_meter.sh");
        let desired = desired_meter_script("/tmp/x.db", "/tmp/lumen-mcp");
        std::fs::write(&script, &desired).unwrap();
        assert!(!script_needs_refresh(&script, &desired));
    }

    #[test]
    fn a_version_bump_alone_does_not_trigger_a_rewrite() {
        // If the stamp were part of the comparison, every release would rewrite
        // every artifact on every machine.
        let h = TempDir::new().unwrap();
        let script = h.path().join("lumen_meter.sh");
        let desired = desired_meter_script("/tmp/x.db", "/tmp/lumen-mcp");
        let older = desired.replace(&stamp_line(), &format!("{GENERATOR_PREFIX} 0.0.1\n"));
        assert_ne!(older, desired, "the fixture must carry a different stamp");
        std::fs::write(&script, &older).unwrap();
        assert!(
            !script_needs_refresh(&script, &desired),
            "only the stamp differs, so nothing functional changed"
        );
    }

    #[test]
    fn the_stamp_never_displaces_the_shebang() {
        // A stamp on line 1 makes the kernel ignore #! and the hook silently
        // stops firing — a regression worse than anything this release fixes.
        for script in [
            desired_meter_script("/tmp/x.db", "/tmp/lumen-mcp"),
            desired_intercept_script("/tmp/x.db", "/tmp/lumen-mcp"),
        ] {
            assert!(
                script.starts_with("#!/usr/bin/env bash\n"),
                "shebang must remain line 1, got: {:?}",
                script.lines().next()
            );
            assert!(
                script
                    .lines()
                    .nth(1)
                    .unwrap_or("")
                    .starts_with(GENERATOR_PREFIX),
                "the stamp belongs on line 2"
            );
        }
    }

    #[test]
    fn a_missing_script_needs_refresh() {
        let h = TempDir::new().unwrap();
        assert!(script_needs_refresh(
            &h.path().join("absent.sh"),
            "anything"
        ));
    }

    #[test]
    fn a_hand_edited_script_needs_refresh() {
        // Inputs unchanged, so an input fingerprint matches; the content does not.
        let h = TempDir::new().unwrap();
        let script = h.path().join("lumen_meter.sh");
        let desired = desired_meter_script("/tmp/x.db", "/tmp/lumen-mcp");
        assert!(desired.contains("exit 0"));
        std::fs::write(&script, desired.replace("exit 0", "exit 1")).unwrap();
        assert!(script_needs_refresh(&script, &desired));
    }

    // ── Per-artifact validation, with negative controls ──────────────────────
    //
    // The 1.1.5 class fix rests on detection working for every artifact, and it was
    // proven on one of five. Each artifact gets a break-it test and a leave-it-alone
    // test: proving a repair fires is half the job, proving it does not fire on a
    // healthy artifact is the other half.

    fn status<'a>(v: &'a [ArtifactStatus], id: &str) -> &'a ArtifactStatus {
        v.iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no status for {id:?}"))
    }

    /// Snapshot enough of a file to prove a later check did not rewrite it.
    fn fingerprint(p: &Path) -> (String, std::time::SystemTime) {
        let meta = std::fs::metadata(p).expect("metadata");
        (
            std::fs::read_to_string(p).expect("read"),
            meta.modified().expect("mtime"),
        )
    }

    fn write_claude_json(home: &Path, body: &str) {
        std::fs::create_dir_all(home).unwrap();
        std::fs::write(claude_json_path_in(home), body).unwrap();
    }

    // ── mcp ──────────────────────────────────────────────────────────────────

    #[test]
    fn a_dangling_mcp_command_is_reported_unhealthy() {
        // The exact shape of the 1.0.1 bug: entry present, JSON valid, path gone.
        let h = TempDir::new().unwrap();
        write_claude_json(
            h.path(),
            r#"{"mcpServers":{"lumen":{"command":"/nonexistent/lumen-mcp","env":{}}}}"#,
        );
        let s = validate_reported_artifacts_in(h.path());
        let mcp = status(&s, "mcp");
        assert!(!mcp.healthy);
        assert!(
            mcp.detail.contains("/nonexistent/lumen-mcp"),
            "{}",
            mcp.detail
        );
    }

    /// `~/.claude.json` as Setup writes it: serialized, so a Windows path's backslashes
    /// are escaped. Spliced into a JSON string by hand, `C:\Users\…` is an invalid `\U`
    /// escape and the file does not parse.
    fn mcp_entry_json(command: &Path, env: serde_json::Value) -> String {
        serde_json::json!({
            "mcpServers": {
                "lumen": { "command": command, "env": env },
                "other": { "command": "/bin/sh" }
            }
        })
        .to_string()
    }

    #[test]
    fn a_dangling_env_path_in_the_mcp_entry_is_reported() {
        // This is the tokenizer bug itself: LUMEN_TOK pointing into an ejected DMG. Made
        // under the temp dir so that it is absolute on Windows too, where "/Volumes/…"
        // is not and is rightly skipped.
        let h = TempDir::new().unwrap();
        let real = h.path().join("lumen-mcp");
        std::fs::write(&real, "").unwrap();
        let gone = h.path().join("dmg.gone").join("lumen-tok");
        write_claude_json(
            h.path(),
            &mcp_entry_json(&real, serde_json::json!({ "LUMEN_TOK": gone })),
        );
        let s = validate_reported_artifacts_in(h.path());
        assert!(!status(&s, "mcp").healthy);
        assert!(status(&s, "mcp").detail.contains("LUMEN_TOK"));
    }

    #[test]
    fn a_missing_lumen_db_path_is_not_treated_as_dangling() {
        // The database is created on demand, so its absence is normal and must not
        // be reported as breakage — a false alarm trains users to ignore the report.
        // Absolute everywhere, so it is the LUMEN_DB exemption that passes it.
        let h = TempDir::new().unwrap();
        let real = h.path().join("lumen-mcp");
        std::fs::write(&real, "").unwrap();
        let db = h.path().join("not").join("created").join("yet.db");
        write_claude_json(
            h.path(),
            &mcp_entry_json(&real, serde_json::json!({ "LUMEN_DB": db })),
        );
        assert!(status(&validate_reported_artifacts_in(h.path()), "mcp").healthy);
    }

    #[test]
    fn a_healthy_mcp_entry_is_reported_healthy_and_left_byte_identical() {
        // Negative control. ~/.claude.json holds every MCP server the user has, so
        // validation must never write to it — that is why it is validate-only.
        let h = TempDir::new().unwrap();
        let real = h.path().join("lumen-mcp");
        std::fs::write(&real, "").unwrap();
        write_claude_json(h.path(), &mcp_entry_json(&real, serde_json::json!({})));
        let path = claude_json_path_in(h.path());
        let before = fingerprint(&path);

        assert!(status(&validate_reported_artifacts_in(h.path()), "mcp").healthy);

        let after = fingerprint(&path);
        assert_eq!(
            before.0, after.0,
            "validation must not rewrite ~/.claude.json"
        );
        assert_eq!(before.1, after.1, "not even the mtime may change");
        assert!(
            after.0.contains("\"other\""),
            "a foreign server must survive"
        );
    }

    #[test]
    fn a_missing_mcp_entry_is_reported_rather_than_silently_ignored() {
        let h = TempDir::new().unwrap();
        write_claude_json(h.path(), r#"{"numStartups":3}"#);
        assert!(!status(&validate_reported_artifacts_in(h.path()), "mcp").healthy);
    }

    // ── hooks ────────────────────────────────────────────────────────────────

    #[test]
    fn a_hook_pointing_at_a_deleted_script_is_reported() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        std::fs::write(
            global_settings_path_in(h.path()),
            r#"{"hooks":{"PostToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"/gone/lumen_meter.sh"}]}]}}"#,
        )
        .unwrap();
        let s = validate_reported_artifacts_in(h.path());
        assert!(!status(&s, "hooks").healthy);
        assert!(status(&s, "hooks").detail.contains("/gone/lumen_meter.sh"));
    }

    #[test]
    fn healthy_hooks_are_reported_healthy_and_settings_is_untouched() {
        let h = TempDir::new().unwrap();
        // The full current matcher set. This fixture used to be a single PostToolUse
        // "Read", which the validator accepted because it only looked for dangling
        // paths; it now also compares the matcher set, so a partial registration is
        // correctly unhealthy and would make this test's own premise false.
        settings_with_matchers(h.path(), &[INTERCEPT_MATCHER], &METER_MATCHERS);
        let settings = global_settings_path_in(h.path());

        // A key Lumen does not own, to prove validation leaves the rest of the file
        // alone. The helper writes only `hooks`, so it is added here.
        let mut root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        root["permissions"] = serde_json::json!({"allow": ["Bash"]});
        std::fs::write(&settings, serde_json::to_string(&root).unwrap()).unwrap();

        let before = fingerprint(&settings);

        assert!(status(&validate_reported_artifacts_in(h.path()), "hooks").healthy);

        let after = fingerprint(&settings);
        assert_eq!(
            before.0, after.0,
            "validation must not rewrite settings.json"
        );
        assert_eq!(before.1, after.1);
        assert!(after.0.contains("permissions"), "foreign keys must survive");
    }

    #[test]
    fn settings_with_no_lumen_hooks_is_reported_as_not_installed() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        std::fs::write(
            global_settings_path_in(h.path()),
            r#"{"hooks":{"PostToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"/opt/other/tool.sh"}]}]}}"#,
        )
        .unwrap();
        let s = validate_reported_artifacts_in(h.path());
        assert!(!status(&s, "hooks").healthy);
        assert!(status(&s, "hooks").detail.contains("no lumen hooks"));
    }

    // ── the 1.1.2 case: a refresh must not undo an opt-out ────────────────────

    #[test]
    fn a_forced_mismatch_does_not_re_enable_an_autostart_the_user_turned_off() {
        // 1.1.2 was precisely this: a repair that silently switched a login item
        // back on for someone who had switched it off. The staleness check exists to
        // fix a wrong PATH, not to overrule a decision.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        // Recorded under a path that no longer matches -> forced mismatch.
        std::fs::write(&marker, "/Applications/Lumen.app/Contents/MacOS/lumen").unwrap();
        let a = FakeAutoStart::default(); // user turned it OFF

        let acted = ensure_autostart_once(&a, &marker, EXE);

        assert!(!acted, "nothing should have been registered");
        assert!(
            !a.enabled.get(),
            "the opt-out must survive a forced mismatch"
        );
        assert_eq!(
            a.enable_calls.get(),
            0,
            "enable() must not be called at all"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            EXE,
            "the stale path is still corrected, so this is not re-evaluated forever"
        );
    }

    #[test]
    fn a_forced_mismatch_does_refresh_an_autostart_that_is_still_on() {
        // The positive control for the test above: staleness must still be repaired
        // when the user has not opted out, or the fix does nothing.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "/old/path/lumen").unwrap();
        let a = FakeAutoStart::on();

        assert!(ensure_autostart_once(&a, &marker, EXE));
        assert_eq!(a.enable_calls.get(), 1);
        assert!(a.enabled.get());
    }

    // ── cli ──────────────────────────────────────────────────────────────────

    #[test]
    fn an_absent_cli_is_reported_healthy_because_it_is_optional() {
        // Never installed is not broken. Reporting it as breakage would push users
        // to install something they deliberately skipped.
        let r = resolver(&[]);
        assert_eq!(
            classify_cli(
                Path::new("/usr/local/bin/lumen"),
                Path::new("/A/lumen-cli"),
                &r
            ),
            CliState::Absent
        );
    }

    #[test]
    fn a_homebrew_cli_is_never_rewritten_even_when_it_points_elsewhere() {
        // brew relinks on upgrade; a repair loop against it would be a new bug of
        // exactly the class 1.1.5 closes.
        let r = resolver(&[(
            "/opt/homebrew/bin/lumen",
            "/opt/homebrew/Cellar/lumen-cli/9.9.9/bin/lumen",
        )]);
        assert_eq!(
            classify_cli(
                Path::new("/opt/homebrew/bin/lumen"),
                Path::new("/Applications/Lumen.app/Contents/MacOS/lumen-cli"),
                &r
            ),
            CliState::Homebrew,
            "a mismatched target under a brew prefix is still brew's to manage"
        );
    }

    // ── Artifact registry ────────────────────────────────────────────────────

    #[test]
    fn every_step_run_setup_emits_is_accounted_for() {
        // The test that makes a fourth instance of the run_setup-only bug
        // impossible. Add a step and forget its freshness check, and its id is
        // unaccounted for here.
        //
        // With ~/.claude present, so every step runs rather than being skipped under
        // its id: a skipped step proves the id exists, not that the step does.
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        let db = h.path().join("data/lumen.db");
        let mcp = h.path().join("lumen-mcp");
        std::fs::write(&mcp, "").unwrap();
        let steps = run_setup_with(
            h.path(),
            &FakeAutoStart::default(),
            &db.to_string_lossy(),
            &mcp.to_string_lossy(),
        );
        assert!(
            steps.iter().all(|s| s.status != StepStatus::Skip),
            "every step must have run: {steps:?}"
        );
        let emitted: std::collections::BTreeSet<String> = steps.into_iter().map(|s| s.id).collect();
        let accounted: std::collections::BTreeSet<String> = PERSISTED_ARTIFACTS
            .iter()
            .chain(NON_PERSISTING.iter())
            .map(|s| s.to_string())
            .collect();

        let unaccounted: Vec<_> = emitted.difference(&accounted).collect();
        assert!(
            unaccounted.is_empty(),
            "these run_setup steps have no freshness check: {unaccounted:?} — add \
             each to PERSISTED_ARTIFACTS (with a validator) or to NON_PERSISTING"
        );
    }

    #[test]
    fn cli_is_in_the_registry_even_though_run_setup_does_not_emit_it() {
        // It is installed by its own command, which is exactly why it was missed
        // the first time. 1.1.4 shipped a symlink pointing at the GUI binary.
        assert!(PERSISTED_ARTIFACTS.contains(&"cli"));
        assert!(USER_OWNED.contains(&"cli"));
    }

    #[test]
    fn user_owned_artifacts_are_a_subset_of_persisted_ones() {
        for a in USER_OWNED {
            assert!(
                PERSISTED_ARTIFACTS.contains(a),
                "{a} is user-owned but not persisted"
            );
        }
    }

    // ── CLI classification ───────────────────────────────────────────────────

    fn resolver(map: &[(&str, &str)]) -> impl Fn(&Path) -> Option<PathBuf> + use<> {
        let owned: Vec<(PathBuf, PathBuf)> = map
            .iter()
            .map(|(k, v)| (PathBuf::from(k), PathBuf::from(v)))
            .collect();
        move |p: &Path| owned.iter().find(|(k, _)| k == p).map(|(_, v)| v.clone())
    }

    #[test]
    fn an_absent_cli_symlink_is_left_alone() {
        // The user never clicked Install CLI; creating one would be unasked-for.
        let r = resolver(&[]);
        assert_eq!(
            classify_cli(
                Path::new("/usr/local/bin/lumen"),
                Path::new("/A/lumen-cli"),
                &r
            ),
            CliState::Absent
        );
    }

    #[test]
    fn a_homebrew_managed_cli_is_reported_not_rewritten() {
        // Real state on the development machine: brew owns bin/lumen.
        let r = resolver(&[(
            "/opt/homebrew/bin/lumen",
            "/opt/homebrew/Cellar/lumen-cli/1.1.4/bin/lumen",
        )]);
        assert_eq!(
            classify_cli(
                Path::new("/opt/homebrew/bin/lumen"),
                Path::new("/Applications/Lumen.app/Contents/MacOS/lumen-cli"),
                &r
            ),
            CliState::Homebrew,
            "two managers must not fight over one path"
        );
    }

    #[test]
    fn a_cli_pointing_at_the_current_bundle_is_current() {
        let want = "/Applications/Lumen.app/Contents/MacOS/lumen-cli";
        let r = resolver(&[("/usr/local/bin/lumen", want)]);
        assert_eq!(
            classify_cli(Path::new("/usr/local/bin/lumen"), Path::new(want), &r),
            CliState::Current
        );
    }

    #[test]
    fn a_cli_pointing_at_the_gui_binary_is_stale() {
        // Precisely what 1.1.4 shipped: the CLI sidecar had been overwritten by
        // the GUI on a case-insensitive filesystem, so the symlink aimed at it.
        let r = resolver(&[(
            "/usr/local/bin/lumen",
            "/Applications/Lumen.app/Contents/MacOS/Lumen",
        )]);
        assert_eq!(
            classify_cli(
                Path::new("/usr/local/bin/lumen"),
                Path::new("/Applications/Lumen.app/Contents/MacOS/lumen-cli"),
                &r
            ),
            CliState::Stale
        );
    }

    #[test]
    fn homebrew_detection_covers_the_known_prefixes() {
        for p in [
            "/opt/homebrew/bin/lumen",
            "/opt/homebrew/Cellar/lumen-cli/1.1.4/bin/lumen",
            "/usr/local/Homebrew/bin/lumen",
            "/home/linuxbrew/.linuxbrew/bin/lumen",
        ] {
            assert!(is_homebrew_managed(Path::new(p)), "{p}");
        }
        for p in [
            "/usr/local/bin/lumen",
            "/Users/me/.local/bin/lumen",
            "/Applications/Lumen.app/Contents/MacOS/lumen-cli",
        ] {
            assert!(!is_homebrew_managed(Path::new(p)), "{p}");
        }
    }

    // ── write_atomic ─────────────────────────────────────────────────────────

    #[test]
    fn write_atomic_unions_the_mode_floor_so_a_lost_exec_bit_is_restored() {
        // Exact preservation would keep a broken hook broken.
        let h = TempDir::new().unwrap();
        let f = h.path().join("lumen_meter.sh");
        std::fs::write(&f, "old").unwrap();
        set_mode(&f, 0o644).unwrap(); // exec bit lost by some earlier bug
        write_atomic(&f, "new", 0o755).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "new");
        #[cfg(unix)]
        assert_eq!(mode_of(&f) & 0o111, 0o111, "exec bit must be restored");
    }

    #[test]
    fn write_atomic_preserves_a_tightened_mode_when_no_floor_is_required() {
        // ~/.claude.json is 0600; a naive temp+rename would leave it 0600 by luck
        // and settings.json (0644) tightened. Preservation must be deliberate.
        let h = TempDir::new().unwrap();
        let f = h.path().join("settings.json");
        std::fs::write(&f, "{}").unwrap();
        set_mode(&f, 0o600).unwrap();
        write_atomic(&f, "{\"a\":1}", 0).unwrap();
        #[cfg(unix)]
        assert_eq!(
            mode_of(&f),
            0o600,
            "a deliberately private file stays private"
        );
    }

    #[test]
    fn write_atomic_writes_through_a_symlink_without_replacing_it() {
        // Dotfile-repo users symlink ~/.claude.json. Renaming onto the link would
        // replace it with a regular file and detach their config.
        let h = TempDir::new().unwrap();
        let real = h.path().join("real.json");
        let link = h.path().join("claude.json");
        std::fs::write(&real, "{}").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, "{\"kept\":true}", 0).unwrap();

        #[cfg(unix)]
        {
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the symlink must survive"
            );
            assert_eq!(std::fs::read_to_string(&real).unwrap(), "{\"kept\":true}");
        }
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let h = TempDir::new().unwrap();
        let f = h.path().join("x.json");
        write_atomic(&f, "{}", 0).unwrap();
        let strays: Vec<_> = std::fs::read_dir(h.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("lumen-tmp"))
            .collect();
        assert!(strays.is_empty(), "temp files left behind: {strays:?}");
    }

    // ── Autostart test double ────────────────────────────────────────────────

    /// In-memory [`AutoStart`] that records what was asked of it.
    #[derive(Default)]
    struct FakeAutoStart {
        enabled: std::cell::Cell<bool>,
        enable_calls: std::cell::Cell<usize>,
        disable_calls: std::cell::Cell<usize>,
        fail_read: bool,
        fail_write: bool,
    }

    impl FakeAutoStart {
        fn on() -> Self {
            Self {
                enabled: std::cell::Cell::new(true),
                ..Default::default()
            }
        }
        fn broken_read() -> Self {
            Self {
                fail_read: true,
                ..Default::default()
            }
        }
        fn broken_write() -> Self {
            Self {
                fail_write: true,
                ..Default::default()
            }
        }
    }

    impl AutoStart for FakeAutoStart {
        fn is_enabled(&self) -> Result<bool, String> {
            if self.fail_read {
                return Err("no login-item service".into());
            }
            Ok(self.enabled.get())
        }
        fn enable(&self) -> Result<(), String> {
            self.enable_calls.set(self.enable_calls.get() + 1);
            if self.fail_write {
                return Err("permission denied".into());
            }
            self.enabled.set(true);
            Ok(())
        }
        fn disable(&self) -> Result<(), String> {
            self.disable_calls.set(self.disable_calls.get() + 1);
            if self.fail_write {
                return Err("permission denied".into());
            }
            self.enabled.set(false);
            Ok(())
        }
    }

    fn find<'a>(steps: &'a [SetupStep], id: &str) -> &'a SetupStep {
        steps
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no step with id {id:?}"))
    }

    // ── step_enable_autostart ────────────────────────────────────────────────

    #[test]
    fn enabling_autostart_registers_the_login_item() {
        let a = FakeAutoStart::default();
        let step = step_enable_autostart(&a);
        assert_eq!(step.status, StepStatus::Ok);
        assert!(a.enabled.get(), "should be enabled afterwards");
        assert_eq!(a.enable_calls.get(), 1);
    }

    #[test]
    fn enabling_an_already_enabled_login_item_does_not_re_register_it() {
        // Setup can be re-run from the UI; doing the work twice would rewrite the
        // LaunchAgent plist for no reason.
        let a = FakeAutoStart::on();
        let step = step_enable_autostart(&a);
        assert_eq!(step.status, StepStatus::Ok);
        assert_eq!(step.detail, "Already enabled");
        assert_eq!(a.enable_calls.get(), 0, "must not call enable() again");
    }

    #[test]
    fn a_login_item_that_cannot_be_registered_warns_rather_than_errors() {
        // Must be Warn: run_setup only writes its marker when every step is Ok or
        // Warn, so an Error would make setup repeat on every single launch.
        let a = FakeAutoStart::broken_write();
        let step = step_enable_autostart(&a);
        assert_eq!(step.status, StepStatus::Warn);
        assert!(step.detail.contains("permission denied"), "{}", step.detail);
    }

    #[test]
    fn an_unreadable_autostart_setting_warns() {
        let step = step_enable_autostart(&FakeAutoStart::broken_read());
        assert_eq!(step.status, StepStatus::Warn);
        assert!(step.detail.contains("no login-item service"));
    }

    // ── step_disable_autostart ───────────────────────────────────────────────

    #[test]
    fn uninstalling_removes_the_login_item() {
        let a = FakeAutoStart::on();
        let step = step_disable_autostart(&a);
        assert_eq!(step.status, StepStatus::Ok);
        assert!(!a.enabled.get());
        assert_eq!(a.disable_calls.get(), 1);
    }

    #[test]
    fn disabling_a_login_item_that_was_never_set_is_a_skip() {
        let a = FakeAutoStart::default();
        let step = step_disable_autostart(&a);
        assert_eq!(step.status, StepStatus::Skip);
        assert_eq!(a.disable_calls.get(), 0);
    }

    #[test]
    fn a_login_item_that_cannot_be_removed_warns() {
        let a = FakeAutoStart::on();
        // Make the write fail while still reporting enabled.
        let broken = FakeAutoStart {
            enabled: std::cell::Cell::new(true),
            fail_write: true,
            ..Default::default()
        };
        assert_eq!(step_disable_autostart(&broken).status, StepStatus::Warn);
        assert!(a.enabled.get(), "unrelated instance untouched");
    }

    // ── ensure_autostart_once ────────────────────────────────────────────────

    #[test]
    fn an_existing_install_gets_a_login_item_on_first_launch_after_upgrade() {
        // The gap this closes: run_setup is skipped once .setup_done exists, so
        // upgrading users would never have had a login item registered.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::default();

        assert!(ensure_autostart_once(&a, &marker, EXE), "should register");
        assert!(a.enabled.get());
        assert!(marker.exists(), "marker records that this ran");
    }

    #[test]
    fn the_one_time_registration_does_not_repeat_on_later_launches() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::default();

        ensure_autostart_once(&a, &marker, EXE);
        assert_eq!(a.enable_calls.get(), 1);

        // Every subsequent launch is a no-op.
        for _ in 0..3 {
            assert!(!ensure_autostart_once(&a, &marker, EXE));
        }
        assert_eq!(a.enable_calls.get(), 1, "must not re-register");
    }

    #[test]
    fn turning_the_toggle_off_is_not_undone_by_the_next_launch() {
        // The whole reason this has its own marker: the user's opt-out has to win.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::default();

        ensure_autostart_once(&a, &marker, EXE);
        assert!(a.enabled.get());

        a.disable().unwrap(); // user flips the toggle off
        assert!(!a.enabled.get());

        ensure_autostart_once(&a, &marker, EXE); // next launch
        assert!(
            !a.enabled.get(),
            "startup must not re-enable what the user turned off"
        );
    }

    #[test]
    fn an_already_enabled_login_item_is_recorded_without_re_registering() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::on();

        assert!(!ensure_autostart_once(&a, &marker, EXE), "nothing to do");
        assert_eq!(a.enable_calls.get(), 0);
        assert!(
            marker.exists(),
            "still recorded, so an opt-out later sticks"
        );
    }

    #[test]
    fn a_failed_registration_is_retried_on_the_next_launch() {
        // No marker is written on failure, so a transient problem self-heals
        // instead of the user silently never getting a login item.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let broken = FakeAutoStart::broken_write();

        assert!(!ensure_autostart_once(&broken, &marker, EXE));
        assert!(!marker.exists(), "must not record a failure as done");
        assert_eq!(broken.enable_calls.get(), 1);

        assert!(!ensure_autostart_once(&broken, &marker, EXE));
        assert_eq!(broken.enable_calls.get(), 2, "retried");
    }

    #[test]
    fn an_unreadable_setting_does_not_write_the_marker() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        assert!(!ensure_autostart_once(
            &FakeAutoStart::broken_read(),
            &marker,
            EXE
        ));
        assert!(!marker.exists());
    }

    // ── stale-path refresh ───────────────────────────────────────────────────

    #[test]
    fn a_login_item_pointing_at_a_stale_executable_is_refreshed() {
        // Renaming the app executable from `lumen` to `Lumen` left every existing
        // login item aimed at the old path. It must be rewritten, not left to rely
        // on a case-insensitive filesystem happening to resolve it.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "/Applications/Lumen.app/Contents/MacOS/lumen").unwrap();
        let a = FakeAutoStart::on();

        let refreshed = ensure_autostart_once(&a, &marker, EXE);

        assert!(refreshed, "a stale path must be re-registered");
        assert_eq!(a.enable_calls.get(), 1);
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            EXE,
            "the marker must record the path it just registered"
        );
    }

    #[test]
    fn an_empty_legacy_marker_counts_as_stale() {
        // 1.1.2 and 1.1.3 wrote an empty marker, so it carries no path to compare.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "").unwrap();

        let a = FakeAutoStart::on();
        assert!(ensure_autostart_once(&a, &marker, EXE));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), EXE);
    }

    #[test]
    fn a_matching_marker_is_left_completely_alone() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, EXE).unwrap();
        let a = FakeAutoStart::on();

        assert!(!ensure_autostart_once(&a, &marker, EXE));
        assert_eq!(
            a.enable_calls.get(),
            0,
            "no work when the path still matches"
        );
    }

    #[test]
    fn a_stale_path_does_not_re_enable_what_the_user_turned_off() {
        // The opt-out still wins: the recorded path is corrected so the next launch
        // sees no staleness, but the item stays off.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "/old/path/lumen").unwrap();
        let a = FakeAutoStart::default(); // disabled — user opted out

        assert!(!ensure_autostart_once(&a, &marker, EXE));
        assert!(!a.enabled.get(), "must stay off");
        assert_eq!(a.enable_calls.get(), 0);
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            EXE,
            "path is corrected so this is not re-evaluated every launch"
        );
    }

    #[test]
    fn a_refresh_that_fails_is_retried_rather_than_recorded() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "/old/path/lumen").unwrap();
        let broken = FakeAutoStart {
            enabled: std::cell::Cell::new(true),
            fail_write: true,
            ..Default::default()
        };

        assert!(!ensure_autostart_once(&broken, &marker, EXE));
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "/old/path/lumen",
            "a failed refresh must not claim the new path"
        );
    }

    #[test]
    fn the_marker_directory_is_created_if_it_does_not_exist() {
        // First launch on a machine with no ~/.claude/lumen yet.
        let h = TempDir::new().unwrap();
        let marker = h.path().join("deep/nested/path/.autostart_done");
        assert!(ensure_autostart_once(
            &FakeAutoStart::default(),
            &marker,
            EXE
        ));
        assert!(marker.exists());
    }

    #[test]
    fn the_autostart_marker_is_separate_from_the_setup_marker() {
        // Sharing .setup_done would mean an upgrading user never gets a login
        // item, which is the bug this whole path exists for.
        let h = TempDir::new().unwrap();
        assert_ne!(autostart_marker_in(h.path()), marker_path_in(h.path()));
    }

    #[test]
    fn enable_then_disable_returns_to_the_original_state() {
        // Setup followed by uninstall must leave no login item behind.
        let a = FakeAutoStart::default();
        assert_eq!(step_enable_autostart(&a).status, StepStatus::Ok);
        assert!(a.enabled.get());
        assert_eq!(step_disable_autostart(&a).status, StepStatus::Ok);
        assert!(!a.enabled.get());
    }

    // ── is_ephemeral_path ────────────────────────────────────────────────────
    //
    // This three-line predicate is the whole of the 1.0.1 bugfix: sidecar paths
    // recorded from a DMG mount or an App Translocation directory go stale as
    // soon as the volume is ejected, and Claude Code then fails to spawn the MCP
    // server with ENOENT.

    #[test]
    fn a_dmg_mount_is_ephemeral() {
        assert!(is_ephemeral_path(Path::new(
            "/Volumes/Lumen 1.1.0/Lumen.app/Contents/MacOS/lumen-mcp"
        )));
    }

    #[test]
    fn an_app_translocation_path_is_ephemeral() {
        assert!(is_ephemeral_path(Path::new(
            "/private/var/folders/ab/xyz/d/AppTranslocation/1234-5678/d/Lumen.app/Contents/MacOS/lumen-mcp"
        )));
    }

    #[test]
    fn an_installed_application_path_is_not_ephemeral() {
        assert!(!is_ephemeral_path(Path::new(
            "/Applications/Lumen.app/Contents/MacOS/lumen-mcp"
        )));
    }

    #[test]
    fn a_user_local_path_is_not_ephemeral() {
        assert!(!is_ephemeral_path(Path::new(
            "/Users/someone/Library/Application Support/io.speedata.lumen/bin/lumen-mcp"
        )));
    }

    #[test]
    fn a_volumes_substring_elsewhere_in_the_path_is_not_ephemeral() {
        // Only a /Volumes/ PREFIX is ephemeral. A directory that merely contains
        // the word must not be misclassified, or we would needlessly copy
        // sidecars for users with such a path.
        assert!(!is_ephemeral_path(Path::new(
            "/Users/someone/Volumes/lumen-mcp"
        )));
    }

    // ── path helpers ─────────────────────────────────────────────────────────

    #[test]
    fn path_helpers_all_hang_off_the_supplied_home() {
        let h = Path::new("/tmp/fake-home");
        assert_eq!(lumen_dir_in(h), Path::new("/tmp/fake-home/.claude/lumen"));
        assert_eq!(
            marker_path_in(h),
            Path::new("/tmp/fake-home/.claude/lumen/.setup_done")
        );
        assert_eq!(
            claude_json_path_in(h),
            Path::new("/tmp/fake-home/.claude.json")
        );
        assert_eq!(
            global_settings_path_in(h),
            Path::new("/tmp/fake-home/.claude/settings.json")
        );
        // The data directory is per-OS, so assert the platform's own layout.
        #[cfg(target_os = "macos")]
        assert_eq!(
            app_support_dir_in(h),
            Path::new("/tmp/fake-home/Library/Application Support/io.speedata.lumen")
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            app_support_dir_in(h),
            Path::new("/tmp/fake-home/AppData/Roaming/io.speedata.lumen")
        );
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        assert_eq!(
            app_support_dir_in(h),
            Path::new("/tmp/fake-home/.local/share/io.speedata.lumen"),
            "Linux follows the XDG default"
        );
    }

    #[test]
    fn the_data_directory_is_under_the_home_on_every_platform() {
        // Whatever the layout, it must stay inside the supplied home — otherwise a
        // test could reach outside its tempdir and touch the real machine.
        let h = Path::new("/tmp/fake-home");
        assert!(app_support_dir_in(h).starts_with(h));
        assert!(app_support_dir_in(h).ends_with(APP_ID));
    }

    #[test]
    fn the_db_lives_inside_the_data_directory() {
        // Derived from app_support_dir_in, so the two cannot drift apart and end
        // up reading and writing different files.
        let h = Path::new("/tmp/fake-home");
        let db = db_path_in(h);
        assert!(
            db.starts_with(&app_support_dir_in(h).to_string_lossy().to_string()),
            "db {db} must sit inside the data dir"
        );
        assert!(db.ends_with("lumen.db"));
    }

    #[test]
    fn the_bundle_id_matches_tauri_conf() {
        // A drift here would put data in a directory the app never reads.
        let conf = include_str!("../tauri.conf.json");
        assert!(
            conf.contains(&format!("\"identifier\": \"{APP_ID}\"")),
            "APP_ID must match tauri.conf.json identifier"
        );
    }

    #[test]
    fn the_real_home_helpers_agree_with_the_parameterised_ones() {
        // Guards against the wrappers drifting from the *_in functions.
        let h = home();
        assert_eq!(lumen_dir(), lumen_dir_in(&h));
        assert_eq!(marker_path(), marker_path_in(&h));
        assert_eq!(app_support_dir(), app_support_dir_in(&h));
    }

    // ── merge_hook_entry ─────────────────────────────────────────────────────
    //
    // Mutates the user's global Claude settings. Losing a foreign hook here
    // silently breaks someone else's tooling, so the preserve cases matter as
    // much as the add case.

    #[test]
    fn merge_creates_the_array_when_the_slot_is_missing() {
        let mut v = json!(null);
        merge_hook_entry(&mut v, "Read", "/bin/lumen_hook.sh");
        assert_eq!(v[0]["matcher"], "Read");
        assert_eq!(v[0]["hooks"][0]["type"], "command");
        assert_eq!(v[0]["hooks"][0]["command"], "/bin/lumen_hook.sh");
    }

    #[test]
    fn merge_coerces_a_non_array_value_rather_than_panicking() {
        let mut v = json!("this should have been an array");
        merge_hook_entry(&mut v, "Read", "/bin/lumen_hook.sh");
        assert!(v.is_array());
        assert_eq!(v.as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_appends_a_new_matcher_alongside_existing_ones() {
        let mut v = json!([{
            "matcher": "Write",
            "hooks": [{ "type": "command", "command": "/other/tool.sh" }]
        }]);
        merge_hook_entry(&mut v, "Read", "/bin/lumen_hook.sh");
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["matcher"], "Write", "the foreign matcher stays first");
        assert_eq!(v[1]["matcher"], "Read");
    }

    #[test]
    fn merge_updates_an_existing_lumen_command_in_place() {
        let mut v = json!([{
            "matcher": "Read",
            "hooks": [{ "type": "command", "command": "/old/path/lumen_intercept.sh" }]
        }]);
        merge_hook_entry(&mut v, "Read", "/new/path/lumen_intercept.sh");
        assert_eq!(
            v.as_array().unwrap().len(),
            1,
            "must not duplicate the matcher"
        );
        assert_eq!(v[0]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(v[0]["hooks"][0]["command"], "/new/path/lumen_intercept.sh");
    }

    #[test]
    fn merge_preserves_a_foreign_hook_on_the_same_matcher() {
        let mut v = json!([{
            "matcher": "Read",
            "hooks": [{ "type": "command", "command": "/somebody/elses/hook.sh" }]
        }]);
        merge_hook_entry(&mut v, "Read", "/bin/lumen_intercept.sh");
        let hooks = v[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2, "the foreign hook must survive");
        assert_eq!(hooks[0]["command"], "/somebody/elses/hook.sh");
        assert_eq!(hooks[1]["command"], "/bin/lumen_intercept.sh");
    }

    #[test]
    fn merge_replaces_only_the_lumen_hook_leaving_neighbours_alone() {
        let mut v = json!([{
            "matcher": "Read",
            "hooks": [
                { "type": "command", "command": "/first/party.sh" },
                { "type": "command", "command": "/old/lumen_intercept.sh" },
                { "type": "command", "command": "/third/party.sh" }
            ]
        }]);
        merge_hook_entry(&mut v, "Read", "/new/lumen_intercept.sh");
        let hooks = v[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 3, "no hook added, none removed");
        assert_eq!(hooks[0]["command"], "/first/party.sh");
        assert_eq!(hooks[1]["command"], "/new/lumen_intercept.sh");
        assert_eq!(hooks[2]["command"], "/third/party.sh");
    }

    #[test]
    fn merge_is_idempotent() {
        let mut v = json!(null);
        merge_hook_entry(&mut v, "Read", "/bin/lumen_hook.sh");
        let after_first = v.clone();
        merge_hook_entry(&mut v, "Read", "/bin/lumen_hook.sh");
        assert_eq!(v, after_first, "re-running setup must not duplicate hooks");
    }

    // ── remove_lumen_hooks ───────────────────────────────────────────────────
    //
    // The uninstall path. Over-removing destroys unrelated user config.

    #[test]
    fn remove_strips_lumen_entries_from_both_phases() {
        let mut root = json!({
            "hooks": {
                "PreToolUse":  [{ "matcher": "Read", "hooks": [{ "command": "/x/lumen_intercept.sh" }] }],
                "PostToolUse": [{ "matcher": "Read", "hooks": [{ "command": "/x/lumen_meter.sh" }] }]
            }
        });
        remove_lumen_hooks(&mut root);
        assert!(root["hooks"]["PreToolUse"].as_array().unwrap().is_empty());
        assert!(root["hooks"]["PostToolUse"].as_array().unwrap().is_empty());
    }

    #[test]
    fn remove_keeps_hooks_that_are_not_ours() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read",  "hooks": [{ "command": "/x/lumen_intercept.sh" }] },
                    { "matcher": "Write", "hooks": [{ "command": "/somebody/else.sh" }] }
                ]
            }
        });
        remove_lumen_hooks(&mut root);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "only the lumen entry is removed");
        assert_eq!(pre[0]["matcher"], "Write");
    }

    #[test]
    fn remove_drops_a_whole_entry_that_contains_any_lumen_hook() {
        // Documented consequence: an entry is matched as a unit, so a foreign
        // hook sharing an entry with a lumen hook goes with it. Pinned so the
        // behaviour is a decision rather than a surprise.
        let mut root = json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Read",
                    "hooks": [
                        { "command": "/somebody/else.sh" },
                        { "command": "/x/lumen_intercept.sh" }
                    ]
                }]
            }
        });
        remove_lumen_hooks(&mut root);
        assert!(root["hooks"]["PreToolUse"].as_array().unwrap().is_empty());
    }

    #[test]
    fn remove_is_a_no_op_on_settings_with_no_hooks_key() {
        let mut root = json!({ "theme": "dark" });
        let before = root.clone();
        remove_lumen_hooks(&mut root);
        assert_eq!(root, before, "unrelated settings must be untouched");
    }

    #[test]
    fn remove_is_a_no_op_on_an_empty_object() {
        let mut root = json!({});
        remove_lumen_hooks(&mut root);
        assert_eq!(root, json!({}), "must not create a hooks key");
    }

    #[test]
    fn remove_does_not_invent_a_hooks_key_on_uninstall() {
        // Regression: `root["hooks"][phase]` auto-vivifies on a &mut Value, and
        // run_uninstall writes the result straight back to ~/.claude/settings.json
        // — so uninstalling used to INJECT "hooks": {"PreToolUse": null,
        // "PostToolUse": null} into settings that never had hooks at all.
        let mut root = json!({ "theme": "dark", "model": "opus" });
        remove_lumen_hooks(&mut root);
        assert!(
            root.get("hooks").is_none(),
            "uninstall must leave no trace, got {root}"
        );
    }

    #[test]
    fn remove_leaves_a_hooks_object_that_lacks_our_phases_alone() {
        let mut root = json!({ "hooks": { "SessionStart": [] } });
        let before = root.clone();
        remove_lumen_hooks(&mut root);
        assert_eq!(root, before, "must not add PreToolUse/PostToolUse keys");
    }

    // ── remove_mcp_entry ─────────────────────────────────────────────────────

    #[test]
    fn remove_mcp_drops_only_the_lumen_server() {
        let mut root = json!({
            "mcpServers": {
                "lumen": { "command": "/x/lumen-mcp" },
                "other": { "command": "/y/other-mcp" }
            }
        });
        remove_mcp_entry(&mut root);
        assert!(root["mcpServers"].get("lumen").is_none());
        assert!(
            root["mcpServers"].get("other").is_some(),
            "another MCP server must survive our uninstall"
        );
    }

    #[test]
    fn remove_mcp_does_not_invent_an_mcpservers_key() {
        // Same auto-vivification regression as the hooks path, on ~/.claude.json.
        let mut root = json!({ "numStartups": 42 });
        remove_mcp_entry(&mut root);
        assert!(
            root.get("mcpServers").is_none(),
            "uninstall must not add mcpServers, got {root}"
        );
    }

    #[test]
    fn remove_mcp_is_idempotent() {
        let mut root = json!({ "mcpServers": { "lumen": { "command": "/x" } } });
        remove_mcp_entry(&mut root);
        let after = root.clone();
        remove_mcp_entry(&mut root);
        assert_eq!(root, after);
    }

    #[test]
    fn remove_then_merge_round_trips() {
        let mut root = json!({ "hooks": { "PreToolUse": null } });
        merge_hook_entry(&mut root["hooks"]["PreToolUse"], "Read", "/x/lumen_hook.sh");
        assert_eq!(root["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        remove_lumen_hooks(&mut root);
        assert!(root["hooks"]["PreToolUse"].as_array().unwrap().is_empty());
    }

    // ── cli_symlink_path ─────────────────────────────────────────────────────

    #[test]
    fn cli_symlink_lands_in_a_bin_directory_for_this_platform() {
        let p = cli_symlink_path();
        let name = p.file_name().unwrap().to_string_lossy();
        #[cfg(windows)]
        assert_eq!(name, "lumen.exe");
        #[cfg(unix)]
        assert_eq!(name, "lumen");
        assert!(
            p.parent().unwrap().ends_with("bin"),
            "must target a bin dir, got {p:?}"
        );
        assert!(p.is_absolute(), "must be absolute so it can be symlinked");
    }

    // ── SetupStep constructors ───────────────────────────────────────────────

    #[test]
    fn setup_step_constructors_carry_their_status() {
        let cases = [
            (SetupStep::ok("i", "l", "d"), StepStatus::Ok),
            (SetupStep::warn("i", "l", "d"), StepStatus::Warn),
            (SetupStep::err("i", "l", "d"), StepStatus::Error),
            (SetupStep::skip("i", "l", "d"), StepStatus::Skip),
        ];
        for (step, expected) in cases {
            assert_eq!(step.id, "i");
            assert_eq!(step.label, "l");
            assert_eq!(step.detail, "d");
            assert_eq!(
                serde_json::to_value(step.status).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
    }

    #[test]
    fn setup_step_serialises_for_the_frontend() {
        let v = serde_json::to_value(SetupStep::ok("detect", "Detect Claude", "found")).unwrap();
        for key in ["id", "label", "status", "detail"] {
            assert!(v.get(key).is_some(), "missing key: {key}");
        }
    }

    // ── step_install_scripts_in ──────────────────────────────────────────────

    #[test]
    fn installing_scripts_writes_both_hooks_executable() {
        let h = TempDir::new().unwrap();
        let step = step_install_scripts_in(h.path(), "/tmp/lumen.db", "/bin/lumen-mcp");
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);

        let dir = lumen_dir_in(h.path());
        for name in ["lumen_meter.sh", "lumen_read_intercept.sh"] {
            let f = dir.join(name);
            assert!(f.exists(), "{name} must be written");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&f).unwrap().permissions().mode();
                assert_eq!(
                    mode & 0o111,
                    0o111,
                    "{name} must be executable, got {mode:o}"
                );
            }
        }
    }

    #[test]
    fn the_meter_script_is_templated_with_the_real_paths() {
        let h = TempDir::new().unwrap();
        step_install_scripts_in(h.path(), "/my/lumen.db", "/my/lumen-mcp");
        for name in ["lumen_meter.sh", "lumen_read_intercept.sh"] {
            let body = std::fs::read_to_string(lumen_dir_in(h.path()).join(name)).unwrap();
            assert!(
                body.contains("default_db='/my/lumen.db'"),
                "{name}: DB path"
            );
            assert!(
                body.contains("default_mcp='/my/lumen-mcp'"),
                "{name}: binary path"
            );
            assert!(
                body.contains("default_spool='/my/faults.jsonl'"),
                "{name}: the spool sits beside the DB, where `lumen report` reads it"
            );
            assert!(
                !body.contains("__LUMEN_"),
                "{name}: no placeholder may survive templating:\n{body}"
            );
        }
    }

    #[test]
    fn reinstalling_scripts_overwrites_stale_paths() {
        let h = TempDir::new().unwrap();
        step_install_scripts_in(h.path(), "/old.db", "/old-mcp");
        step_install_scripts_in(h.path(), "/new.db", "/new-mcp");
        let body = std::fs::read_to_string(lumen_dir_in(h.path()).join("lumen_meter.sh")).unwrap();
        assert!(body.contains("/new.db"));
        assert!(!body.contains("/old.db"), "the stale path must be gone");
    }

    // ── step_register_mcp_in ─────────────────────────────────────────────────

    #[test]
    fn registering_the_mcp_server_creates_claude_json() {
        let h = TempDir::new().unwrap();
        let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        let entry = &v["mcpServers"]["lumen"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["command"], "/bin/lumen-mcp");
        assert_eq!(
            entry["env"],
            serde_json::json!({"LUMEN_DB": "/db"}),
            "nothing reads LUMEN_TOK since the hooks moved into lumen-mcp"
        );
    }

    /// An upgrade from 1.5.1 must take out the LUMEN_TOK that version registered,
    /// and leave every other server and key exactly as it found them.
    #[test]
    fn re_registering_drops_the_lumen_tok_an_earlier_version_wrote() {
        let h = TempDir::new().unwrap();
        std::fs::write(
            claude_json_path_in(h.path()),
            r#"{"mcpServers":{"lumen":{"type":"stdio","command":"/old/lumen-mcp","args":[],
                "env":{"LUMEN_DB":"/db","LUMEN_TOK":"/old/lumen-tok"}},
                "other":{"command":"/other","env":{"LUMEN_TOK":"theirs"}}}}"#,
        )
        .unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        assert_eq!(
            v["mcpServers"]["lumen"]["env"],
            serde_json::json!({"LUMEN_DB": "/db"})
        );
        assert_eq!(
            v["mcpServers"]["other"],
            serde_json::json!({"command": "/other", "env": {"LUMEN_TOK": "theirs"}}),
            "a foreign server is not ours to edit, even where it uses the same name"
        );
    }

    #[test]
    fn registering_preserves_other_mcp_servers_and_unrelated_keys() {
        let h = TempDir::new().unwrap();
        std::fs::write(
            claude_json_path_in(h.path()),
            r#"{"numStartups":17,"mcpServers":{"other":{"command":"/other"}}}"#,
        )
        .unwrap();

        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        assert_eq!(v["numStartups"], 17, "unrelated settings must survive");
        assert_eq!(v["mcpServers"]["other"]["command"], "/other");
        assert_eq!(v["mcpServers"]["lumen"]["command"], "/bin/lumen-mcp");
    }

    #[test]
    fn registering_backs_up_the_previous_claude_json() {
        let h = TempDir::new().unwrap();
        let path = claude_json_path_in(h.path());
        std::fs::write(&path, r#"{"numStartups":1}"#).unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        let bak = path.with_extension("json.lumen_bak");
        assert!(bak.exists(), "a backup must be taken before rewriting");
        assert!(std::fs::read_to_string(&bak)
            .unwrap()
            .contains("numStartups"));
    }

    /// The backup used to be `let _ = fs::copy(..)`: when it failed, Setup and Uninstall
    /// rewrote the file anyway, with no undo behind them and nothing said. A directory
    /// where the backup goes makes the copy fail on every OS.
    #[test]
    fn a_config_that_cannot_be_backed_up_is_left_as_it_is() {
        let h = TempDir::new().unwrap();
        let claude_json = claude_json_path_in(h.path());
        let settings = global_settings_path_in(h.path());
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let mcp_before = r#"{"mcpServers":{"lumen":{"command":"/old/lumen-mcp"}}}"#;
        let hooks_before = r#"{"hooks":{"PostToolUse":[]}}"#;
        std::fs::write(&claude_json, mcp_before).unwrap();
        std::fs::write(&settings, hooks_before).unwrap();
        for path in [&claude_json, &settings] {
            std::fs::create_dir(path.with_extension("json.lumen_bak")).unwrap();
        }

        let mut steps = vec![
            step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db"),
            step_install_hooks_in(h.path()),
        ];
        steps.extend(
            run_uninstall_in(h.path(), &FakeAutoStart::default())
                .into_iter()
                .filter(|s| s.id == "mcp" || s.id == "hooks"),
        );

        assert_eq!(steps.len(), 4, "{steps:?}");
        for step in &steps {
            assert_eq!(
                step.status,
                StepStatus::Error,
                "{}: {}",
                step.id,
                step.detail
            );
            assert!(step.detail.contains("cannot back up"), "{}", step.detail);
        }
        assert_eq!(std::fs::read_to_string(&claude_json).unwrap(), mcp_before);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), hooks_before);
    }

    /// `~/.claude.json` is Claude Code's own state: projects, history, account. Setup
    /// used to answer a parse error by starting from `{}` and writing that back, so
    /// one stray byte cost the user everything in the file but a `.lumen_bak`. A file
    /// Setup cannot parse is now left exactly as it is, and the step says why.
    #[test]
    fn registering_refuses_a_corrupt_claude_json_and_leaves_it_byte_identical() {
        let h = TempDir::new().unwrap();
        let path = claude_json_path_in(h.path());
        std::fs::write(&path, "{ not json at all").unwrap();
        let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        assert_eq!(step.status, StepStatus::Error, "{}", step.detail);
        assert!(step.detail.contains("not valid JSON"), "{}", step.detail);
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json at all");
        assert!(
            !path.with_extension("json.lumen_bak").exists(),
            "a refusal writes nothing, not even the backup"
        );
    }

    /// Valid JSON of the wrong shape. `root["mcpServers"]["lumen"] = …` panics when
    /// `mcpServers` is an array or a string, and a panic in a Tauri command is not
    /// a step that fails: it is a Setup that never answers.
    #[test]
    fn registering_refuses_a_claude_json_of_the_wrong_shape() {
        for odd in [
            r#"{"mcpServers":[]}"#,
            r#"{"mcpServers":"lumen"}"#,
            r#"[{"mcpServers":{}}]"#,
            "null",
        ] {
            let h = TempDir::new().unwrap();
            let path = claude_json_path_in(h.path());
            std::fs::write(&path, odd).unwrap();
            let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
            assert_eq!(step.status, StepStatus::Error, "{odd}: {}", step.detail);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                odd,
                "{odd} was rewritten"
            );
        }
    }

    /// The negative control for the two refusals: an empty file and a null
    /// `mcpServers` are what a first run looks like, and must still register.
    #[test]
    fn registering_accepts_an_empty_claude_json_and_a_null_mcp_servers() {
        for start in ["", "  \n", r#"{"mcpServers":null,"numStartups":3}"#] {
            let h = TempDir::new().unwrap();
            let path = claude_json_path_in(h.path());
            std::fs::write(&path, start).unwrap();
            let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
            assert_eq!(step.status, StepStatus::Ok, "{start:?}: {}", step.detail);
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(
                v["mcpServers"]["lumen"]["command"], "/bin/lumen-mcp",
                "{start:?}"
            );
        }
    }

    #[test]
    fn registering_is_idempotent() {
        let h = TempDir::new().unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        let first = std::fs::read_to_string(claude_json_path_in(h.path())).unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        let second = std::fs::read_to_string(claude_json_path_in(h.path())).unwrap();
        assert_eq!(first, second, "re-running setup must be a no-op");
    }

    // ── step_install_hooks_in ────────────────────────────────────────────────

    #[test]
    fn installing_hooks_registers_the_intercept_and_the_read_and_bash_meters() {
        let h = TempDir::new().unwrap();
        let step = step_install_hooks_in(h.path());
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();

        // The program each command runs, read the way Claude Code reads it: the
        // command is a quoted path now, so it no longer ends in `.sh` as text.
        let program =
            |e: &serde_json::Value| hook_command_path(e["hooks"][0]["command"].as_str().unwrap());
        let dir = lumen_dir_in(h.path());

        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "one PreToolUse matcher: Read");
        assert_eq!(pre[0]["matcher"], "Read");
        assert_eq!(
            program(&pre[0]),
            shell_path(&dir.join("lumen_read_intercept.sh").to_string_lossy())
        );

        let post = v["hooks"]["PostToolUse"].as_array().unwrap();
        let matchers: Vec<&str> = post
            .iter()
            .map(|e| e["matcher"].as_str().unwrap())
            .collect();
        assert_eq!(
            matchers,
            vec!["Read", "Bash"],
            "the meter handles exactly Read and Bash; the mcp__lumen__* tools meter \
             themselves in-process, so registering them only started a hook per call \
             to do nothing"
        );
        for e in post {
            assert_eq!(
                program(e),
                shell_path(&dir.join("lumen_meter.sh").to_string_lossy()),
                "every PostToolUse matcher points at the meter"
            );
        }
    }

    /// Bash must be routed to the meter, or the meter's Bash arm is dead code.
    ///
    /// It was exactly that from E7 until 1.2.1: the branch existed, nothing invoked
    /// it, and `bash_output` had zero rows in 51 days. The old version of this test
    /// asserted the four-matcher list and so actively locked the gap in place, which
    /// is why this assertion is separate and named for the consequence. That the arm
    /// does something once reached is `hook_e2e::a_captured_bash_payload_lands_as_a_bash_output_row`.
    #[test]
    fn the_bash_branch_of_the_meter_is_actually_reachable() {
        let h = TempDir::new().unwrap();
        step_install_hooks_in(h.path());
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();

        let post = v["hooks"]["PostToolUse"].as_array().unwrap();
        assert!(
            post.iter().any(|e| e["matcher"] == "Bash"),
            "Bash is not registered, so the meter's Bash arm can never run"
        );
        assert!(
            v.get("env").is_none(),
            "a fresh install opts out of nothing: {v}"
        );
    }

    /// Upgrading an install that carries the pre-1.2.1 matchers must clean them up.
    #[test]
    fn upgrading_replaces_the_dead_mcp_matchers_with_bash() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        let meter = lumen_dir_in(h.path())
            .join("lumen_meter.sh")
            .to_string_lossy()
            .to_string();
        // Exactly what 1.2.0 left behind.
        std::fs::write(
            global_settings_path_in(h.path()),
            serde_json::to_string(&serde_json::json!({
                "hooks": {"PostToolUse": [
                    {"matcher": "Read", "hooks": [{"type": "command", "command": meter}]},
                    {"matcher": "mcp__lumen__smart_read", "hooks": [{"type": "command", "command": meter}]},
                    {"matcher": "mcp__lumen__recall_file", "hooks": [{"type": "command", "command": meter}]},
                    {"matcher": "mcp__lumen__compress_logs", "hooks": [{"type": "command", "command": meter}]},
                ]}
            }))
            .unwrap(),
        )
        .unwrap();

        step_install_hooks_in(h.path());

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();
        let matchers: Vec<&str> = v["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["matcher"].as_str().unwrap())
            .collect();
        assert_eq!(
            matchers,
            vec!["Read", "Bash"],
            "an upgrade must drop the three dead matchers and add Bash"
        );
        assert!(
            v.get("env").is_none(),
            "a 1.2.0 install never had Bash, so it opted out of nothing: {v}"
        );
    }

    /// A user who deleted the Bash entry, as the README said to through 1.5.1, keeps
    /// Bash output unrecorded after Setup. The entry comes back, as Setup's own
    /// validator wants it, and with it LUMEN_METER_BASH=0, which later runs keep.
    #[test]
    fn a_removed_bash_entry_is_kept_off_as_lumen_meter_bash() {
        let h = TempDir::new().unwrap();
        let settings = global_settings_path_in(h.path());
        let read = || -> serde_json::Value {
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap()
        };
        settings_with_matchers(h.path(), &["Read"], &["Read"]);
        let mut v = read();
        v["env"] = serde_json::json!({"EDITOR": "vim"});
        std::fs::write(&settings, v.to_string()).unwrap();

        let step = step_install_hooks_in(h.path());
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);
        assert!(
            step.detail.contains("LUMEN_METER_BASH=0"),
            "{}",
            step.detail
        );
        let v = read();
        assert_eq!(
            v["env"],
            serde_json::json!({"EDITOR": "vim", "LUMEN_METER_BASH": "0"})
        );
        let matchers: Vec<&str> = v["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["matcher"].as_str().unwrap())
            .collect();
        assert_eq!(matchers, vec!["Read", "Bash"]);
        let st = validate_hooks_in(h.path());
        assert!(st.healthy, "{}", st.detail);

        let again = std::fs::read(&settings).unwrap();
        let step = step_install_hooks_in(h.path());
        assert_eq!(step.detail, "Hooks merged into ~/.claude/settings.json");
        assert!(
            std::fs::read(&settings).unwrap() == again,
            "a second run keeps it as it is"
        );

        // An `env` that is not an object is not written into.
        settings_with_matchers(h.path(), &["Read"], &["Read"]);
        let mut v = read();
        v["env"] = serde_json::json!("vim");
        std::fs::write(&settings, v.to_string()).unwrap();
        let before = std::fs::read(&settings).unwrap();
        let step = step_install_hooks_in(h.path());
        assert_eq!(step.status, StepStatus::Error, "{}", step.detail);
        assert!(step.detail.contains("\"env\""), "{}", step.detail);
        assert!(std::fs::read(&settings).unwrap() == before);
    }

    /// The cleanup must not take a user's own hook with it.
    ///
    /// `remove_lumen_hooks` drops a whole matcher entry once it spots a lumen
    /// command; reusing it here would delete a co-registered third-party hook. This
    /// is the negative control for that mistake.
    #[test]
    fn removing_our_matcher_leaves_a_foreign_hook_on_it_alone() {
        let mut arr = serde_json::json!([
            {"matcher": "mcp__lumen__smart_read", "hooks": [
                {"type": "command", "command": "/x/lumen_meter.sh"},
                {"type": "command", "command": "/opt/someone-elses-tool.sh"}
            ]},
            {"matcher": "mcp__lumen__recall_file", "hooks": [
                {"type": "command", "command": "/x/lumen_meter.sh"}
            ]}
        ]);

        unmerge_hook_entry(&mut arr, "mcp__lumen__smart_read");
        unmerge_hook_entry(&mut arr, "mcp__lumen__recall_file");

        let a = arr.as_array().unwrap();
        assert_eq!(
            a.len(),
            1,
            "the entry with a foreign hook survives; the lumen-only entry is removed"
        );
        assert_eq!(a[0]["matcher"], "mcp__lumen__smart_read");
        let hooks = a[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1, "only the lumen hook was pulled");
        assert_eq!(hooks[0]["command"], "/opt/someone-elses-tool.sh");
    }

    /// Write a settings.json with the given lumen matchers, and touch the scripts so
    /// the dangling-path check passes and only the matcher comparison is under test.
    fn settings_with_matchers(home: &Path, pre: &[&str], post: &[&str]) {
        std::fs::create_dir_all(lumen_dir_in(home)).unwrap();
        let meter = lumen_dir_in(home).join("lumen_meter.sh");
        let intercept = lumen_dir_in(home).join("lumen_read_intercept.sh");
        std::fs::write(&meter, "#!/bin/sh\n").unwrap();
        std::fs::write(&intercept, "#!/bin/sh\n").unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();

        let entry = |m: &str, cmd: &Path| {
            serde_json::json!({
                "matcher": m,
                "hooks": [{"type": "command", "command": hook_command(cmd)}]
            })
        };
        std::fs::write(
            global_settings_path_in(home),
            serde_json::to_string(&serde_json::json!({"hooks": {
                "PreToolUse":  pre.iter().map(|m| entry(m, &intercept)).collect::<Vec<_>>(),
                "PostToolUse": post.iter().map(|m| entry(m, &meter)).collect::<Vec<_>>(),
            }}))
            .unwrap(),
        )
        .unwrap();
    }

    /// The state of a real 1.2.3 install: every script present and current, but the
    /// matcher set left over from 1.2.0.
    ///
    /// This reported `healthy: true` with the detail "5 hook commands, all present",
    /// because the validator only checked for dangling paths. Hooks are not
    /// auto-repaired, so that report was the only signal the user would ever get — and
    /// it said nothing was wrong while the Bash meter was not installed.
    #[test]
    fn a_stale_matcher_set_is_reported_even_when_every_script_exists() {
        let h = TempDir::new().unwrap();
        settings_with_matchers(
            h.path(),
            &["Read"],
            &[
                "Read",
                "mcp__lumen__smart_read",
                "mcp__lumen__recall_file",
                "mcp__lumen__compress_logs",
            ],
        );

        let st = validate_hooks_in(h.path());
        assert!(
            !st.healthy,
            "an install without the Bash matcher is not healthy, got: {}",
            st.detail
        );
        assert!(
            st.detail.contains("Bash"),
            "the report must name the missing matcher so repair is actionable: {}",
            st.detail
        );
        assert!(
            st.detail.contains("mcp__lumen__smart_read"),
            "and name the retired matchers still registered: {}",
            st.detail
        );
    }

    /// What `step_install_hooks_in` produces must validate as healthy. This is the pair
    /// that keeps the installer and the validator from drifting apart again: if either
    /// side changes alone, one of these two tests fails.
    #[test]
    fn what_setup_installs_validates_as_current() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(lumen_dir_in(h.path())).unwrap();
        std::fs::write(lumen_dir_in(h.path()).join("lumen_meter.sh"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            lumen_dir_in(h.path()).join("lumen_read_intercept.sh"),
            "#!/bin/sh\n",
        )
        .unwrap();

        step_install_hooks_in(h.path());

        let st = validate_hooks_in(h.path());
        assert!(
            st.healthy,
            "setup's own output must satisfy the validator: {}",
            st.detail
        );
    }

    /// A dangling command is still caught, alongside the new matcher check.
    #[test]
    fn a_missing_script_is_still_reported() {
        let h = TempDir::new().unwrap();
        settings_with_matchers(h.path(), &["Read"], &["Read", "Bash"]);
        std::fs::remove_file(lumen_dir_in(h.path()).join("lumen_meter.sh")).unwrap();

        let st = validate_hooks_in(h.path());
        assert!(!st.healthy);
        assert!(
            st.detail.contains("dangling"),
            "expected a dangling report, got: {}",
            st.detail
        );
    }

    #[test]
    fn installing_hooks_preserves_unrelated_settings() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        std::fs::write(
            global_settings_path_in(h.path()),
            r#"{"theme":"dark","hooks":{"SessionStart":[{"matcher":"*","hooks":[]}]}}"#,
        )
        .unwrap();

        step_install_hooks_in(h.path());

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();
        assert_eq!(v["theme"], "dark");
        assert!(
            v["hooks"]["SessionStart"].is_array(),
            "another hook phase must survive"
        );
        assert!(v["hooks"]["PreToolUse"].is_array());
    }

    #[test]
    fn installing_hooks_is_idempotent() {
        let h = TempDir::new().unwrap();
        step_install_hooks_in(h.path());
        let first = std::fs::read_to_string(global_settings_path_in(h.path())).unwrap();
        step_install_hooks_in(h.path());
        let second = std::fs::read_to_string(global_settings_path_in(h.path())).unwrap();
        assert_eq!(first, second, "re-running setup must not duplicate hooks");
    }

    /// settings.json that does not parse is refused, not replaced.
    ///
    /// This test used to assert the opposite: Setup read the file as `{}`, wrote back
    /// an object holding only Lumen's hooks, and counted it a success because a backup
    /// existed. Every permission, environment variable and foreign hook the user had
    /// was gone from the live file. The same rule now covers ~/.claude.json, where the
    /// loss was every other MCP server.
    #[test]
    fn installing_hooks_refuses_corrupt_settings_and_leaves_them_byte_identical() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        let settings = global_settings_path_in(h.path());
        std::fs::write(&settings, "]]not json[[").unwrap();
        let before = fingerprint(&settings);

        let step = step_install_hooks_in(h.path());

        assert_eq!(step.status, StepStatus::Error, "{}", step.detail);
        assert!(step.detail.contains("not valid JSON"), "{}", step.detail);
        assert_eq!(fingerprint(&settings), before, "the file must be untouched");
        assert!(
            !settings.with_extension("json.lumen_bak").exists(),
            "nothing was replaced, so there is nothing to back up"
        );
        // The validator says the same thing rather than "no lumen hooks — run Setup",
        // which would send the user to a Setup that refuses the same file.
        let st = validate_hooks_in(h.path());
        assert!(!st.healthy);
        assert!(st.detail.contains("not valid JSON"), "{}", st.detail);
    }

    /// The negative control: a valid file of the right shape is merged as before.
    #[test]
    fn installing_hooks_accepts_an_empty_file_and_a_null_hooks_key() {
        for body in [
            "",
            "  \n",
            r#"{"hooks":null}"#,
            r#"{"hooks":{"PreToolUse":null}}"#,
        ] {
            let h = TempDir::new().unwrap();
            std::fs::create_dir_all(h.path().join(".claude")).unwrap();
            std::fs::write(global_settings_path_in(h.path()), body).unwrap();
            let step = step_install_hooks_in(h.path());
            assert_eq!(step.status, StepStatus::Ok, "{body:?}: {}", step.detail);
        }
    }

    /// A `hooks` value Setup would have to replace to proceed is the user's to fix.
    /// Indexing into it would otherwise panic the Setup command.
    #[test]
    fn installing_hooks_refuses_settings_of_the_wrong_shape() {
        for body in [
            r#"[]"#,
            r#"{"hooks":[]}"#,
            r#"{"hooks":"x"}"#,
            r#"{"hooks":{"PostToolUse":{"matcher":"Read"}}}"#,
        ] {
            let h = TempDir::new().unwrap();
            std::fs::create_dir_all(h.path().join(".claude")).unwrap();
            let settings = global_settings_path_in(h.path());
            std::fs::write(&settings, body).unwrap();
            let step = step_install_hooks_in(h.path());
            assert_eq!(step.status, StepStatus::Error, "{body}: {}", step.detail);
            assert!(
                step.detail.contains("left untouched"),
                "{body}: {}",
                step.detail
            );
            assert_eq!(std::fs::read_to_string(&settings).unwrap(), body);
        }
    }

    // ── run_uninstall_in ─────────────────────────────────────────────────────

    #[test]
    fn uninstall_removes_everything_setup_installed() {
        let h = TempDir::new().unwrap();
        step_install_scripts_in(h.path(), "/db", "/mcp");
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        step_install_hooks_in(h.path());

        let steps = run_uninstall_in(h.path(), &FakeAutoStart::default());
        assert!(
            steps.iter().all(|s| s.status != StepStatus::Error),
            "no step may fail: {steps:?}"
        );

        // MCP entry gone.
        let claude: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        assert!(claude["mcpServers"].get("lumen").is_none());

        // Hooks gone.
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();
        assert!(settings["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(settings["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .is_empty());

        // Scripts directory gone.
        assert!(!lumen_dir_in(h.path()).exists());
    }

    #[test]
    fn uninstall_keeps_a_foreign_mcp_server_and_foreign_hooks() {
        let h = TempDir::new().unwrap();
        std::fs::write(
            claude_json_path_in(h.path()),
            r#"{"mcpServers":{"other":{"command":"/other"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        std::fs::write(
            global_settings_path_in(h.path()),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Write","hooks":[{"command":"/theirs.sh"}]}]}}"#,
        )
        .unwrap();

        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        step_install_hooks_in(h.path());
        run_uninstall_in(h.path(), &FakeAutoStart::default());

        let claude: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        assert_eq!(
            claude["mcpServers"]["other"]["command"], "/other",
            "another MCP server must survive our uninstall"
        );

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();
        let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "the foreign hook must remain");
        assert_eq!(pre[0]["matcher"], "Write");
    }

    #[test]
    fn uninstall_on_a_clean_machine_skips_rather_than_failing() {
        let h = TempDir::new().unwrap();
        let steps = run_uninstall_in(h.path(), &FakeAutoStart::default());
        assert!(
            steps.iter().all(|s| s.status != StepStatus::Error),
            "nothing installed is not an error: {steps:?}"
        );
        assert!(
            steps.iter().any(|s| s.status == StepStatus::Skip),
            "it should report skips: {steps:?}"
        );
    }

    #[test]
    fn uninstall_is_idempotent() {
        let h = TempDir::new().unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        step_install_hooks_in(h.path());
        run_uninstall_in(h.path(), &FakeAutoStart::default());
        let steps = run_uninstall_in(h.path(), &FakeAutoStart::default());
        assert!(steps.iter().all(|s| s.status != StepStatus::Error));
    }

    #[test]
    fn uninstall_reports_one_step_per_action() {
        let h = TempDir::new().unwrap();
        let ids: Vec<String> = run_uninstall_in(h.path(), &FakeAutoStart::default())
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["mcp", "hooks", "scripts", "cli", "autostart"]);
    }

    #[test]
    fn uninstall_actually_removes_an_enabled_login_item() {
        // The unit test above proves the step; this proves it is wired into the
        // uninstall path, which is what would leave a removed app relaunching at
        // every login if it were forgotten.
        let h = TempDir::new().unwrap();
        let a = FakeAutoStart::on();
        let steps = run_uninstall_in(h.path(), &a);
        assert_eq!(find(&steps, "autostart").status, StepStatus::Ok);
        assert!(!a.enabled.get(), "login item must be gone after uninstall");
    }

    #[test]
    fn uninstall_still_completes_when_the_login_item_cannot_be_removed() {
        // A locked-down login-item mechanism must not stop the rest of uninstall
        // from cleaning up ~/.claude.
        let h = TempDir::new().unwrap();
        let broken = FakeAutoStart {
            enabled: std::cell::Cell::new(true),
            fail_write: true,
            ..Default::default()
        };
        let steps = run_uninstall_in(h.path(), &broken);
        assert_eq!(find(&steps, "autostart").status, StepStatus::Warn);
        for id in ["mcp", "hooks", "scripts", "cli"] {
            assert_ne!(
                find(&steps, id).status,
                StepStatus::Error,
                "{id} should still have been attempted"
            );
        }
    }

    // ── install then uninstall round trip ────────────────────────────────────

    #[test]
    fn a_full_round_trip_leaves_config_as_it_started() {
        // The strongest guarantee an uninstaller can offer: byte-identical config.
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        let claude_before = r#"{
  "numStartups": 42
}"#;
        let settings_before = r#"{
  "theme": "dark"
}"#;
        std::fs::write(claude_json_path_in(h.path()), claude_before).unwrap();
        std::fs::write(global_settings_path_in(h.path()), settings_before).unwrap();

        step_install_scripts_in(h.path(), "/db", "/mcp");
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db");
        step_install_hooks_in(h.path());
        run_uninstall_in(h.path(), &FakeAutoStart::default());

        let claude: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();

        assert_eq!(claude["numStartups"], 42);
        assert_eq!(settings["theme"], "dark");
        // Empty arrays remain where we merged; nothing of ours is left behind.
        assert!(claude["mcpServers"].get("lumen").is_none());
        assert!(settings["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    // ── Setup re-run over an autostart opt-out ───────────────────────────────

    /// A home Setup can complete in: `~/.claude` present and a lumen-mcp to bake.
    fn setup_home(home: &Path) -> (String, String) {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let mcp = home.join("lumen-mcp");
        std::fs::write(&mcp, "").unwrap();
        (
            home.join("data/lumen.db").to_string_lossy().to_string(),
            mcp.to_string_lossy().to_string(),
        )
    }

    /// 1.6.0 tells Windows users to re-run Setup to repair their hook commands. A
    /// re-run that switched the login item back on would be 1.1.2's bug again, through
    /// a second door.
    #[test]
    fn re_running_setup_leaves_a_login_item_the_user_turned_off_alone() {
        let h = TempDir::new().unwrap();
        let (db, mcp) = setup_home(h.path());
        let a = FakeAutoStart::default();
        // The startup registration, then the user's toggle.
        assert!(ensure_autostart_once(
            &a,
            &autostart_marker_in(h.path()),
            EXE
        ));
        a.disable().unwrap();

        let steps = run_setup_with(h.path(), &a, &db, &mcp);

        let step = find(&steps, AUTOSTART_ID);
        assert_eq!(step.status, StepStatus::Ok, "{step:?}");
        assert!(step.detail.contains("Left off"), "{}", step.detail);
        assert!(!a.enabled.get(), "Setup switched the login item back on");
        assert_eq!(
            a.enable_calls.get(),
            1,
            "only the startup registration enabled it"
        );
        assert!(
            marker_path_in(h.path()).exists(),
            "an opt-out is not a failed step"
        );
    }

    /// The negative control: with no marker nobody turned anything off, and Setup
    /// registers the login item as it always has.
    #[test]
    fn setup_registers_the_login_item_when_nothing_was_turned_off() {
        let h = TempDir::new().unwrap();
        let (db, mcp) = setup_home(h.path());
        let a = FakeAutoStart::default();

        let steps = run_setup_with(h.path(), &a, &db, &mcp);

        assert_eq!(find(&steps, AUTOSTART_ID).status, StepStatus::Ok);
        assert!(a.enabled.get());
        assert_eq!(a.enable_calls.get(), 1);
    }

    /// An unreadable state is not an opt-out. The marker alone proves nothing, so this
    /// warns as it always did instead of claiming the user chose it.
    #[test]
    fn an_unreadable_login_item_with_a_marker_still_warns() {
        let h = TempDir::new().unwrap();
        let (db, mcp) = setup_home(h.path());
        let marker = autostart_marker_in(h.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, EXE).unwrap();

        let steps = run_setup_with(h.path(), &FakeAutoStart::broken_read(), &db, &mcp);

        let step = find(&steps, AUTOSTART_ID);
        assert_eq!(step.status, StepStatus::Warn, "{step:?}");
        assert!(!step.detail.contains("Left off"), "{}", step.detail);
    }

    // ── hook commands: what Claude Code runs is the script Setup wrote ───────

    /// Each registered command must scan back to the file it names, for the homes that
    /// broke the bare path: a space, a quote, shell metacharacters, a tab.
    #[test]
    fn a_registered_command_names_its_script_whole() {
        for p in [
            "/Users/Jane Doe/.claude/lumen/lumen_meter.sh",
            "/home/o'brien/.claude/lumen/lumen_meter.sh",
            "/home/$USER `id`/.claude/lumen/lumen_meter.sh",
            "/home/tab\there/.claude/lumen/lumen_meter.sh",
            "/home/plain/.claude/lumen/lumen_meter.sh",
        ] {
            let cmd = hook_command(Path::new(p));
            assert_eq!(hook_command_path(&cmd), shell_path(p), "{cmd}");
            assert!(!command_needs_quotes(&cmd), "{cmd}");
        }
    }

    /// The Windows failure. Claude Code's scan takes a backslash as an escape, so a bare
    /// `C:\Users\…` loses every separator before bash is even started.
    #[test]
    fn a_windows_path_survives_the_scan_only_with_forward_slashes() {
        let win = r"C:\Users\Jane\.claude\lumen\lumen_meter.sh";
        assert_eq!(
            hook_command_path(win),
            "C:UsersJane.claudelumenlumen_meter.sh"
        );
        assert!(command_needs_quotes(win), "the validator must flag it");

        let cmd = sh_quote(&shell_path_for(win, true));
        assert_eq!(cmd, "'C:/Users/Jane/.claude/lumen/lumen_meter.sh'");
        assert_eq!(
            hook_command_path(&cmd),
            "C:/Users/Jane/.claude/lumen/lumen_meter.sh"
        );
        assert!(!command_needs_quotes(&cmd));
    }

    /// Off Windows a backslash is an ordinary filename byte. Rewriting it would name a
    /// different file.
    #[test]
    fn a_backslash_is_kept_in_a_unix_path() {
        assert_eq!(shell_path_for(r"/tmp/a\b.sh", false), r"/tmp/a\b.sh");
        let cmd = sh_quote(&shell_path_for(r"/tmp/a\b.sh", false));
        assert_eq!(hook_command_path(&cmd), r"/tmp/a\b.sh");
    }

    /// The pre-1.6.0 registration in a home with a space. Every script exists, so the
    /// old dangling check passed and this reported healthy while no hook ran.
    #[test]
    fn an_unquoted_command_is_reported_even_though_its_script_exists() {
        let root = TempDir::new().unwrap();
        let home = root.path().join("Jane Doe");
        settings_with_matchers(&home, &["Read"], &["Read", "Bash"]);
        // Each command as the bare path 1.5.1 wrote.
        let path = global_settings_path_in(&home);
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for phase in ["PreToolUse", "PostToolUse"] {
            for e in v["hooks"][phase].as_array_mut().unwrap() {
                let c = &mut e["hooks"][0]["command"];
                *c = json!(hook_command_path(c.as_str().unwrap()));
            }
        }
        std::fs::write(&path, v.to_string()).unwrap();

        let st = validate_hooks_in(&home);
        assert!(!st.healthy, "{}", st.detail);
        assert!(st.detail.contains("unquoted path"), "{}", st.detail);
        assert!(
            !st.detail.contains("dangling"),
            "every script exists: {}",
            st.detail
        );

        step_install_hooks_in(&home);
        let st = validate_hooks_in(&home);
        assert!(st.healthy, "re-running Setup must repair it: {}", st.detail);
    }

    /// The negative control. A bare path with nothing the shell splits or expands runs
    /// as written, and flagging it would send every macOS user of 1.5.1 to re-run Setup
    /// for nothing.
    #[test]
    fn a_bare_path_the_shell_takes_literally_is_not_flagged() {
        assert!(!command_needs_quotes(
            "/Users/jane/.claude/lumen/lumen_meter.sh"
        ));
        assert!(!command_needs_quotes(
            "/home/j.doe-1/.claude/lumen/lumen_meter.sh"
        ));
    }

    // ── find_binary_from ─────────────────────────────────────────────────────

    /// Windows bundles `lumen-mcp.exe`. Probing for the bare name never matched it, so
    /// Setup there reported the sidecar missing beside a working binary.
    #[test]
    fn the_sidecar_is_found_under_this_platforms_file_name() {
        let d = TempDir::new().unwrap();
        let file = format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(d.path().join(&file), "").unwrap();
        assert_eq!(
            find_binary_from(d.path(), "lumen-mcp"),
            Some(d.path().join(file))
        );
    }

    #[test]
    fn a_sidecar_under_another_platforms_name_is_not_found() {
        let d = TempDir::new().unwrap();
        let other = if cfg!(windows) {
            "lumen-mcp"
        } else {
            "lumen-mcp.exe"
        };
        std::fs::write(d.path().join(other), "").unwrap();
        assert_eq!(find_binary_from(d.path(), "lumen-mcp"), None);
    }

    /// The dev layout: the app runs from `target/debug` and the sidecar was built into
    /// `target/release`.
    #[test]
    fn a_dev_build_finds_the_release_sidecar_by_walking_up() {
        let d = TempDir::new().unwrap();
        let release = d.path().join("target/release");
        let start = d.path().join("target/debug/deps");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::create_dir_all(&start).unwrap();
        let file = format!("lumen-mcp{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(release.join(&file), "").unwrap();
        assert_eq!(
            find_binary_from(&start, "lumen-mcp"),
            Some(release.join(file))
        );
    }

    // ── claude_on_path ───────────────────────────────────────────────────────

    /// npm installs `claude.cmd` on Windows, and `which` does not exist there.
    #[test]
    fn claude_is_found_on_path_under_this_platforms_name() {
        let d = TempDir::new().unwrap();
        let name = if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        };
        std::fs::write(d.path().join(name), "").unwrap();
        let path =
            std::env::join_paths([d.path().join("nonexistent").as_path(), d.path()]).unwrap();
        assert!(claude_on_path(Some(&path)));
    }

    #[test]
    fn claude_is_not_found_where_it_is_not() {
        let d = TempDir::new().unwrap();
        // A directory of that name is not a program, and neither is a lookalike.
        std::fs::create_dir(d.path().join("claude")).unwrap();
        std::fs::write(d.path().join("claude-code"), "").unwrap();
        let path = std::env::join_paths([d.path()]).unwrap();
        assert!(!claude_on_path(Some(&path)));
        assert!(!claude_on_path(None));
    }

    // ── marker / setup-needed detection ──────────────────────────────────────

    #[test]
    fn setup_is_needed_when_the_marker_is_absent() {
        let dir = TempDir::new().unwrap();
        assert!(!marker_path_in(dir.path()).exists());
    }

    #[test]
    fn setup_is_satisfied_once_the_marker_exists() {
        let dir = TempDir::new().unwrap();
        let marker = marker_path_in(dir.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "").unwrap();
        assert!(marker.exists());
        assert!(
            marker.starts_with(dir.path()),
            "the marker must live under the supplied home, never the real one"
        );
    }

    /// The cask must uninstall the LaunchAgent that actually exists.
    ///
    /// It targeted `io.speedata.lumen` — the bundle id — while the autostart plugin writes
    /// `Lumen.plist` with Label `Lumen`. `launchctl` matches on the Label, so every
    /// uninstall left the login item in place, still trying to launch a deleted app at
    /// each boot. Reported from the field as part of issue #5.
    #[test]
    fn the_cask_uninstalls_the_launch_agent_that_exists() {
        let cask = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("Casks/lumen-app.rb");
        let text = match std::fs::read_to_string(&cask) {
            Ok(t) => t,
            Err(_) => return,
        };

        assert!(
            text.contains("launchctl:  \"Lumen\"") || text.contains("launchctl: \"Lumen\""),
            "the cask must unload the agent by its Label (Lumen), not by the bundle id"
        );
        assert!(
            text.contains("~/Library/LaunchAgents/Lumen.plist"),
            "zap must remove the plist the plugin actually writes"
        );
    }
}
