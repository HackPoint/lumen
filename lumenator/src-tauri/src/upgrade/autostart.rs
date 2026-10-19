//! Lumen's login entry under another name: found by the binary it runs, not by its name.
//!
//! The autostart plugin names its entry after the product (`Lumen.plist`, the `Lumen` Run value,
//! `Lumen.desktop`). An install made before that rename can have an entry under another name that
//! launches the very same executable. After an upgrade such an entry would start a second copy, or
//! keep starting Lumen after the user turned it off, so it is found by content and settled:
//!
//! | OS | where | it is Lumen's when | enabled means |
//! |---|---|---|---|
//! | macOS | `~/Library/LaunchAgents/*.plist` | `ProgramArguments[0]` or `Program` equals the running executable or ends in `.app/Contents/MacOS/<its name>` | the file is there and `Disabled` is not true |
//! | Windows | values of `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` | the program's file name equals the running executable's, case-folded | the value is there and `StartupApproved\Run\<value>` does not mark it disabled |
//! | Linux | `~/.config/autostart/*.desktop` | the `Exec=` program's base name equals the executable's (or `$APPIMAGE`'s) | no `Hidden=true`, no `X-GNOME-Autostart-enabled=false` |
//!
//! [`adopt_other_entries`] then does, for every such entry that is not the plugin's own:
//!
//! 1. Enabled, and the plugin's entry is not: turn the plugin's entry on.
//! 2. Disabled: write `.autostart_done` first if it is missing, so that [`crate::setup::ensure_autostart_once`]
//!    does not take the missing plugin entry for "never registered" and switch Lumen on (the user
//!    turned it off).
//! 3. Move the entry into the backup's `autostart/` folder (Windows: its value and its
//!    `StartupApproved` bytes into a `run.json` there, then remove both). A file keeps no name of
//!    its own there (`login.plist`, `login.desktop`, `-2`, ...), as the old name may be the previous app's.
//!
//! The plugin's own entry is the user's choice and is never touched. A crash between steps 1 and 3
//! leaves the entry for the next start, which only moves it. The result is one entry when Lumen was
//! on, none when it was off. Nothing is deleted.
//!
//! Every parser is pure and every effect sits behind a seam ([`RunKey`], [`AutoStart`], [`Ops`]),
//! so each platform's scan is tested on every host.

use crate::setup::AutoStart;
use lumen_core::legacy::retire::Ops;
use lumen_core::legacy::satellites::{move_into_backup, Platform};
use std::path::{Path, PathBuf};

/// One login entry that runs Lumen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file name (macOS, Linux) or the Run value's name (Windows).
    pub name: String,
    /// The entry's file, on macOS and Linux.
    pub path: Option<PathBuf>,
    pub enabled: bool,
    /// It is the autostart plugin's own entry.
    pub plugins_own: bool,
    /// Windows: the Run value's data.
    pub data: Option<String>,
    /// Windows: the `StartupApproved` bytes, if the value has any.
    pub approved: Option<Vec<u8>>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

fn fold_names(p: Platform) -> bool {
    p != Platform::Linux
}

fn names_eq(p: Platform, a: &str, b: &str) -> bool {
    if fold_names(p) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The last component of a path in either separator style.
fn base_name(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// The first word of a command line: quoted with `"` or `'`, or up to the first blank.
fn first_token(cmd: &str) -> &str {
    let cmd = cmd.trim_start();
    for q in ['"', '\''] {
        if let Some(rest) = cmd.strip_prefix(q) {
            return rest.split(q).next().unwrap_or(rest);
        }
    }
    cmd.split_whitespace().next().unwrap_or("")
}

/// The program of a Windows Run value: quoted, or unquoted. An unquoted path may hold spaces
/// (`C:\Program Files\Lumen\Lumen.exe --x`), so it ends at the first `.exe` that is followed by a
/// blank or the end; without one it is the first word.
fn windows_program(data: &str) -> &str {
    let data = data.trim_start();
    if data.starts_with('"') || data.starts_with('\'') {
        return first_token(data);
    }
    let lower = data.to_ascii_lowercase();
    for (i, _) in lower.match_indices(".exe") {
        let end = i + 4;
        if data[end..].chars().next().is_none_or(char::is_whitespace) {
            return &data[..end];
        }
    }
    first_token(data)
}

#[derive(Debug, Clone, PartialEq)]
enum Pl {
    Dict(Vec<(String, Pl)>),
    Arr(Vec<Pl>),
    Str(String),
    Bool(bool),
    Other,
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Open(String),
    Close(String),
    Empty(String),
    Text(String),
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('<') {
            if let Some(after) = r.strip_prefix("!--") {
                rest = after.split_once("-->").map_or("", |(_, t)| t);
                continue;
            }
            let Some((tag, after)) = r.split_once('>') else {
                break;
            };
            rest = after;
            if tag.starts_with('?') || tag.starts_with('!') {
                continue;
            }
            if let Some(name) = tag.strip_prefix('/') {
                out.push(Tok::Close(name.trim().to_string()));
            } else if let Some(name) = tag.strip_suffix('/') {
                out.push(Tok::Empty(
                    name.split_whitespace().next().unwrap_or("").to_string(),
                ));
            } else {
                out.push(Tok::Open(
                    tag.split_whitespace().next().unwrap_or("").to_string(),
                ));
            }
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            out.push(Tok::Text(unescape(&rest[..end])));
            rest = &rest[end..];
        }
    }
    out
}

struct Parser {
    toks: Vec<Tok>,
    at: usize,
}

impl Parser {
    /// The next token that is not blank text.
    fn next_struct(&mut self) -> Option<Tok> {
        while let Some(Tok::Text(t)) = self.toks.get(self.at) {
            if t.trim().is_empty() {
                self.at += 1;
            } else {
                break;
            }
        }
        let t = self.toks.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn parse_value(&mut self, open: Tok, depth: u32) -> Option<Pl> {
        if depth > 32 {
            return None;
        }
        match open {
            Tok::Empty(n) => Some(match n.as_str() {
                "true" => Pl::Bool(true),
                "false" => Pl::Bool(false),
                "string" => Pl::Str(String::new()),
                "dict" => Pl::Dict(Vec::new()),
                "array" => Pl::Arr(Vec::new()),
                _ => Pl::Other,
            }),
            Tok::Open(n) => match n.as_str() {
                "dict" => {
                    let mut v = Vec::new();
                    loop {
                        match self.next_struct()? {
                            Tok::Close(c) if c == "dict" => return Some(Pl::Dict(v)),
                            Tok::Open(k) if k == "key" => {
                                let key = match self.toks.get(self.at) {
                                    Some(Tok::Text(t)) => {
                                        let t = t.clone();
                                        self.at += 1;
                                        t
                                    }
                                    _ => String::new(),
                                };
                                match self.next_struct()? {
                                    Tok::Close(c) if c == "key" => {}
                                    _ => return None,
                                }
                                let o = match self.next_struct()? {
                                    o @ (Tok::Open(_) | Tok::Empty(_)) => o,
                                    _ => return None,
                                };
                                v.push((key, self.parse_value(o, depth + 1)?));
                            }
                            _ => return None,
                        }
                    }
                }
                "array" => {
                    let mut v = Vec::new();
                    loop {
                        let o = match self.next_struct()? {
                            Tok::Close(c) if c == "array" => return Some(Pl::Arr(v)),
                            o @ (Tok::Open(_) | Tok::Empty(_)) => o,
                            _ => return None,
                        };
                        v.push(self.parse_value(o, depth + 1)?);
                    }
                }
                _ => {
                    // A scalar: its text up to its closing tag.
                    let mut text = String::new();
                    loop {
                        match self.toks.get(self.at)? {
                            Tok::Text(t) => text.push_str(t),
                            Tok::Close(c) if *c == n => {
                                self.at += 1;
                                break;
                            }
                            _ => return None,
                        }
                        self.at += 1;
                    }
                    Some(if n == "string" {
                        Pl::Str(text)
                    } else {
                        Pl::Other
                    })
                }
            },
            _ => None,
        }
    }
}

/// The top-level value of an XML property list.
fn parse_plist(text: &str) -> Option<Pl> {
    let mut p = Parser {
        toks: tokens(text),
        at: 0,
    };
    match p.next_struct()? {
        Tok::Open(n) if n == "plist" => {}
        _ => return None,
    }
    let open = match p.next_struct()? {
        o @ (Tok::Open(_) | Tok::Empty(_)) => o,
        _ => return None,
    };
    p.parse_value(open, 0)
}

/// What a LaunchAgent plist runs, and whether it is switched off.
#[derive(Debug, PartialEq, Eq)]
struct Agent {
    program: Option<String>,
    disabled: bool,
}

fn agent_of(text: &str) -> Option<Agent> {
    let Pl::Dict(d) = parse_plist(text)? else {
        return None;
    };
    let get = |k: &str| d.iter().find(|(n, _)| n == k).map(|(_, v)| v);
    let first_arg = match get("ProgramArguments") {
        Some(Pl::Arr(a)) => match a.first() {
            Some(Pl::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    };
    let program = first_arg.or(match get("Program") {
        Some(Pl::Str(s)) => Some(s.clone()),
        _ => None,
    });
    Some(Agent {
        program,
        disabled: matches!(get("Disabled"), Some(Pl::Bool(true))),
    })
}

/// A `.desktop` file's `Exec` and the keys that switch it off.
#[derive(Debug, PartialEq, Eq)]
struct Desktop {
    exec: Option<String>,
    off: bool,
}

fn desktop_of(text: &str) -> Desktop {
    let mut in_entry = false;
    let mut d = Desktop {
        exec: None,
        off: false,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Exec" if d.exec.is_none() => d.exec = Some(v.to_string()),
            "Hidden" if v.eq_ignore_ascii_case("true") => d.off = true,
            "X-GNOME-Autostart-enabled" if v.eq_ignore_ascii_case("false") => d.off = true,
            _ => {}
        }
    }
    d
}

/// A Run value's `StartupApproved` bytes say the user switched it off in Task Manager or Settings.
/// `03` is "disabled by user" and `07` its variant; `02` and `06` are enabled. A value without
/// bytes is enabled. Reading an unknown first byte as enabled would switch Lumen on against the
/// user's choice, so only the known enabled markers and a missing value count as on.
fn approved_is_on(bytes: Option<&[u8]>) -> bool {
    match bytes.and_then(|b| b.first()) {
        None => true,
        Some(b) => !matches!(b, 0x03 | 0x07),
    }
}

// ── Scanning ──────────────────────────────────────────────────────────────────

/// The name of the executable the entry has to run, and the `$APPIMAGE` that stands in for it.
pub struct Exe<'a> {
    pub path: &'a str,
    pub appimage: Option<&'a str>,
}

fn runs_macos(cmd: &str, exe: &Exe) -> bool {
    let name = base_name(exe.path);
    let tail = format!(".app/Contents/MacOS/{name}").to_ascii_lowercase();
    let c = cmd.to_ascii_lowercase();
    c == exe.path.to_ascii_lowercase() || c.ends_with(&tail)
}

fn runs_linux(exec: &str, exe: &Exe) -> bool {
    let prog = base_name(first_token(exec));
    if prog.is_empty() {
        return false;
    }
    prog == base_name(exe.path) || exe.appimage.is_some_and(|a| prog == base_name(a))
}

fn runs_windows(data: &str, exe: &Exe) -> bool {
    let prog = base_name(windows_program(data));
    !prog.is_empty() && prog.eq_ignore_ascii_case(base_name(exe.path))
}

/// Read a plist as text. XML is parsed as it is; anything else (a binary plist) goes through
/// `plutil -convert xml1 -o -`, which is macOS-only.
fn plist_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"bplist") {
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "xml1", "-o", "-"])
            .arg(path)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Lumen's entries among the files of a LaunchAgents (macOS) or autostart (Linux) folder.
/// `plugin_name` is the name the plugin gives its own entry (the product name).
pub fn scan_files(platform: Platform, dir: &Path, exe: &Exe, plugin_name: &str) -> Vec<Entry> {
    let ext = match platform {
        Platform::MacOs => "plist",
        Platform::Linux => "desktop",
        Platform::Windows => return Vec::new(),
    };
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let Some(stem) = name
            .rsplit_once('.')
            .filter(|(_, e)| names_eq(platform, e, ext))
            .map(|(s, _)| s)
        else {
            continue;
        };
        // A symlink is somebody's arrangement, not an entry the plugin made.
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            continue;
        }
        let own = names_eq(platform, stem, plugin_name);
        let (runs, enabled) = match platform {
            Platform::MacOs => match plist_text(&path).and_then(|t| agent_of(&t)) {
                Some(a) => (
                    a.program.as_deref().is_some_and(|p| runs_macos(p, exe)),
                    !a.disabled,
                ),
                None => (false, false),
            },
            _ => match std::fs::read_to_string(&path) {
                Ok(t) => {
                    let d = desktop_of(&t);
                    (
                        d.exec.as_deref().is_some_and(|e| runs_linux(e, exe)),
                        !d.off,
                    )
                }
                Err(_) => (false, false),
            },
        };
        if runs || own {
            out.push(Entry {
                name,
                path: Some(path),
                enabled,
                plugins_own: own,
                data: None,
                approved: None,
            });
        }
    }
    out
}

/// A value of the Run key, with its `StartupApproved` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunValue {
    pub name: String,
    pub data: String,
    pub approved: Option<Vec<u8>>,
}

/// `HKCU\...\Run`, as a seam.
pub trait RunKey {
    fn values(&self) -> Result<Vec<RunValue>, String>;
    /// Remove the Run value and its `StartupApproved` value.
    fn delete_entry(&self, name: &str) -> Result<(), String>;
}

/// Lumen's values of the Run key.
pub fn scan_registry(
    key: &dyn RunKey,
    exe: &Exe,
    plugin_value: &str,
) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for v in key.values()? {
        let own = v.name.eq_ignore_ascii_case(plugin_value);
        if own || runs_windows(&v.data, exe) {
            out.push(Entry {
                name: v.name,
                path: None,
                enabled: approved_is_on(v.approved.as_deref()),
                plugins_own: own,
                data: Some(v.data),
                approved: v.approved,
            });
        }
    }
    Ok(out)
}

// ── Settling ──────────────────────────────────────────────────────────────────

/// Where to look, and what to call on.
pub struct Env<'a> {
    pub platform: Platform,
    pub exe: Exe<'a>,
    /// The product name, which the plugin gives its entry.
    pub plugin_name: &'a str,
    /// `~/Library/LaunchAgents` or the autostart folder. Unused on Windows.
    pub dir: Option<&'a Path>,
    /// The Run key. Windows only.
    pub run_key: Option<&'a dyn RunKey>,
    pub plugin: &'a dyn AutoStart,
    /// `~/.claude/lumen/.autostart_done`.
    pub marker: &'a Path,
    /// The backup folder (B). Entries go to `B/autostart/`.
    pub backup: &'a Path,
    pub new_id: &'a str,
    pub ops: &'a dyn Ops,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Entries moved into the backup (their names).
    pub moved: Vec<String>,
    /// The plugin's entry was switched on because another entry was on.
    pub enabled_plugin: bool,
    /// Entries that stayed, with why; the next start tries again.
    pub problems: Vec<String>,
}

/// Every Lumen entry on this machine, the plugin's own included.
pub fn scan(env: &Env) -> Result<Vec<Entry>, String> {
    match env.platform {
        Platform::Windows => match env.run_key {
            Some(k) => scan_registry(k, &env.exe, env.plugin_name),
            None => Ok(Vec::new()),
        },
        p => match env.dir {
            Some(d) => Ok(scan_files(p, d, &env.exe, env.plugin_name)),
            // No folder to look in on this platform: there is nothing to find.
            None => Ok(Vec::new()),
        },
    }
}

/// Write the one-time-registration marker if it is missing, as `ensure_autostart_once` does.
fn record_opt_out(marker: &Path, exe: &str) -> Result<(), String> {
    if marker.exists() {
        return Ok(());
    }
    marker
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(marker, exe))
        .map_err(|e| format!("{}: {e}", marker.display()))
}

/// `<dir>/<base>`, then `<base>-2`, `-3`, ... until unused.
fn unused(dir: &Path, base: &str) -> PathBuf {
    let mut p = dir.join(base);
    let mut n = 2u32;
    while std::fs::symlink_metadata(&p).is_ok() {
        p = dir.join(format!("{base}-{n}"));
        n += 1;
    }
    p
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Put a Windows entry's value and `StartupApproved` bytes into `B/autostart/run.json`.
fn write_run_json(env: &Env, e: &Entry) -> Result<(), String> {
    use std::io::Write;
    let dir = env.backup.join("autostart");
    std::fs::create_dir_all(&dir).map_err(|x| format!("{}: {x}", dir.display()))?;
    let path = unused(&dir, "run.json");
    let body = serde_json::json!({
        "name": e.name,
        "data": e.data,
        "approved": e.approved.as_deref().map(hex),
    });
    let mut f = std::fs::File::create_new(&path).map_err(|x| format!("{}: {x}", path.display()))?;
    f.write_all(body.to_string().as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|x| format!("{}: {x}", path.display()))
}

/// Settle every Lumen entry that is not the plugin's own (see the module comment). Run at every
/// start once a migration has a backup; a no-op when there is nothing to settle.
pub fn adopt_other_entries(env: &Env) -> Outcome {
    let mut out = Outcome::default();
    let entries = match scan(env) {
        Ok(e) => e,
        Err(e) => {
            out.problems
                .push(format!("could not read the login entries: {e}"));
            return out;
        }
    };
    // Whether the plugin's own entry is on, asked once and kept.
    let mut plugin_on: Option<bool> = None;
    for e in entries.iter().filter(|e| !e.plugins_own) {
        if e.enabled {
            let on = match plugin_on {
                Some(on) => on,
                None => match env.plugin.is_enabled() {
                    Ok(true) => true,
                    Ok(false) => match env.plugin.enable() {
                        Ok(()) => {
                            out.enabled_plugin = true;
                            true
                        }
                        Err(x) => {
                            out.problems
                                .push(format!("{}: could not turn the login item on: {x}", e.name));
                            continue;
                        }
                    },
                    Err(x) => {
                        out.problems.push(format!(
                            "{}: could not read the login item's state: {x}",
                            e.name
                        ));
                        continue;
                    }
                },
            };
            plugin_on = Some(on);
        } else if let Err(x) = record_opt_out(env.marker, env.exe.path) {
            // Moving the entry without the record would let the next start switch Lumen on.
            out.problems
                .push(format!("{}: could not record the opt-out: {x}", e.name));
            continue;
        }
        match settle(env, e) {
            Ok(()) => out.moved.push(e.name.clone()),
            Err(x) => out.problems.push(format!("{}: {x}", e.name)),
        }
    }
    out
}

/// Move one entry into the backup.
fn settle(env: &Env, e: &Entry) -> Result<(), String> {
    if let Some(path) = &e.path {
        // A neutral name: the entry's own may be the previous app's (`<id>.plist`), and that
        // name must not reappear inside the backup. The `-2`, `-3` rule keeps several apart.
        let ext = Path::new(&e.name)
            .extension()
            .map(|x| x.to_string_lossy().into_owned());
        let rel = Path::new("autostart").join(match ext {
            Some(x) if x.chars().all(|c| c.is_ascii_alphanumeric()) => format!("login.{x}"),
            _ => "login".to_string(),
        });
        move_into_backup(path, &rel, env.backup, env.new_id, false, env.ops).map(|_| ())
    } else {
        let key = env.run_key.ok_or("no Run key")?;
        write_run_json(env, e)?;
        key.delete_entry(&e.name)
    }
}

// ── The real Run key ──────────────────────────────────────────────────────────

// T7 wires the real key into the start; until then nothing names it.
#[cfg(windows)]
#[allow(unused_imports)]
pub use registry::HkcuRun;

#[cfg(windows)]
mod registry {
    use super::{RunKey, RunValue};
    use std::io::ErrorKind;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

    /// `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
    pub struct HkcuRun;

    impl RunKey for HkcuRun {
        fn values(&self) -> Result<Vec<RunValue>, String> {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let run = match hkcu.open_subkey_with_flags(RUN, KEY_READ) {
                Ok(k) => k,
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(e.to_string()),
            };
            let approved = hkcu.open_subkey_with_flags(APPROVED, KEY_READ).ok();
            let mut out = Vec::new();
            for item in run.enum_values() {
                let (name, _) = item.map_err(|e| e.to_string())?;
                // Only text values can name a program.
                let Ok(data) = run.get_value::<String, _>(&name) else {
                    continue;
                };
                let approved = approved
                    .as_ref()
                    .and_then(|k| k.get_raw_value(&name).ok())
                    .map(|v| v.bytes.into_owned());
                out.push(RunValue {
                    name,
                    data,
                    approved,
                });
            }
            Ok(out)
        }

        fn delete_entry(&self, name: &str) -> Result<(), String> {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let run = hkcu
                .open_subkey_with_flags(RUN, KEY_SET_VALUE)
                .map_err(|e| e.to_string())?;
            match run.delete_value(name) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            if let Ok(k) = hkcu.open_subkey_with_flags(APPROVED, KEY_SET_VALUE) {
                match k.delete_value(name) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(())
        }
    }
}

/// The real machine's entry folder: `~/Library/LaunchAgents` or the XDG autostart folder.
pub fn real_dir(platform: Platform) -> Option<PathBuf> {
    match platform {
        Platform::MacOs => dirs::home_dir().map(|h| h.join("Library/LaunchAgents")),
        Platform::Linux => dirs::config_dir().map(|c| c.join("autostart")),
        Platform::Windows => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::legacy::retire::Native;
    use std::cell::{Cell, RefCell};
    use tempfile::TempDir;

    const MAC_EXE: &str = "/Applications/Lumen.app/Contents/MacOS/Lumen";
    const LINUX_EXE: &str = "/opt/Lumen/Lumen";
    const WIN_EXE: &str = r"C:\Program Files\Lumen\Lumen.exe";
    const APPIMAGE: &str = "/home/u/Apps/Lumen_1.2.AppImage";
    const NEW: &str = "io.hackpoint.lumen";
    const ALL: [Platform; 3] = [Platform::MacOs, Platform::Linux, Platform::Windows];

    fn exe_of(p: Platform) -> &'static str {
        match p {
            Platform::MacOs => MAC_EXE,
            Platform::Linux => LINUX_EXE,
            Platform::Windows => WIN_EXE,
        }
    }

    /// A LaunchAgent plist as the auto-launch crate writes it (header and all).
    fn plist(args: &[&str], extra: &str) -> String {
        let strings: String = args
            .iter()
            .map(|a| format!("<string>{a}</string>"))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n  \
             <dict>\n  \
                 <key>Label</key>\n  \
                 <string>Lumen</string>\n  \
                 <key>AssociatedBundleIdentifiers</key>\n  \
                 <array><string>{NEW}</string></array>\n  \
                 <key>ProgramArguments</key>\n  \
                 <array>{strings}</array>\n  \
                 <key>RunAtLoad</key>\n  \
                 <true/>\n  \
                 {extra}</dict>\n\
             </plist>"
        )
    }

    /// A `.desktop` file as the auto-launch crate writes it.
    fn desktop(exec: &str, extra: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nVersion=1.0\nName=Lumen\n\
             Comment=Lumen startup script\nExec={exec}\nStartupNotify=false\nTerminal=false\n{extra}"
        )
    }

    const ENABLED: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const DISABLED: [u8; 12] = [3, 0, 0, 0, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x01];

    fn val(name: &str, data: &str, approved: Option<&[u8]>) -> RunValue {
        RunValue {
            name: name.into(),
            data: data.into(),
            approved: approved.map(<[u8]>::to_vec),
        }
    }

    /// One machine's login items, standing for the plugin, the Run key and the folder at once.
    struct Machine {
        platform: Platform,
        dir: TempDir,
        values: RefCell<Vec<RunValue>>,
        enables: Cell<u32>,
        fail_enable: Cell<bool>,
    }

    impl Machine {
        fn new(platform: Platform) -> Machine {
            Machine {
                platform,
                dir: TempDir::new().unwrap(),
                values: RefCell::new(Vec::new()),
                enables: Cell::new(0),
                fail_enable: Cell::new(false),
            }
        }

        fn put(&self, name: &str, body: &str) {
            std::fs::write(self.dir.path().join(name), body).unwrap();
        }

        fn exe(&self) -> Exe<'static> {
            Exe {
                path: exe_of(self.platform),
                appimage: (self.platform == Platform::Linux).then_some(APPIMAGE),
            }
        }

        fn own_file(&self) -> &'static str {
            match self.platform {
                Platform::MacOs => "Lumen.plist",
                _ => "Lumen.desktop",
            }
        }

        /// Seed the plugin's own entry, on or off.
        fn seed_own(&self, on: bool) {
            match self.platform {
                Platform::MacOs => self.put(
                    "Lumen.plist",
                    &plist(
                        &[MAC_EXE, "--hidden"],
                        if on { "" } else { "<key>Disabled</key><true/>" },
                    ),
                ),
                Platform::Linux => self.put(
                    "Lumen.desktop",
                    &desktop(
                        &format!("{LINUX_EXE} --hidden"),
                        if on { "" } else { "Hidden=true\n" },
                    ),
                ),
                Platform::Windows => self.values.borrow_mut().push(val(
                    "Lumen",
                    &format!("{WIN_EXE} --hidden "),
                    Some(if on { &ENABLED } else { &DISABLED }),
                )),
            }
        }

        /// Seed an entry under another name that runs Lumen.
        fn seed_old(&self, on: bool) {
            match self.platform {
                Platform::MacOs => self.put(
                    "com.old.login.plist",
                    &plist(
                        &[MAC_EXE],
                        if on { "" } else { "<key>Disabled</key><true/>" },
                    ),
                ),
                Platform::Linux => self.put(
                    "old-login.desktop",
                    &desktop(LINUX_EXE, if on { "" } else { "Hidden=true\n" }),
                ),
                Platform::Windows => self.values.borrow_mut().push(val(
                    "OldLogin",
                    &format!("\"{WIN_EXE}\" --minimized"),
                    Some(if on { &ENABLED } else { &DISABLED }),
                )),
            }
        }

        fn env<'a>(&'a self, marker: &'a Path, backup: &'a Path) -> Env<'a> {
            Env {
                platform: self.platform,
                exe: self.exe(),
                plugin_name: "Lumen",
                dir: Some(self.dir.path()),
                run_key: Some(self),
                plugin: self,
                marker,
                backup,
                new_id: NEW,
                ops: &Native,
            }
        }

        fn entries(&self, marker: &Path, backup: &Path) -> Vec<Entry> {
            scan(&self.env(marker, backup)).unwrap()
        }
    }

    impl RunKey for Machine {
        fn values(&self) -> Result<Vec<RunValue>, String> {
            Ok(self.values.borrow().clone())
        }
        fn delete_entry(&self, name: &str) -> Result<(), String> {
            self.values.borrow_mut().retain(|v| v.name != name);
            Ok(())
        }
    }

    impl AutoStart for Machine {
        fn is_enabled(&self) -> Result<bool, String> {
            let none = Path::new("");
            Ok(self
                .entries(none, none)
                .iter()
                .any(|e| e.plugins_own && e.enabled))
        }
        fn enable(&self) -> Result<(), String> {
            if self.fail_enable.get() {
                return Err("refused".into());
            }
            self.enables.set(self.enables.get() + 1);
            // Enabling writes the plugin's entry with the current path, over whatever was there.
            self.values.borrow_mut().retain(|v| v.name != "Lumen");
            self.seed_own(true);
            Ok(())
        }
        fn disable(&self) -> Result<(), String> {
            Err("not used".into())
        }
    }

    fn names(v: &[Entry]) -> Vec<(&str, bool, bool)> {
        v.iter()
            .map(|e| (e.name.as_str(), e.enabled, e.plugins_own))
            .collect()
    }

    #[test]
    fn autostart_entries_are_found_by_the_command_they_run() {
        // macOS: the plugin's own plist, an old-named one, one that is switched off and uses
        // `Program`, and the look-alikes that run something else.
        let m = Machine::new(Platform::MacOs);
        m.seed_own(true);
        m.put(
            "com.old.lumen-login.plist",
            &plist(&[MAC_EXE, "--hidden"], ""),
        );
        m.put(
            "old-moved.plist",
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>Program</key><string>/Volumes/Backup/Lumen.app/Contents/MacOS/Lumen</string>\
             <key>Disabled</key><true/></dict></plist>",
        );
        m.put(
            "lumen-sync.plist",
            &plist(&["/usr/local/bin/lumen-sync"], ""),
        );
        m.put(
            "helper.plist",
            &plist(&["/Applications/Lumen.app/Contents/MacOS/LumenHelper"], ""),
        );
        m.put("com.other.plist", &plist(&["/usr/bin/true", MAC_EXE], ""));
        m.put("broken.plist", "this is not a plist");
        m.put("notes.txt", MAC_EXE);
        let got = scan_files(Platform::MacOs, m.dir.path(), &m.exe(), "Lumen");
        assert_eq!(
            names(&got),
            [
                ("Lumen.plist", true, true),
                ("com.old.lumen-login.plist", true, false),
                ("old-moved.plist", false, false),
            ]
        );

        // Linux: quotes and field codes, `Hidden=true`, the GNOME switch, an AppImage, and an
        // `Exec` that sits in another group of the file.
        let m = Machine::new(Platform::Linux);
        m.seed_own(true);
        m.put("quoted.desktop", &desktop("\"/opt/Lumen/Lumen\" %U", ""));
        m.put("off-hidden.desktop", &desktop(LINUX_EXE, "Hidden=true\n"));
        m.put(
            "off-gnome.desktop",
            &desktop(LINUX_EXE, "X-GNOME-Autostart-enabled=false\n"),
        );
        m.put("appimage.desktop", &desktop(&format!("{APPIMAGE} --x"), ""));
        m.put("lumen-sync.desktop", &desktop("/usr/bin/lumen-sync", ""));
        m.put("other.desktop", &desktop("/usr/bin/other", ""));
        m.put(
            "groups.desktop",
            "[Desktop Entry]\nExec=/usr/bin/other\n[Desktop Action x]\nExec=/opt/Lumen/Lumen\n",
        );
        let got = scan_files(Platform::Linux, m.dir.path(), &m.exe(), "Lumen");
        assert_eq!(
            names(&got),
            [
                ("Lumen.desktop", true, true),
                ("appimage.desktop", true, false),
                ("off-gnome.desktop", false, false),
                ("off-hidden.desktop", false, false),
                ("quoted.desktop", true, false),
            ]
        );

        // Windows: the plugin's value as the crate writes it (unquoted, trailing blank), a quoted
        // value with arguments, a value switched off in Settings, no StartupApproved at all, and
        // the look-alikes.
        let m = Machine::new(Platform::Windows);
        m.seed_own(true);
        *m.values.borrow_mut() = vec![
            val("Lumen", &format!("{WIN_EXE} "), Some(&ENABLED)),
            val(
                "OldLogin",
                &format!("\"{WIN_EXE}\" --minimized"),
                Some(&ENABLED),
            ),
            val(
                "Lumen Off",
                "\"c:\\program files\\lumen\\LUMEN.EXE\"",
                Some(&DISABLED),
            ),
            val("Bare", r"C:\Apps\Lumen.exe", None),
            val(
                "Spotify",
                r"C:\Users\u\AppData\Roaming\Spotify\Spotify.exe --autostart --minimized",
                Some(&ENABLED),
            ),
            val("lumen-sync", r#""C:\Tools\lumen-sync.exe""#, Some(&ENABLED)),
            val("Updater", r#""C:\Program Files\Lumen\Updater.exe""#, None),
        ];
        let got = scan_registry(&m, &m.exe(), "Lumen").unwrap();
        assert_eq!(
            names(&got),
            [
                ("Lumen", true, true),
                ("OldLogin", true, false),
                ("Lumen Off", false, false),
                ("Bare", true, false),
            ]
        );
        assert_eq!(got[2].approved.as_deref(), Some(&DISABLED[..]));
    }

    #[test]
    fn the_parsers_read_what_the_tools_write() {
        // Entities, an empty string, a nested dictionary and a comment do not derail the plist.
        let a = agent_of(
            "<plist version=\"1.0\"><!-- c --><dict><key>EnvironmentVariables</key><dict>\
             <key>A</key><string/></dict><key>ProgramArguments</key><array>\
             <string>/Users/a&amp;b/Lumen.app/Contents/MacOS/Lumen</string></array>\
             <key>Disabled</key><false/></dict></plist>",
        )
        .unwrap();
        assert_eq!(
            a.program.as_deref(),
            Some("/Users/a&b/Lumen.app/Contents/MacOS/Lumen")
        );
        assert!(!a.disabled);
        assert!(agent_of("<plist><array/></plist>").is_none());
        assert!(agent_of("<dict>").is_none());

        // An unquoted Windows path with spaces ends at `.exe`.
        assert_eq!(
            windows_program(r"C:\Program Files\Lumen\Lumen.exe --a b"),
            r"C:\Program Files\Lumen\Lumen.exe"
        );
        assert_eq!(windows_program(r"C:\x\Lumen.exe2 --a"), r"C:\x\Lumen.exe2");
        assert_eq!(base_name(r"C:\a/b\Lumen.exe"), "Lumen.exe");
        assert!(!approved_is_on(Some(&[3, 0])));
        assert!(!approved_is_on(Some(&[7, 0])));
        assert!(approved_is_on(Some(&[6, 0])));
        assert!(approved_is_on(None));
        assert!(approved_is_on(Some(&[])));
    }

    /// The marker, the backup and the machine, in one place.
    fn run(m: &Machine, t: &Path) -> Outcome {
        adopt_other_entries(&m.env(&t.join("lumen/.autostart_done"), &t.join("N/backup")))
    }

    #[test]
    fn an_entry_under_another_name_that_was_on_stays_on_with_one_entry() {
        for p in ALL {
            // Nothing of the plugin's own yet: the old entry is the only thing keeping Lumen on.
            let m = Machine::new(p);
            let t = TempDir::new().unwrap();
            m.seed_old(true);
            let out = run(&m, t.path());
            assert_eq!(out.problems, Vec::<String>::new(), "{p:?}");
            assert!(out.enabled_plugin, "{p:?}");
            assert_eq!(out.moved.len(), 1, "{p:?}");
            assert_eq!(m.enables.get(), 1, "{p:?}");
            let left = m.entries(&t.path().join("m"), &t.path().join("b"));
            assert_eq!(left.len(), 1, "{p:?}: exactly one entry: {left:?}");
            assert!(left[0].plugins_own && left[0].enabled, "{p:?}");
            assert!(
                t.path().join("N/backup/autostart").is_dir(),
                "{p:?}: the old entry is in the backup"
            );

            // The plugin's entry is already on: the old one only moves.
            let m = Machine::new(p);
            let t = TempDir::new().unwrap();
            m.seed_own(true);
            m.seed_old(true);
            let out = run(&m, t.path());
            assert!(
                !out.enabled_plugin && out.problems.is_empty(),
                "{p:?}: {out:?}"
            );
            assert_eq!(m.enables.get(), 0, "{p:?}");
            assert_eq!(m.entries(Path::new(""), Path::new("")).len(), 1, "{p:?}");

            // Running again changes nothing.
            let again = run(&m, t.path());
            assert_eq!(again, Outcome::default(), "{p:?}");
        }
    }

    #[test]
    fn an_old_named_entry_that_was_off_is_not_turned_on_by_the_next_start() {
        for p in ALL {
            let t = TempDir::new().unwrap();
            let marker = t.path().join("lumen/.autostart_done");

            // The control: with no record and no plugin entry, the one-time registration turns
            // Lumen on. That is what this test guards against.
            let control = Machine::new(p);
            assert!(crate::setup::ensure_autostart_once(
                &control,
                &t.path().join("control/.autostart_done"),
                exe_of(p)
            ));
            assert_eq!(control.enables.get(), 1);

            let m = Machine::new(p);
            m.seed_old(false);
            let out = run(&m, t.path());
            assert!(out.problems.is_empty(), "{p:?}: {out:?}");
            assert_eq!(out.moved.len(), 1, "{p:?}");
            assert!(!out.enabled_plugin, "{p:?}");
            assert!(
                marker.exists(),
                "{p:?}: the opt-out is recorded before the entry goes"
            );

            // The next start.
            let on = crate::setup::ensure_autostart_once(&m, &marker, exe_of(p));
            assert!(!on, "{p:?}: the next start switched Lumen back on");
            assert_eq!(m.enables.get(), 0, "{p:?}");
            assert!(m.entries(&marker, t.path()).is_empty(), "{p:?}");
        }
    }

    #[test]
    fn the_plugins_own_entry_is_never_touched() {
        for p in ALL {
            for on in [true, false] {
                let m = Machine::new(p);
                let t = TempDir::new().unwrap();
                m.seed_own(on);
                let file = m.dir.path().join(m.own_file());
                let before = std::fs::read(&file).ok();
                let values = m.values.borrow().clone();
                let out = run(&m, t.path());
                assert_eq!(out, Outcome::default(), "{p:?} on={on}");
                assert_eq!(m.enables.get(), 0, "{p:?} on={on}");
                assert_eq!(std::fs::read(&file).ok(), before, "{p:?} on={on}");
                assert_eq!(*m.values.borrow(), values, "{p:?} on={on}");
                assert!(
                    !t.path().join("lumen/.autostart_done").exists(),
                    "{p:?} on={on}: no marker"
                );
                assert!(
                    !t.path().join("N").exists(),
                    "{p:?} on={on}: no backup made"
                );
            }
        }
    }

    #[test]
    fn a_windows_entry_goes_into_run_json_with_its_bytes() {
        let m = Machine::new(Platform::Windows);
        let t = TempDir::new().unwrap();
        m.seed_old(false);
        run(&m, t.path());
        assert!(m.values.borrow().is_empty(), "the value is removed");
        let j: serde_json::Value = serde_json::from_slice(
            &std::fs::read(t.path().join("N/backup/autostart/run.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(j["name"], "OldLogin");
        assert_eq!(j["data"], format!("\"{WIN_EXE}\" --minimized"));
        assert_eq!(j["approved"], hex(&DISABLED));
    }

    /// Every name under `root`, as lower-case text.
    fn names_under(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = vec![root.to_path_buf()];
        while let Some(d) = todo.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                out.push(e.file_name().to_string_lossy().to_lowercase());
                todo.push(e.path());
            }
        }
        out
    }

    #[test]
    fn an_entry_named_after_the_previous_app_gets_a_neutral_name_in_the_backup() {
        // Moves the first time as the OS refuses (different volumes), so the fallback is covered.
        struct Exdev(Cell<bool>);
        impl Ops for Exdev {
            fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
                if !self.0.replace(true) {
                    return Err(std::io::ErrorKind::CrossesDevices.into());
                }
                std::fs::rename(from, to)
            }
            fn link_dir(&self, _: &Path, _: &Path) -> std::io::Result<()> {
                Err(std::io::ErrorKind::Unsupported.into())
            }
        }
        let x = "example.previous.app";
        for (p, file, body, ext) in [
            (
                Platform::MacOs,
                format!("{x}.plist"),
                plist(&[MAC_EXE], ""),
                "plist",
            ),
            (
                Platform::Linux,
                format!("{x}.desktop"),
                desktop(LINUX_EXE, ""),
                "desktop",
            ),
        ] {
            for exdev in [false, true] {
                let m = Machine::new(p);
                let t = TempDir::new().unwrap();
                m.seed_own(true);
                m.put(&file, &body);
                let backup = t.path().join("N/legacy-backup-2026-10-19");
                let marker = t.path().join("m");
                let ops = Exdev(Cell::new(!exdev));
                let mut env = m.env(&marker, &backup);
                env.ops = &ops;
                let out = adopt_other_entries(&env);
                assert_eq!(out.moved.len(), 1, "{p:?} {exdev}: {out:?}");
                assert!(out.problems.is_empty(), "{p:?} {exdev}: {out:?}");
                let at = if exdev {
                    m.dir.path().join("io.hackpoint.lumen")
                } else {
                    t.path().to_path_buf()
                };
                let seen = names_under(&at);
                assert!(
                    seen.iter().all(|n| !n.contains(x)),
                    "{p:?} {exdev}: the previous name survives inside B as {seen:?}"
                );
                assert!(
                    seen.contains(&format!("login.{ext}")),
                    "{p:?} {exdev}: {seen:?}"
                );
            }
        }
    }

    #[test]
    fn an_entry_that_cannot_move_stays_and_is_tried_again() {
        struct Refuse;
        impl Ops for Refuse {
            fn rename(&self, _: &Path, _: &Path) -> std::io::Result<()> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn link_dir(&self, _: &Path, _: &Path) -> std::io::Result<()> {
                Err(std::io::ErrorKind::Unsupported.into())
            }
        }
        let m = Machine::new(Platform::MacOs);
        let t = TempDir::new().unwrap();
        m.seed_old(true);
        let marker = t.path().join("m");
        let backup = t.path().join("b");
        let mut env = m.env(&marker, &backup);
        env.ops = &Refuse;
        let out = adopt_other_entries(&env);
        assert_eq!(out.moved.len(), 0);
        assert_eq!(out.problems.len(), 1, "{out:?}");
        assert!(m.dir.path().join("com.old.login.plist").exists());
        // Next start, nothing in the way.
        let env = m.env(&marker, &backup);
        let out = adopt_other_entries(&env);
        assert_eq!(out.moved.len(), 1, "{out:?}");
        assert!(!m.dir.path().join("com.old.login.plist").exists());
        assert!(backup.join("autostart/login.plist").exists());
    }

    #[test]
    fn an_entry_whose_replacement_could_not_be_made_is_left_where_it_is() {
        let m = Machine::new(Platform::Linux);
        let t = TempDir::new().unwrap();
        m.seed_old(true);
        m.fail_enable.set(true);
        let out = run(&m, t.path());
        assert!(out.moved.is_empty(), "{out:?}");
        assert_eq!(out.problems.len(), 1, "{out:?}");
        assert!(m.dir.path().join("old-login.desktop").exists());
    }

    #[test]
    fn an_opt_out_that_cannot_be_recorded_keeps_the_entry() {
        let m = Machine::new(Platform::Linux);
        let t = TempDir::new().unwrap();
        m.seed_old(false);
        // A file where the marker's folder should be.
        std::fs::write(t.path().join("lumen"), "x").unwrap();
        let out = run(&m, t.path());
        assert!(out.moved.is_empty(), "{out:?}");
        assert_eq!(out.problems.len(), 1, "{out:?}");
        assert!(m.dir.path().join("old-login.desktop").exists());
    }
}
