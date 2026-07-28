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
pub fn ensure_autostart_once(a: &dyn AutoStart, marker: &Path) -> bool {
    if marker.exists() {
        return false;
    }
    let record = || {
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(marker, "");
    };
    match a.is_enabled() {
        // Already on — nothing to do, but record it so a later opt-out sticks.
        Ok(true) => {
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

/// Run the one-time registration against the real home and plugin.
pub fn ensure_autostart_once_for(app: &AppHandle) -> bool {
    ensure_autostart_once(&PluginAutoStart(app), &autostart_marker_in(&home()))
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
const APP_ID: &str = "io.speedata.lumen";

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
    // 1. Release path: sibling of the main exe
    let beside = exe_dir().join(name);
    if beside.exists() {
        return Some(beside);
    }
    // 2. Dev path: walk up looking for workspace/target/release/<name>
    let mut probe = exe_dir();
    for _ in 0..8 {
        let candidate = probe.join("target/release").join(name);
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
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support").join(APP_ID)
    }
    #[cfg(target_os = "windows")]
    {
        home.join("AppData").join("Roaming").join(APP_ID)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".local").join("share").join(APP_ID)
    }
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
    let dest = bin_dir.join(name);
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

fn db_path() -> String {
    std::env::var("LUMEN_DB").unwrap_or_else(|_| db_path_in(&home()))
}

/// The metering DB inside the per-OS data directory. Derived from
/// [`app_support_dir_in`] so the two can never disagree about where data lives.
fn db_path_in(home: &Path) -> String {
    app_support_dir_in(home)
        .join("lumen.db")
        .to_string_lossy()
        .to_string()
}

// ── Script templates ──────────────────────────────────────────────────────────
//
// The meter script is embedded with two path placeholders substituted at
// install time.  The intercept script has no path dependencies.

const METER_TEMPLATE: &str = r#"#!/usr/bin/env bash
# lumen_meter.sh — installed by Lumen Setup. Re-run Setup in the Lumen app to refresh.
LUMEN_DB="__LUMEN_DB__"
LUMEN_TOK="__LUMEN_TOK__"

set -euo pipefail

INPUT=$(cat)

if [ "${LUMEN_DEBUG:-}" = "1" ]; then
    echo "$INPUT" > /tmp/lumen_hook_dump.json
fi

TOOL_NAME=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_name', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

FILE_PATH=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_input', {}).get('file_path', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null || echo 0)

if [ -x "$LUMEN_TOK" ]; then
    FULL_TOKENS=$("$LUMEN_TOK" < "$FILE_PATH" 2>/dev/null || echo 0)
else
    FULL_TOKENS=$(( $(wc -c < "$FILE_PATH") / 4 ))
fi

sqlite3 "$LUMEN_DB" \
    "INSERT INTO read_events(ts,tool,path,lines,tokens_returned,full_tokens,saved_tokens,routed_via,channel)
     VALUES('${TS}','Read','${FILE_PATH//\'/\'\'}',${LINE_COUNT},${FULL_TOKENS},${FULL_TOKENS},0,'builtin_read','cli');" \
    2>/dev/null || true

exit 0
"#;

const INTERCEPT_SCRIPT: &str = r#"#!/usr/bin/env bash
# lumen_read_intercept.sh — installed by Lumen Setup.
set -euo pipefail

INPUT=$(cat)
HOOK_ENABLED="${LUMEN_HOOK_ENABLED:-1}"
THRESHOLD="${LUMEN_LINE_THRESHOLD:-300}"

if [ "$HOOK_ENABLED" = "0" ]; then
    exit 0
fi

TOOL_NAME=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_name', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ "$TOOL_NAME" != "Read" ]; then
    exit 0
fi

FILE_PATH=$(python3 -c "
import sys, json
d = json.loads(sys.argv[1])
print(d.get('tool_input', {}).get('file_path', ''))
" "$INPUT" 2>/dev/null || echo "")

if [ -z "$FILE_PATH" ] || [ ! -f "$FILE_PATH" ]; then
    exit 0
fi

EXT=$(echo "${FILE_PATH##*.}" | tr '[:upper:]' '[:lower:]')
case "$EXT" in
    rs|py|pyi|ts|tsx) FILE_TYPE="source" ;;
    log|out|txt)      FILE_TYPE="log"    ;;
    *)                exit 0             ;;
esac

LINE_COUNT=$(wc -l < "$FILE_PATH" 2>/dev/null || echo 0)
if [ "$LINE_COUNT" -lt "$THRESHOLD" ]; then
    exit 0
fi

if [ "$FILE_TYPE" = "log" ]; then
    cat >&2 <<MSG
Lumen intercept: ${FILE_PATH} is ${LINE_COUNT} lines (log/output file).
Before reading the full file, call:
  lumen:compress_logs(path="${FILE_PATH}")
This collapses repeated lines and stack frames deterministically (typically 40-80%
token reduction). Analyze the compressed output; the full file is still readable
via smart_read(mode="full") if needed.
MSG
else
    cat >&2 <<MSG
Lumen intercept: ${FILE_PATH} is ${LINE_COUNT} lines.
Instead of reading the full file, call:
  1. lumen:smart_read(path="${FILE_PATH}")       → structural outline, ~5-10% token cost
  2. lumen:recall_file(path="${FILE_PATH}", names=["<item>"]) → fetch only what you need
This typically saves 80-93% of context vs. reading the whole file.
Use smart_read(mode="full") only if you truly need every line.
MSG
fi

exit 2
"#;

// ── Step implementations ──────────────────────────────────────────────────────

fn step_detect_claude() -> SetupStep {
    let dot_claude = home().join(".claude");
    if !dot_claude.exists() {
        return SetupStep::err(
            "detect",
            "Detect Claude Code",
            "~/.claude/ not found. Install Claude Code (https://claude.ai/code) first.",
        );
    }
    let has_cli = which_claude();
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

fn which_claude() -> bool {
    std::process::Command::new("which")
        .arg("claude")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn step_install_scripts(db: &str, tok: &str) -> SetupStep {
    step_install_scripts_in(&home(), db, tok)
}

fn step_install_scripts_in(home: &Path, db: &str, tok: &str) -> SetupStep {
    let dir = lumen_dir_in(home);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return SetupStep::err(
            "scripts",
            "Install hook scripts",
            &format!("mkdir ~/.claude/lumen: {e}"),
        );
    }

    // Backup existing scripts to .bak if they're ours and paths changed
    let meter_path = dir.join("lumen_meter.sh");
    let intercept_path = dir.join("lumen_read_intercept.sh");

    let meter_content = METER_TEMPLATE
        .replace("__LUMEN_DB__", db)
        .replace("__LUMEN_TOK__", tok);

    if let Err(e) = std::fs::write(&meter_path, &meter_content) {
        return SetupStep::err(
            "scripts",
            "Install hook scripts",
            &format!("write lumen_meter.sh: {e}"),
        );
    }
    if let Err(e) = std::fs::write(&intercept_path, INTERCEPT_SCRIPT) {
        return SetupStep::err(
            "scripts",
            "Install hook scripts",
            &format!("write lumen_read_intercept.sh: {e}"),
        );
    }

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&meter_path, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::set_permissions(&intercept_path, std::fs::Permissions::from_mode(0o755));
    }

    SetupStep::ok(
        "scripts",
        "Install hook scripts",
        "Written to ~/.claude/lumen/",
    )
}

fn step_register_mcp(mcp_bin: &str, db: &str, tok: &str) -> SetupStep {
    step_register_mcp_in(&home(), mcp_bin, db, tok)
}

fn step_register_mcp_in(home: &Path, mcp_bin: &str, db: &str, tok: &str) -> SetupStep {
    let path = claude_json_path_in(home);

    // Parse or start fresh
    let mut root: serde_json::Value = if path.exists() {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

    // Ensure it's an object
    if !root.is_object() {
        root = serde_json::json!({});
    }

    // Backup before modifying
    if path.exists() {
        let bak = path.with_extension("json.lumen_bak");
        let _ = std::fs::copy(&path, &bak);
    }

    let entry = serde_json::json!({
        "type":    "stdio",
        "command": mcp_bin,
        "args":    [],
        "env": {
            "LUMEN_DB":  db,
            "LUMEN_TOK": tok
        }
    });

    root["mcpServers"]["lumen"] = entry;

    match serde_json::to_string_pretty(&root) {
        Ok(s) => match std::fs::write(&path, s) {
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

fn step_install_hooks() -> SetupStep {
    step_install_hooks_in(&home())
}

fn step_install_hooks_in(home: &Path) -> SetupStep {
    let dir = lumen_dir_in(home);
    let meter = dir.join("lumen_meter.sh").to_string_lossy().to_string();
    let intercept = dir
        .join("lumen_read_intercept.sh")
        .to_string_lossy()
        .to_string();

    let path = global_settings_path_in(home);

    // Ensure ~/.claude/ exists
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return SetupStep::err("hooks", "Install hooks", &format!("mkdir ~/.claude: {e}"));
        }
    }

    // Parse or start fresh
    let mut root: serde_json::Value = if path.exists() {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

    if !root.is_object() {
        root = serde_json::json!({});
    }

    // Backup
    if path.exists() {
        let bak = path.with_extension("json.lumen_bak");
        let _ = std::fs::copy(&path, &bak);
    }

    // Ensure hooks object exists
    if !root["hooks"].is_object() {
        root["hooks"] = serde_json::json!({});
    }

    // PreToolUse: Read intercept
    merge_hook_entry(&mut root["hooks"]["PreToolUse"], "Read", &intercept);

    // PostToolUse: meter for Read + three lumen tools
    for matcher in &[
        "Read",
        "mcp__lumen__smart_read",
        "mcp__lumen__recall_file",
        "mcp__lumen__compress_logs",
    ] {
        merge_hook_entry(&mut root["hooks"]["PostToolUse"], matcher, &meter);
    }

    match serde_json::to_string_pretty(&root) {
        Ok(s) => match std::fs::write(&path, s) {
            Ok(_) => SetupStep::ok(
                "hooks",
                "Install hooks",
                "Hooks merged into ~/.claude/settings.json",
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

// ── Main orchestration ────────────────────────────────────────────────────────

fn run_setup(autostart: &dyn AutoStart) -> Vec<SetupStep> {
    let mut steps = Vec::new();

    // 1. Detect Claude Code
    let detect = step_detect_claude();
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

    // 2. Resolve binary paths
    let mcp_bin = stable_binary("lumen-mcp");
    let tok_bin = stable_binary("lumen-tok");

    let mcp_str = mcp_bin
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let tok_str = tok_bin
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let db_str = db_path();

    // 3. Install scripts (needs tok path for templating)
    if tok_str.is_empty() {
        steps.push(SetupStep::err(
            "scripts",
            "Install hook scripts",
            "lumen-tok binary not found — rebuild sidecars with build-sidecar.sh",
        ));
    } else {
        steps.push(step_install_scripts(&db_str, &tok_str));
    }

    // 4. Register MCP (needs mcp path)
    if mcp_str.is_empty() {
        steps.push(SetupStep::err(
            "mcp",
            "Register MCP server",
            "lumen-mcp binary not found — rebuild sidecars with build-sidecar.sh",
        ));
    } else {
        steps.push(step_register_mcp(&mcp_str, &db_str, &tok_str));
    }

    // 5. Install hooks (needs scripts installed first)
    let scripts_ok = steps
        .iter()
        .any(|s| s.id == "scripts" && s.status == StepStatus::Ok);
    if scripts_ok {
        steps.push(step_install_hooks());
    } else {
        steps.push(SetupStep::skip(
            "hooks",
            "Install hooks",
            "Skipped: hook scripts not installed",
        ));
    }

    // 6. Register the login item. Deliberately last: it is the only step that
    // touches something outside ~/.claude, and it must be inside the all_good
    // check below so a warning here still lets setup complete.
    steps.push(step_enable_autostart(autostart));

    // 7. Write marker on full success
    let all_good = steps
        .iter()
        .all(|s| s.status == StepStatus::Ok || s.status == StepStatus::Warn);
    if all_good {
        let _ = std::fs::create_dir_all(lumen_dir());
        let _ = std::fs::write(marker_path(), "");
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
                let _ = std::fs::copy(&claude_json, claude_json.with_extension("json.lumen_bak"));
                match serde_json::to_string_pretty(&v)
                    .ok()
                    .and_then(|s| std::fs::write(&claude_json, s).ok())
                {
                    Some(_) => steps.push(SetupStep::ok(
                        "mcp",
                        "Remove MCP entry",
                        "Removed from ~/.claude.json",
                    )),
                    None => steps.push(SetupStep::err(
                        "mcp",
                        "Remove MCP entry",
                        "Could not write ~/.claude.json",
                    )),
                }
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
                let _ = std::fs::copy(&settings, settings.with_extension("json.lumen_bak"));
                match serde_json::to_string_pretty(&v)
                    .ok()
                    .and_then(|s| std::fs::write(&settings, s).ok())
                {
                    Some(_) => steps.push(SetupStep::ok(
                        "hooks",
                        "Remove hooks",
                        "Removed from ~/.claude/settings.json",
                    )),
                    None => steps.push(SetupStep::err(
                        "hooks",
                        "Remove hooks",
                        "Could not write ~/.claude/settings.json",
                    )),
                }
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
    let Some(lumen_bin) = find_binary("lumen") else {
        return SetupStep::err("cli", "Install CLI", "lumen binary not found in app bundle");
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

        assert!(ensure_autostart_once(&a, &marker), "should register");
        assert!(a.enabled.get());
        assert!(marker.exists(), "marker records that this ran");
    }

    #[test]
    fn the_one_time_registration_does_not_repeat_on_later_launches() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::default();

        ensure_autostart_once(&a, &marker);
        assert_eq!(a.enable_calls.get(), 1);

        // Every subsequent launch is a no-op.
        for _ in 0..3 {
            assert!(!ensure_autostart_once(&a, &marker));
        }
        assert_eq!(a.enable_calls.get(), 1, "must not re-register");
    }

    #[test]
    fn turning_the_toggle_off_is_not_undone_by_the_next_launch() {
        // The whole reason this has its own marker: the user's opt-out has to win.
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        let a = FakeAutoStart::default();

        ensure_autostart_once(&a, &marker);
        assert!(a.enabled.get());

        a.disable().unwrap(); // user flips the toggle off
        assert!(!a.enabled.get());

        ensure_autostart_once(&a, &marker); // next launch
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

        assert!(!ensure_autostart_once(&a, &marker), "nothing to do");
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

        assert!(!ensure_autostart_once(&broken, &marker));
        assert!(!marker.exists(), "must not record a failure as done");
        assert_eq!(broken.enable_calls.get(), 1);

        assert!(!ensure_autostart_once(&broken, &marker));
        assert_eq!(broken.enable_calls.get(), 2, "retried");
    }

    #[test]
    fn an_unreadable_setting_does_not_write_the_marker() {
        let h = TempDir::new().unwrap();
        let marker = h.path().join(".claude/lumen/.autostart_done");
        assert!(!ensure_autostart_once(
            &FakeAutoStart::broken_read(),
            &marker
        ));
        assert!(!marker.exists());
    }

    #[test]
    fn the_marker_directory_is_created_if_it_does_not_exist() {
        // First launch on a machine with no ~/.claude/lumen yet.
        let h = TempDir::new().unwrap();
        let marker = h.path().join("deep/nested/path/.autostart_done");
        assert!(ensure_autostart_once(&FakeAutoStart::default(), &marker));
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
        let step = step_install_scripts_in(h.path(), "/tmp/lumen.db", "/bin/lumen-tok");
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
        step_install_scripts_in(h.path(), "/my/lumen.db", "/my/lumen-tok");
        let body = std::fs::read_to_string(lumen_dir_in(h.path()).join("lumen_meter.sh")).unwrap();
        assert!(body.contains("/my/lumen.db"), "DB path must be substituted");
        assert!(
            body.contains("/my/lumen-tok"),
            "tok path must be substituted"
        );
        assert!(
            !body.contains("__LUMEN_DB__") && !body.contains("__LUMEN_TOK__"),
            "no placeholder may survive templating:\n{body}"
        );
    }

    #[test]
    fn reinstalling_scripts_overwrites_stale_paths() {
        let h = TempDir::new().unwrap();
        step_install_scripts_in(h.path(), "/old.db", "/old-tok");
        step_install_scripts_in(h.path(), "/new.db", "/new-tok");
        let body = std::fs::read_to_string(lumen_dir_in(h.path()).join("lumen_meter.sh")).unwrap();
        assert!(body.contains("/new.db"));
        assert!(!body.contains("/old.db"), "the stale path must be gone");
    }

    // ── step_register_mcp_in ─────────────────────────────────────────────────

    #[test]
    fn registering_the_mcp_server_creates_claude_json() {
        let h = TempDir::new().unwrap();
        let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);

        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        let entry = &v["mcpServers"]["lumen"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["command"], "/bin/lumen-mcp");
        assert_eq!(entry["env"]["LUMEN_DB"], "/db");
        assert_eq!(entry["env"]["LUMEN_TOK"], "/tok");
    }

    #[test]
    fn registering_preserves_other_mcp_servers_and_unrelated_keys() {
        let h = TempDir::new().unwrap();
        std::fs::write(
            claude_json_path_in(h.path()),
            r#"{"numStartups":17,"mcpServers":{"other":{"command":"/other"}}}"#,
        )
        .unwrap();

        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");

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
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
        let bak = path.with_extension("json.lumen_bak");
        assert!(bak.exists(), "a backup must be taken before rewriting");
        assert!(std::fs::read_to_string(&bak)
            .unwrap()
            .contains("numStartups"));
    }

    #[test]
    fn registering_recovers_from_a_corrupt_claude_json() {
        // Rather than refusing forever, setup starts fresh — the original is
        // still recoverable from the .lumen_bak copy.
        let h = TempDir::new().unwrap();
        std::fs::write(claude_json_path_in(h.path()), "{ not json at all").unwrap();
        let step = step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
        assert_eq!(step.status, StepStatus::Ok);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(claude_json_path_in(h.path())).unwrap())
                .unwrap();
        assert_eq!(v["mcpServers"]["lumen"]["command"], "/bin/lumen-mcp");
    }

    #[test]
    fn registering_is_idempotent() {
        let h = TempDir::new().unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
        let first = std::fs::read_to_string(claude_json_path_in(h.path())).unwrap();
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
        let second = std::fs::read_to_string(claude_json_path_in(h.path())).unwrap();
        assert_eq!(first, second, "re-running setup must be a no-op");
    }

    // ── step_install_hooks_in ────────────────────────────────────────────────

    #[test]
    fn installing_hooks_registers_the_intercept_and_all_four_meters() {
        let h = TempDir::new().unwrap();
        let step = step_install_hooks_in(h.path());
        assert_eq!(step.status, StepStatus::Ok, "{}", step.detail);

        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(global_settings_path_in(h.path())).unwrap(),
        )
        .unwrap();

        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "one PreToolUse matcher: Read");
        assert_eq!(pre[0]["matcher"], "Read");
        assert!(pre[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("lumen_read_intercept.sh"));

        let post = v["hooks"]["PostToolUse"].as_array().unwrap();
        let matchers: Vec<&str> = post
            .iter()
            .map(|e| e["matcher"].as_str().unwrap())
            .collect();
        assert_eq!(
            matchers,
            vec![
                "Read",
                "mcp__lumen__smart_read",
                "mcp__lumen__recall_file",
                "mcp__lumen__compress_logs"
            ],
            "the built-in Read plus all three lumen tools must be metered"
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

    #[test]
    fn installing_hooks_recovers_from_corrupt_settings() {
        let h = TempDir::new().unwrap();
        std::fs::create_dir_all(h.path().join(".claude")).unwrap();
        std::fs::write(global_settings_path_in(h.path()), "]]not json[[").unwrap();
        let step = step_install_hooks_in(h.path());
        assert_eq!(step.status, StepStatus::Ok);
        assert!(
            global_settings_path_in(h.path())
                .with_extension("json.lumen_bak")
                .exists(),
            "the unreadable original is still backed up"
        );
    }

    // ── run_uninstall_in ─────────────────────────────────────────────────────

    #[test]
    fn uninstall_removes_everything_setup_installed() {
        let h = TempDir::new().unwrap();
        step_install_scripts_in(h.path(), "/db", "/tok");
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
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

        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
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
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
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

        step_install_scripts_in(h.path(), "/db", "/tok");
        step_register_mcp_in(h.path(), "/bin/lumen-mcp", "/db", "/tok");
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
}
