//! `lumen doctor` — everything a maintainer would otherwise ask for, in one paste.
//!
//! Written because diagnosing issue #5 took a round-trip per fact, and the first thing asked
//! for did not even exist: the app's logger was registered under `cfg!(debug_assertions)`, so
//! the log file named in the CHANGELOG is never created in a released build.
//!
//! The report is built from an injected `Facts` struct rather than read inline, so the
//! formatting and — more importantly — the *verdict* are unit-testable. Whether Lumen decides
//! "your menu-bar icon was dragged off the bar" is exactly the judgement that needs a test;
//! collecting a `defaults` value does not.

use std::fmt::Write as _;

/// A `NSStatusItem …` preference key and its value, as `defaults` would print it.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusItemPref {
    pub domain: String,
    pub key: String,
    /// `None` when the key exists but is not a boolean (a position, say).
    pub visible: Option<bool>,
}

/// Everything collected about the machine. Injected so the verdict can be tested.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub platform: String,
    pub cli_version: String,
    pub app_version: Option<String>,
    /// Every `NSStatusItem *` key found, across every domain checked.
    pub status_item_prefs: Vec<StatusItemPref>,
    /// Process names matching Lumen, as reported by the OS.
    pub processes: Vec<String>,
    /// Menu-bar managers that are **running**. Only these can be hiding a status item.
    pub menu_bar_managers: Vec<String>,
    /// Menu-bar managers present in /Applications but not running. Context, never a finding —
    /// reporting one as a cause sends the reader after something that cannot be responsible.
    pub menu_bar_managers_installed: Vec<String>,
    pub log_path: Option<String>,
    /// `None` when the log file does not exist.
    pub log_size_bytes: Option<u64>,
    pub db_path: Option<String>,
    pub db_size_bytes: Option<u64>,
    /// `(x, y, w, h)` per monitor.
    pub monitors: Vec<(i64, i64, i64, i64)>,
    pub launch_agent: Option<String>,
    pub launch_agent_target_exists: Option<bool>,
    /// The Lumen tools that have no PreToolUse hook in `~/.claude/settings.json`: the hook is
    /// how the server learns which conversation a call belongs to. `None` when settings.json
    /// could not be read as JSON; a plugin install keeps its hooks elsewhere, so absence from
    /// this file alone is not proof, and the ledger's own account (below) can overrule it.
    pub identity_hook_missing: Option<Vec<String>>,
    /// Of the newest calls to a Lumen tool that carry a repeat plan: how many were answered as
    /// `unidentified`, and how many there were in all. `None` when there is no ledger to read.
    pub recent_repeat_plans: Option<(u64, u64)>,
    /// Whether repeat reads are on, as the server would decide it from this machine: the
    /// environment (or Claude Code's `env` block in settings.json, which is where a server it
    /// starts gets it from), then the setting file, then on. `None` when it could not be worked
    /// out, which is read as the default, on.
    pub repeat_reads: Option<lumen_core::settings::Resolved>,
    /// Where the setting file is, for the report and for a finding about it.
    pub repeat_reads_file: Option<String>,
    /// `LUMEN_REPEAT_READS` was taken from Claude Code's `env` block in settings.json, not from
    /// this shell.
    pub repeat_reads_env_from_claude_settings: bool,
}

/// What doctor concluded, most actionable first.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    /// A `NSStatusItem Visible …` or `NSStatusItem VisibleCC …` key is false: the icon was
    /// ⌘-dragged off the menu bar and macOS remembers it. The cause of the reported symptom, and
    /// it has a one-line fix.
    IconHiddenByPreference { domain: String, key: String },
    /// A **running** menu-bar manager, which can hide the icon into an overflow area.
    MenuBarManager { name: String },
    /// Lumen is not running at all, so no icon is expected.
    NotRunning,
    /// The app never wrote a log. Expected on 1.5.1 and earlier — the logger was debug-only.
    NoLog,
    /// A login item points at a path that no longer exists.
    StaleLoginItem { path: String },
    /// The Lumen tools cannot tell which conversation a call is from, so every `smart_read`,
    /// `recall_file` and `compress_logs` is answered in full and repeat reads save nothing.
    /// `missing` is what settings.json lacks; `unidentified` is the ledger's own count.
    RepeatReadsInert {
        missing: Vec<String>,
        unidentified: Option<(u64, u64)>,
    },
    /// The repeat-reads switch holds something that cannot be read as on or off, so repeat
    /// reads are off and every read is in full. Turned off on purpose is not a finding; this
    /// is the off nobody chose.
    RepeatReadsSetting {
        problem: lumen_core::settings::Problem,
        file: String,
    },
}

impl Finding {
    /// The remedy, or an explanation when there is not one.
    pub fn remedy(&self) -> String {
        match self {
            Finding::IconHiddenByPreference { domain, key } => {
                format!("defaults delete {domain} \"{key}\" && killall Lumen; open -a Lumen")
            }
            Finding::MenuBarManager { name } => {
                format!("check {name}'s hidden/overflow section before assuming Lumen failed")
            }
            Finding::NotRunning => "open -a Lumen".to_string(),
            Finding::NoLog => {
                "expected on 1.5.1 and earlier: the logger was registered only in debug builds, \
                 so no log was ever written. Upgrade for a real log."
                    .to_string()
            }
            Finding::StaleLoginItem { .. } => {
                "launchctl bootout gui/$(id -u)/Lumen; rm -f ~/Library/LaunchAgents/Lumen.plist"
                    .to_string()
            }
            Finding::RepeatReadsInert { .. } => INERT_REMEDY.to_string(),
            Finding::RepeatReadsSetting { problem, file } => match problem {
                lumen_core::settings::Problem::Env(_) => format!(
                    "set LUMEN_REPEAT_READS to on or off, or unset it (check your shell profile \
                     and the \"env\" block of ~/.claude/settings.json), then restart Claude Code. \
                     To keep the setting file ({file}) in charge, unset the variable."
                ),
                lumen_core::settings::Problem::File(_) => format!(
                    "fix {file} so it reads {{\"repeat_reads\": true}} (or false), or delete it \
                     to get the default, on. In the Lumen app, the Repeat reads switch in the \
                     Home window rewrites it. It applies to the next Lumen call, with no restart."
                ),
            },
        }
    }
}

/// What to do when the PreToolUse hook that tells the server which conversation a call is from
/// is missing. It depends on how Lumen was installed, and only the person at the keyboard knows
/// that, so each way is named. The app does not edit `~/.claude/settings.json` when it
/// launches; Setup does, and Setup runs on its own only while its marker file is missing,
/// which is why the first fix removes the marker.
const INERT_REMEDY: &str = "how to fix it depends on how you installed Lumen. \
     (1) Lumen app (Homebrew cask or .dmg): quit Lumen, run `rm ~/.claude/lumen/.setup_done`, \
     open Lumen again - it opens on Setup and registers the hook - then restart Claude Code. \
     (2) Claude Code plugin: update or reinstall the plugin (its hooks come with it), then \
     restart Claude Code. \
     (3) Homebrew `lumen-cli` only: that installs the `lumen` command and nothing else, no MCP \
     server and no hooks, so there are no repeat reads to fix - install the Lumen app or the \
     plugin to get them. To turn repeat reads off instead, set LUMEN_REPEAT_READS=off";

/// Whether a hook `matcher` selects `tool_name`. Claude Code reads a matcher made only of
/// letters, digits, `_` and `|` as an exact name or a list of them, and anything else as a
/// regular expression; the two spellings of "everything" and a trailing `.*` are the only
/// regular expressions worth recognising here, since a wrongly raised finding sends the
/// reader after something that is not broken.
fn matcher_selects(matcher: &str, tool_name: &str) -> bool {
    let m = matcher.trim();
    if m.is_empty() || m == "*" || m == ".*" {
        return true;
    }
    if let Some(prefix) = m.strip_suffix(".*") {
        return tool_name.starts_with(prefix);
    }
    m.split('|').any(|name| name == tool_name)
}

/// The Lumen tools no PreToolUse hook in `settings` selects, as their short names.
pub fn identity_hook_missing(settings: &serde_json::Value) -> Vec<String> {
    // No `hooks.PreToolUse` array is no entries: every Lumen tool is then reported as unhooked.
    let none = Vec::new();
    let entries = settings["hooks"]["PreToolUse"].as_array().unwrap_or(&none);
    lumen_core::identity::TOOLS
        .iter()
        .filter(|tool| {
            let full = format!("{}{tool}", lumen_core::identity::TOOL_PREFIX);
            !entries.iter().any(|e| {
                e["hooks"].as_array().is_some_and(|h| !h.is_empty())
                    && matcher_selects(e["matcher"].as_str().unwrap_or(""), &full)
            })
        })
        .map(|tool| tool.to_string())
        .collect()
}

/// Decide what is wrong, most actionable first.
///
/// Ordering is the point: a hidden-by-preference icon explains the whole symptom and has a
/// one-line fix, so it must never be buried under a note about log sizes.
pub fn findings(f: &Facts) -> Vec<Finding> {
    let mut out = Vec::new();

    for p in &f.status_item_prefs {
        if is_visibility_key(&p.key) && p.visible == Some(false) {
            out.push(Finding::IconHiddenByPreference {
                domain: p.domain.clone(),
                key: p.key.clone(),
            });
        }
    }

    for m in &f.menu_bar_managers {
        out.push(Finding::MenuBarManager { name: m.clone() });
    }

    if f.processes.is_empty() {
        out.push(Finding::NotRunning);
    }

    // Only worth reporting while the app is running: a log that was never written by a process
    // that was never started says nothing.
    if !f.processes.is_empty() && f.log_size_bytes.is_none() {
        out.push(Finding::NoLog);
    }

    if f.launch_agent_target_exists == Some(false) {
        out.push(Finding::StaleLoginItem {
            path: f.launch_agent.clone().unwrap_or_default(),
        });
    }

    if let Some(r) = &f.repeat_reads
        && let Some(problem) = &r.problem
    {
        out.push(Finding::RepeatReadsSetting {
            problem: problem.clone(),
            file: f
                .repeat_reads_file
                .clone()
                .unwrap_or_else(|| lumen_core::settings::FILE_NAME.to_string()),
        });
    }

    if let Some(finding) = repeat_reads_inert(f) {
        out.push(finding);
    }

    out
}

/// Whether repeat reads are off on this machine, for whatever reason. Unknown is not off: the
/// default is on, and a doctor that stays quiet because it could not tell would hide the one
/// thing it is for.
fn repeat_reads_off(f: &Facts) -> bool {
    f.repeat_reads
        .as_ref()
        .is_some_and(|r| r.mode == lumen_core::ranked::Mode::Off)
}

/// Whether repeat reads cannot work for want of the identity hook.
///
/// Silent when repeat reads are off: then nothing is stubbed, nothing needs to know which
/// conversation a call is from, and a missing hook costs only the ledger's session column. Off
/// by a setting nobody could read is reported on its own (`RepeatReadsSetting`).
///
/// Two accounts of it, and the ledger's wins. settings.json lacking the hook is a reason to
/// suspect, but a plugin install registers its hooks from the plugin, so the file alone would
/// accuse a healthy machine. What the server has actually done is not in doubt: if every recent
/// call that carried a plan was answered `unidentified`, repeat reads are inert whatever the
/// file says; if any was not, the hook is reaching the server and a missing entry here is the
/// plugin's doing.
fn repeat_reads_inert(f: &Facts) -> Option<Finding> {
    if repeat_reads_off(f) {
        return None;
    }
    // settings.json unreadable (`None`) accuses nothing: only the ledger can then raise the finding.
    let missing: Vec<String> = match &f.identity_hook_missing {
        Some(m) => m.clone(),
        None => Vec::new(),
    };
    let seen = f.recent_repeat_plans;
    let every_call_unidentified = seen.is_some_and(|(u, all)| all > 0 && u == all);
    let some_call_identified = seen.is_some_and(|(u, all)| u < all);
    if every_call_unidentified || (!missing.is_empty() && !some_call_identified) {
        return Some(Finding::RepeatReadsInert {
            missing,
            unidentified: seen,
        });
    }
    None
}

fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// Render the report. Pure, so the whole thing is snapshot-testable.
pub fn render(f: &Facts) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "Lumen doctor");
    let _ = writeln!(s, "============");
    let _ = writeln!(s);
    let _ = writeln!(s, "platform      {}", f.platform);
    let _ = writeln!(s, "cli           {}", f.cli_version);
    let _ = writeln!(
        s,
        "app           {}",
        f.app_version.as_deref().unwrap_or("not found")
    );
    let _ = writeln!(s);

    let _ = writeln!(s, "processes");
    if f.processes.is_empty() {
        let _ = writeln!(s, "  (none — Lumen is not running)");
    } else {
        for p in &f.processes {
            let _ = writeln!(s, "  {p}");
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "menu-bar status item");
    if f.status_item_prefs.is_empty() {
        let _ = writeln!(
            s,
            "  (no NSStatusItem preferences — nothing is suppressing it)"
        );
    } else {
        for p in &f.status_item_prefs {
            let v = match p.visible {
                Some(true) => "true",
                Some(false) => "FALSE  <-- hidden",
                None => "(not a boolean)",
            };
            let _ = writeln!(s, "  [{}] {} = {}", p.domain, p.key, v);
        }
    }
    if !f.menu_bar_managers.is_empty() {
        let _ = writeln!(s, "  managers RUNNING: {}", f.menu_bar_managers.join(", "));
    }
    if !f.menu_bar_managers_installed.is_empty() {
        let _ = writeln!(
            s,
            "  managers installed but not running: {}  (cannot be hiding it)",
            f.menu_bar_managers_installed.join(", ")
        );
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "monitors");
    if f.monitors.is_empty() {
        let _ = writeln!(
            s,
            "  (not available from the CLI — needs the app; see docs/troubleshooting-the-tray.md step 3)"
        );
    } else {
        for (x, y, w, h) in &f.monitors {
            let _ = writeln!(s, "  {w}x{h} at ({x},{y})");
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "files");
    let _ = writeln!(
        s,
        "  log   {}  {}",
        f.log_path.as_deref().unwrap_or("(unknown path)"),
        match f.log_size_bytes {
            Some(n) => human_bytes(n),
            None => "MISSING".to_string(),
        }
    );
    let _ = writeln!(
        s,
        "  db    {}  {}",
        f.db_path.as_deref().unwrap_or("(unknown path)"),
        match f.db_size_bytes {
            Some(n) => human_bytes(n),
            None => "MISSING".to_string(),
        }
    );
    if let Some(agent) = &f.launch_agent {
        let _ = writeln!(
            s,
            "  agent {agent}  target {}",
            match f.launch_agent_target_exists {
                Some(true) => "exists",
                Some(false) => "MISSING",
                None => "unknown",
            }
        );
    }
    let _ = writeln!(s);

    let _ = writeln!(
        s,
        "repeat reads (on by default; which conversation a call is from)"
    );
    let _ = writeln!(
        s,
        "  state                   {}",
        repeat_reads_state_line(f)
    );
    if let Some(file) = &f.repeat_reads_file {
        let _ = writeln!(s, "  setting file            {file}");
    }
    match &f.identity_hook_missing {
        None => {
            let _ = writeln!(s, "  hook in settings.json   (settings.json not readable)");
        }
        Some(m) if m.is_empty() => {
            let _ = writeln!(
                s,
                "  hook in settings.json   registered for all three tools"
            );
        }
        Some(m) => {
            let _ = writeln!(s, "  hook in settings.json   MISSING for {}", m.join(", "));
        }
    }
    match f.recent_repeat_plans {
        None => {
            let _ = writeln!(s, "  recent calls            (ledger not readable)");
        }
        Some((_, 0)) => {
            let _ = writeln!(s, "  recent calls            none with a repeat plan yet");
        }
        Some((u, all)) => {
            let _ = writeln!(
                s,
                "  recent calls            {u} of the last {all} were answered in full for want of an identity"
            );
        }
    }
    let _ = writeln!(s);

    let found = findings(f);
    let _ = writeln!(s, "findings");
    if found.is_empty() {
        let _ = writeln!(s, "  nothing obviously wrong.");
        let _ = writeln!(
            s,
            "  if the menu-bar icon is missing anyway, the next step is in"
        );
        let _ = writeln!(s, "  docs/troubleshooting-the-tray.md");
    } else {
        for (i, x) in found.iter().enumerate() {
            let _ = writeln!(s, "  {}. {}", i + 1, describe(x));
            let _ = writeln!(s, "     fix: {}", x.remedy());
        }
    }
    s
}

fn repeat_reads_state_line(f: &Facts) -> String {
    use lumen_core::ranked::Mode;
    use lumen_core::settings::Source;
    let Some(r) = &f.repeat_reads else {
        return "(could not be worked out; the default is on)".to_string();
    };
    let env = if f.repeat_reads_env_from_claude_settings {
        "LUMEN_REPEAT_READS in ~/.claude/settings.json"
    } else {
        "LUMEN_REPEAT_READS"
    };
    let word = match r.mode {
        Mode::On => "on",
        Mode::Off => "OFF",
        Mode::Ab => "A/B experiment",
    };
    let why = match (&r.problem, r.source) {
        (Some(_), _) => {
            "turned off because the setting could not be read (see findings)".to_string()
        }
        (None, Source::Default) => "the default".to_string(),
        (None, Source::File) => "set by the setting file".to_string(),
        (None, Source::Env) => format!("set by {env}"),
    };
    format!("{word}  ({why})")
}

fn describe(x: &Finding) -> String {
    match x {
        Finding::IconHiddenByPreference { domain, .. } => format!(
            "The menu-bar icon is hidden by a saved preference in {domain}. macOS writes this \
             when an icon is ⌘-dragged off the menu bar, and it persists across launches — the \
             icon is created and then immediately hidden, so nothing appears to fail."
        ),
        Finding::MenuBarManager { name } => {
            format!("{name} is running and may be holding the icon in an overflow area.")
        }
        Finding::NotRunning => "Lumen is not running, so there would be no icon.".to_string(),
        Finding::NoLog => "Lumen is running but has written no log file.".to_string(),
        Finding::StaleLoginItem { path } => {
            format!("A login item at {path} points at an app that no longer exists.")
        }
        Finding::RepeatReadsSetting { problem, file } => {
            lumen_core::settings::Problem::describe(problem, std::path::Path::new(file))
        }
        Finding::RepeatReadsInert {
            missing,
            unidentified,
        } => {
            let mut why = String::new();
            if !missing.is_empty() {
                why.push_str(&format!(
                    " settings.json has no PreToolUse hook for {}.",
                    missing.join(", ")
                ));
            }
            if let Some((u, all)) = unidentified.filter(|(u, _)| *u > 0) {
                why.push_str(&format!(
                    " {u} of the last {all} calls were answered in full for want of an identity."
                ));
            }
            format!(
                "Repeat reads are inert: without the hook, smart_read, recall_file and \
                 compress_logs cannot tell which conversation a call is from, so every result \
                 is sent in full (the safe answer) and the ledger's session column stays \
                 empty.{why}"
            )
        }
    }
}

// ── Collection ────────────────────────────────────────────────────────────────
//
// Deliberately below the tests: this half talks to the OS and cannot be unit-tested, so it is
// kept as thin as possible and every judgement lives in `findings` above.

/// The preference domains Lumen has written status-item state under. Both are real — this
/// machine has `NSStatusItem Preferred Position Item-0` in each, with different values.
#[cfg(target_os = "macos")]
pub const PREF_DOMAINS: [&str; 2] = ["io.speedata.lumen", "Lumen"];

#[cfg(target_os = "macos")]
const MENU_BAR_MANAGERS: [&str; 7] = [
    "Bartender",
    "Ice",
    "Hidden Bar",
    "Dozer",
    "Vanilla",
    "TopNotch",
    "Barbee",
];

#[cfg(target_os = "macos")]
fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Whether a preference key is a status item's visibility flag. macOS writes it in two forms,
/// `NSStatusItem Visible <autosave>` and, under Control Center on current macOS,
/// `NSStatusItem VisibleCC <autosave>`; a match on "Visible " with the space missed the second.
/// Without the space it catches both, and still not `NSStatusItem Preferred Position …`, which
/// is where the item sat rather than whether it shows. The app's `health::is_visibility_key`
/// matches the same way.
fn is_visibility_key(key: &str) -> bool {
    key.starts_with("NSStatusItem Visible")
}

/// One line of `defaults read <domain>`, if it holds a status-item key: `"<key>" = <value>;`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_pref_line(domain: &str, line: &str) -> Option<StatusItemPref> {
    let line = line.trim();
    if !line.contains("NSStatusItem") {
        return None;
    }
    let (raw_key, raw_val) = line.split_once('=')?;
    let key = raw_key.trim().trim_matches('"').to_string();
    let val = raw_val.trim().trim_end_matches(';').trim();
    let visible = if is_visibility_key(&key) {
        match val {
            "0" | "false" | "NO" => Some(false),
            "1" | "true" | "YES" => Some(true),
            _ => None,
        }
    } else {
        None
    };
    Some(StatusItemPref {
        domain: domain.to_string(),
        key,
        visible,
    })
}

/// `~/.claude/settings.json` as JSON; `None` when it is absent or is not JSON. Setup writes the
/// hooks there, and Setup is where they are repaired.
fn claude_settings() -> Option<serde_json::Value> {
    let path = dirs::home_dir()?.join(".claude").join("settings.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `LUMEN_REPEAT_READS` in the `env` block of Claude Code's settings.json: Claude Code puts that
/// block in the environment of the servers it starts, so it is where a user who wants repeat
/// reads off for Claude Code alone would put it, and a terminal running doctor cannot see it.
pub fn repeat_reads_env_in(settings: &serde_json::Value) -> Option<String> {
    let v = settings
        .get("env")?
        .get(lumen_core::settings::REPEAT_READS_ENV)?;
    Some(
        v.as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
    )
}

/// Decide the way the server would: the shell's environment first, then Claude Code's `env`
/// block, then the setting file, then on. The bool says the value came from the `env` block.
pub fn decide_repeat_reads(
    process_env: Option<String>,
    claude_env: Option<String>,
    file: &lumen_core::settings::FileState,
) -> (lumen_core::settings::Resolved, bool) {
    let says_something = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
    let (env, from_claude) = if says_something(&process_env) {
        (process_env, false)
    } else if says_something(&claude_env) {
        (claude_env, true)
    } else {
        (None, false)
    };
    (
        lumen_core::settings::resolve(env.as_deref(), file),
        from_claude,
    )
}

/// Of the newest calls to a Lumen tool that carry a repeat plan, `(unidentified, all)`. Read
/// only, and `None` when the ledger cannot be opened or has no such column (a database from
/// before 1.7.0), which the report says rather than reading as "none".
fn recent_repeat_plans(db: &std::path::Path) -> Option<(u64, u64)> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let (unidentified, all): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(repeat_plan = ?1), 0), COUNT(*) FROM (
                 SELECT repeat_plan FROM read_events
                 WHERE routed_via IN ('smart_read', 'recall_file', 'compress_logs')
                   AND repeat_plan IS NOT NULL
                 ORDER BY rowid DESC LIMIT 50)",
            [lumen_core::repeat::PLAN_UNIDENTIFIED],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()?;
    Some((u64::try_from(unidentified).ok()?, u64::try_from(all).ok()?))
}

/// Collect what we can. Every field is optional because doctor's whole purpose is to run on a
/// machine where things are missing.
pub fn collect() -> Facts {
    let mut f = Facts {
        platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        ..Default::default()
    };

    if let Some(db) = lumen_core::meter::db_path() {
        f.db_size_bytes = std::fs::metadata(&db).ok().map(|m| m.len());
        f.db_path = Some(db.to_string_lossy().to_string());
        f.recent_repeat_plans = recent_repeat_plans(&db);
    }
    let settings = claude_settings();
    f.identity_hook_missing = settings.as_ref().map(identity_hook_missing);

    // The shell's variable may not be unicode; it is then a value that means nothing, which the
    // decision reports, rather than an absent one.
    let process_env = match std::env::var(lumen_core::settings::REPEAT_READS_ENV) {
        Ok(v) => Some(v),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(raw)) => Some(raw.to_string_lossy().into_owned()),
    };
    let db = lumen_core::meter::db_path();
    let file = db
        .as_deref()
        .map(lumen_core::settings::settings_path)
        .map(|p| (lumen_core::settings::read_file(&p), p));
    let (state, from_claude) = decide_repeat_reads(
        process_env,
        settings.as_ref().and_then(repeat_reads_env_in),
        file.as_ref()
            .map(|(s, _)| s)
            .unwrap_or(&lumen_core::settings::FileState::Absent),
    );
    f.repeat_reads = Some(state);
    f.repeat_reads_env_from_claude_settings = from_claude;
    f.repeat_reads_file = file.map(|(_, p)| p.to_string_lossy().to_string());

    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().unwrap_or_default();

        // `defaults read <domain>` prints a plist; parsing the two keys we care about out of it
        // is enough, and far less machinery than linking CoreFoundation into the CLI.
        for domain in PREF_DOMAINS {
            let Some(text) = sh("defaults", &["read", domain]) else {
                continue;
            };
            f.status_item_prefs
                .extend(text.lines().filter_map(|l| parse_pref_line(domain, l)));
        }

        if let Some(ps) = sh("/bin/ps", &["-ax", "-o", "comm"]) {
            f.processes = ps
                .lines()
                .filter(|l| l.contains("Lumen.app") || l.contains("lumen-daemon"))
                .map(|l| l.trim().to_string())
                .collect();
            // Matched against each process's own path, not as a substring of the whole ps output.
            //
            // The old code did `ps.to_lowercase().contains("ice")`, and **"ice" is a substring of
            // "Services"** — so it matched `CoreServices`, `XPCServices`, `fseventsd` and 127 other
            // lines on a stock macOS install. `lumen doctor` therefore reported Ice as a present
            // menu-bar manager on a machine where Ice has never been installed, and would have
            // done so on essentially any Mac. Any short manager name has the same exposure.
            //
            // This was the top and only finding in the output, and it nearly went out in a comment
            // on issue #5 as evidence. A diagnostic that invents a cause is worse than no
            // diagnostic: it costs the reporter a round-trip chasing something that is not there.
            for m in MENU_BAR_MANAGERS {
                let needle = format!("/{m}.app/");
                if ps.lines().any(|l| l.contains(&needle)) {
                    f.menu_bar_managers.push(m.to_string());
                }
            }
        }
        // Installed but not running is context, not a cause: it cannot hide anything. Kept
        // separate so the two claims stay distinguishable in the output.
        for m in MENU_BAR_MANAGERS {
            let p = format!("/Applications/{m}.app");
            if std::path::Path::new(&p).exists() && !f.menu_bar_managers.contains(&m.to_string()) {
                f.menu_bar_managers_installed.push(m.to_string());
            }
        }

        let log = home.join("Library/Logs/io.speedata.lumen/Lumen.log");
        f.log_size_bytes = std::fs::metadata(&log).ok().map(|m| m.len());
        f.log_path = Some(log.to_string_lossy().to_string());

        let app = std::path::Path::new("/Applications/Lumen.app");
        if app.exists() {
            f.app_version = sh(
                "/usr/bin/defaults",
                &[
                    "read",
                    "/Applications/Lumen.app/Contents/Info.plist",
                    "CFBundleShortVersionString",
                ],
            )
            .map(|s| s.trim().to_string());
        }

        // The label is `Lumen`, not the bundle id — the distinction that made every cask
        // uninstall leave a login item behind.
        let agent = home.join("Library/LaunchAgents/Lumen.plist");
        if agent.exists() {
            f.launch_agent = Some(agent.to_string_lossy().to_string());
            f.launch_agent_target_exists = sh("/usr/bin/plutil", &["-p", &agent.to_string_lossy()])
                .map(|txt| {
                    txt.split('"')
                        .find(|s| s.contains("Lumen.app"))
                        .map(|p| std::path::Path::new(p).exists())
                        .unwrap_or(true)
                });
        }
    }

    f
}

/// Ask a running Lumen to show its window.
///
/// macOS only by design. There is no single-instance plugin, so launching the app elsewhere
/// starts a *second* GUI whose daemon cannot bind 9999 — the version-skew failure the daemon
/// documents at length. Printing an instruction is better than causing that.
pub fn show() -> i32 {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("/usr/bin/open")
            .args(["-b", "io.speedata.lumen"])
            .status()
        {
            Ok(s) if s.success() => {
                println!("Asked Lumen to show its window.");
                println!("If nothing appeared, Lumen may not be running: open -a Lumen");
                0
            }
            Ok(s) => {
                eprintln!("open exited {s}. Is Lumen installed in /Applications?");
                1
            }
            Err(e) => {
                eprintln!("could not run open: {e}");
                1
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "`lumen show` is macOS-only.\n\
             On this platform, launching Lumen again would start a second instance whose daemon\n\
             cannot bind 127.0.0.1:9999, leaving the UI reading from the wrong process.\n\
             Start it from your launcher instead."
        );
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Facts {
        Facts {
            platform: "macos aarch64".into(),
            cli_version: "1.5.2".into(),
            app_version: Some("1.5.2".into()),
            processes: vec!["Lumen".into(), "lumen-daemon".into()],
            log_path: Some("/tmp/Lumen.log".into()),
            log_size_bytes: Some(2048),
            db_path: Some("/tmp/lumen.db".into()),
            db_size_bytes: Some(18 * 1024 * 1024),
            monitors: vec![(0, 0, 1920, 1080)],
            ..Default::default()
        }
    }

    #[test]
    fn a_healthy_machine_reports_nothing_wrong() {
        assert!(findings(&healthy()).is_empty());
        let out = render(&healthy());
        assert!(out.contains("nothing obviously wrong"), "{out}");
    }

    #[test]
    fn the_hidden_icon_preference_is_found_and_comes_first() {
        // The issue #5 case. It must be finding #1: it explains the entire symptom and the fix
        // is one line, so burying it under anything else wastes the round-trip.
        let mut f = healthy();
        f.menu_bar_managers = vec!["Bartender".into()];
        f.status_item_prefs = vec![
            StatusItemPref {
                domain: "io.speedata.lumen".into(),
                key: "NSStatusItem Preferred Position Item-0".into(),
                visible: None,
            },
            StatusItemPref {
                domain: "io.speedata.lumen".into(),
                key: "NSStatusItem Visible Item-0".into(),
                visible: Some(false),
            },
        ];
        let found = findings(&f);
        assert!(
            matches!(found[0], Finding::IconHiddenByPreference { .. }),
            "{found:?}"
        );
        assert!(found[0].remedy().contains("defaults delete"));
        assert!(found[0].remedy().contains("NSStatusItem Visible Item-0"));
    }

    #[test]
    fn the_control_center_form_of_the_key_is_found_too() {
        // `NSStatusItem VisibleCC <autosave>` is the form macOS writes under Control Center.
        // Lumen's own domain once held it false, and a match on "Visible " with a trailing
        // space never saw it, so doctor reported nothing wrong on a machine hiding the icon.
        let pref = |visible| StatusItemPref {
            domain: "io.speedata.lumen".into(),
            key: "NSStatusItem VisibleCC Item-0".into(),
            visible,
        };
        let mut f = healthy();
        f.status_item_prefs = vec![pref(Some(false))];
        let found = findings(&f);
        assert_eq!(
            found,
            [Finding::IconHiddenByPreference {
                domain: "io.speedata.lumen".into(),
                key: "NSStatusItem VisibleCC Item-0".into(),
            }]
        );
        assert!(
            found[0]
                .remedy()
                .contains(r#"defaults delete io.speedata.lumen "NSStatusItem VisibleCC Item-0""#)
        );
        // The control: the same key set true is what every other app on a Mac has.
        f.status_item_prefs = vec![pref(Some(true))];
        assert!(findings(&f).is_empty(), "{:?}", findings(&f));
    }

    #[test]
    fn a_defaults_read_line_is_parsed_in_both_key_forms() {
        // The lines as `defaults read <domain>` prints them. `collect()` shells out, so the
        // parsing is checked here, on every host.
        let parse = |line| parse_pref_line("Lumen", line).map(|p| (p.key, p.visible));
        let key = |k: &str| k.to_string();
        assert_eq!(
            parse(r#"    "NSStatusItem VisibleCC Item-0" = 0;"#),
            Some((key("NSStatusItem VisibleCC Item-0"), Some(false)))
        );
        assert_eq!(
            parse(r#"    "NSStatusItem Visible Item-0" = 1;"#),
            Some((key("NSStatusItem Visible Item-0"), Some(true)))
        );
        // Where the item sat, not whether it shows.
        assert_eq!(
            parse(r#"    "NSStatusItem Preferred Position Item-0" = 612;"#),
            Some((key("NSStatusItem Preferred Position Item-0"), None))
        );
        assert_eq!(parse(r#"    NSWindowFrame = "0 0 800 600";"#), None);
        assert_eq!(parse("{"), None);
    }

    #[test]
    fn a_manager_that_is_installed_but_not_running_is_not_a_finding() {
        // Only a *running* manager can be hiding a status item.
        let mut f = healthy();
        f.menu_bar_managers_installed = vec!["Ice".into()];
        assert!(findings(&f).is_empty(), "{:?}", findings(&f));

        let out = render(&f);
        assert!(out.contains("installed but not running"), "{out}");
        assert!(out.contains("cannot be hiding it"), "{out}");
    }

    #[test]
    fn a_manager_name_that_is_a_substring_of_a_system_path_is_not_detected() {
        // The real defect. `contains("ice")` over the whole ps output matched "CoreServices",
        // "XPCServices" and 128 other lines on a stock install, so Ice was reported as present on
        // a machine where it has never been installed. This asserts the shape of the fix — the
        // manager list is only ever populated from an "/<Name>.app/" path match — at the level a
        // reader can check, since `collect()` itself shells out and cannot be unit-tested.
        for line in [
            "/System/Library/Frameworks/CoreServices.framework/Versions/A/Support/fseventsd",
            "/System/Library/PrivateFrameworks/XPCServices.framework/xpcservice",
        ] {
            assert!(
                line.to_lowercase().contains("ice"),
                "the substring really is there, which is why the old check matched: {line}"
            );
            assert!(
                !line.contains("/Ice.app/"),
                "but the path form must not match: {line}"
            );
        }
    }

    #[test]
    fn a_running_manager_is_still_a_finding() {
        let mut f = healthy();
        f.menu_bar_managers = vec!["Bartender".into()];
        let found = findings(&f);
        assert!(
            matches!(found[0], Finding::MenuBarManager { .. }),
            "{found:?}"
        );
        assert!(found[0].remedy().contains("Bartender"));
        assert!(render(&f).contains("managers RUNNING"));
    }

    #[test]
    fn a_position_key_alone_is_not_a_finding() {
        // Every machine with a working tray has one of these. Reporting it would send people
        // chasing a non-problem — this machine has exactly this shape and the tray works.
        let mut f = healthy();
        f.status_item_prefs = vec![StatusItemPref {
            domain: "io.speedata.lumen".into(),
            key: "NSStatusItem Preferred Position Item-0".into(),
            visible: None,
        }];
        assert!(findings(&f).is_empty());
    }

    #[test]
    fn a_visible_true_key_is_not_a_finding_either() {
        let mut f = healthy();
        f.status_item_prefs = vec![StatusItemPref {
            domain: "Lumen".into(),
            key: "NSStatusItem Visible Item-0".into(),
            visible: Some(true),
        }];
        assert!(findings(&f).is_empty());
    }

    #[test]
    fn both_domains_are_reported_separately_because_lumen_has_written_to_both() {
        let mut f = healthy();
        f.status_item_prefs = vec![
            StatusItemPref {
                domain: "io.speedata.lumen".into(),
                key: "NSStatusItem Visible Item-0".into(),
                visible: Some(false),
            },
            StatusItemPref {
                domain: "Lumen".into(),
                key: "NSStatusItem Visible Item-0".into(),
                visible: Some(false),
            },
        ];
        let found = findings(&f);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].remedy().contains("io.speedata.lumen"));
        assert!(found[1].remedy().contains("delete Lumen "));
    }

    #[test]
    fn a_missing_log_is_only_reported_when_the_app_is_actually_running() {
        let mut f = healthy();
        f.log_size_bytes = None;
        assert!(findings(&f).iter().any(|x| matches!(x, Finding::NoLog)));

        // Not running: a log that was never written by a process that never started is not a
        // finding, it is arithmetic.
        f.processes.clear();
        let found = findings(&f);
        assert!(
            !found.iter().any(|x| matches!(x, Finding::NoLog)),
            "{found:?}"
        );
        assert!(found.iter().any(|x| matches!(x, Finding::NotRunning)));
    }

    #[test]
    fn a_login_item_aimed_at_a_deleted_app_is_reported() {
        // The other bug the same field report exposed: the cask unloaded the wrong label, so
        // every uninstall left one of these behind.
        let mut f = healthy();
        f.launch_agent = Some("/Users/x/Library/LaunchAgents/Lumen.plist".into());
        f.launch_agent_target_exists = Some(false);
        let found = findings(&f);
        assert!(
            found
                .iter()
                .any(|x| matches!(x, Finding::StaleLoginItem { .. })),
            "{found:?}"
        );
    }

    #[test]
    fn an_unknown_login_item_target_is_not_reported_as_broken() {
        let mut f = healthy();
        f.launch_agent = Some("/Users/x/Library/LaunchAgents/Lumen.plist".into());
        f.launch_agent_target_exists = None;
        assert!(findings(&f).is_empty());
    }

    #[test]
    fn the_report_renders_every_section_even_when_everything_is_missing() {
        // doctor is what someone runs when the app will not start, so it must never panic or
        // bail on absent data.
        let out = render(&Facts::default());
        for section in [
            "platform",
            "processes",
            "menu-bar status item",
            "monitors",
            "files",
            "findings",
        ] {
            assert!(out.contains(section), "missing {section} in:\n{out}");
        }
        assert!(out.contains("MISSING"));
    }

    #[test]
    fn a_hidden_icon_is_described_in_terms_a_user_can_act_on() {
        let f = Facts {
            status_item_prefs: vec![StatusItemPref {
                domain: "io.speedata.lumen".into(),
                key: "NSStatusItem Visible Item-0".into(),
                visible: Some(false),
            }],
            processes: vec!["Lumen".into()],
            log_size_bytes: Some(1),
            ..Default::default()
        };
        let out = render(&f);
        assert!(out.contains("FALSE  <-- hidden"), "{out}");
        assert!(out.contains("⌘-dragged"), "{out}");
        assert!(out.contains("defaults delete"), "{out}");
    }

    #[test]
    fn byte_sizes_are_human_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(18 * 1024 * 1024), "18.0 MB");
    }

    // ── repeat reads and the identity hook ───────────────────────────────────────

    fn settings_with(pre: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "hooks": { "PreToolUse": pre } })
    }

    fn entry(matcher: &str) -> serde_json::Value {
        serde_json::json!({
            "matcher": matcher,
            "hooks": [{ "type": "command", "command": "/h/lumen_read_intercept.sh" }]
        })
    }

    fn inert(f: &Facts) -> Option<Finding> {
        findings(f)
            .into_iter()
            .find(|x| matches!(x, Finding::RepeatReadsInert { .. }))
    }

    /// An install from before 1.8.0 registers the intercept on Read alone. Its repeat reads
    /// cannot tell a `/clear` from a repeat, so every Lumen call is answered in full, and
    /// `lumen doctor` is where that is said.
    #[test]
    fn a_1_7_0_install_is_reported_as_having_inert_repeat_reads() {
        let old = settings_with(serde_json::json!([entry("Read")]));
        let missing = identity_hook_missing(&old);
        assert_eq!(missing, ["smart_read", "recall_file", "compress_logs"]);

        let f = Facts {
            identity_hook_missing: Some(missing),
            ..healthy()
        };
        let out = render(&f);
        assert!(out.contains("Repeat reads are inert"), "{out}");
        assert!(
            out.contains("no PreToolUse hook for smart_read, recall_file, compress_logs"),
            "{out}"
        );
        assert!(out.contains("rm ~/.claude/lumen/.setup_done"), "{out}");
        assert!(out.contains("MISSING for smart_read"), "{out}");
    }

    fn resolved(
        env: Option<&str>,
        file: lumen_core::settings::FileState,
    ) -> lumen_core::settings::Resolved {
        lumen_core::settings::resolve(env, &file)
    }

    /// Off on purpose, by either switch: a missing hook then costs only the ledger's session
    /// column, and `lumen doctor` has nothing to say about repeat reads.
    #[test]
    fn a_user_who_turned_repeat_reads_off_is_not_told_the_hook_is_missing() {
        use lumen_core::settings::FileState;
        let missing = identity_hook_missing(&settings_with(serde_json::json!([entry("Read")])));
        for off in [
            resolved(Some("off"), FileState::Absent),
            resolved(None, FileState::Text(r#"{"repeat_reads": false}"#.into())),
            resolved(
                Some("0"),
                FileState::Text(r#"{"repeat_reads": true}"#.into()),
            ),
        ] {
            let f = Facts {
                identity_hook_missing: Some(missing.clone()),
                // What a full-results server records: no plan on any call.
                recent_repeat_plans: Some((0, 0)),
                repeat_reads: Some(off),
                ..healthy()
            };
            assert_eq!(inert(&f), None, "{:?}", f.repeat_reads);
            assert_eq!(findings(&f), Vec::<Finding>::new());
            let out = render(&f);
            assert!(!out.contains("inert"), "{out}");
            assert!(out.contains("OFF"), "{out}");
        }
    }

    /// The same machine with repeat reads on, or with nothing known about it, is loud.
    #[test]
    fn repeat_reads_on_with_the_hook_missing_is_loud_and_says_what_to_run() {
        use lumen_core::settings::FileState;
        let missing = identity_hook_missing(&settings_with(serde_json::json!([entry("Read")])));
        for state in [
            Some(resolved(None, FileState::Absent)),
            Some(resolved(Some("on"), FileState::Absent)),
            Some(resolved(
                None,
                FileState::Text(r#"{"repeat_reads": true}"#.into()),
            )),
            None,
        ] {
            let f = Facts {
                identity_hook_missing: Some(missing.clone()),
                repeat_reads: state,
                ..healthy()
            };
            let found = inert(&f).expect("loud");
            let fix = found.remedy();
            for must in [
                "rm ~/.claude/lumen/.setup_done",
                "Claude Code plugin",
                "lumen-cli",
                "LUMEN_REPEAT_READS=off",
            ] {
                assert!(fix.contains(must), "{must}: {fix}");
            }
        }
    }

    #[test]
    fn a_setting_that_cannot_be_read_is_reported_with_what_to_fix() {
        use lumen_core::settings::FileState;
        let file = "/data/lumen_settings.json".to_string();
        let f = Facts {
            repeat_reads: Some(resolved(None, FileState::Text("{broken".into()))),
            repeat_reads_file: Some(file.clone()),
            // Even with the hook missing, the off nobody chose is reported once, as itself.
            identity_hook_missing: Some(vec!["smart_read".into()]),
            ..healthy()
        };
        let found = findings(&f);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(matches!(
            &found[0],
            Finding::RepeatReadsSetting {
                problem: lumen_core::settings::Problem::File(_),
                ..
            }
        ));
        let out = render(&f);
        assert!(out.contains(&file), "{out}");
        assert!(out.contains("every read is in full"), "{out}");
        assert!(
            out.contains("turned off because the setting could not be read"),
            "{out}"
        );
        assert!(
            found[0].remedy().contains("repeat_reads"),
            "{}",
            found[0].remedy()
        );

        let f = Facts {
            repeat_reads: Some(resolved(Some("enabled"), FileState::Absent)),
            ..healthy()
        };
        let found = findings(&f);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].remedy().contains("unset it"),
            "{}",
            found[0].remedy()
        );
        assert!(render(&f).contains("\"enabled\""), "{}", render(&f));
    }

    #[test]
    fn the_remedy_for_an_unreadable_file_names_the_switch_in_the_app() {
        use lumen_core::settings::FileState;
        let f = Facts {
            repeat_reads: Some(resolved(None, FileState::Text("{broken".into()))),
            repeat_reads_file: Some("/data/lumen_settings.json".into()),
            ..healthy()
        };
        let fix = findings(&f)[0].remedy();
        assert!(
            fix.contains("Repeat reads switch in the Home window"),
            "{fix}"
        );
        assert!(fix.contains("rewrites"), "{fix}");

        // A bad variable is not something the switch can fix: the variable wins over the file.
        let f = Facts {
            repeat_reads: Some(resolved(Some("enabled"), FileState::Absent)),
            ..healthy()
        };
        let fix = findings(&f)[0].remedy();
        assert!(!fix.contains("Home window"), "{fix}");
    }

    #[test]
    fn a_healthy_default_install_says_on_by_default_and_reports_nothing() {
        let f = Facts {
            identity_hook_missing: Some(Vec::new()),
            repeat_reads: Some(resolved(None, lumen_core::settings::FileState::Absent)),
            ..healthy()
        };
        assert_eq!(findings(&f), Vec::<Finding>::new());
        assert!(render(&f).contains("on  (the default)"), "{}", render(&f));
    }

    #[test]
    fn the_env_block_of_claude_settings_is_where_a_server_gets_its_variable() {
        use lumen_core::ranked::Mode;
        use lumen_core::settings::{FileState, Source};
        let settings = serde_json::json!({ "env": { "LUMEN_REPEAT_READS": "off" } });
        let claude = repeat_reads_env_in(&settings);
        assert_eq!(claude.as_deref(), Some("off"));
        assert_eq!(repeat_reads_env_in(&serde_json::json!({})), None);

        // The shell says nothing: the block decides, and the report says so.
        let (r, from_claude) = decide_repeat_reads(None, claude.clone(), &FileState::Absent);
        assert_eq!(
            (r.mode, r.source, from_claude),
            (Mode::Off, Source::Env, true)
        );
        let f = Facts {
            repeat_reads: Some(r),
            repeat_reads_env_from_claude_settings: from_claude,
            ..healthy()
        };
        assert!(
            render(&f).contains("in ~/.claude/settings.json"),
            "{}",
            render(&f)
        );

        // An empty shell variable says nothing either.
        let (r, _) = decide_repeat_reads(Some(" ".into()), claude.clone(), &FileState::Absent);
        assert_eq!(r.mode, Mode::Off);
        // The shell's own value wins over the block, as it would for a server started from it.
        let (r, from_claude) = decide_repeat_reads(Some("on".into()), claude, &FileState::Absent);
        assert_eq!((r.mode, from_claude), (Mode::On, false));
        // A value that is not a string and not a word for a mode is reported, not skipped.
        let odd = serde_json::json!({ "env": { "LUMEN_REPEAT_READS": ["off"] } });
        let (r, _) = decide_repeat_reads(None, repeat_reads_env_in(&odd), &FileState::Absent);
        assert!(r.problem.is_some(), "{r:?}");
    }

    #[test]
    fn the_hook_on_all_three_tools_is_not_reported() {
        let new = settings_with(serde_json::json!([
            entry("Read"),
            entry("mcp__lumen__smart_read"),
            entry("mcp__lumen__recall_file"),
            entry("mcp__lumen__compress_logs"),
        ]));
        assert!(identity_hook_missing(&new).is_empty());
        // The two spellings Claude Code accepts for a list, and the catch-alls.
        for m in [
            "mcp__lumen__smart_read|mcp__lumen__recall_file|mcp__lumen__compress_logs",
            "mcp__lumen__.*",
            ".*",
            "*",
            "",
        ] {
            let one = settings_with(serde_json::json!([entry(m)]));
            assert!(identity_hook_missing(&one).is_empty(), "matcher {m:?}");
        }
        let f = Facts {
            identity_hook_missing: Some(Vec::new()),
            ..healthy()
        };
        assert_eq!(inert(&f), None);
        assert!(
            render(&f).contains("registered for all three tools"),
            "{}",
            render(&f)
        );
    }

    #[test]
    fn one_missing_tool_is_named_and_a_hook_with_no_command_does_not_count() {
        let two = settings_with(serde_json::json!([entry(
            "mcp__lumen__smart_read|mcp__lumen__compress_logs"
        ),]));
        assert_eq!(identity_hook_missing(&two), ["recall_file"]);

        let empty = serde_json::json!({ "hooks": { "PreToolUse": [
            { "matcher": "mcp__lumen__smart_read", "hooks": [] }
        ] } });
        assert_eq!(
            identity_hook_missing(&empty),
            ["smart_read", "recall_file", "compress_logs"]
        );
        // No hooks at all, and a hook of another server's tool, are the same as none.
        assert_eq!(identity_hook_missing(&serde_json::json!({})).len(), 3);
        let other = settings_with(serde_json::json!([entry("mcp__other__smart_read")]));
        assert_eq!(identity_hook_missing(&other).len(), 3);
    }

    /// settings.json alone cannot condemn a plugin install, whose hooks live in the plugin:
    /// what the server has answered can.
    #[test]
    fn the_ledger_overrules_a_settings_file_that_lacks_the_hook() {
        let missing = Some(vec!["smart_read".to_string()]);
        let identified = Facts {
            identity_hook_missing: missing.clone(),
            recent_repeat_plans: Some((1, 10)),
            ..healthy()
        };
        assert_eq!(inert(&identified), None, "the hook reached the server");

        let unidentified = Facts {
            identity_hook_missing: missing.clone(),
            recent_repeat_plans: Some((10, 10)),
            ..healthy()
        };
        assert!(inert(&unidentified).is_some());

        // Proven by the ledger alone, with settings.json absent or unreadable.
        let proven = Facts {
            identity_hook_missing: None,
            recent_repeat_plans: Some((4, 4)),
            ..healthy()
        };
        let out = render(&proven);
        assert!(out.contains("Repeat reads are inert"), "{out}");
        assert!(
            out.contains("4 of the last 4 calls were answered in full for want of an identity"),
            "{out}"
        );

        // Nothing known, or nothing yet, is not a finding.
        for facts in [
            Facts {
                identity_hook_missing: None,
                recent_repeat_plans: None,
                ..healthy()
            },
            Facts {
                identity_hook_missing: None,
                recent_repeat_plans: Some((0, 0)),
                ..healthy()
            },
        ] {
            assert_eq!(inert(&facts), None);
        }
    }

    #[test]
    fn the_ledger_is_read_for_its_newest_lumen_calls_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("lumen.db");
        let conn = lumen_core::meter::connect_db(&db).unwrap();
        let add = |routed_via: &str, plan: Option<&str>| {
            conn.execute(
                "INSERT INTO read_events(ts, tool, path, tokens_returned, full_tokens, \
                 saved_tokens, routed_via, repeat_plan) \
                 VALUES ('2026-01-01T00:00:00Z', 't', 'p', 1, 1, 0, ?1, ?2)",
                rusqlite::params![routed_via, plan],
            )
            .unwrap();
        };
        // A ledger with no calls yet.
        assert_eq!(recent_repeat_plans(&db), Some((0, 0)));
        add("smart_read", Some("fresh"));
        add("recall_file", Some(lumen_core::repeat::PLAN_UNIDENTIFIED));
        add("compress_logs", Some(lumen_core::repeat::PLAN_UNIDENTIFIED));
        // Neither a built-in Read nor a call that carried no plan is a repeat-read verdict.
        add("builtin_read", Some(lumen_core::repeat::PLAN_UNIDENTIFIED));
        add("smart_read", None);
        assert_eq!(recent_repeat_plans(&db), Some((2, 3)));

        // No database at all is unknown, not zero.
        assert_eq!(recent_repeat_plans(&dir.path().join("absent.db")), None);
    }
}
