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
    /// Menu-bar managers found installed or running.
    pub menu_bar_managers: Vec<String>,
    pub log_path: Option<String>,
    /// `None` when the log file does not exist.
    pub log_size_bytes: Option<u64>,
    pub db_path: Option<String>,
    pub db_size_bytes: Option<u64>,
    /// `(x, y, w, h)` per monitor.
    pub monitors: Vec<(i64, i64, i64, i64)>,
    pub launch_agent: Option<String>,
    pub launch_agent_target_exists: Option<bool>,
}

/// What doctor concluded, most actionable first.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    /// A `NSStatusItem Visible …` key is false: the icon was ⌘-dragged off the menu bar and
    /// macOS remembers it. The cause of the reported symptom, and it has a one-line fix.
    IconHiddenByPreference { domain: String, key: String },
    /// A menu-bar manager is present, which can hide the icon into an overflow area.
    MenuBarManager { name: String },
    /// Lumen is not running at all, so no icon is expected.
    NotRunning,
    /// The app never wrote a log. Expected on 1.5.1 and earlier — the logger was debug-only.
    NoLog,
    /// A login item points at a path that no longer exists.
    StaleLoginItem { path: String },
}

impl Finding {
    /// The remedy, or an explanation when there is not one.
    pub fn remedy(&self) -> String {
        match self {
            Finding::IconHiddenByPreference { domain, key } => format!(
                "defaults delete {domain} \"{key}\" && killall Lumen; open -a Lumen"
            ),
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
        }
    }
}

/// Decide what is wrong, most actionable first.
///
/// Ordering is the point: a hidden-by-preference icon explains the whole symptom and has a
/// one-line fix, so it must never be buried under a note about log sizes.
pub fn findings(f: &Facts) -> Vec<Finding> {
    let mut out = Vec::new();

    for p in &f.status_item_prefs {
        if p.key.starts_with("NSStatusItem Visible ") && p.visible == Some(false) {
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

    out
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
        let _ = writeln!(s, "  (no NSStatusItem preferences — nothing is suppressing it)");
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
        let _ = writeln!(s, "  managers present: {}", f.menu_bar_managers.join(", "));
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "monitors");
    if f.monitors.is_empty() {
        let _ = writeln!(s, "  (not available from the CLI — needs the app; see docs/troubleshooting-the-tray.md step 3)");
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

fn describe(x: &Finding) -> String {
    match x {
        Finding::IconHiddenByPreference { domain, .. } => format!(
            "The menu-bar icon is hidden by a saved preference in {domain}. macOS writes this \
             when an icon is ⌘-dragged off the menu bar, and it persists across launches — the \
             icon is created and then immediately hidden, so nothing appears to fail."
        ),
        Finding::MenuBarManager { name } => {
            format!("{name} is present and may be holding the icon in an overflow area.")
        }
        Finding::NotRunning => "Lumen is not running, so there would be no icon.".to_string(),
        Finding::NoLog => {
            "Lumen is running but has written no log file.".to_string()
        }
        Finding::StaleLoginItem { path } => {
            format!("A login item at {path} points at an app that no longer exists.")
        }
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
        assert!(matches!(found[0], Finding::IconHiddenByPreference { .. }), "{found:?}");
        assert!(found[0].remedy().contains("defaults delete"));
        assert!(found[0].remedy().contains("NSStatusItem Visible Item-0"));
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
        assert!(!found.iter().any(|x| matches!(x, Finding::NoLog)), "{found:?}");
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
        assert!(found.iter().any(|x| matches!(x, Finding::StaleLoginItem { .. })), "{found:?}");
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
        for section in ["platform", "processes", "menu-bar status item", "monitors", "files", "findings"] {
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
}

// ── Collection ────────────────────────────────────────────────────────────────
//
// Deliberately below the tests: this half talks to the OS and cannot be unit-tested, so it is
// kept as thin as possible and every judgement lives in `findings` above.

/// The preference domains Lumen has written status-item state under. Both are real — this
/// machine has `NSStatusItem Preferred Position Item-0` in each, with different values.
pub const PREF_DOMAINS: [&str; 2] = ["io.speedata.lumen", "Lumen"];

const MENU_BAR_MANAGERS: [&str; 7] = [
    "Bartender", "Ice", "Hidden Bar", "Dozer", "Vanilla", "TopNotch", "Barbee",
];

#[cfg(target_os = "macos")]
fn sh(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
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
    }

    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().unwrap_or_default();

        // `defaults read <domain>` prints a plist; parsing the two keys we care about out of it
        // is enough, and far less machinery than linking CoreFoundation into the CLI.
        for domain in PREF_DOMAINS {
            let Some(text) = sh("defaults", &["read", domain]) else { continue };
            for line in text.lines() {
                let line = line.trim();
                if !line.contains("NSStatusItem") {
                    continue;
                }
                let Some((raw_key, raw_val)) = line.split_once('=') else { continue };
                let key = raw_key.trim().trim_matches('"').to_string();
                let val = raw_val.trim().trim_end_matches(';').trim();
                let visible = if key.starts_with("NSStatusItem Visible ") {
                    match val {
                        "0" | "false" | "NO" => Some(false),
                        "1" | "true" | "YES" => Some(true),
                        _ => None,
                    }
                } else {
                    None
                };
                f.status_item_prefs.push(StatusItemPref { domain: domain.to_string(), key, visible });
            }
        }

        if let Some(ps) = sh("/bin/ps", &["-ax", "-o", "comm"]) {
            f.processes = ps
                .lines()
                .filter(|l| l.contains("Lumen.app") || l.contains("lumen-daemon"))
                .map(|l| l.trim().to_string())
                .collect();
            for m in MENU_BAR_MANAGERS {
                if ps.to_lowercase().contains(&m.to_lowercase()) {
                    f.menu_bar_managers.push(m.to_string());
                }
            }
        }
        for m in MENU_BAR_MANAGERS {
            let p = format!("/Applications/{m}.app");
            if std::path::Path::new(&p).exists() && !f.menu_bar_managers.contains(&m.to_string()) {
                f.menu_bar_managers.push(m.to_string());
            }
        }

        let log = home.join("Library/Logs/io.speedata.lumen/Lumen.log");
        f.log_size_bytes = std::fs::metadata(&log).ok().map(|m| m.len());
        f.log_path = Some(log.to_string_lossy().to_string());

        let app = std::path::Path::new("/Applications/Lumen.app");
        if app.exists() {
            f.app_version = sh(
                "/usr/bin/defaults",
                &["read", "/Applications/Lumen.app/Contents/Info.plist", "CFBundleShortVersionString"],
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
