//! Find a previous Lumen install by what is in it, not by what it is called.
//!
//! A folder in a data root is Lumen's when its `lumen.db` has Lumen's tables and columns.
//! The identifier of the old app is therefore never needed: it is learned here, at runtime,
//! as the name of the folder that was found.
//!
//! Only the app calls [`discover`], once per start while N has no marker. It is never on the
//! hook path. It creates and changes no file: the ledger is read through SQLite's
//! `immutable=1`, which takes no locks and makes no `-shm` or `-wal`.

use super::{merge, name_eq, same_dir};
use rusqlite::{Connection, OpenFlags};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Tauri's generic development identifier. It stays a ledger-only source of its own.
const GENERIC_DEV_ID: &str = "com.tauri.dev";
const LEDGER: &str = "lumen.db";

/// The columns every released `read_events` has, from 1.1.4 to 1.9.0.
pub const READ_EVENTS_COLUMNS: [&str; 7] = [
    "ts",
    "tool",
    "path",
    "tokens_returned",
    "full_tokens",
    "saved_tokens",
    "routed_via",
];

/// The columns every released `turns` has, from 1.1.4 to 1.9.0.
pub const TURNS_COLUMNS: [&str; 7] = [
    "message_id",
    "session_id",
    "ts",
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// What looking at one `lumen.db` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Lumen's ledger.
    Lumen,
    /// Something else, or nothing.
    NotLumen,
    /// Cannot tell yet: retry at the next start, and do not settle.
    Unknown(String),
}

/// Whether the file at `db` is a Lumen ledger.
///
/// 1. The first 16 bytes are SQLite's magic.
/// 2. Opened with `immutable=1`, `read_events` and `turns` have the known columns.
/// 3. The case that is neither yes nor no: the main file lacks the tables but a non-empty
///    `-wal` sits beside it. That is a brand-new ledger whose schema has not been
///    checkpointed, so a writer is live. It is `Unknown`, not absent.
///
/// The connection is dropped before this returns, because an open handle blocks a rename
/// on Windows.
pub fn is_lumen_ledger(db: &Path) -> Verdict {
    let wal_has_bytes = wal_has_bytes(db);
    let mut head = [0u8; 16];
    let read = fs::File::open(db).and_then(|mut f| f.read(&mut head));
    match read {
        Ok(16) => {}
        Ok(0) if wal_has_bytes => {
            return Verdict::Unknown("the ledger file is empty and its -wal holds data".into());
        }
        Ok(_) => return Verdict::NotLumen,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Verdict::NotLumen,
        Err(e) => return Verdict::Unknown(format!("cannot read {}: {e}", db.display())),
    }
    if &head != b"SQLite format 3\0" {
        return Verdict::NotLumen;
    }
    match tables_have_lumens_columns(db) {
        Ok(true) => Verdict::Lumen,
        Ok(false) if wal_has_bytes => Verdict::Unknown(
            "the ledger has no tables in its main file, and its -wal holds data".into(),
        ),
        Ok(false) => Verdict::NotLumen,
        Err(e) => Verdict::Unknown(format!("cannot read {}: {e}", db.display())),
    }
}

fn wal_has_bytes(db: &Path) -> bool {
    let Some(name) = db.file_name() else {
        return false;
    };
    let mut wal = OsString::from(name);
    wal.push("-wal");
    fs::metadata(db.with_file_name(wal)).is_ok_and(|m| m.len() > 0)
}

/// `Ok(true)` when both tables exist with every known column.
fn tables_have_lumens_columns(db: &Path) -> Result<bool, String> {
    let uri = merge::ro_uri(&db.to_string_lossy())?;
    let conn = Connection::open_with_flags(
        format!("{uri}&immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    for (table, needed) in [
        ("read_events", &READ_EVENTS_COLUMNS),
        ("turns", &TURNS_COLUMNS),
    ] {
        let mut st = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(|e| e.to_string())?;
        let have: Vec<String> = st
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        if !needed
            .iter()
            .all(|c| have.iter().any(|h| h.eq_ignore_ascii_case(c)))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A folder in a data root that holds a Lumen ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `X`: the folder's name. Stored only in the marker, the pending file and the logs.
    pub name: String,
    /// `L`: `root/X`.
    pub dir: PathBuf,
    /// When the ledger or its `-wal` was last written.
    pub written: Option<SystemTime>,
}

/// What [`discover`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// The folder to migrate.
    pub chosen: Option<Candidate>,
    /// The other Lumen folders: reported with the import command, never moved.
    pub others: Vec<PathBuf>,
    /// Ledgers that could not be told apart yet.
    pub unknown: Vec<PathBuf>,
}

impl Discovery {
    /// A fresh install may write the `none` marker only when there is no candidate and
    /// nothing unknown. Anything unknown is reported and retried at the next start.
    pub fn may_settle(&self) -> bool {
        self.chosen.is_none() && self.others.is_empty() && self.unknown.is_empty()
    }
}

/// Where Lumen was pointed, in the order of trust: the pointer file, the `LUMEN_DB` of
/// Lumen's MCP entry in `~/.claude.json`, then the `default_db=` line of the meter shim.
/// Each is a ledger path, or a path inside the folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints(pub Vec<PathBuf>);

/// Read the hints under `home`. A file that is missing or does not parse gives none.
pub fn hints_in(home: &Path) -> Hints {
    let mut v = Vec::new();
    if let Ok(text) = fs::read_to_string(home.join(".lumen_db_path"))
        && !text.trim().is_empty()
    {
        v.push(PathBuf::from(text.trim()));
    }
    if let Ok(bytes) = fs::read(home.join(".claude.json"))
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
    {
        v.extend(mcp_entry_dbs(&json).into_iter().map(PathBuf::from));
    }
    if let Ok(text) = fs::read_to_string(home.join(".claude").join("lumen").join("lumen_meter.sh"))
        && let Some(p) = shim_default_db(&text)
    {
        v.push(PathBuf::from(p));
    }
    Hints(v)
}

/// `LUMEN_DB` of every Lumen entry in `mcpServers` and in `projects.*.mcpServers`. An entry
/// is Lumen's when its `command` runs a program named `lumen-mcp`, or its key is `lumen`.
/// Foreign entries are never read past their key and command.
pub fn mcp_entry_dbs(json: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut scan = |servers: Option<&serde_json::Value>| {
        let Some(map) = servers.and_then(|s| s.as_object()) else {
            return;
        };
        for (key, entry) in map {
            let command = entry.get("command").and_then(|c| c.as_str()).unwrap_or("");
            if (key == "lumen" || program_is_lumen_mcp(command))
                && let Some(db) = entry
                    .get("env")
                    .and_then(|e| e.get("LUMEN_DB"))
                    .and_then(|d| d.as_str())
                && !db.is_empty()
            {
                out.push(db.to_string());
            }
        }
    };
    scan(json.get("mcpServers"));
    if let Some(projects) = json.get("projects").and_then(|p| p.as_object()) {
        for project in projects.values() {
            scan(project.get("mcpServers"));
        }
    }
    out
}

/// The `command` of an MCP entry is one program, so it is not split at spaces. Quotes are
/// stripped, the name is case-folded and a trailing `.exe` dropped.
fn program_is_lumen_mcp(command: &str) -> bool {
    let c = command.trim().trim_matches(|ch| ch == '"' || ch == '\'');
    let base = c
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(c)
        .to_ascii_lowercase();
    base.strip_suffix(".exe").unwrap_or(&base) == "lumen-mcp"
}

/// The path in the shim's `default_db=` line, without its quotes.
fn shim_default_db(script: &str) -> Option<String> {
    let line = script
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("default_db="))?;
    let v = line.trim();
    let v = v
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(v);
    (!v.is_empty()).then(|| v.to_string())
}

/// Look for a previous Lumen install in `roots`, one level deep.
///
/// Skipped: `new_id` (N, case-folded where the file system folds), names starting with `.`,
/// symlinks and junctions, Tauri's generic development folder, and entries without a regular
/// file `lumen.db`. An entry that errors is skipped, except a `lumen.db` that is there and
/// cannot be read, which goes in [`Discovery::unknown`].
///
/// The choice: the one candidate; else the candidate the first hint points into; else the one
/// whose `lumen.db` or `-wal` was written most recently. The rest are `others`.
pub fn discover(roots: &[&Path], new_id: &str, hints: &Hints) -> Discovery {
    let mut found: Vec<Candidate> = Vec::new();
    let mut unknown: Vec<PathBuf> = Vec::new();
    let mut seen_roots: Vec<&Path> = Vec::new();
    for root in roots {
        if seen_roots.iter().any(|r| same_dir(r, root)) {
            continue;
        }
        seen_roots.push(root);
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy().into_owned();
            if name.starts_with('.')
                || name_eq(&file_name, new_id)
                || name_eq(&file_name, GENERIC_DEV_ID)
            {
                continue;
            }
            let dir = entry.path();
            // `is_symlink` is also true for a Windows junction (a name-surrogate reparse point).
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                _ => continue,
            }
            let db = dir.join(LEDGER);
            match fs::symlink_metadata(&db) {
                Ok(m) if m.is_file() => {}
                _ => continue,
            }
            match is_lumen_ledger(&db) {
                Verdict::Lumen => found.push(Candidate {
                    name,
                    written: last_written(&db),
                    dir,
                }),
                Verdict::NotLumen => {}
                Verdict::Unknown(_) => unknown.push(dir),
            }
        }
    }
    // Newest first, then by name, so the order never depends on read_dir.
    found.sort_by(|a, b| b.written.cmp(&a.written).then_with(|| a.name.cmp(&b.name)));
    unknown.sort();
    let pick = pick_index(&found, hints);
    let chosen = pick.map(|i| found.remove(i));
    Discovery {
        chosen,
        others: found.into_iter().map(|c| c.dir).collect(),
        unknown,
    }
}

/// `found` is newest first: the index of the one candidate, or the first hint's, or 0.
fn pick_index(found: &[Candidate], hints: &Hints) -> Option<usize> {
    if found.is_empty() {
        return None;
    }
    for hint in &hints.0 {
        if let Some(i) = found
            .iter()
            .position(|c| hint.ancestors().any(|a| same_dir(a, &c.dir)))
        {
            return Some(i);
        }
    }
    Some(0)
}

fn last_written(db: &Path) -> Option<SystemTime> {
    let mut wal = OsString::from(db.file_name()?);
    wal.push("-wal");
    let a = fs::metadata(db).and_then(|m| m.modified()).ok();
    let b = fs::metadata(db.with_file_name(wal))
        .and_then(|m| m.modified())
        .ok();
    a.max(b)
}

#[cfg(test)]
mod tests;
