//! Re-point Lumen's own entries in Claude's files after its data folder moved (F53 v2, §7.1).
//!
//! The entries are found by what they run, never by a name: an MCP entry whose command is
//! `lumen-mcp` (or whose key is `lumen`), in `mcpServers` and in every project's
//! `mcpServers`; a hook whose command is one of Lumen's shims or runs `lumen-mcp hook`; and
//! the top-level `LUMEN_*` values of settings.json's `env`. Every other entry and key is left
//! as it is, value for value.
//!
//! A value that sits inside the previous folder or inside its backup (`B/data`) is moved to
//! N, with one exception: a `command` is this build's `lumen-mcp`, never a path mapped into
//! `B/data/bin`, which holds the old binary.
//!
//! Nothing here runs at start yet: the app calls it from T7.

use super::{
    back_up, claude_json_path_in, fold_path, global_settings_path_in, hook_command, write_atomic,
};
use lumen_core::legacy::commit::{backup_dir, DATA_DIR};
use lumen_core::legacy::marker::MarkerV2;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// How many times a rewrite is rebuilt when another writer changes the file under it.
const ATTEMPTS: usize = 3;

// ── what counts as inside the moved folders ──────────────────────────────────

/// The folders whose files now live in N: the previous folder, and its backup's `data`.
pub(super) struct Moved {
    n: PathBuf,
    /// Each as one slash-separated string without a trailing slash.
    prefixes: Vec<String>,
}

fn slashed(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_string()
}

impl Moved {
    /// From a committed marker, for the install whose data folder is `n`.
    pub(super) fn of(m: &MarkerV2, n: &Path) -> Option<Moved> {
        if m.source_dir.is_empty() {
            return None;
        }
        let mut prefixes = vec![slashed(&m.source_dir)];
        if let Some(b) = backup_dir(n, m) {
            prefixes.push(slashed(&b.join(DATA_DIR).to_string_lossy()));
        }
        Some(Moved {
            n: n.to_path_buf(),
            prefixes,
        })
    }

    /// From a pre-release marker, which names the folder and not where it was: the name under
    /// each data root.
    pub(super) fn of_names(roots: &[PathBuf], source: &str, n: &Path) -> Moved {
        Moved {
            n: n.to_path_buf(),
            prefixes: roots
                .iter()
                .map(|r| slashed(&r.join(source).to_string_lossy()))
                .collect(),
        }
    }

    /// What follows the moved folder in `value`: `Some("")` for the folder itself. A tail that
    /// could walk out of it (`..`) is not inside.
    fn tail_inside(&self, value: &str) -> Option<String> {
        let s = slashed(value);
        let f = fold_path(&s);
        for p in &self.prefixes {
            let fp = fold_path(p);
            let tail = if f == fp {
                ""
            } else if f.starts_with(&format!("{fp}/")) {
                // Case folding keeps byte lengths (ASCII only), so the cut is the same.
                s.get(p.len() + 1..)?
            } else {
                continue;
            };
            if tail.split('/').any(|c| c == "..") {
                return None;
            }
            return Some(tail.to_string());
        }
        None
    }

    pub(super) fn inside(&self, value: &str) -> bool {
        self.tail_inside(value).is_some()
    }

    /// The same file in N.
    fn moved_path(&self, value: &str) -> Option<PathBuf> {
        let tail = self.tail_inside(value)?;
        let mut out = self.n.clone();
        for c in tail.split('/').filter(|c| !c.is_empty()) {
            out.push(c);
        }
        Some(out)
    }
}

// ── finding Lumen's entries ──────────────────────────────────────────────────

/// Where in an entry a value sits.
#[derive(Clone, Copy)]
pub(super) enum Slot<'a> {
    /// An MCP entry's `command`.
    McpCommand,
    /// One of an MCP entry's `args`.
    Arg,
    /// A value of `env`, by key.
    Env(&'a str),
    /// A hook's whole command line.
    Hook,
}

/// Decides, for one value of Lumen's, what it becomes (`None` leaves it).
pub(super) type Rewrite<'a> = dyn FnMut(Slot<'_>, &str) -> Option<String> + 'a;

/// A command's first word, read the way Claude Code reads it (a quote runs to its partner, a
/// backslash takes the next character), and what follows it.
fn first_word(command: &str) -> (String, &str) {
    let trimmed = command.trim_start();
    let mut out = String::new();
    let mut it = trimmed.char_indices();
    let mut end = trimmed.len();
    while let Some((i, c)) = it.next() {
        match c {
            '"' | '\'' => {
                for (_, d) in it.by_ref() {
                    if d == c {
                        break;
                    }
                    out.push(d);
                }
            }
            '\\' => out.push(it.next().map(|(_, d)| d).unwrap_or('\\')),
            c if c.is_whitespace() => {
                end = i;
                break;
            }
            c => out.push(c),
        }
    }
    (out, &trimmed[end..])
}

/// A program's file name, case-folded.
fn base_name(program: &str) -> String {
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// An MCP entry's `command` is one program path that Claude Code spawns without a shell: no
/// word split and no escapes, only a pair of quotes round the whole value is dropped.
fn is_mcp_command(command: &str) -> bool {
    is_mcp_program(command.trim().trim_matches(['"', '\'']))
}

fn is_mcp_program(program: &str) -> bool {
    let b = base_name(program);
    b.strip_suffix(".exe").unwrap_or(&b) == "lumen-mcp"
}

/// Whether a hook command is Lumen's: one of its two shims, or `lumen-mcp hook`.
fn is_lumen_hook(command: &str) -> bool {
    let (word, rest) = first_word(command);
    let b = base_name(&word);
    b == "lumen_meter.sh"
        || b == "lumen_read_intercept.sh"
        || (is_mcp_program(&word) && first_word(rest).0 == "hook")
}

fn visit_string(v: Option<&mut Value>, slot: Slot<'_>, f: &mut Rewrite<'_>) {
    if let Some(Value::String(s)) = v {
        if let Some(new) = f(slot, s) {
            *s = new;
        }
    }
}

fn visit_mcp_entry(key: &str, entry: &mut Value, f: &mut Rewrite<'_>) {
    let by_command = entry
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(is_mcp_command);
    if key != "lumen" && !by_command {
        return;
    }
    visit_string(entry.get_mut("command"), Slot::McpCommand, f);
    if let Some(args) = entry.get_mut("args").and_then(|a| a.as_array_mut()) {
        for a in args {
            visit_string(Some(a), Slot::Arg, f);
        }
    }
    visit_env(entry.get_mut("env"), |_| true, f);
}

fn visit_env(env: Option<&mut Value>, wanted: impl Fn(&str) -> bool, f: &mut Rewrite<'_>) {
    if let Some(env) = env.and_then(|e| e.as_object_mut()) {
        for (k, v) in env.iter_mut() {
            if wanted(k) {
                visit_string(Some(v), Slot::Env(k), f);
            }
        }
    }
}

fn visit_mcp_servers(servers: Option<&mut Value>, f: &mut Rewrite<'_>) {
    if let Some(map) = servers.and_then(|s| s.as_object_mut()) {
        for (key, entry) in map.iter_mut() {
            visit_mcp_entry(key, entry, f);
        }
    }
}

/// Every value of Lumen's in `~/.claude.json`: its MCP entries, global and per project.
pub(super) fn visit_claude_json(root: &mut Value, f: &mut Rewrite<'_>) {
    visit_mcp_servers(root.get_mut("mcpServers"), f);
    if let Some(projects) = root.get_mut("projects").and_then(|p| p.as_object_mut()) {
        for project in projects.values_mut() {
            visit_mcp_servers(project.get_mut("mcpServers"), f);
        }
    }
}

/// Every value of Lumen's in `~/.claude/settings.json`: its hooks, on any event, and the
/// top-level `LUMEN_*` values of `env`.
pub(super) fn visit_settings(root: &mut Value, f: &mut Rewrite<'_>) {
    if let Some(events) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in events.values_mut().filter_map(|e| e.as_array_mut()) {
            for entry in entries {
                let Some(hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                    continue;
                };
                for hook in hooks {
                    let is_ours = hook
                        .get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(is_lumen_hook);
                    if is_ours {
                        visit_string(hook.get_mut("command"), Slot::Hook, f);
                    }
                }
            }
        }
    }
    visit_env(root.get_mut("env"), |k| k.starts_with("LUMEN_"), f);
}

/// The values of Lumen's that still sit inside the moved folders, as "where value".
pub(super) fn stale_values(
    claude_json: Option<&mut Value>,
    settings: Option<&mut Value>,
    moved: &Moved,
) -> Vec<String> {
    let mut hits = Vec::new();
    let mut f = |slot: Slot<'_>, v: &str| {
        if moved.inside(v) {
            hits.push(match slot {
                Slot::McpCommand => format!("command {v}"),
                Slot::Arg => format!("arg {v}"),
                Slot::Env(k) => format!("{k} {v}"),
                Slot::Hook => format!("hook {v}"),
            });
        }
        None
    };
    if let Some(r) = claude_json {
        visit_claude_json(r, &mut f);
    }
    if let Some(r) = settings {
        visit_settings(r, &mut f);
    }
    hits
}

// ── the rewrite ──────────────────────────────────────────────────────────────

/// What a re-point did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Report {
    /// The files that were rewritten.
    pub rewritten: Vec<String>,
    /// Why something was skipped; the next start or Setup does it.
    pub problems: Vec<String>,
}

/// Re-point Lumen's entries in `~/.claude.json` and `~/.claude/settings.json`.
///
/// `mcp_bin` is this build's `lumen-mcp`, `new_db` the ledger now in use (in N).
pub(super) fn repoint_lumen_entries_in(
    home: &Path,
    marker: &MarkerV2,
    mcp_bin: &str,
    new_db: &Path,
) -> Report {
    repoint_with(home, marker, mcp_bin, new_db, &mut |_| {})
}

/// [`repoint_lumen_entries_in`], with `between` called after the new text is built and
/// before the file is read again to see whether anyone wrote it meanwhile. A test uses it to
/// be that writer.
pub(super) fn repoint_with(
    home: &Path,
    marker: &MarkerV2,
    mcp_bin: &str,
    new_db: &Path,
    between: &mut dyn FnMut(&Path),
) -> Report {
    let mut report = Report::default();
    let Some(n) = new_db.parent() else {
        report.problems.push("no data folder for the ledger".into());
        return report;
    };
    let Some(moved) = Moved::of(marker, n) else {
        return report;
    };
    let spool = n.join("faults.jsonl");
    let mut skipped: Vec<String> = Vec::new();
    let mut rewrite = |slot: Slot<'_>, v: &str| -> Option<String> {
        // A hook is a whole command line; what must be inside is its program.
        let probe = match slot {
            Slot::Hook => first_word(v).0,
            _ => v.to_string(),
        };
        if !moved.inside(&probe) {
            return None;
        }
        let mut need_mcp = || {
            if mcp_bin.is_empty() {
                skipped.push(format!("no lumen-mcp of this build to point {v} at"));
                None
            } else {
                Some(mcp_bin.to_string())
            }
        };
        match slot {
            Slot::McpCommand => need_mcp(),
            Slot::Arg => moved
                .moved_path(v)
                .map(|p| p.to_string_lossy().into_owned()),
            Slot::Env("LUMEN_DB") => Some(new_db.to_string_lossy().into_owned()),
            Slot::Env("LUMEN_FAULT_SPOOL") => Some(spool.to_string_lossy().into_owned()),
            Slot::Env(_) => moved
                .moved_path(v)
                .map(|p| p.to_string_lossy().into_owned()),
            Slot::Hook => {
                let (word, rest) = first_word(v);
                let program = if is_mcp_program(&word) {
                    need_mcp()?
                } else {
                    moved.moved_path(&word)?.to_string_lossy().into_owned()
                };
                Some(format!("{}{rest}", hook_command(Path::new(&program))))
            }
        }
    };

    let files: [(&str, PathBuf, fn(&mut Value, &mut Rewrite<'_>)); 2] = [
        (
            "claude.json",
            claude_json_path_in(home),
            visit_claude_json as fn(&mut Value, &mut Rewrite<'_>),
        ),
        (
            "settings.json",
            global_settings_path_in(home),
            visit_settings,
        ),
    ];
    let backup = backup_dir(n, marker);
    for (label, path, visit) in files {
        match repoint_file(
            &path,
            label,
            backup.as_deref(),
            visit,
            &mut rewrite,
            between,
        ) {
            Ok(true) => report.rewritten.push(label.into()),
            Ok(false) => {}
            Err(e) => report.problems.push(e),
        }
    }
    report.problems.append(&mut skipped);
    report
}

/// One file. `Ok(true)` when it was rewritten, `Ok(false)` when there was nothing to change or
/// nothing there.
fn repoint_file(
    path: &Path,
    label: &str,
    backup: Option<&Path>,
    visit: fn(&mut Value, &mut Rewrite<'_>),
    rewrite: &mut Rewrite<'_>,
    between: &mut dyn FnMut(&Path),
) -> Result<bool, String> {
    let mut copied = false;
    for _ in 0..ATTEMPTS {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                return Err(format!(
                    "cannot read {}: {e} — left unchanged",
                    path.display()
                ))
            }
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(false);
        }
        let mut root: Value = match serde_json::from_slice(&bytes) {
            Ok(v) if Value::is_object(&v) => v,
            Ok(_) => {
                return Err(format!(
                    "{} is not a JSON object — left unchanged",
                    path.display()
                ))
            }
            Err(e) => {
                return Err(format!(
                    "{} is not valid JSON ({e}) — left unchanged",
                    path.display()
                ))
            }
        };
        let mut changed = false;
        visit(&mut root, &mut |slot, v| {
            let new = rewrite(slot, v);
            changed |= new.is_some();
            new
        });
        if !changed {
            return Ok(false);
        }
        let text = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("serialize {}: {e} — left unchanged", path.display()))?;

        // The first time only: the original goes into the backup, so the undo is there
        // whatever happens to the `.lumen_bak` beside the file.
        if !copied {
            let Some(b) = backup else {
                return Err(format!(
                    "no backup folder to keep {label} in — left unchanged"
                ));
            };
            let dir = b.join("claude");
            let dest = dir.join(label);
            if !dest.exists() {
                std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(&dest, &bytes))
                    .map_err(|e| {
                        format!(
                            "cannot keep a copy of {} in {}: {e} — left unchanged",
                            path.display(),
                            dir.display()
                        )
                    })?;
            }
            copied = true;
        }

        between(path);
        // Claude Code writes these files too. Written over, its change would be lost, so a
        // file that changed since it was read is read again and the text rebuilt.
        if std::fs::read(path).ok().as_deref() != Some(&bytes[..]) {
            continue;
        }
        back_up(path)?;
        write_atomic(path, &text, 0)
            .map_err(|e| format!("cannot write {}: {e} — left unchanged", path.display()))?;
        return Ok(true);
    }
    Err(format!(
        "{} kept changing while it was rewritten — skipped; the next start or Setup does it",
        path.display()
    ))
}

// ── the CLI link ─────────────────────────────────────────────────────────────

/// Re-point the `lumen` link when it targets a file inside the moved folders (a DMG install's
/// copy under `bin/`), the way Install CLI makes it: a link to this build's `lumen-cli`.
///
/// `Ok(true)` when the link was replaced. Windows has a copy, not a link, so nothing is done
/// there.
#[cfg_attr(windows, allow(unused_variables))]
pub(super) fn repoint_cli_link(
    link: &Path,
    marker: &MarkerV2,
    n: &Path,
    cli: Option<&Path>,
) -> Result<bool, String> {
    #[cfg(unix)]
    {
        let Some(moved) = Moved::of(marker, n) else {
            return Ok(false);
        };
        let Ok(target) = std::fs::read_link(link) else {
            return Ok(false);
        };
        if !moved.inside(&target.to_string_lossy()) {
            return Ok(false);
        }
        let Some(cli) = cli else {
            return Err(format!(
                "{} points into the previous folder and this build has no lumen-cli to point it at",
                link.display()
            ));
        };
        let tmp = link.with_file_name(".lumen-link-tmp");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(cli, &tmp)
            .and_then(|_| std::fs::rename(&tmp, link))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("cannot re-point {}: {e}", link.display())
            })?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
