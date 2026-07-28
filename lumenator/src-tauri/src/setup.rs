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
pub fn lumen_run_setup(_app: AppHandle) -> Vec<SetupStep> {
    run_setup()
}

/// Remove all Lumen configuration from ~/.claude/. Returns one entry per action.
#[tauri::command]
pub fn lumen_uninstall() -> Vec<SetupStep> {
    run_uninstall()
}

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

fn claude_json_path() -> PathBuf {
    claude_json_path_in(&home())
}

fn global_settings_path() -> PathBuf {
    global_settings_path_in(&home())
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

fn app_support_dir_in(home: &Path) -> PathBuf {
    home.join("Library/Application Support/io.speedata.lumen")
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
    std::env::var("LUMEN_DB").unwrap_or_else(|_| {
        home()
            .join("Library/Application Support/io.speedata.lumen/lumen.db")
            .to_string_lossy()
            .to_string()
    })
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
    let dir = lumen_dir();
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
    let path = claude_json_path();

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
    let dir = lumen_dir();
    let meter = dir.join("lumen_meter.sh").to_string_lossy().to_string();
    let intercept = dir
        .join("lumen_read_intercept.sh")
        .to_string_lossy()
        .to_string();

    let path = global_settings_path();

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

fn run_setup() -> Vec<SetupStep> {
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

    // 6. Write marker on full success
    let all_good = steps
        .iter()
        .all(|s| s.status == StepStatus::Ok || s.status == StepStatus::Warn);
    if all_good {
        let _ = std::fs::create_dir_all(lumen_dir());
        let _ = std::fs::write(marker_path(), "");
    }

    steps
}

fn run_uninstall() -> Vec<SetupStep> {
    let mut steps = Vec::new();

    // Remove MCP entry from ~/.claude.json
    let claude_json = claude_json_path();
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
    let settings = global_settings_path();
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
    let dir = lumen_dir();
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
        assert_eq!(
            app_support_dir_in(h),
            Path::new("/tmp/fake-home/Library/Application Support/io.speedata.lumen")
        );
    }

    #[test]
    fn the_real_home_helpers_agree_with_the_parameterised_ones() {
        // Guards against the wrappers drifting from the *_in functions.
        let h = home();
        assert_eq!(lumen_dir(), lumen_dir_in(&h));
        assert_eq!(marker_path(), marker_path_in(&h));
        assert_eq!(claude_json_path(), claude_json_path_in(&h));
        assert_eq!(global_settings_path(), global_settings_path_in(&h));
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
