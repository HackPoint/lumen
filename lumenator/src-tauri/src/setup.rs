#![allow(clippy::unnecessary_map_or)] // map_or style kept for clarity in async context
use serde::Serialize;
use std::path::PathBuf;
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

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn lumen_dir() -> PathBuf {
    home().join(".claude/lumen")
}

fn marker_path() -> PathBuf {
    lumen_dir().join(".setup_done")
}

fn claude_json_path() -> PathBuf {
    home().join(".claude.json")
}

fn global_settings_path() -> PathBuf {
    home().join(".claude/settings.json")
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
    let mcp_bin = find_binary("lumen-mcp");
    let tok_bin = find_binary("lumen-tok");

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
                if let Some(mcp) = v["mcpServers"].as_object_mut() {
                    mcp.remove("lumen");
                }
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

fn remove_lumen_hooks(root: &mut serde_json::Value) {
    for phase in &["PreToolUse", "PostToolUse"] {
        if let Some(arr) = root["hooks"][phase].as_array_mut() {
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
